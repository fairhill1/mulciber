//! Game-loop timing and input snapshots for Mulciber.
//!
//! The first runtime slice decouples a fixed-rate simulation from variable-rate rendering. It owns
//! the accumulator and bounded catch-up policy while leaving previous/current game state and its
//! interpolation with the application.
//!
//! The runtime also owns the display-interval frame pacer. Drain the graphics surface's
//! presentation feedback into [`Runtime::record_presented`] every frame and
//! [`Runtime::begin_frame`] advances simulation time by whole display intervals of the observed
//! cadence when that keeps cumulative drift within 16 ms of elapsed time. Otherwise it uses
//! wall-clock gaps between build starts. Unsmoothed wall-clock gaps can reintroduce
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
mod limiter;
mod pacing;
mod timing;

use std::time::Instant;

pub use input::{InputSnapshot, ScrollSample};
pub use limiter::FrameStartLimiter;
use mulciber_platform::{InputEvent, WindowEvent};
pub use pacing::{FramePacer, FrameSchedule, IntervalSummary, PacingDiagnostics, PacingReport};
pub use timing::{FramePlan, RuntimeConfig, RuntimeConfigError};

/// Coordinates simulation-latched input, a fixed-rate simulation clock, and presentation pacing.
#[derive(Debug)]
pub struct Runtime {
    input: InputSnapshot,
    clock: timing::FrameClock,
    pacer: FramePacer,
}

/// One scoped runtime frame containing its timing plan and immutable input snapshot.
///
/// Dropping a frame that schedules at least one fixed update clears pressed/released transitions
/// and scroll samples while preserving held controls. A zero-update render frame retains those
/// transients for the next simulation-bearing frame, so one-shot input cannot fall between ticks.
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
    /// the wall-clock fallback while feedback is missing, stale, or would cause timing drift.
    pub const fn schedule(&self) -> FrameSchedule {
        self.schedule
    }

    /// Returns held state and transitions accumulated since the last simulation-bearing frame.
    #[must_use]
    pub const fn input(&self) -> &InputSnapshot {
        self.input
    }
}

