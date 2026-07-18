//! Forge Run through wgpu/winit with equivalent fixed-step and interpolation behavior.

mod game;
mod gpu;
mod scene;

use std::collections::HashSet;
use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use game::Game;
use gpu::Gpu;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

const FIXED_STEP: Duration = Duration::from_nanos(16_666_667);
const MAX_FRAME_DELTA: Duration = Duration::from_millis(250);
const MAX_FIXED_STEPS: u32 = 8;

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    let mut app = App::default();
    event_loop.run_app(&mut app)?;
    if let Some(failure) = app.failure {
        return Err(failure);
    }
    Ok(())
}

#[derive(Default)]
struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    clock: Option<FrameClock>,
    input: InputState,
    game: Game,
    failure: Option<Box<dyn Error>>,
}

#[derive(Default)]
struct InputState {
    held: HashSet<KeyCode>,
    pressed: HashSet<KeyCode>,
}

impl InputState {
    fn key_event(&mut self, key: KeyCode, state: ElementState) {
        match state {
            ElementState::Pressed if self.held.insert(key) => {
                self.pressed.insert(key);
            }
            ElementState::Released => {
                self.held.remove(&key);
            }
            ElementState::Pressed => {}
        }
    }

    fn focus(&mut self, focused: bool) {
        if !focused {
            self.held.clear();
        }
    }

    fn key_held(&self, key: KeyCode) -> bool {
        self.held.contains(&key)
    }

    fn key_pressed(&self, key: KeyCode) -> bool {
        self.pressed.contains(&key)
    }

    fn end_frame(&mut self) {
        self.pressed.clear();
    }
}

struct FrameClock {
    previous: Instant,
    accumulator: Duration,
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
        }
    }

    fn advance(&mut self, now: Instant) -> FramePlan {
        let elapsed = now.saturating_duration_since(self.previous);
        self.previous = now;
        let frame_delta = elapsed.min(MAX_FRAME_DELTA);
        self.accumulator += frame_delta;

        let available = self.accumulator.as_nanos() / FIXED_STEP.as_nanos();
        let fixed_steps = u32::try_from(available.min(u128::from(MAX_FIXED_STEPS)))
            .expect("step count is bounded by MAX_FIXED_STEPS");
        self.accumulator -= FIXED_STEP * fixed_steps;
        if available > u128::from(fixed_steps) {
            let remainder = self.accumulator.as_nanos() % FIXED_STEP.as_nanos();
            self.accumulator = Duration::from_nanos(
                u64::try_from(remainder).expect("remainder is less than one fixed step"),
            );
        }

        FramePlan {
            frame_delta,
            fixed_steps,
            interpolation: self.accumulator.as_secs_f64() / FIXED_STEP.as_secs_f64(),
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("wgpu — Forge Run")
            .with_inner_size(LogicalSize::new(1100, 700));
        match event_loop.create_window(attributes) {
            Ok(window) => {
                let window = Arc::new(window);
                match pollster::block_on(Gpu::new(Arc::clone(&window))) {
                    Ok(gpu) => {
                        println!(
                            "backend: wgpu ({:?}), samples: {}",
                            gpu.backend(),
                            if gpu.sample_count() == 4 {
                                "Four"
                            } else {
                                "One"
                            }
                        );
                        println!(
                            "forge run: W/A/S/D or arrows move; recover eight crystals, avoid sentries, R resets"
                        );
                        self.gpu = Some(gpu);
                        self.clock = Some(FrameClock::new(Instant::now()));
                        self.window = Some(window);
                    }
                    Err(error) => {
                        self.failure = Some(error);
                        event_loop.exit();
                    }
                }
            }
            Err(error) => {
                self.failure = Some(Box::new(error));
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let (Some(window), Some(gpu), Some(clock)) =
            (self.window.as_ref(), self.gpu.as_mut(), self.clock.as_mut())
        else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Focused(focused) => self.input.focus(focused),
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(key) = event.physical_key {
                    self.input.key_event(key, event.state);
                }
            }
            WindowEvent::Resized(size) => gpu.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                let plan = clock.advance(Instant::now());
                self.game.handle_frame_input(&self.input);
                for _ in 0..plan.fixed_steps {
                    self.game
                        .fixed_update(&self.input, FIXED_STEP.as_secs_f32());
                }
                self.game.variable_update(plan.frame_delta.as_secs_f32());
                self.input.end_frame();

                if let Err(error) = gpu.render(&self.game, plan.interpolation) {
                    self.failure = Some(error);
                    event_loop.exit();
                    return;
                }
                window.request_redraw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}
