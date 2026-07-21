//! Renders a textured cube offscreen at a reduced render scale and applies a fullscreen
//! post-processing pass that resamples it to the native surface extent.

mod scene;

use std::error::Error;
use std::time::Instant;

use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, OpenedGraphics, PostprocessPipelineDescriptor,
    PostprocessedDraw, RenderScale, SampleCount, ShaderArtifact,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};

use scene::{CUBE_INDICES, CUBE_VERTICES, checkerboard, transform};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/postprocess.shaderbin"));
const CLEAR: ClearColor = ClearColor::opaque(0.025, 0.035, 0.055);
/// The scene pass renders at half the presentable extent; the postprocess pass upsamples.
/// Deliberately visible so the render-scale path has observable evidence.
const RENDER_SCALE_PERCENT: u32 = 50;

#[allow(clippy::cast_precision_loss)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — postprocessed textured cube",
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

    let mesh = graphics.device.create_mesh(&CUBE_VERTICES, &CUBE_INDICES)?;
    let texture = graphics
        .device
        .create_rgba8_srgb_texture(8, 8, &checkerboard())?;
    let scene_pipeline = graphics
        .device
        .create_textured_pipeline(ShaderArtifact::new(SHADER)?)?;
    let postprocess_pipeline =
        graphics
            .device
            .create_postprocess_pipeline(PostprocessPipelineDescriptor {
                shader: ShaderArtifact::new(SHADER)?,
                uniform_size: Some(64),
            })?;
    let render_scale = RenderScale::percent(RENDER_SCALE_PERCENT)?;
    println!("render scale: {} percent", render_scale.as_percent());
    let mut targets = graphics
        .device
        .create_scaled_postprocess_targets(graphics.surface.info()?, render_scale)?;
    let started = Instant::now();

    loop {
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            let WindowEvent::RedrawRequested(metrics) = event else {
                return Ok(());
            };
            match graphics.surface.acquire(metrics)? {
                FrameAcquire::Ready(frame) => {
                    let info = frame.surface_info();
                    if info != targets.info() {
                        targets = graphics
                            .device
                            .create_scaled_postprocess_targets(info, render_scale)?;
                    }
                    let aspect = info.extent().width() as f32 / info.extent().height() as f32;
                    let elapsed = started.elapsed().as_secs_f32();
                    let postprocess_uniform = animated_postprocess_uniform(elapsed);
                    graphics.queue.draw_textured_postprocessed_and_present(
                        frame,
                        PostprocessedDraw {
                            mesh: &mesh,
                            texture: &texture,
                            scene_pipeline: &scene_pipeline,
                            postprocess_pipeline: &postprocess_pipeline,
                            targets: &targets,
                            uniform: &postprocess_uniform,
                            model_view_projection: transform(elapsed, aspect),
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

/// One 64-byte block matching `DrawConstants`. The fullscreen shader interprets the first two
/// floats as the smooth underwater transition and animation time; the scene pass receives its own
/// independent transform storage.
fn animated_postprocess_uniform(elapsed: f32) -> [u8; 64] {
    let transition = 0.5 - 0.5 * (elapsed * 0.45).cos();
    let mut bytes = [0_u8; 64];
    bytes[..4].copy_from_slice(&transition.to_ne_bytes());
    bytes[4..8].copy_from_slice(&elapsed.to_ne_bytes());
    bytes
}
