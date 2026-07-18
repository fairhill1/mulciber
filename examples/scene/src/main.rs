//! Renders one hundred independently transformed objects in one postprocessed scene submission.

mod scene;

use std::error::Error;
use std::time::Instant;

use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, OpenedGraphics, PostprocessedScene, ShaderArtifact,
    TexturedSceneDraw,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};

use scene::{
    CUBE_INDICES, CUBE_VERTICES, PYRAMID_INDICES, PYRAMID_VERTICES, checkerboard, transforms,
};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/scene.shaderbin"));
const CLEAR: ClearColor = ClearColor::opaque(0.012, 0.018, 0.032);

#[allow(clippy::cast_precision_loss)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — 100-object scene",
        LogicalSize::new(1100, 700),
    ))?;
    let initial_metrics = application.wait_for_first_metrics(&window)?;
    let mut graphics = OpenedGraphics::open(
        window.surface_target(),
        initial_metrics,
        DeviceRequest::default(),
    )?;
    println!(
        "backend: {}, samples: {:?}, scene objects: 100",
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
        .create_textured_pipeline(ShaderArtifact::new(SHADER)?)?;
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
            let transforms = transforms(started.elapsed().as_secs_f32(), aspect);
            let draws: Vec<_> = transforms
                .iter()
                .enumerate()
                .map(|(index, &model_view_projection)| TexturedSceneDraw {
                    mesh: if index % 2 == 0 { &cube } else { &pyramid },
                    texture: if index % 3 == 0 { &violet } else { &amber },
                    pipeline: &scene_pipeline,
                    model_view_projection,
                })
                .collect();
            graphics
                .queue
                .draw_textured_scene_postprocessed_and_present(
                    frame,
                    PostprocessedScene {
                        draws: &draws,
                        postprocess_pipeline: &postprocess_pipeline,
                        targets: &targets,
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
