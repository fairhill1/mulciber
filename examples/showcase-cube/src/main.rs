//! Combines native input transitions with the two-pass postprocessing checkpoint.

mod scene;

use std::error::Error;
use std::time::Instant;

use glam::Quat;
use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, OpenedGraphics, PostprocessedDraw, SampleCount,
    ShaderArtifact,
};
use mulciber_platform::{
    Application, ButtonState, InputEvent, KeyCode, LogicalPosition, LogicalSize, PointerButton,
    PumpStatus, ScrollDelta, WindowDescriptor, WindowEvent,
};

use scene::{CUBE_INDICES, CUBE_VERTICES, checkerboard, interactive_transform};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/postprocess.shaderbin"));
const CLEAR: ClearColor = ClearColor::opaque(0.025, 0.035, 0.055);

struct Interaction {
    animation_seconds: f32,
    last_frame_seconds: Option<f32>,
    paused: bool,
    orientation: Quat,
    distance_offset: f32,
    dragging: bool,
    pointer: Option<LogicalPosition>,
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
    #[allow(clippy::cast_possible_truncation)]
    fn handle(&mut self, event: InputEvent) {
        match event {
            InputEvent::FocusChanged { focused: false } => {
                self.dragging = false;
                self.pointer = None;
            }
            InputEvent::Keyboard {
                key,
                state: ButtonState::Pressed,
                repeat,
                ..
            } => match key {
                KeyCode::KeyA | KeyCode::ArrowLeft => self.rotate(-0.12, 0.0),
                KeyCode::KeyD | KeyCode::ArrowRight => self.rotate(0.12, 0.0),
                KeyCode::KeyW | KeyCode::ArrowUp => self.rotate(0.0, -0.12),
                KeyCode::KeyS | KeyCode::ArrowDown => self.rotate(0.0, 0.12),
                KeyCode::Space if !repeat => self.paused = !self.paused,
                KeyCode::KeyR if !repeat => self.reset_view(),
                _ => {}
            },
            InputEvent::PointerButton {
                button: PointerButton::Primary,
                state,
                position,
                ..
            } => {
                self.dragging = state == ButtonState::Pressed;
                self.pointer = self.dragging.then_some(position);
            }
            InputEvent::PointerMoved { position, .. } if self.dragging => {
                if let Some(previous) = self.pointer {
                    let yaw = (position.x() - previous.x()) as f32 * 0.008;
                    let pitch = (position.y() - previous.y()) as f32 * 0.008;
                    self.rotate(yaw, pitch);
                }
                self.pointer = Some(position);
            }
            InputEvent::Scroll { delta, .. } => {
                let y = match delta {
                    ScrollDelta::Precise { y, .. } => y * 0.015,
                    ScrollDelta::Coarse { y, .. } => y * 0.18,
                };
                self.distance_offset = (self.distance_offset - y as f32).clamp(-1.5, 8.0);
            }
            _ => {}
        }
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

#[allow(clippy::cast_precision_loss)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — interactive postprocess showcase",
        LogicalSize::new(960, 540),
    ))?;
    let initial_metrics = application.wait_for_first_metrics(&window)?;
    let mut graphics = OpenedGraphics::open(
        window.surface_target(),
        initial_metrics,
        DeviceRequest {
            preferred_sample_count: SampleCount::Four,
        },
    )?;
    println!(
        "backend: {}, samples: {:?}",
        graphics.selection.backend(),
        graphics.selection.sample_count()
    );
    println!(
        "input: W/A/S/D or arrows rotate, primary-button drag orbits, scroll zooms, Space toggles spin, R resets"
    );

    let mesh = graphics.device.create_mesh(&CUBE_VERTICES, &CUBE_INDICES)?;
    let texture = graphics
        .device
        .create_rgba8_srgb_texture(8, 8, &checkerboard())?;
    let scene_pipeline = graphics
        .device
        .create_textured_pipeline(ShaderArtifact::new(SHADER)?)?;
    let postprocess_pipeline = graphics
        .device
        .create_postprocess_pipeline(ShaderArtifact::new(SHADER)?)?;
    let mut targets = graphics
        .device
        .create_postprocess_targets(graphics.surface.info()?)?;
    let started = Instant::now();
    let mut interaction = Interaction::default();

    loop {
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            let metrics = match event {
                WindowEvent::Input(input) => {
                    interaction.handle(input);
                    return Ok(());
                }
                WindowEvent::RedrawRequested(metrics) => metrics,
                _ => return Ok(()),
            };
            match graphics.surface.acquire(metrics)? {
                FrameAcquire::Ready(frame) => {
                    let info = frame.surface_info();
                    if info != targets.info() {
                        targets = graphics.device.create_postprocess_targets(info)?;
                    }
                    let aspect = info.extent().width() as f32 / info.extent().height() as f32;
                    let animation_seconds =
                        interaction.animation_time(started.elapsed().as_secs_f32());
                    graphics.queue.draw_textured_postprocessed_and_present(
                        frame,
                        PostprocessedDraw {
                            mesh: &mesh,
                            texture: &texture,
                            scene_pipeline: &scene_pipeline,
                            postprocess_pipeline: &postprocess_pipeline,
                            targets: &targets,
                            uniform: &[],
                            model_view_projection: interactive_transform(
                                animation_seconds,
                                aspect,
                                interaction.orientation,
                                4.0 + interaction.distance_offset,
                            ),
                            clear: CLEAR,
                        },
                    )?;
                }
                FrameAcquire::Unavailable(_) => {}
            }
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            break;
        }
    }

    graphics.shutdown()?;
    Ok(())
}
