//! Renders a spinning textured cube through Mulciber's target-selected native backend.
//!
//! The smallest program that draws: one mesh in a declared vertex layout, one texture, and one
//! material pipeline whose declaration matches `cube.wgsl` (compiled by `build.rs`), drawn as one
//! record a frame with its transform as uniform bytes.

mod scene;

use std::error::Error;
use std::time::Instant;

use mulciber::{
    BlendMode, ClearColor, DepthMode, Device, DeviceRequest, FrameAcquire, GeometrySource,
    GraphicsError, MaterialBinding, MaterialPipeline, MaterialPipelineDescriptor, MaterialRecord,
    MeshDescriptor, MeshIndices, MipChain, OpenedGraphics, SampleCount, SamplerAddress,
    SamplerFilter, Scene, SceneColor, SceneOutput, SceneSubmission, ShaderArtifact, TexelData,
    TextureDescriptor, TextureShape,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};

use scene::{CUBE_INDICES, CUBE_LAYOUT, checkerboard, cube_vertices, transform};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cube.shaderbin"));
const CLEAR: ClearColor = ClearColor::opaque(0.025, 0.035, 0.055);

#[allow(clippy::cast_precision_loss)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — textured cube",
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

    let mesh = graphics.device.create_mesh(&MeshDescriptor {
        layout: CUBE_LAYOUT,
        vertices: &cube_vertices(),
        parts: &[MeshIndices::U16(&CUBE_INDICES)],
    })?;
    let texture = graphics.device.create_texture(&TextureDescriptor {
        shape: TextureShape::D2 {
            width: 8,
            height: 8,
        },
        mips: MipChain::Single,
        data: TexelData::Rgba8Srgb(&[&checkerboard()]),
    })?;
    let pipeline = cube_pipeline(&graphics.device)?;
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
                    // A resized or reconfigured surface needs targets to match it.
                    let info = frame.surface_info();
                    if info != targets.info() {
                        targets = graphics.device.create_render_targets(info)?;
                    }
                    let aspect = info.extent().width() as f32 / info.extent().height() as f32;
                    let uniform = transform(started.elapsed().as_secs_f32(), aspect);
                    graphics.queue.render_and_present(
                        frame,
                        SceneSubmission {
                            scene: Scene {
                                records: &[MaterialRecord {
                                    pipeline: &pipeline,
                                    geometry: GeometrySource::Mesh(&mesh),
                                    textures: &[&texture],
                                    shadow_map: None,
                                    uniform: &uniform,
                                    storage: &[],
                                    instances: &[],
                                }],
                                shadow: None,
                                output: SceneOutput::Direct(&targets),
                            },
                            clear: CLEAR,
                        },
                    )?;
                }
                // Minimized, occluded or mid-resize: nothing to draw this time.
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

/// The pipeline drawing `cube.wgsl`. Its declaration is checked against the bindings and inputs
/// the compiler recorded in the artifact, so a mismatch with the shader fails here, naming the
/// slot or location.
fn cube_pipeline(device: &Device<'_>) -> Result<MaterialPipeline, GraphicsError> {
    device.create_material_pipeline(MaterialPipelineDescriptor {
        shader: ShaderArtifact::new(SHADER)?,
        vertex_entry: "cube_vertex",
        fragment_entry: "cube_fragment",
        vertex_layout: CUBE_LAYOUT,
        instance_layout: None,
        bindings: &[
            MaterialBinding::Uniform {
                binding: 0,
                size: 64,
            },
            MaterialBinding::Texture { binding: 1 },
            MaterialBinding::Sampler {
                binding: 2,
                filter: SamplerFilter::Nearest,
                address: SamplerAddress::Repeat,
            },
        ],
        blend: BlendMode::Opaque,
        depth: DepthMode::TestWrite,
        scene_color: SceneColor::Surface,
    })
}
