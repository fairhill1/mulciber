//! Game-loop timing and input snapshots for Mulciber.
//!
//! The first runtime slice decouples a fixed-rate simulation from variable-rate rendering. It owns
//! the accumulator and bounded catch-up policy while leaving previous/current game state and its
//! interpolation with the application.
//!
//! The runtime also owns the display-interval frame pacer. Drain the graphics surface's
//! presentation feedback into [`Runtime::record_presented`] every frame and
//! [`Runtime::begin_frame`] advances simulation time by whole display intervals of the observed
//! cadence instead of by wall-clock gaps between build starts — wall-clock gaps reintroduce
//! visible judder on a steadily presenting display even with fixed simulation steps. Skipping the
//! feedback drain observably degrades every frame to the wall-clock fallback; check
//! [`RuntimeFrame::schedule`] or [`Runtime::pacing_report`] rather than assuming pacing engaged.
//!
//! The canonical loop, with presented instants standing in for a drained
//! `Surface::take_present_feedback`:
//!
//! ```
//! use std::time::Instant;
//! use mulciber_runtime::{Runtime, RuntimeConfig};
//!
//! # fn main() -> Result<(), mulciber_runtime::RuntimeConfigError> {
//! let mut runtime = Runtime::new(RuntimeConfig::fixed_hz(120)?, Instant::now());
//! // Every frame, before beginning it: drain presentation feedback into the runtime.
//! runtime.record_presented(Instant::now());
//! // Then begin the frame; once a cadence is estimated, deltas follow the display.
//! let frame = runtime.begin_frame(Instant::now());
//! let plan = frame.plan();
//! for _ in 0..plan.fixed_steps() {
//!     // fixed_update(plan.fixed_step());
//! }
//! // render(previous, current, plan.interpolation());
//! # Ok(())
//! # }
//! ```

mod input;
mod pacing;
mod timing;

use std::time::Instant;

pub use input::{InputSnapshot, ScrollSample};
use mulciber_platform::{InputEvent, WindowEvent};
pub use pacing::{FramePacer, FrameSchedule, IntervalSummary, PacingDiagnostics, PacingReport};
pub use timing::{FramePlan, RuntimeConfig, RuntimeConfigError};

/// Coordinates frame-scoped input, a fixed-rate simulation clock, and presentation pacing.
#[derive(Debug)]
pub struct Runtime {
    input: InputSnapshot,
    clock: timing::FrameClock,
    pacer: FramePacer,
}

/// One scoped runtime frame containing its timing plan and immutable input snapshot.
///
/// Dropping the frame clears pressed/released transitions and scroll samples while preserving held
/// controls. This includes early returns from surface acquisition or rendering errors.
#[derive(Debug)]
#[must_use = "a runtime frame must be consumed by update/render work"]
pub struct RuntimeFrame<'runtime> {
    input: &'runtime mut InputSnapshot,
    plan: FramePlan,
    schedule: FrameSchedule,
}

impl RuntimeFrame<'_> {
    /// Returns the fixed/variable timing work and render interpolation for this frame.
    #[must_use]
    pub const fn plan(&self) -> FramePlan {
        self.plan
    }

    /// Returns how this frame's delta was derived: paced onto the observed display cadence, or
    /// the wall-clock fallback while presentation feedback is missing or stale.
    pub const fn schedule(&self) -> FrameSchedule {
        self.schedule
    }

    /// Returns the held state and transitions accumulated for this frame.
    #[must_use]
    pub const fn input(&self) -> &InputSnapshot {
        self.input
    }
}

impl Drop for RuntimeFrame<'_> {
    fn drop(&mut self) {
        self.input.end_frame();
    }
}

impl Runtime {
    /// Starts a runtime clock at `started_at` with no accumulated simulation debt.
    #[must_use]
    pub fn new(config: RuntimeConfig, started_at: Instant) -> Self {
        let mut pacer = FramePacer::new();
        pacer.resume(started_at);
        Self {
            input: InputSnapshot::default(),
            clock: timing::FrameClock::new(config),
            pacer,
        }
    }

    /// Records one presented frame with the display time the backend reported for it.
    ///
    /// Drain the graphics surface's presentation feedback into this method (or
    /// [`Self::record_untimed_presented`]) every frame. Once the recorded timestamps yield a
    /// cadence estimate, [`Self::begin_frame`] advances simulation time by whole display
    /// intervals instead of wall-clock gaps.
    pub fn record_presented(&mut self, presented_at: Instant) {
        self.pacer.record_presented(presented_at);
    }

    /// Records one frame whose presentation completed without a reported display time, such as
    /// while the window is off screen.
    pub fn record_untimed_presented(&mut self) {
        self.pacer.record_untimed_presented();
    }

    /// Summarizes the presentation pacing recorded so far.
    #[must_use]
    pub fn pacing_report(&self) -> PacingReport {
        self.pacer.report()
    }

    /// Adds one ordered native input transition to the current snapshot.
    pub fn handle_input(&mut self, event: InputEvent) {
        self.input.handle_event(event);
    }

