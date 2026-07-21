//! Renders application-authored materials through Mulciber's target-selected native backend.
//!
//! This is the forcing slice for the custom-material vocabulary: six WGSL modules the crate has
//! never seen, four different application-declared vertex layouts, application-packed uniform
//! bytes updated every frame, a material sampling two textures, a cascaded depth-only shadow
//! pre-pass whose layered map the floor material samples through a comparison sampler, a
//! skinned kelp strand whose bone palette flows through a read-only storage slot into both its
//! material and its per-cascade shadow casters, and a depth-off translucent HUD gauge whose
//! geometry is rebuilt every frame and submitted as frame-transient bytes. Cascade policy —
//! split distances, per-cascade light matrices, texel snapping, depth bias, and cascade
//! selection — is application code; the crate only sees the layered map, per-cascade record
//! lists, and bytes.

mod scene;

use std::error::Error;
use std::time::Instant;

use glam::Vec3;
use mulciber::{
    BlendMode, CascadedShadowPass, ClearColor, DepthMode, DeviceRequest, FrameAcquire,
    GeometrySource, MaterialBinding, MaterialPipelineDescriptor, MaterialRecord, MeshIndices,
    OpenedGraphics, SampleCount, SamplerAddress, SamplerFilter, SceneContent, SceneOutput,
    SceneSubmission, ShaderArtifact, ShadowPipelineDescriptor, ShadowPrepass, ShadowRecord,
    ShadowSource, TransientGeometry,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};

