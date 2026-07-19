//! Combines `winit` input with an equivalent two-pass `wgpu` postprocess path.

mod gpu;
mod scene;

use std::error::Error;
use std::sync::Arc;
use std::time::Instant;

use glam::Quat;
use gpu::Gpu;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    let mut app = App {
        window: None,
        gpu: None,
        started: Instant::now(),
        failure: None,
        interaction: Interaction::default(),
    };
    event_loop.run_app(&mut app)?;
    if let Some(failure) = app.failure {
        return Err(failure);
    }
    Ok(())
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    started: Instant,
    failure: Option<Box<dyn Error>>,
    interaction: Interaction,
}

struct Interaction {
    animation_seconds: f32,
    last_frame_seconds: Option<f32>,
    paused: bool,
    orientation: Quat,
    distance_offset: f32,
    dragging: bool,
    pointer: Option<LogicalPosition<f64>>,
}

impl Default for Interaction {
    fn default() -> Self {
        Self {
            animation_seconds: 0.0,
            last_frame_seconds: None,
            paused: true,
            orientation: Quat::IDENTITY,
            distance_offset: 0.0,
            dragging: false,
            pointer: None,
        }
    }
}

impl Interaction {
    fn key(&mut self, key: KeyCode, repeat: bool) {
        match key {
            KeyCode::KeyA | KeyCode::ArrowLeft => self.rotate(-0.12, 0.0),
            KeyCode::KeyD | KeyCode::ArrowRight => self.rotate(0.12, 0.0),
            KeyCode::KeyW | KeyCode::ArrowUp => self.rotate(0.0, -0.12),
            KeyCode::KeyS | KeyCode::ArrowDown => self.rotate(0.0, 0.12),
            KeyCode::Space if !repeat => self.paused = !self.paused,
            KeyCode::KeyR if !repeat => self.reset_view(),
            _ => {}
        }
    }

    fn focus(&mut self, focused: bool) {
        if !focused {
            self.dragging = false;
            self.pointer = None;
        }
    }

    fn pointer_moved(&mut self, position: LogicalPosition<f64>) {
        if self.dragging
            && let Some(previous) = self.pointer
        {
            let yaw = (position.x - previous.x) as f32 * 0.008;
            let pitch = (position.y - previous.y) as f32 * 0.008;
            self.rotate(yaw, pitch);
        }
        self.pointer = Some(position);
    }

    fn primary_button(&mut self, state: ElementState) {
        self.dragging = state == ElementState::Pressed;
    }

    fn scroll(&mut self, delta: MouseScrollDelta, scale_factor: f64) {
        let y = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(y) * 0.18,
            MouseScrollDelta::PixelDelta(position) => position.y / scale_factor * 0.015,
        };
        self.distance_offset = (self.distance_offset - y as f32).clamp(-1.5, 8.0);
    }

    fn reset_view(&mut self) {
        self.orientation = Quat::IDENTITY;
        self.distance_offset = 0.0;
    }

    fn rotate(&mut self, yaw: f32, pitch: f32) {
        let screen_rotation = Quat::from_rotation_x(pitch) * Quat::from_rotation_y(yaw);
        self.orientation = (screen_rotation * self.orientation).normalize();
    }

    fn animation_time(&mut self, frame_seconds: f32) -> f32 {
        if let Some(previous) = self.last_frame_seconds
            && !self.paused
        {
            self.animation_seconds += frame_seconds - previous;
        }
        self.last_frame_seconds = Some(frame_seconds);
        self.animation_seconds
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("wgpu — interactive postprocess showcase")
            .with_inner_size(LogicalSize::new(960, 540));
        match event_loop.create_window(attributes) {
            Ok(window) => {
                let window = Arc::new(window);
                match pollster::block_on(Gpu::new(window.clone())) {
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
                            "input: W/A/S/D or arrows rotate, primary-button drag orbits, scroll zooms, Space toggles spin, R resets"
                        );
                        self.gpu = Some(gpu);
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
        let (Some(window), Some(gpu)) = (self.window.as_ref(), self.gpu.as_mut()) else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Focused(focused) => self.interaction.focus(focused),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let PhysicalKey::Code(key) = event.physical_key {
                    self.interaction.key(key, event.repeat);
                }
            }
            WindowEvent::CursorMoved { position, .. } => self
                .interaction
                .pointer_moved(position.to_logical(window.scale_factor())),
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => self.interaction.primary_button(state),
            WindowEvent::MouseWheel { delta, .. } => {
                self.interaction.scroll(delta, window.scale_factor());
            }
            WindowEvent::Resized(size) => gpu.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                let animation_seconds = self
                    .interaction
                    .animation_time(self.started.elapsed().as_secs_f32());
                if let Err(error) = gpu.render(
                    animation_seconds,
                    self.interaction.orientation,
                    4.0 + self.interaction.distance_offset,
                ) {
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
