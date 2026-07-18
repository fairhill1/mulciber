//! Game-loop timing and input snapshots for Mulciber.
//!
//! The first runtime slice decouples a fixed-rate simulation from variable-rate rendering. It owns
//! the accumulator and bounded catch-up policy while leaving previous/current game state and its
//! interpolation with the application.

mod input;
mod timing;

use std::time::Instant;

pub use input::{InputSnapshot, ScrollSample};
use mulciber_platform::InputEvent;
pub use timing::{FramePlan, RuntimeConfig, RuntimeConfigError};

/// Coordinates frame-scoped input with a fixed-rate simulation clock.
#[derive(Debug)]
pub struct Runtime {
    input: InputSnapshot,
    clock: timing::FrameClock,
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

    /// Returns the held state and transitions accumulated for the current frame.
    #[must_use]
    pub const fn input(&self) -> &InputSnapshot {
        &self.input
    }

    /// Advances timing and returns the fixed simulation work and render interpolation for a frame.
    pub fn begin_frame(&mut self, now: Instant) -> FramePlan {
        self.clock.advance(now)
    }

    /// Ends the current frame, preserving held controls while clearing one-frame transitions.
    pub fn end_frame(&mut self) {
        self.input.end_frame();
    }
}