const CRYSTAL_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/crystal.shaderbin"));
const HUD_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/hud.shaderbin"));
const LAVA_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/lava.shaderbin"));
const SHADOW_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shadow.shaderbin"));
const SKINNED_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/skinned.shaderbin"));
const SKINNED_SHADOW_SHADER: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/skinned-shadow.shaderbin"));
/// Bytes in the kelp strand's palette: six column-major `mat4x4<f32>` bone matrices.
const KELP_PALETTE_SIZE: u32 = 384;
/// Bytes in the floor's cascade block: one column-major `mat4x4<f32>` per shadow cascade.
#[allow(clippy::cast_possible_truncation)]
const LAVA_CASCADES_SIZE: u32 = (scene::CASCADE_COUNT * 64) as u32;
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
                blend: BlendMode::Opaque,
                depth: DepthMode::TestWrite,
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
                MaterialBinding::DepthTextureArray { binding: 3 },
                MaterialBinding::ComparisonSampler { binding: 4 },
                MaterialBinding::Storage {
                    binding: 5,
                    size: LAVA_CASCADES_SIZE,
                },
            ],
            blend: BlendMode::Opaque,
            depth: DepthMode::TestWrite,
        })?;
    let hud_pipeline = graphics
        .device
        .create_material_pipeline(MaterialPipelineDescriptor {
            shader: ShaderArtifact::new(HUD_SHADER)?,
            vertex_entry: "hud_vertex",
            fragment_entry: "hud_fragment",
            vertex_layout: scene::HUD_LAYOUT,
            bindings: &[],
            blend: BlendMode::PremultipliedTranslucent,
            depth: DepthMode::Off,
        })?;
    let shadow_pipeline = graphics
        .device
        .create_shadow_pipeline(ShadowPipelineDescriptor {
            shader: ShaderArtifact::new(SHADOW_SHADER)?,
            vertex_entry: "shadow_vertex",
            vertex_layout: scene::CRYSTAL_LAYOUT,
            bindings: &[MaterialBinding::Uniform {
                binding: 0,
                size: 64,
            }],
        })?;
    let kelp_mesh = graphics.device.create_mesh_with_layout(
        scene::SKINNED_LAYOUT,
        &scene::kelp_vertices(),
        MeshIndices::U16(&scene::kelp_indices()),
    )?;
    let skinned_pipeline =
        graphics
            .device
            .create_material_pipeline(MaterialPipelineDescriptor {
                shader: ShaderArtifact::new(SKINNED_SHADER)?,
                vertex_entry: "skinned_vertex",
                fragment_entry: "skinned_fragment",
                vertex_layout: scene::SKINNED_LAYOUT,
                bindings: &[
                    MaterialBinding::Uniform {
                        binding: 0,
                        size: 64,
                    },
                    MaterialBinding::Storage {
                        binding: 1,
                        size: KELP_PALETTE_SIZE,
                    },
                ],
                blend: BlendMode::Opaque,
                depth: DepthMode::TestWrite,
            })?;
    let skinned_shadow_pipeline =
        graphics
            .device
            .create_shadow_pipeline(ShadowPipelineDescriptor {
                shader: ShaderArtifact::new(SKINNED_SHADOW_SHADER)?,
                vertex_entry: "skinned_shadow_vertex",
                vertex_layout: scene::SKINNED_LAYOUT,
                bindings: &[
                    MaterialBinding::Uniform {
                        binding: 0,
                        size: 64,
                    },
                    MaterialBinding::Storage {
                        binding: 1,
                        size: KELP_PALETTE_SIZE,
                    },
                ],
            })?;
    #[allow(clippy::cast_possible_truncation)]
    let shadow_map = graphics
        .device
        .create_shadow_map_array(scene::SHADOW_MAP_SIZE, scene::CASCADE_COUNT as u32)?;
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
                    let kelp_palette = scene::kelp_bone_palette(seconds);
                    let skinned_uniform = scene::skinned_uniform(aspect);
                    let cascade_lights = scene::cascade_light_view_projections(aspect);
                    let lava_cascades = scene::lava_cascades(&cascade_lights);
                    // Every caster renders once per cascade under that cascade's light matrix.
                    let cascade_uniforms: Vec<(Vec<Vec<u8>>, Vec<u8>)> = cascade_lights
                        .iter()
                        .map(|&light| {
                            let crystals = crystal_offsets
                                .iter()
                                .enumerate()
                                .map(|(index, &offset)| {
                                    scene::crystal_shadow_uniform(
                                        light,
                                        seconds,
                                        index as f32 * 2.1,
                                        offset,
                                    )
                                })
                                .collect();
                            (crystals, scene::skinned_shadow_uniform(light))
                        })
                        .collect();
                    let cascade_records: Vec<Vec<ShadowRecord<'_>>> = cascade_uniforms
                        .iter()
                        .map(|(crystal_uniforms, kelp_uniform)| {
                            let mut records: Vec<ShadowRecord<'_>> = crystal_uniforms
                                .iter()
                                .map(|uniform| ShadowRecord {
                                    pipeline: &shadow_pipeline,
                                    mesh: &crystal_mesh,
                                    uniform,
                                    storage: &[],
                                })
                                .collect();
                            records.push(ShadowRecord {
                                pipeline: &skinned_shadow_pipeline,
                                mesh: &kelp_mesh,
                                uniform: kelp_uniform,
                                storage: &kelp_palette,
                            });
                            records
                        })
                        .collect();
                    let cascade_lists: Vec<&[ShadowRecord<'_>]> =
                        cascade_records.iter().map(Vec::as_slice).collect();
                    let crystal_textures = [&crystal_base, &crystal_glow];
                    let lava_textures = [&lava];
                    let (hud_vertices, hud_indices) = scene::hud_geometry(seconds);
                    let mut records = Vec::with_capacity(crystal_uniforms.len() + 3);
                    records.push(MaterialRecord {
                        pipeline: &lava_pipeline,
                        geometry: GeometrySource::Mesh(&floor_mesh),
                        textures: &lava_textures,
                        shadow_map: Some(ShadowSource::Array(&shadow_map)),
                        uniform: &lava_uniform,
                        storage: &lava_cascades,
                    });
                    for uniform in &crystal_uniforms {
                        records.push(MaterialRecord {
                            pipeline: &crystal_pipeline,
                            geometry: GeometrySource::Mesh(&crystal_mesh),
                            textures: &crystal_textures,
                            shadow_map: None,
                            uniform,
                            storage: &[],
                        });
                    }
                    records.push(MaterialRecord {
                        pipeline: &skinned_pipeline,
                        geometry: GeometrySource::Mesh(&kelp_mesh),
                        textures: &[],
                        shadow_map: None,
                        uniform: &skinned_uniform,
                        storage: &kelp_palette,
                    });
                    // The overlay draws last with depth off, its geometry rebuilt this frame
                    // and staged through the frame-transient geometry region.
                    records.push(MaterialRecord {
                        pipeline: &hud_pipeline,
                        geometry: GeometrySource::Transient(TransientGeometry {
                            vertices: &hud_vertices,
                            indices: MeshIndices::U16(&hud_indices),
                        }),
                        textures: &[],
                        shadow_map: None,
                        uniform: &[],
                        storage: &[],
                    });
                    graphics.queue.render_and_present(
                        frame,
                        SceneSubmission {
                            content: SceneContent::Material(&records),
                            output: SceneOutput::Direct(&targets),
                            shadow: Some(ShadowPrepass::Cascaded(CascadedShadowPass {
                                map: &shadow_map,
                                cascades: &cascade_lists,
                            })),
                            overlay: None,
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