    /// Applies the input and rendering-lifecycle parts of one platform window event.
    ///
    /// Redraw, metrics, and close policy remain with the application. Lower-level input, suspend,
    /// and resume methods remain available when an application uses a different coordination shape.
    pub fn handle_window_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::Input(input) => self.handle_input(input),
            WindowEvent::RenderingSuspended => self.suspend(),
            WindowEvent::RenderingResumed(_) => self.resume(Instant::now()),
            _ => {}
        }
    }

    /// Returns the held state and transitions accumulated for the current frame.
    #[must_use]
    pub const fn input(&self) -> &InputSnapshot {
        &self.input
    }

    /// Begins a scoped frame with fixed simulation work, input, and render interpolation.
    ///
    /// While recorded presentation feedback yields a fresh cadence estimate, the frame delta is a
    /// whole number of display intervals; call this once per frame that will be presented, since
    /// every paced call advances at least one interval. Without feedback, or when it goes stale,
    /// the delta observably falls back to the wall-clock gap since the previous frame — see
    /// [`RuntimeFrame::schedule`].
    ///
    /// Dropping the returned frame automatically ends the input snapshot, including on early return.
    pub fn begin_frame(&mut self, now: Instant) -> RuntimeFrame<'_> {
        let (plan, schedule) = if self.clock.suspended() {
            (self.clock.idle_plan(), FrameSchedule::idle())
        } else {
            let schedule = self.pacer.schedule(now);
            (self.clock.advance_by(schedule.frame_delta()), schedule)
        };
        RuntimeFrame {
            input: &mut self.input,
            plan,
            schedule,
        }
    }

    /// Pauses frame timing and releases every held input control.
    ///
    /// The fractional fixed-step accumulator is preserved so rendering can resume without a small
    /// interpolation jump. Calls to [`Self::begin_frame`] while suspended schedule no updates.
    pub fn suspend(&mut self) {
        self.clock.suspend();
        self.input.release_all();
    }

    /// Resumes frame timing from `now` without treating the suspended interval as elapsed game time.
    pub fn resume(&mut self, now: Instant) {
        self.clock.resume();
        self.pacer.resume(now);
    }

    /// Returns whether frame timing is currently suspended.
    #[must_use]
    pub const fn suspended(&self) -> bool {
        self.clock.suspended()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use mulciber_platform::{ButtonState, InputEvent, KeyCode, Modifiers, WindowEvent};

    use super::{Runtime, RuntimeConfig};

    const STEP: Duration = Duration::from_micros(16_667);

    /// Returns a runtime fed `count` steady presents along with the last presented instant.
    fn runtime_after_steady_presents(count: u32) -> (Runtime, Instant) {
        let mut at = Instant::now();
        let mut runtime = Runtime::new(RuntimeConfig::fixed_hz(60).unwrap(), at);
        for _ in 1..count {
            runtime.record_presented(at);
            at += STEP;
        }
        runtime.record_presented(at);
        (runtime, at)
    }

    #[test]
    fn recorded_feedback_paces_jittered_frame_starts_onto_the_display_cadence() {
        let (mut runtime, mut presented) = runtime_after_steady_presents(30);
        let mut now = presented + Duration::from_millis(1);
        drop(runtime.begin_frame(now));
        for jitter_ms in [3_u64, 18, 2, 17] {
            now += Duration::from_millis(jitter_ms);
            let frame = runtime.begin_frame(now);
            assert!(frame.schedule().paced(), "jitter {jitter_ms} ms");
            assert_eq!(frame.plan().frame_delta(), STEP, "jitter {jitter_ms} ms");
            drop(frame);
            presented += STEP;
            runtime.record_presented(presented);
        }
        assert_eq!(runtime.pacing_report().estimated_cadence, Some(STEP));
    }

    #[test]
    fn without_feedback_frames_observably_fall_back_to_wall_clock_gaps() {
        let start = Instant::now();
        let mut runtime = Runtime::new(RuntimeConfig::fixed_hz(10).unwrap(), start);
        let frame = runtime.begin_frame(start + Duration::from_millis(40));
        assert!(!frame.schedule().paced());
        assert_eq!(frame.plan().fixed_steps(), 0);
        assert!((frame.plan().interpolation() - 0.4).abs() < f64::EPSILON);
        drop(frame);
        let frame = runtime.begin_frame(start + Duration::from_millis(125));
        assert!(!frame.schedule().paced());
        assert_eq!(frame.plan().fixed_steps(), 1);
        assert!((frame.plan().interpolation() - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn suspension_advances_nothing_and_resume_discards_the_suspended_interval() {
        let (mut runtime, presented) = runtime_after_steady_presents(30);
        let now = presented + Duration::from_millis(1);
        drop(runtime.begin_frame(now));

        runtime.suspend();
        let idle = runtime.begin_frame(now + Duration::from_millis(120));
        assert!(!idle.schedule().paced());
        assert_eq!(idle.plan().frame_delta(), Duration::ZERO);
        assert_eq!(idle.plan().fixed_steps(), 0);
        drop(idle);

        // Presents recorded while suspended keep feedback fresh; the suspended interval must
        // still not enter the first resumed frame as elapsed time.
        let resumed_at = now + Duration::from_millis(120);
        runtime.record_presented(presented + Duration::from_millis(120));
        runtime.resume(resumed_at);
        let frame = runtime.begin_frame(resumed_at + Duration::from_millis(2));
        assert!(frame.schedule().paced());
        assert_eq!(frame.plan().frame_delta(), STEP);
    }

    #[test]
    fn scoped_frame_cleanup_and_window_suspension_release_input() {
        let mut runtime = Runtime::new(RuntimeConfig::fixed_hz(60).unwrap(), Instant::now());
        runtime.handle_window_event(WindowEvent::Input(InputEvent::Keyboard {
            key: KeyCode::KeyW,
            state: ButtonState::Pressed,
            repeat: false,
            modifiers: Modifiers::default(),
        }));
        let frame = runtime.begin_frame(Instant::now());
        assert!(frame.input().key_pressed(KeyCode::KeyW));
        drop(frame);
        assert!(!runtime.input().key_pressed(KeyCode::KeyW));
        assert!(runtime.input().key_held(KeyCode::KeyW));

        runtime.handle_window_event(WindowEvent::RenderingSuspended);
        assert!(runtime.suspended());
        assert!(!runtime.input().key_held(KeyCode::KeyW));
        assert!(runtime.input().key_released(KeyCode::KeyW));
    }
}
