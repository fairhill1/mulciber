//! Forge Run directly through maintained AppKit and Metal Rust bindings.

mod game;
mod gpu;
mod scene;

use std::collections::HashSet;
use std::error::Error;
use std::time::{Duration, Instant};

use game::Game;
use gpu::Gpu;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent, NSEventMask,
    NSEventType, NSView, NSWindow, NSWindowOcclusionState, NSWindowStyleMask,
};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};

const FIXED_STEP: Duration = Duration::from_nanos(16_666_667);
const MAX_FRAME_DELTA: Duration = Duration::from_millis(250);
const MAX_FIXED_STEPS: u32 = 8;

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum Key {
    A,
    D,
    Down,
    Left,
    R,
    Right,
    S,
    Up,
    W,
}

#[derive(Default)]
struct InputState {
    held: HashSet<Key>,
    pressed: HashSet<Key>,
}

impl InputState {
    fn key_event(&mut self, key: Key, pressed: bool) {
        if pressed && self.held.insert(key) {
            self.pressed.insert(key);
        } else if !pressed {
            self.held.remove(&key);
        }
    }

    fn focus(&mut self, focused: bool) {
        if !focused {
            self.held.clear();
        }
    }

    fn key_held(&self, key: Key) -> bool {
        self.held.contains(&key)
    }

    fn key_pressed(&self, key: Key) -> bool {
        self.pressed.contains(&key)
    }

    fn end_frame(&mut self) {
        self.pressed.clear();
    }
}

struct FrameClock {
    previous: Instant,
    accumulator: Duration,
    suspended: bool,
}

struct FramePlan {
    frame_delta: Duration,
    fixed_steps: u32,
    interpolation: f64,
}

impl FrameClock {
    fn new(now: Instant) -> Self {
        Self {
            previous: now,
            accumulator: Duration::ZERO,
            suspended: false,
        }
    }

    fn advance(&mut self, now: Instant) -> FramePlan {
        if self.suspended {
            return FramePlan {
                frame_delta: Duration::ZERO,
                fixed_steps: 0,
                interpolation: self.accumulator.as_secs_f64() / FIXED_STEP.as_secs_f64(),
            };
        }
        let frame_delta = now
            .saturating_duration_since(self.previous)
            .min(MAX_FRAME_DELTA);
        self.previous = now;
        self.accumulator += frame_delta;
        let available = self.accumulator.as_nanos() / FIXED_STEP.as_nanos();
        let fixed_steps = u32::try_from(available.min(u128::from(MAX_FIXED_STEPS)))
            .expect("step count is bounded");
        self.accumulator -= FIXED_STEP * fixed_steps;
        if available > u128::from(fixed_steps) {
            self.accumulator = Duration::from_nanos(
                u64::try_from(self.accumulator.as_nanos() % FIXED_STEP.as_nanos())
                    .expect("remainder fits"),
            );
        }
        FramePlan {
            frame_delta,
            fixed_steps,
            interpolation: self.accumulator.as_secs_f64() / FIXED_STEP.as_secs_f64(),
        }
    }

    fn set_suspended(&mut self, suspended: bool) {
        if suspended && !self.suspended {
            self.suspended = true;
        } else if !suspended && self.suspended {
            self.previous = Instant::now();
            self.suspended = false;
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mtm = MainThreadMarker::new().ok_or("Forge Run must start on the main thread")?;
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    application.finishLaunching();

    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1100.0, 700.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    let view = NSView::initWithFrame(NSView::alloc(mtm), rect);
    window.setContentView(Some(&view));
    window.setTitle(&NSString::from_str("Direct Metal — Forge Run"));
    unsafe { window.setReleasedWhenClosed(false) };
    window.makeKeyAndOrderFront(None);
    #[allow(deprecated)]
    application.activateIgnoringOtherApps(true);

    let (width, height) = drawable_size(&view);
    let mut gpu = Gpu::new(&view, width, height)?;
    println!(
        "backend: direct Metal, samples: {}",
        if gpu.sample_count() == 4 {
            "Four"
        } else {
            "One"
        }
    );
    println!("forge run: W/A/S/D or arrows move; recover eight crystals, avoid sentries, R resets");

    let mut input = InputState::default();
    let mut game = Game::default();
    let mut clock = FrameClock::new(Instant::now());
    let mut focused = true;
    while window.isVisible() {
        poll_events(&application, &mut input);
        application.updateWindows();
        if !window.isVisible() {
            break;
        }

        let now_focused = window.isKeyWindow();
        if now_focused != focused {
            focused = now_focused;
            input.focus(focused);
        }
        let (width, height) = drawable_size(&view);
        gpu.resize(width, height)?;
        let suspended = window.isMiniaturized()
            || !window
                .occlusionState()
                .contains(NSWindowOcclusionState::Visible)
            || width == 0
            || height == 0;
        clock.set_suspended(suspended);
        if suspended {
            input.focus(false);
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }

        let plan = clock.advance(Instant::now());
        game.handle_frame_input(&input);
        for _ in 0..plan.fixed_steps {
            game.fixed_update(&input, FIXED_STEP.as_secs_f32());
        }
        game.variable_update(plan.frame_delta.as_secs_f32());
        input.end_frame();
        gpu.render(&game, plan.interpolation)?;
    }
    gpu.shutdown()?;
    Ok(())
}

fn poll_events(application: &NSApplication, input: &mut InputState) {
    let until = NSDate::distantPast();
    while let Some(event) = application.nextEventMatchingMask_untilDate_inMode_dequeue(
        NSEventMask::Any,
        Some(&until),
        unsafe { NSDefaultRunLoopMode },
        true,
    ) {
        let handled = match event.r#type() {
            NSEventType::KeyDown => key(&event).is_some_and(|key| {
                input.key_event(key, true);
                true
            }),
            NSEventType::KeyUp => key(&event).is_some_and(|key| {
                input.key_event(key, false);
                true
            }),
            _ => false,
        };
        if !handled {
            application.sendEvent(&event);
        }
    }
}

fn key(event: &NSEvent) -> Option<Key> {
    Some(match event.keyCode() {
        0 => Key::A,
        1 => Key::S,
        2 => Key::D,
        13 => Key::W,
        15 => Key::R,
        123 => Key::Left,
        124 => Key::Right,
        125 => Key::Down,
        126 => Key::Up,
        _ => return None,
    })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn drawable_size(view: &NSView) -> (u32, u32) {
    let backing = view.convertRectToBacking(view.bounds());
    (
        backing.size.width.max(0.0).round() as u32,
        backing.size.height.max(0.0).round() as u32,
    )
}