impl Drop for RuntimeFrame<'_> {
    fn drop(&mut self) {
        if self.plan.fixed_steps() != 0 {
            self.input.end_frame();
        }
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
    /// intervals when doing so respects the cumulative drift bound, otherwise by wall-clock gaps.
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

    /// Returns held state and transitions accumulated since the last simulation-bearing frame.
    #[must_use]
    pub const fn input(&self) -> &InputSnapshot {
        &self.input
    }

    /// Begins a scoped frame with fixed simulation work, input, and render interpolation.
    ///
    /// While recorded presentation feedback yields a fresh cadence estimate, the frame delta is a
    /// whole number of display intervals if doing so keeps cumulative pacing drift within 16 ms
    /// of elapsed time. Call this once per frame that will be presented. Without fresh feedback,
    /// or when the drift limit would be exceeded, the delta observably falls back to the wall-clock
    /// gap since the previous frame — see
    /// [`RuntimeFrame::schedule`].
    ///
    /// Dropping a frame with fixed updates consumes transient input, including on early return.
    /// A frame with no fixed update retains it for the next simulation-bearing frame.
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
        runtime.resume(now);
        for elapsed in [
            STEP + Duration::from_millis(7),
            STEP.checked_sub(Duration::from_millis(7)).unwrap(),
        ]
        .repeat(100)
        {
            now += elapsed;
            let frame = runtime.begin_frame(now);
            assert!(frame.schedule().paced());
            assert_eq!(frame.plan().frame_delta(), STEP);
            drop(frame);
            presented += STEP;
            runtime.record_presented(presented);
        }
        assert_eq!(runtime.pacing_report().estimated_cadence, Some(STEP));
    }

    #[test]
    fn fps_changes_keep_scheduled_time_and_simulation_near_elapsed_time() {
        use std::collections::VecDeque;

        for feedback_delay in [0, 2] {
            let config = RuntimeConfig::fixed_hz(60).unwrap();
            let start = Instant::now();
            let mut now = start;
            let mut runtime = Runtime::new(config, start);
            let mut feedback = VecDeque::new();
            let mut scheduled = Duration::ZERO;
            let mut simulated = Duration::ZERO;
            // Fill the 240-interval window before each transition; include small changes that
            // would defeat a guard considering only the current frame's error.
            for hz in [30, 60, 20, 60, 40, 80, 60, 61, 120, 240, 30, 60] {
                let period = Duration::from_secs_f64(1.0 / f64::from(hz));
                let segment_start = now;
                let simulation_start = simulated;
                for _ in 0..300 {
                    now += period;
                    feedback.push_back(now);
                    if feedback.len() > feedback_delay {
                        runtime.record_presented(feedback.pop_front().unwrap());
                    }
                    let frame = runtime.begin_frame(now);
                    let plan = frame.plan();
                    scheduled += frame.schedule().frame_delta();
                    simulated += plan.fixed_step() * plan.fixed_steps();
                    assert_eq!(plan.dropped_time(), Duration::ZERO);
                    assert!((0.0..1.0).contains(&plan.interpolation()));
                    let elapsed = now.duration_since(start);
                    assert!(
                        scheduled.abs_diff(elapsed) <= Duration::from_millis(16),
                        "{hz} Hz, delay {feedback_delay}: {scheduled:?} vs {elapsed:?}"
                    );
                    assert!(
                        simulated.abs_diff(elapsed)
                            <= Duration::from_millis(16) + config.fixed_step()
                    );
                    // Bound progress from each transition too, not just the start of the run.
                    assert!(
                        simulated
                            .checked_sub(simulation_start)
                            .unwrap()
                            .abs_diff(now - segment_start)
                            <= Duration::from_millis(32) + config.fixed_step()
                    );
                }
            }
            // Recover continuously instead of jumping between two fixed cadences.
            for micros in (8_000..50_000).rev().step_by(37) {
                now += Duration::from_micros(micros);
                runtime.record_presented(now);
                let frame = runtime.begin_frame(now);
                scheduled += frame.schedule().frame_delta();
                simulated += frame.plan().fixed_step() * frame.plan().fixed_steps();
                assert!(scheduled.abs_diff(now - start) <= Duration::from_millis(16));
                assert!(
                    simulated.abs_diff(now - start)
                        <= Duration::from_millis(16) + config.fixed_step()
                );
            }
        }
    }

    #[test]
    fn stale_feedback_and_hitches_do_not_refill_the_smoothing_budget_or_repay_dropped_time() {
        let start = Instant::now();
        let mut now = start;
        let mut runtime = Runtime::new(RuntimeConfig::fixed_hz(60).unwrap(), start);
        let mut scheduled = Duration::ZERO;
        for _ in 0..300 {
            now += STEP;
            runtime.record_presented(now);
            scheduled += runtime.begin_frame(now).schedule().frame_delta();
        }
        // Repeated fallback/re-entry must not grant a new 16 ms of synthetic time each time.
        for _ in 0..20 {
            now += Duration::from_millis(2);
            runtime.record_presented(now);
            scheduled += runtime.begin_frame(now).schedule().frame_delta();
            now += Duration::from_secs(1);
            let frame = runtime.begin_frame(now);
            scheduled += frame.schedule().frame_delta();
            assert!(!frame.schedule().paced());
            assert_eq!(frame.plan().fixed_steps(), 8);
            assert!(frame.plan().dropped_time() > Duration::from_millis(850));
            drop(frame);

            let mut recovery_simulation = Duration::ZERO;
            for _ in 0..120 {
                now += STEP;
                runtime.record_presented(now);
                let frame = runtime.begin_frame(now);
                scheduled += frame.schedule().frame_delta();
                recovery_simulation += frame.plan().fixed_step() * frame.plan().fixed_steps();
                assert!(scheduled.abs_diff(now - start) <= Duration::from_millis(16));
            }
            assert!(recovery_simulation.abs_diff(STEP * 120) < Duration::from_millis(50));
        }
    }

    #[test]
    fn resume_with_a_slow_cadence_does_not_advance_without_elapsed_time() {
        let start = Instant::now();
        let mut runtime = Runtime::new(RuntimeConfig::fixed_hz(60).unwrap(), start);
        let mut now = start;
        for _ in 0..300 {
            now += Duration::from_millis(50);
            runtime.record_presented(now);
            drop(runtime.begin_frame(now));
        }
        runtime.suspend();
        now += Duration::from_secs(10);
        runtime.record_presented(now);
        runtime.resume(now);
        let frame = runtime.begin_frame(now);
        assert_eq!(frame.plan().fixed_steps(), 0);
        assert_eq!(frame.schedule().frame_delta(), Duration::ZERO);
        drop(frame);
        let fixed_step = RuntimeConfig::fixed_hz(60).unwrap().fixed_step();
        let frame = runtime.begin_frame(now + fixed_step);
        assert_eq!(frame.schedule().frame_delta(), fixed_step);
        assert_eq!(frame.plan().fixed_steps(), 1);
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
        let started = Instant::now();
        let mut runtime = Runtime::new(RuntimeConfig::fixed_hz(60).unwrap(), started);
        runtime.handle_window_event(WindowEvent::Input(InputEvent::Keyboard {
            key: KeyCode::KeyW,
            state: ButtonState::Pressed,
            repeat: false,
            modifiers: Modifiers::default(),
        }));
        let frame = runtime.begin_frame(started + STEP);
        assert_eq!(frame.plan().fixed_steps(), 1);
        assert!(frame.input().key_pressed(KeyCode::KeyW));
        drop(frame);
        assert!(!runtime.input().key_pressed(KeyCode::KeyW));
        assert!(runtime.input().key_held(KeyCode::KeyW));

        runtime.handle_window_event(WindowEvent::RenderingSuspended);
        assert!(runtime.suspended());
        assert!(!runtime.input().key_held(KeyCode::KeyW));
        assert!(runtime.input().key_released(KeyCode::KeyW));
    }

    #[test]
    fn zero_step_frame_preserves_transitions_until_simulation_can_consume_them() {
        const DISPLAY_75_HZ_FRAME: Duration = Duration::from_nanos(13_333_333);

        let started = Instant::now();
        let mut runtime = Runtime::new(RuntimeConfig::fixed_hz(60).unwrap(), started);
        runtime.handle_input(InputEvent::Keyboard {
            key: KeyCode::Space,
            state: ButtonState::Pressed,
            repeat: false,
            modifiers: Modifiers::default(),
        });

        let render_only = runtime.begin_frame(started + DISPLAY_75_HZ_FRAME);
        assert_eq!(render_only.plan().fixed_steps(), 0);
        assert!(render_only.input().key_pressed(KeyCode::Space));
        drop(render_only);
        assert!(runtime.input().key_pressed(KeyCode::Space));

        let simulation_frame = runtime.begin_frame(started + DISPLAY_75_HZ_FRAME * 2);
        assert_eq!(simulation_frame.plan().fixed_steps(), 1);
        assert!(simulation_frame.input().key_pressed(KeyCode::Space));
        drop(simulation_frame);
        assert!(!runtime.input().key_pressed(KeyCode::Space));
        assert!(runtime.input().key_held(KeyCode::Space));
    }
}
