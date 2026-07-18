//! Game-loop timing and input snapshots for Mulciber.
//!
//! The first runtime slice decouples a fixed-rate simulation from variable-rate rendering. It owns
//! the accumulator and bounded catch-up policy while leaving previous/current game state and its
//! interpolation with the application.

mod input;
mod pacing;
mod timing;

use std::time::Instant;

pub use input::{InputSnapshot, ScrollSample};
use mulciber_platform::{InputEvent, WindowEvent};
pub use pacing::{IntervalSummary, PacingDiagnostics, PacingReport};
pub use timing::{FramePlan, RuntimeConfig, RuntimeConfigError};

/// Coordinates frame-scoped input with a fixed-rate simulation clock.
#[derive(Debug)]
pub struct Runtime {
    input: InputSnapshot,
    clock: timing::FrameClock,
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
}

impl RuntimeFrame<'_> {
    /// Returns the fixed/variable timing work and render interpolation for this frame.
    #[must_use]
    pub const fn plan(&self) -> FramePlan {
        self.plan
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
        Self {
            input: InputSnapshot::default(),
            clock: timing::FrameClock::new(config, started_at),
        }
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
    /// Dropping the returned frame automatically ends the input snapshot, including on early return.
    pub fn begin_frame(&mut self, now: Instant) -> RuntimeFrame<'_> {
        RuntimeFrame {
            input: &mut self.input,
            plan: self.clock.advance(now),
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
        self.clock.resume(now);
    }

    /// Returns whether frame timing is currently suspended.
    #[must_use]
    pub const fn suspended(&self) -> bool {
        self.clock.suspended()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use mulciber_platform::{ButtonState, InputEvent, KeyCode, Modifiers, WindowEvent};

    use super::{Runtime, RuntimeConfig};

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
