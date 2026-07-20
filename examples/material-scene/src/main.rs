//! Renders application-authored materials through Mulciber's target-selected native backend.
//!
//! This is the forcing slice for the custom-material vocabulary: two WGSL modules the crate has
//! never seen, two different application-declared vertex layouts, application-packed uniform
//! bytes updated every frame, and a material sampling two textures.

mod scene;

use std::error::Error;
use std::time::Instant;

use glam::Vec3;
use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, MaterialBinding, MaterialPipelineDescriptor,
    MaterialRecord, MeshIndices, OpenedGraphics, SampleCount, SamplerAddress, SamplerFilter,
    SceneContent, SceneOutput, SceneSubmission, ShaderArtifact,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};

const CRYSTAL_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/crystal.shaderbin"));
const LAVA_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/lava.shaderbin"));
const CLEAR: ClearColor = ClearColor::opaque(0.015, 0.02, 0.045);

#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — application-authored materials",
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

    let crystal_mesh = graphics.device.create_mesh_with_layout(
        scene::CRYSTAL_LAYOUT,
        &scene::crystal_vertices(),
        MeshIndices::U16(&scene::CUBE_INDICES),
    )?;
    let floor_mesh = graphics.device.create_mesh_with_layout(
        scene::LAVA_LAYOUT,
        &scene::floor_vertices(),
        MeshIndices::U16(&scene::FLOOR_INDICES),
    )?;
    let crystal_base =
        graphics
            .device
            .create_rgba8_srgb_texture(8, 8, &scene::crystal_base_texture())?;
    let crystal_glow =
        graphics
            .device
            .create_rgba8_srgb_texture(8, 8, &scene::crystal_glow_texture())?;
    let lava = graphics
        .device
        .create_rgba8_srgb_texture(16, 16, &scene::lava_texture())?;
    let crystal_pipeline =
        graphics
            .device
            .create_material_pipeline(MaterialPipelineDescriptor {
                shader: ShaderArtifact::new(CRYSTAL_SHADER)?,
                vertex_entry: "crystal_vertex",
                fragment_entry: "crystal_fragment",
                vertex_layout: scene::CRYSTAL_LAYOUT,
                bindings: &[
                    MaterialBinding::Uniform {
                        binding: 0,
                        size: 144,
                    },
                    MaterialBinding::Texture { binding: 1 },
                    MaterialBinding::Texture { binding: 2 },
                    MaterialBinding::Sampler {
                        binding: 3,
                        filter: SamplerFilter::Linear,
                        address: SamplerAddress::Repeat,
                    },
                ],
            })?;
    let lava_pipeline = graphics
        .device
        .create_material_pipeline(MaterialPipelineDescriptor {
            shader: ShaderArtifact::new(LAVA_SHADER)?,
            vertex_entry: "lava_vertex",
            fragment_entry: "lava_fragment",
            vertex_layout: scene::LAVA_LAYOUT,
            bindings: &[
                MaterialBinding::Uniform {
                    binding: 0,
                    size: 80,
                },
                MaterialBinding::Texture { binding: 1 },
                MaterialBinding::Sampler {
                    binding: 2,
                    filter: SamplerFilter::Linear,
                    address: SamplerAddress::Repeat,
                },
            ],
        })?;
    let mut targets = graphics
        .device
        .create_render_targets(graphics.surface.info()?)?;
    let started = Instant::now();
    let crystal_offsets = [
        Vec3::new(-2.6, 0.4, 0.0),
        Vec3::new(0.0, 0.9, -0.6),
        Vec3::new(2.6, 0.4, 0.2),
    ];

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
                    let seconds = started.elapsed().as_secs_f32();
                    let crystal_uniforms: Vec<Vec<u8>> = crystal_offsets
                        .iter()
                        .enumerate()
                        .map(|(index, &offset)| {
                            scene::crystal_uniform(seconds, aspect, index as f32 * 2.1, offset)
                        })
                        .collect();
                    let lava_uniform = scene::lava_uniform(seconds, aspect);
                    let crystal_textures = [&crystal_base, &crystal_glow];
                    let lava_textures = [&lava];
                    let mut records = Vec::with_capacity(crystal_uniforms.len() + 1);
                    records.push(MaterialRecord {
                        pipeline: &lava_pipeline,
                        mesh: &floor_mesh,
                        textures: &lava_textures,
                        uniform: &lava_uniform,
                    });
                    for uniform in &crystal_uniforms {
                        records.push(MaterialRecord {
                            pipeline: &crystal_pipeline,
                            mesh: &crystal_mesh,
                            textures: &crystal_textures,
                            uniform,
                        });
                    }
                    graphics.queue.render_and_present(
                        frame,
                        SceneSubmission {
                            content: SceneContent::Material(&records),
                            output: SceneOutput::Direct(&targets),
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
