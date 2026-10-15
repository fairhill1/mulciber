//! Renders a spinning textured cube through Mulciber's target-selected native backend.

mod scene;

use std::error::Error;
use std::time::Instant;

use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, MipChain, OpenedGraphics, SampleCount, Scene,
    SceneOutput, SceneSubmission, ShaderArtifact, TexelData, TextureDescriptor, TextureShape,
    TexturedSceneDraw,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};

use scene::{CUBE_INDICES, CUBE_VERTICES, checkerboard, transform};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cube.shaderbin"));
const CLEAR: ClearColor = ClearColor::opaque(0.025, 0.035, 0.055);

#[allow(clippy::cast_precision_loss)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — same-source textured cube",
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
    let texture = graphics.device.create_texture(&TextureDescriptor {
        shape: TextureShape::D2 {
            width: 8,
            height: 8,
        },
        mips: MipChain::Single,
        data: TexelData::Rgba8Srgb(&[&checkerboard()]),
    })?;
    let pipeline = graphics
        .device
        .create_textured_pipeline(ShaderArtifact::new(SHADER)?)?;
    let mut targets = graphics
        .device
        .create_render_targets(graphics.surface.info()?)?;
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
                        targets = graphics.device.create_render_targets(info)?;
                    }
                    let aspect = info.extent().width() as f32 / info.extent().height() as f32;
                    graphics.queue.render_and_present(
                        frame,
                        SceneSubmission {
                            scene: Scene::Textured {
                                draws: &[TexturedSceneDraw {
                                    mesh: &mesh,
                                    texture: &texture,
                                    pipeline: &pipeline,
                                    model_view_projection: transform(
                                        started.elapsed().as_secs_f32(),
                                        aspect,
                                    ),
                                }],
                                output: SceneOutput::Direct(&targets),
                            },
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
