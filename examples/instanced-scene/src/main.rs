//! Renders one hundred objects as four native instance batches plus one postprocess pass.

mod scene;

use std::error::Error;
use std::time::Instant;

use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, OpenedGraphics, SceneContent, SceneOutput,
    SceneSubmission, ShaderArtifact, TexturedInstanceBatch,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};

use scene::{
    CUBE_INDICES, CUBE_VERTICES, PYRAMID_INDICES, PYRAMID_VERTICES, checkerboard,
    instance_transforms,
};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/instanced.shaderbin"));
const CLEAR: ClearColor = ClearColor::opaque(0.012, 0.018, 0.032);

#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — 100-object instanced scene",
        LogicalSize::new(1100, 700),
    ))?;
    let initial_metrics = application.wait_for_first_metrics(&window)?;
    let mut graphics = OpenedGraphics::open(
        window.surface_target(),
        initial_metrics,
        DeviceRequest::default(),
    )?;
    println!(
        "backend: {}, samples: {:?}, scene objects: 100, instance batches: 4",
        graphics.selection.backend(),
        graphics.selection.sample_count()
    );

    let cube = graphics.device.create_mesh(&CUBE_VERTICES, &CUBE_INDICES)?;
    let pyramid = graphics
        .device
        .create_mesh(&PYRAMID_VERTICES, &PYRAMID_INDICES)?;
    let amber = graphics.device.create_rgba8_srgb_texture(
        8,
        8,
        &checkerboard([245, 165, 40], [35, 90, 210]),
    )?;
    let violet = graphics.device.create_rgba8_srgb_texture(
        8,
        8,
        &checkerboard([175, 70, 235], [25, 180, 145]),
    )?;
    let scene_pipeline = graphics
        .device
        .create_instanced_textured_pipeline(ShaderArtifact::new(SHADER)?)?;
    let postprocess_pipeline = graphics
        .device
        .create_postprocess_pipeline(ShaderArtifact::new(SHADER)?)?;
    let mut targets = graphics
        .device
        .create_postprocess_targets(graphics.surface.info()?)?;
    let started = Instant::now();

    loop {
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            let WindowEvent::RedrawRequested(metrics) = event else {
                return Ok(());
            };
            let FrameAcquire::Ready(frame) = graphics.surface.acquire(metrics)? else {
                return Ok(());
            };
            let info = frame.surface_info();
            if info != targets.info() {
                targets = graphics.device.create_postprocess_targets(info)?;
            }
            let aspect = info.extent().width() as f32 / info.extent().height() as f32;
            let transforms = instance_transforms(started.elapsed().as_secs_f32(), aspect);
            let batches = [
                TexturedInstanceBatch {
                    mesh: &cube,
                    texture: &amber,
                    pipeline: &scene_pipeline,
                    model_view_projections: &transforms[0],
                },
                TexturedInstanceBatch {
                    mesh: &cube,
                    texture: &violet,
                    pipeline: &scene_pipeline,
                    model_view_projections: &transforms[1],
                },
                TexturedInstanceBatch {
                    mesh: &pyramid,
                    texture: &amber,
                    pipeline: &scene_pipeline,
                    model_view_projections: &transforms[2],
                },
                TexturedInstanceBatch {
                    mesh: &pyramid,
                    texture: &violet,
                    pipeline: &scene_pipeline,
                    model_view_projections: &transforms[3],
                },
            ];
            graphics.queue.render_and_present(
                frame,
                SceneSubmission {
                    content: SceneContent::Instanced(&batches),
                    output: SceneOutput::Postprocessed {
                        pipeline: &postprocess_pipeline,
                        targets: &targets,
                        uniform: &[],
                    },
                    shadow: None,
                    overlay: None,
                    clear: CLEAR,
                },
            )?;
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            break;
        }
    }

    graphics.shutdown()?;
    Ok(())
}
