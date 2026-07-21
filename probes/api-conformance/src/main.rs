//! Asserted conformance cases for the public graphics slice.
//!
//! Unlike the interactive example and the finite validation probes, every case here asserts its
//! observable outcome and the process exits nonzero on the first divergence. The cases cover
//! invalid usage, deferred abandonment recovery, abandonment-driven surface generations, the
//! observable one-sample fallback, explicit and drop-driven resource reclamation, mixed-session
//! rejection, material declaration/interface validation, frame-transient geometry supply and
//! validation, and fallible shutdown.

use std::error::Error;
use std::time::Instant;

use mulciber::{
    BlendMode, CascadedShadowPass, ClearColor, DepthMode, DeviceRequest, FrameAcquire,
    GeometrySource, GraphicsErrorKind, MaterialBinding, MaterialRecord, Mesh, MeshIndices,
    OpenedGraphics, PostprocessPipelineDescriptor, PostprocessedDraw, PostprocessedScene,
    RenderScale, SampleCount, SamplerAddress, SamplerFilter, SceneContent, SceneOutput,
    SceneSubmission, ShaderArtifact, ShadowPass, ShadowPrepass, ShadowRecord, ShadowSource,
    TRANSIENT_GEOMETRY_SIZE_LIMIT, TexturedDraw, TexturedInstanceBatch, TexturedScene,
    TexturedSceneDraw, TransientGeometry, Vertex, VertexAttribute, VertexFormat, VertexLayout,
};
use mulciber_platform::{
    Application, LogicalSize, PumpStatus, Window, WindowDescriptor, WindowEvent, WindowMetrics,
};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cube.shaderbin"));
const INSTANCED_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/instanced.shaderbin"));
const MATERIAL_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/material.shaderbin"));
const LAVA_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/lava.shaderbin"));
const SHADOW_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shadow.shaderbin"));
const SKINNED_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/skinned.shaderbin"));
const SKINNED_SHADOW_SHADER: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/skinned-shadow.shaderbin"));
const SPROUT_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sprout.shaderbin"));
const SPROUT_SHADOW_SHADER: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/sprout-shadow.shaderbin"));

/// Bytes in the skinned module's recorded palette: six column-major `mat4x4<f32>` bones.
const PALETTE_SIZE: u32 = 384;

/// Bytes in the lava module's recorded cascade block: three column-major `mat4x4<f32>`
/// light-from-model matrices.
const CASCADES_SIZE: u32 = 192;

/// The crystal module's recorded vertex interface: position, normal, texture coordinate, and a
/// per-vertex glow weight.
const MATERIAL_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 36,
    attributes: &[
        VertexAttribute {
            location: 0,
            format: VertexFormat::Float32x3,
            offset: 0,
        },
        VertexAttribute {
            location: 1,
            format: VertexFormat::Float32x3,
            offset: 12,
        },
        VertexAttribute {
            location: 2,
            format: VertexFormat::Float32x2,
            offset: 24,
        },
        VertexAttribute {
            location: 3,
            format: VertexFormat::Float32,
            offset: 32,
        },
    ],
};

/// The crystal module's recorded binding interface: one 144-byte uniform, two sampled textures,
/// and one sampler.
const MATERIAL_BINDINGS: [MaterialBinding; 4] = [
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
];

/// The lava module's recorded vertex interface: position and texture coordinate.
const LAVA_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 20,
    attributes: &[
        VertexAttribute {
            location: 0,
            format: VertexFormat::Float32x3,
            offset: 0,
        },
        VertexAttribute {
            location: 1,
            format: VertexFormat::Float32x2,
            offset: 12,
        },
    ],
};

/// The lava module's recorded binding interface, including the shadow map array's
/// depth-texture-array and fixed-recipe comparison-sampler slots plus the per-cascade
/// light-matrix storage slot.
const LAVA_BINDINGS: [MaterialBinding; 6] = [
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
        size: CASCADES_SIZE,
    },
];

/// The skinned module's recorded vertex interface: position, normal, bone indices, and weights.
const SKINNED_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 56,
    attributes: &[
        VertexAttribute {
            location: 0,
            format: VertexFormat::Float32x3,
            offset: 0,
        },
        VertexAttribute {
            location: 1,
            format: VertexFormat::Float32x3,
            offset: 12,
        },
        VertexAttribute {
            location: 2,
            format: VertexFormat::Uint32x4,
            offset: 24,
        },
        VertexAttribute {
            location: 3,
            format: VertexFormat::Float32x4,
            offset: 40,
        },
    ],
};

/// The skinned module's recorded binding interface: one 64-byte uniform and the bone palette in
/// a read-only storage slot.
const SKINNED_BINDINGS: [MaterialBinding; 2] = [
    MaterialBinding::Uniform {
        binding: 0,
        size: 64,
    },
    MaterialBinding::Storage {
        binding: 1,
        size: PALETTE_SIZE,
    },
];

/// The sprout module's recorded per-vertex interface: position and texture coordinate.
const SPROUT_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 20,
    attributes: &[
        VertexAttribute {
            location: 0,
            format: VertexFormat::Float32x3,
            offset: 0,
        },
        VertexAttribute {
            location: 1,
            format: VertexFormat::Float32x2,
            offset: 12,
        },
    ],
};

/// The sprout module's recorded instance-rate interface: one column-major `mat4x4<f32>` model
/// matrix as four instance-stepped `vec4<f32>` locations.
const SPROUT_INSTANCE_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 64,
    attributes: &[
        VertexAttribute {
            location: 4,
            format: VertexFormat::Float32x4,
            offset: 0,
        },
        VertexAttribute {
            location: 5,
            format: VertexFormat::Float32x4,
            offset: 16,
        },
        VertexAttribute {
            location: 6,
            format: VertexFormat::Float32x4,
            offset: 32,
        },
        VertexAttribute {
            location: 7,
            format: VertexFormat::Float32x4,
            offset: 48,
        },
    ],
};

/// The sprout modules' recorded binding interface: one 64-byte uniform, one sampled texture,
/// and one sampler — shared by the material pipeline and its cutout shadow caster.
const SPROUT_BINDINGS: [MaterialBinding; 3] = [
    MaterialBinding::Uniform {
        binding: 0,
        size: 64,
    },
    MaterialBinding::Texture { binding: 1 },
    MaterialBinding::Sampler {
        binding: 2,
        filter: SamplerFilter::Linear,
        address: SamplerAddress::ClampToEdge,
    },
];

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

const SHIFTED: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.35, 0.0, 0.0, 1.0],
];

const TRIANGLE_VERTICES: [Vertex; 3] = [
    Vertex {
        position: [-0.5, -0.5, 0.0],
        color: [1.0, 1.0, 1.0],
        uv: [0.0, 1.0],
    },
    Vertex {
        position: [0.5, -0.5, 0.0],
        color: [1.0, 1.0, 1.0],
        uv: [1.0, 1.0],
    },
    Vertex {
        position: [0.0, 0.5, 0.0],
        color: [1.0, 1.0, 1.0],
        uv: [0.5, 0.0],
    },
];

fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — public API conformance",
        LogicalSize::new(640, 400),
    ))?;
    let initial_metrics = application.wait_for_first_metrics(&window)?;
    let mut cases = Cases::new(&window, initial_metrics)?;
    let started = Instant::now();
    let mut passed = None;

    while passed.is_none() {
        if started.elapsed().as_secs() > 60 {
            return Err("conformance run exceeded its 60 second budget".into());
        }
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            if passed.is_some() {
                return Ok(());
            }
            let WindowEvent::RedrawRequested(metrics) = event else {
                return Ok(());
            };
            if cases.advance(metrics)? {
                passed = Some(cases.passed);
            }
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            return Err("window closed before the conformance cases completed".into());
        }
    }

    let passed = passed.expect("loop exits only after every case passed");
    println!("conformance: {passed} case(s) passed");
    Ok(())
}

/// Sequential conformance state driven by one acquisition-consuming step per redraw.
struct Cases<'window> {
    step: u32,
    passed: u32,
    graphics: Option<OpenedGraphics<'window>>,
    window: &'window Window,
    mesh: Option<Mesh>,
    foreign_mesh: Option<Mesh>,
    texture: Option<mulciber::Texture>,
    pipeline: Option<mulciber::TexturedPipeline>,
    instanced_pipeline: Option<mulciber::InstancedTexturedPipeline>,
    targets: Option<mulciber::RenderTargets>,
    postprocess_pipeline: Option<mulciber::PostprocessPipeline>,
    postprocess_uniform_pipeline: Option<mulciber::PostprocessPipeline>,
    postprocess_targets: Option<mulciber::PostprocessTargets>,
    material_pipeline: Option<mulciber::MaterialPipeline>,
    foreign_material_pipeline: Option<mulciber::MaterialPipeline>,
    material_mesh: Option<Mesh>,
    shadow_map: Option<mulciber::ShadowMap>,
    shadow_map_array: Option<mulciber::ShadowMapArray>,
    shadow_pipeline: Option<mulciber::ShadowPipeline>,
    shadowed_pipeline: Option<mulciber::MaterialPipeline>,
    floor_mesh: Option<Mesh>,
    skinned_pipeline: Option<mulciber::MaterialPipeline>,
    skinned_shadow_pipeline: Option<mulciber::ShadowPipeline>,
    skinned_mesh: Option<Mesh>,
    sprout_pipeline: Option<mulciber::MaterialPipeline>,
    sprout_shadow_pipeline: Option<mulciber::ShadowPipeline>,
    sprout_mesh: Option<Mesh>,
    sprout_shadow_map: Option<mulciber::ShadowMap>,
}

impl<'window> Cases<'window> {
    fn new(window: &'window Window, metrics: WindowMetrics) -> Result<Self, Box<dyn Error>> {
        let graphics =
            OpenedGraphics::open(window.surface_target(), metrics, DeviceRequest::default())?;
        Ok(Self {
            step: 0,
            passed: 0,
            graphics: Some(graphics),
            window,
            mesh: None,
            foreign_mesh: None,
            texture: None,
            pipeline: None,
            instanced_pipeline: None,
            targets: None,
            postprocess_pipeline: None,
            postprocess_uniform_pipeline: None,
            postprocess_targets: None,
            material_pipeline: None,
            foreign_material_pipeline: None,
            material_mesh: None,
            shadow_map: None,
            shadow_map_array: None,
            shadow_pipeline: None,
            shadowed_pipeline: None,
            floor_mesh: None,
            skinned_pipeline: None,
            skinned_shadow_pipeline: None,
            skinned_mesh: None,
            sprout_pipeline: None,
            sprout_shadow_pipeline: None,
            sprout_mesh: None,
            sprout_shadow_map: None,
        })
    }

    fn pass(&mut self, name: &str) {
        self.passed += 1;
        println!("conformance: {name} ok");
    }

    /// Runs the next step. Returns `Ok(true)` when every case has passed.
    #[allow(clippy::too_many_lines)]
    fn advance(&mut self, metrics: WindowMetrics) -> Result<bool, Box<dyn Error>> {
        match self.step {
            // Creation-time validation and one draw-time validation failure, all on session A.
            0 => {
                {
                    let graphics = self.graphics.as_ref().expect("session A is open");
                    expect_error(
                        graphics.device.create_mesh(&[], &[]).map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "non-empty",
                        "empty mesh rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_mesh(&TRIANGLE_VERTICES, &[0, 1, 3])
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "out-of-range",
                        "out-of-range index rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_rgba8_srgb_texture(4, 4, &[0_u8; 4])
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "does not match",
                        "texture byte mismatch rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_rgba8_srgb_texture(0, 4, &[])
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "does not match",
                        "zero-dimension texture rejected",
                    )?;
                }
                self.pass("empty mesh rejected");
                self.pass("out-of-range index rejected");
                self.pass("texture byte mismatch rejected");
                self.pass("zero-dimension texture rejected");

                let graphics = self.graphics.as_ref().expect("session A is open");
                let mesh = graphics
                    .device
                    .create_mesh(&TRIANGLE_VERTICES, &[0, 1, 2])?;
                let texture = graphics
                    .device
                    .create_rgba8_srgb_texture(2, 2, &[255_u8; 16])?;
                let pipeline = graphics
                    .device
                    .create_textured_pipeline(ShaderArtifact::new(SHADER)?)?;
                let instanced_pipeline = graphics
                    .device
                    .create_instanced_textured_pipeline(ShaderArtifact::new(INSTANCED_SHADER)?)?;
                let targets = graphics
                    .device
                    .create_render_targets(graphics.surface.info()?)?;
                self.mesh = Some(mesh);
                self.texture = Some(texture);
                self.pipeline = Some(pipeline);
                self.instanced_pipeline = Some(instanced_pipeline);
                self.targets = Some(targets);
                self.step = 1;
                Ok(false)
            }
            // A non-finite transform is rejected at draw time; the consumed frame's deferred
            // abandonment must not poison the session.
            1 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let graphics = self.graphics.as_mut().expect("session A is open");
                let mut transform = IDENTITY;
                transform[3][3] = f32::NAN;
                let draw = TexturedDraw {
                    mesh: self.mesh.as_ref().expect("mesh exists"),
                    texture: self.texture.as_ref().expect("texture exists"),
                    pipeline: self.pipeline.as_ref().expect("pipeline exists"),
                    targets: self.targets.as_ref().expect("targets exist"),
                    model_view_projection: transform,
                    clear: ClearColor::BLACK,
                };
                expect_error(
                    graphics
                        .queue
                        .draw_textured_and_present(frame, draw)
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "finite",
                    "non-finite transform rejected",
                )?;
                self.pass("non-finite transform rejected");
                self.step = 2;
                Ok(false)
            }
            // Explicit abandonment succeeds and the session recovers.
            2 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                frame.abandon()?;
                self.pass("acquired frame abandoned");
                self.step = 3;
                Ok(false)
            }
            // If abandonment replaced the generation, stale targets must be rejected before a
            // rebuilt set presents; otherwise the original targets still present. Either branch
            // must end in one successful presentation.
            3 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let info = frame.surface_info();
                let stale_info = self.targets.as_ref().expect("targets exist").info();
                if info == stale_info {
                    let disposition = self.draw(frame, IDENTITY)?;
                    assert_presented(disposition)?;
                    self.pass("presentation after abandonment (stable generation)");
                    self.step = 5;
                } else {
                    let graphics = self.graphics.as_mut().expect("session A is open");
                    let draw = TexturedDraw {
                        mesh: self.mesh.as_ref().expect("mesh exists"),
                        texture: self.texture.as_ref().expect("texture exists"),
                        pipeline: self.pipeline.as_ref().expect("pipeline exists"),
                        targets: self.targets.as_ref().expect("targets exist"),
                        model_view_projection: IDENTITY,
                        clear: ClearColor::BLACK,
                    };
                    expect_error(
                        graphics
                            .queue
                            .draw_textured_and_present(frame, draw)
                            .map(|_| ()),
                        GraphicsErrorKind::StaleResource,
                        "stale",
                        "superseded-generation targets rejected",
                    )?;
                    self.targets = Some(graphics.device.create_render_targets(info)?);
                    self.pass("superseded-generation targets rejected");
                    self.step = 4;
                }
                Ok(false)
            }
            // Present with the rebuilt targets after the stale rejection.
            4 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let disposition = self.draw(frame, IDENTITY)?;
                assert_presented(disposition)?;
                self.pass("presentation after generation replacement");
                self.step = 5;
                Ok(false)
            }
            // Every resource kind can be destroyed explicitly after presentation. Repeatedly
            // dropping meshes also drives generational slot reuse before replacement resources
            // are created for another draw.
            5 => {
                let graphics = self.graphics.as_ref().expect("session A is open");
                let postprocess_pipeline = graphics
                    .device
                    .create_postprocess_pipeline(ShaderArtifact::new(SHADER)?)?;
                let postprocess_targets = graphics
                    .device
                    .create_postprocess_targets(graphics.surface.info()?)?;

                graphics
                    .device
                    .destroy_postprocess_targets(postprocess_targets)?;
                graphics
                    .device
                    .destroy_postprocess_pipeline(postprocess_pipeline)?;
                graphics
                    .device
                    .destroy_render_targets(self.targets.take().expect("render targets exist"))?;
                graphics.device.destroy_textured_pipeline(
                    self.pipeline.take().expect("textured pipeline exists"),
                )?;
                graphics.device.destroy_instanced_textured_pipeline(
                    self.instanced_pipeline
                        .take()
                        .expect("instanced textured pipeline exists"),
                )?;
                graphics
                    .device
                    .destroy_texture(self.texture.take().expect("texture exists"))?;
                graphics
                    .device
                    .destroy_mesh(self.mesh.take().expect("mesh exists"))?;

                for _ in 0..32 {
                    drop(
                        graphics
                            .device
                            .create_mesh(&TRIANGLE_VERTICES, &[0, 1, 2])?,
                    );
                }

                self.foreign_mesh = Some(
                    graphics
                        .device
                        .create_mesh(&TRIANGLE_VERTICES, &[0, 1, 2])?,
                );
                self.mesh = Some(
                    graphics
                        .device
                        .create_mesh(&TRIANGLE_VERTICES, &[0, 1, 2])?,
                );
                self.texture = Some(graphics.device.create_rgba8_srgb_texture(
                    2,
                    2,
                    &[255_u8; 16],
                )?);
                self.pipeline = Some(
                    graphics
                        .device
                        .create_textured_pipeline(ShaderArtifact::new(SHADER)?)?,
                );
                self.instanced_pipeline =
                    Some(graphics.device.create_instanced_textured_pipeline(
                        ShaderArtifact::new(INSTANCED_SHADER)?,
                    )?);
                self.targets = Some(
                    graphics
                        .device
                        .create_render_targets(graphics.surface.info()?)?,
                );
                self.postprocess_pipeline = Some(
                    graphics
                        .device
                        .create_postprocess_pipeline(ShaderArtifact::new(SHADER)?)?,
                );
                self.postprocess_uniform_pipeline =
                    Some(graphics.device.create_postprocess_pipeline(
                        PostprocessPipelineDescriptor {
                            shader: ShaderArtifact::new(SHADER)?,
                            uniform_size: Some(64),
                        },
                    )?);
                self.postprocess_targets = Some(
                    graphics
                        .device
                        .create_postprocess_targets(graphics.surface.info()?)?,
                );
                self.pass("explicit destruction for every resource kind");
                self.pass("drop-driven resource churn");
                self.step = 6;
                Ok(false)
            }
            // Replacement resources remain usable across multiple direct draws after old arena
            // slots were reclaimed.
            6 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let draws = scene_draws(
                    self.mesh.as_ref().expect("mesh exists"),
                    self.texture.as_ref().expect("texture exists"),
                    self.pipeline.as_ref().expect("pipeline exists"),
                );
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.draw_textured_scene_and_present(
                    frame,
                    TexturedScene {
                        draws: &draws,
                        targets: self.targets.as_ref().expect("targets exist"),
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("multi-draw presentation after resource replacement");
                self.step = 7;
                Ok(false)
            }
            // The same object sequence remains valid through resolved scene color and the
            // fullscreen postprocess pass.
            7 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let draws = scene_draws(
                    self.mesh.as_ref().expect("mesh exists"),
                    self.texture.as_ref().expect("texture exists"),
                    self.pipeline.as_ref().expect("pipeline exists"),
                );
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics
                    .queue
                    .draw_textured_scene_postprocessed_and_present(
                        frame,
                        PostprocessedScene {
                            draws: &draws,
                            postprocess_pipeline: self
                                .postprocess_pipeline
                                .as_ref()
                                .expect("postprocess pipeline exists"),
                            targets: self
                                .postprocess_targets
                                .as_ref()
                                .expect("postprocess targets exist"),
                            uniform: &[],
                            clear: ClearColor::BLACK,
                        },
                    )?;
                assert_presented(disposition)?;
                self.pass("postprocessed multi-draw presentation");
                self.step = 8;
                Ok(false)
            }
            // Native instance-rate transforms draw the same mesh twice in one batch.
            8 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let transforms = [IDENTITY, SHIFTED];
                let batches = [TexturedInstanceBatch {
                    mesh: self.mesh.as_ref().expect("mesh exists"),
                    texture: self.texture.as_ref().expect("texture exists"),
                    pipeline: self
                        .instanced_pipeline
                        .as_ref()
                        .expect("instanced pipeline exists"),
                    model_view_projections: &transforms,
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.render_and_present(
                    frame,
                    SceneSubmission {
                        content: SceneContent::Instanced(&batches),
                        output: SceneOutput::Direct(self.targets.as_ref().expect("targets exist")),
                        shadow: None,
                        overlay: None,
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("instanced presentation");
                self.step = 9;
                Ok(false)
            }
            // The instance path remains valid through resolved color and post-processing.
            9 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let transforms = [IDENTITY, SHIFTED];
                let batches = [TexturedInstanceBatch {
                    mesh: self.mesh.as_ref().expect("mesh exists"),
                    texture: self.texture.as_ref().expect("texture exists"),
                    pipeline: self
                        .instanced_pipeline
                        .as_ref()
                        .expect("instanced pipeline exists"),
                    model_view_projections: &transforms,
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.render_and_present(
                    frame,
                    SceneSubmission {
                        content: SceneContent::Instanced(&batches),
                        output: SceneOutput::Postprocessed {
                            pipeline: self
                                .postprocess_pipeline
                                .as_ref()
                                .expect("postprocess pipeline exists"),
                            targets: self
                                .postprocess_targets
                                .as_ref()
                                .expect("postprocess targets exist"),
                            uniform: &[],
                        },
                        shadow: None,
                        overlay: None,
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("postprocessed instanced presentation");
                self.step = 10;
                Ok(false)
            }
            // Material declarations are validated against the artifact's compiler-recorded
            // interface, and raw vertex bytes against their declared layout, before any native
            // pipeline work happens. Each rejection must name the offending slot or location.
            10 => {
                let shader = ShaderArtifact::new(MATERIAL_SHADER)?;
                {
                    let graphics = self.graphics.as_ref().expect("session A is open");
                    expect_error(
                        graphics
                            .device
                            .create_postprocess_pipeline(PostprocessPipelineDescriptor {
                                shader: ShaderArtifact::new(SHADER)?,
                                uniform_size: Some(32),
                            })
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "declares 32 bytes but the shader artifact records 64",
                        "postprocess uniform interface mismatch rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_postprocess_pipeline(PostprocessPipelineDescriptor {
                                shader: ShaderArtifact::new(SHADER)?,
                                uniform_size: Some(257),
                            })
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "outside the supported 1 through 256",
                        "oversized postprocess uniform declaration rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_material_pipeline(mulciber::MaterialPipelineDescriptor {
                                shader,
                                vertex_entry: "crystal_vertex",
                                fragment_entry: "crystal_fragment",
                                vertex_layout: MATERIAL_LAYOUT,
                                bindings: &[
                                    MaterialBinding::Uniform {
                                        binding: 0,
                                        size: 80,
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
                                instance_layout: None,
                            })
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "declares 80 bytes",
                        "material uniform size mismatch rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_material_pipeline(mulciber::MaterialPipelineDescriptor {
                                shader,
                                vertex_entry: "missing_vertex",
                                fragment_entry: "crystal_fragment",
                                vertex_layout: MATERIAL_LAYOUT,
                                bindings: &MATERIAL_BINDINGS,
                                blend: BlendMode::Opaque,
                                depth: DepthMode::TestWrite,
                                instance_layout: None,
                            })
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "no vertex entry point",
                        "missing entry point rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_material_pipeline(mulciber::MaterialPipelineDescriptor {
                                shader,
                                vertex_entry: "crystal_vertex",
                                fragment_entry: "crystal_fragment",
                                vertex_layout: VertexLayout {
                                    stride: 36,
                                    attributes: &MATERIAL_LAYOUT.attributes[..3],
                                },
                                bindings: &MATERIAL_BINDINGS,
                                blend: BlendMode::Opaque,
                                depth: DepthMode::TestWrite,
                                instance_layout: None,
                            })
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "consumes location 3",
                        "undeclared vertex input rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_material_pipeline(mulciber::MaterialPipelineDescriptor {
                                shader,
                                vertex_entry: "crystal_vertex",
                                fragment_entry: "crystal_fragment",
                                vertex_layout: MATERIAL_LAYOUT,
                                bindings: &MATERIAL_BINDINGS[..3],
                                blend: BlendMode::Opaque,
                                depth: DepthMode::TestWrite,
                                instance_layout: None,
                            })
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "records binding slot 3",
                        "undeclared binding slot rejected",
                    )?;
                    let truncated = material_triangle_vertices();
                    expect_error(
                        graphics
                            .device
                            .create_mesh_with_layout(
                                MATERIAL_LAYOUT,
                                &truncated[..truncated.len() - 4],
                                MeshIndices::U16(&[0, 1, 2]),
                            )
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "multiple of the layout stride",
                        "vertex byte stride mismatch rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_mesh_with_layout(
                                MATERIAL_LAYOUT,
                                &material_triangle_vertices(),
                                MeshIndices::U32(&[0, 1, 3]),
                            )
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "out-of-range index",
                        "out-of-range u32 mesh index rejected",
                    )?;
                }
                self.pass("material uniform size mismatch rejected");
                self.pass("postprocess uniform interface mismatch rejected");
                self.pass("oversized postprocess uniform declaration rejected");
                self.pass("missing entry point rejected");
                self.pass("undeclared vertex input rejected");
                self.pass("undeclared binding slot rejected");
                self.pass("vertex byte stride mismatch rejected");
                self.pass("out-of-range u32 mesh index rejected");

                let graphics = self.graphics.as_ref().expect("session A is open");
                self.material_pipeline = Some(graphics.device.create_material_pipeline(
                    mulciber::MaterialPipelineDescriptor {
                        shader,
                        vertex_entry: "crystal_vertex",
                        fragment_entry: "crystal_fragment",
                        vertex_layout: MATERIAL_LAYOUT,
                        bindings: &MATERIAL_BINDINGS,
                        blend: BlendMode::Opaque,
                        depth: DepthMode::TestWrite,
                        instance_layout: None,
                    },
                )?);
                self.foreign_material_pipeline = Some(graphics.device.create_material_pipeline(
                    mulciber::MaterialPipelineDescriptor {
                        shader,
                        vertex_entry: "crystal_vertex",
                        fragment_entry: "crystal_fragment",
                        vertex_layout: MATERIAL_LAYOUT,
                        bindings: &MATERIAL_BINDINGS,
                        blend: BlendMode::Opaque,
                        depth: DepthMode::TestWrite,
                        instance_layout: None,
                    },
                )?);
                // 32-bit indices deliberately back the shared material mesh so every material
                // draw, presentation, and reclamation case exercises the u32 index path.
                self.material_mesh = Some(graphics.device.create_mesh_with_layout(
                    MATERIAL_LAYOUT,
                    &material_triangle_vertices(),
                    MeshIndices::U32(&[0, 1, 2]),
                )?);
                self.step = 11;
                Ok(false)
            }
            // A record whose uniform bytes do not match the declared size is rejected.
            11 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let short_uniform = [0_u8; 80];
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Mesh(
                        self.material_mesh.as_ref().expect("material mesh"),
                    ),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &short_uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "uniform bytes",
                    "material uniform length mismatch rejected",
                )?;
                self.pass("material uniform length mismatch rejected");
                self.step = 12;
                Ok(false)
            }
            // A record with the wrong number of textures for the declared slots is rejected.
            12 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let textures = [self.texture.as_ref().expect("texture exists")];
                let uniform = [0_u8; 144];
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Mesh(
                        self.material_mesh.as_ref().expect("material mesh"),
                    ),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "texture slots",
                    "material texture count mismatch rejected",
                )?;
                self.pass("material texture count mismatch rejected");
                self.step = 13;
                Ok(false)
            }
            // A mesh whose declared layout differs from the pipeline's is rejected.
            13 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = [0_u8; 144];
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Mesh(
                        self.mesh.as_ref().expect("fixed-layout mesh exists"),
                    ),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "declared layout",
                    "material mesh layout mismatch rejected",
                )?;
                self.pass("material mesh layout mismatch rejected");
                self.step = 14;
                Ok(false)
            }
            // Four material records with application-packed uniform bytes present directly; after
            // the baseline record, one draws through a nearest-filter clamp-to-edge sampler
            // pipeline, one through an alpha-to-coverage cutout pipeline with depth off, and one
            // through a premultiplied-translucent pipeline with a read-only depth test, so every
            // declared sampler, blend, and depth mode reaches native pipeline state in one
            // submission.
            14 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let (nearest_pipeline, cutout_pipeline, translucent_pipeline) = {
                    let graphics = self.graphics.as_ref().expect("session A is open");
                    let nearest = graphics.device.create_material_pipeline(
                        mulciber::MaterialPipelineDescriptor {
                            shader: ShaderArtifact::new(MATERIAL_SHADER)?,
                            vertex_entry: "crystal_vertex",
                            fragment_entry: "crystal_fragment",
                            vertex_layout: MATERIAL_LAYOUT,
                            bindings: &[
                                MaterialBinding::Uniform {
                                    binding: 0,
                                    size: 144,
                                },
                                MaterialBinding::Texture { binding: 1 },
                                MaterialBinding::Texture { binding: 2 },
                                MaterialBinding::Sampler {
                                    binding: 3,
                                    filter: SamplerFilter::Nearest,
                                    address: SamplerAddress::ClampToEdge,
                                },
                            ],
                            blend: BlendMode::Opaque,
                            depth: DepthMode::TestWrite,
                            instance_layout: None,
                        },
                    )?;
                    let cutout = graphics.device.create_material_pipeline(
                        mulciber::MaterialPipelineDescriptor {
                            shader: ShaderArtifact::new(MATERIAL_SHADER)?,
                            vertex_entry: "crystal_vertex",
                            fragment_entry: "crystal_fragment",
                            vertex_layout: MATERIAL_LAYOUT,
                            bindings: &MATERIAL_BINDINGS,
                            blend: BlendMode::Cutout,
                            depth: DepthMode::Off,
                            instance_layout: None,
                        },
                    )?;
                    let translucent = graphics.device.create_material_pipeline(
                        mulciber::MaterialPipelineDescriptor {
                            shader: ShaderArtifact::new(MATERIAL_SHADER)?,
                            vertex_entry: "crystal_vertex",
                            fragment_entry: "crystal_fragment",
                            vertex_layout: MATERIAL_LAYOUT,
                            bindings: &MATERIAL_BINDINGS,
                            blend: BlendMode::PremultipliedTranslucent,
                            depth: DepthMode::TestOnly,
                            instance_layout: None,
                        },
                    )?;
                    (nearest, cutout, translucent)
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let records = [
                    MaterialRecord {
                        pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                        geometry: GeometrySource::Mesh(
                            self.material_mesh.as_ref().expect("material mesh"),
                        ),
                        textures: &textures,
                        shadow_map: None,
                        uniform: &uniform,
                        storage: &[],
                        instances: &[],
                    },
                    MaterialRecord {
                        pipeline: &nearest_pipeline,
                        geometry: GeometrySource::Mesh(
                            self.material_mesh.as_ref().expect("material mesh"),
                        ),
                        textures: &textures,
                        shadow_map: None,
                        uniform: &uniform,
                        storage: &[],
                        instances: &[],
                    },
                    MaterialRecord {
                        pipeline: &cutout_pipeline,
                        geometry: GeometrySource::Mesh(
                            self.material_mesh.as_ref().expect("material mesh"),
                        ),
                        textures: &textures,
                        shadow_map: None,
                        uniform: &uniform,
                        storage: &[],
                        instances: &[],
                    },
                    MaterialRecord {
                        pipeline: &translucent_pipeline,
                        geometry: GeometrySource::Mesh(
                            self.material_mesh.as_ref().expect("material mesh"),
                        ),
                        textures: &textures,
                        shadow_map: None,
                        uniform: &uniform,
                        storage: &[],
                        instances: &[],
                    },
                ];
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.render_and_present(
                    frame,
                    SceneSubmission {
                        content: SceneContent::Material(&records),
                        output: SceneOutput::Direct(self.targets.as_ref().expect("targets exist")),
                        shadow: None,
                        overlay: None,
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("material presentation");
                let graphics = self.graphics.as_ref().expect("session A is open");
                graphics
                    .device
                    .destroy_material_pipeline(nearest_pipeline)?;
                graphics.device.destroy_material_pipeline(cutout_pipeline)?;
                graphics
                    .device
                    .destroy_material_pipeline(translucent_pipeline)?;
                self.pass("nearest clamp sampler material presentation");
                self.pass("cutout depth-off material presentation");
                self.pass("translucent depth-test-only material presentation");
                self.step = 15;
                Ok(false)
            }
            // A reversed-Z scene: an opaque greater-compare depth-write record, a translucent
            // greater-compare read-only record, and a depth-off record present together, so both
            // greater modes reach native pipeline state and the submission derives the 0.0
            // depth clear.
            15 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let (greater_write_pipeline, greater_only_pipeline, off_pipeline) = {
                    let graphics = self.graphics.as_ref().expect("session A is open");
                    let greater_write = graphics.device.create_material_pipeline(
                        mulciber::MaterialPipelineDescriptor {
                            shader: ShaderArtifact::new(MATERIAL_SHADER)?,
                            vertex_entry: "crystal_vertex",
                            fragment_entry: "crystal_fragment",
                            vertex_layout: MATERIAL_LAYOUT,
                            bindings: &MATERIAL_BINDINGS,
                            blend: BlendMode::Opaque,
                            depth: DepthMode::TestWriteGreater,
                            instance_layout: None,
                        },
                    )?;
                    let greater_only = graphics.device.create_material_pipeline(
                        mulciber::MaterialPipelineDescriptor {
                            shader: ShaderArtifact::new(MATERIAL_SHADER)?,
                            vertex_entry: "crystal_vertex",
                            fragment_entry: "crystal_fragment",
                            vertex_layout: MATERIAL_LAYOUT,
                            bindings: &MATERIAL_BINDINGS,
                            blend: BlendMode::PremultipliedTranslucent,
                            depth: DepthMode::TestOnlyGreater,
                            instance_layout: None,
                        },
                    )?;
                    let off = graphics.device.create_material_pipeline(
                        mulciber::MaterialPipelineDescriptor {
                            shader: ShaderArtifact::new(MATERIAL_SHADER)?,
                            vertex_entry: "crystal_vertex",
                            fragment_entry: "crystal_fragment",
                            vertex_layout: MATERIAL_LAYOUT,
                            bindings: &MATERIAL_BINDINGS,
                            blend: BlendMode::Opaque,
                            depth: DepthMode::Off,
                            instance_layout: None,
                        },
                    )?;
                    (greater_write, greater_only, off)
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let records = [
                    MaterialRecord {
                        pipeline: &off_pipeline,
                        geometry: GeometrySource::Mesh(
                            self.material_mesh.as_ref().expect("material mesh"),
                        ),
                        textures: &textures,
                        shadow_map: None,
                        uniform: &uniform,
                        storage: &[],
                        instances: &[],
                    },
                    MaterialRecord {
                        pipeline: &greater_write_pipeline,
                        geometry: GeometrySource::Mesh(
                            self.material_mesh.as_ref().expect("material mesh"),
                        ),
                        textures: &textures,
                        shadow_map: None,
                        uniform: &uniform,
                        storage: &[],
                        instances: &[],
                    },
                    MaterialRecord {
                        pipeline: &greater_only_pipeline,
                        geometry: GeometrySource::Mesh(
                            self.material_mesh.as_ref().expect("material mesh"),
                        ),
                        textures: &textures,
                        shadow_map: None,
                        uniform: &uniform,
                        storage: &[],
                        instances: &[],
                    },
                ];
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.render_and_present(
                    frame,
                    SceneSubmission {
                        content: SceneContent::Material(&records),
                        output: SceneOutput::Direct(self.targets.as_ref().expect("targets exist")),
                        shadow: None,
                        overlay: None,
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                let graphics = self.graphics.as_ref().expect("session A is open");
                graphics
                    .device
                    .destroy_material_pipeline(greater_write_pipeline)?;
                graphics
                    .device
                    .destroy_material_pipeline(greater_only_pipeline)?;
                graphics.device.destroy_material_pipeline(off_pipeline)?;
                self.pass("reversed-z greater depth-write material presentation");
                self.pass("reversed-z greater depth-test-only material presentation");
                self.step = 16;
                Ok(false)
            }
            // A scene mixing a less-compare record with a greater-compare record is rejected:
            // one depth target cannot serve both clear conventions in a single pass.
            16 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let greater_pipeline = {
                    let graphics = self.graphics.as_ref().expect("session A is open");
                    graphics.device.create_material_pipeline(
                        mulciber::MaterialPipelineDescriptor {
                            shader: ShaderArtifact::new(MATERIAL_SHADER)?,
                            vertex_entry: "crystal_vertex",
                            fragment_entry: "crystal_fragment",
                            vertex_layout: MATERIAL_LAYOUT,
                            bindings: &MATERIAL_BINDINGS,
                            blend: BlendMode::Opaque,
                            depth: DepthMode::TestWriteGreater,
                            instance_layout: None,
                        },
                    )?
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let records = [
                    MaterialRecord {
                        pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                        geometry: GeometrySource::Mesh(
                            self.material_mesh.as_ref().expect("material mesh"),
                        ),
                        textures: &textures,
                        shadow_map: None,
                        uniform: &uniform,
                        storage: &[],
                        instances: &[],
                    },
                    MaterialRecord {
                        pipeline: &greater_pipeline,
                        geometry: GeometrySource::Mesh(
                            self.material_mesh.as_ref().expect("material mesh"),
                        ),
                        textures: &textures,
                        shadow_map: None,
                        uniform: &uniform,
                        storage: &[],
                        instances: &[],
                    },
                ];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "mixes less-compare and greater-compare",
                    "mixed depth compare directions rejected",
                )?;
                let graphics = self.graphics.as_ref().expect("session A is open");
                graphics
                    .device
                    .destroy_material_pipeline(greater_pipeline)?;
                self.pass("mixed depth compare directions rejected");
                self.step = 17;
                Ok(false)
            }
            // The overlay pass composes with material content and postprocessed output only;
            // pairing it with direct output is rejected by name.
            17 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Mesh(
                        self.material_mesh.as_ref().expect("material mesh"),
                    ),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: Some(&records),
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::Unsupported,
                    "overlay pass composes with material scene content and postprocessed",
                    "overlay with direct output rejected",
                )?;
                self.pass("overlay with direct output rejected");
                self.step = 18;
                Ok(false)
            }
            // The presentable pass carries no depth target, so an overlay record whose pipeline
            // tests depth is rejected by name.
            18 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Mesh(
                        self.material_mesh.as_ref().expect("material mesh"),
                    ),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Postprocessed {
                                    pipeline: self
                                        .postprocess_pipeline
                                        .as_ref()
                                        .expect("postprocess pipeline exists"),
                                    targets: self
                                        .postprocess_targets
                                        .as_ref()
                                        .expect("postprocess targets exist"),
                                    uniform: &[],
                                },
                                shadow: None,
                                overlay: Some(&records),
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "must declare DepthMode::Off",
                    "depth-testing overlay record rejected",
                )?;
                self.pass("depth-testing overlay record rejected");
                self.step = 19;
                Ok(false)
            }
            // The material path remains valid through resolved color and post-processing — with
            // one sampled texture carrying an application-supplied mip chain and a depth-off
            // overlay record drawn into the presentable target after the resolve — and the new
            // handle kind supports explicit destruction plus drop-driven reclamation. Chains
            // that stop short of 1x1 and levels with mismatched byte counts are rejected by
            // name.
            19 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let mip_levels: [Vec<u8>; 4] = [
                    vec![255_u8; 8 * 8 * 4],
                    vec![200_u8; 4 * 4 * 4],
                    vec![128_u8; 2 * 2 * 4],
                    vec![64_u8; 4],
                ];
                let mip_refs: [&[u8]; 4] = [
                    &mip_levels[0],
                    &mip_levels[1],
                    &mip_levels[2],
                    &mip_levels[3],
                ];
                let mip_texture = {
                    let graphics = self.graphics.as_ref().expect("session A is open");
                    expect_error(
                        graphics
                            .device
                            .create_rgba8_srgb_texture_with_mips(8, 8, &mip_refs[..2])
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "needs 4 levels",
                        "partial mip chain rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_rgba8_srgb_texture_with_mips(
                                8,
                                8,
                                &[
                                    &mip_levels[0],
                                    &mip_levels[1],
                                    &mip_levels[3],
                                    &mip_levels[3],
                                ],
                            )
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "mip level 2",
                        "mip level byte mismatch rejected",
                    )?;
                    graphics
                        .device
                        .create_rgba8_srgb_texture_with_mips(8, 8, &mip_refs)?
                };
                let (unorm_texture, unorm_mip_texture) = {
                    let graphics = self.graphics.as_ref().expect("session A is open");
                    (
                        graphics
                            .device
                            .create_rgba8_unorm_texture(8, 8, &mip_levels[0])?,
                        graphics
                            .device
                            .create_rgba8_unorm_texture_with_mips(8, 8, &mip_refs)?,
                    )
                };
                let overlay_pipeline = {
                    let graphics = self.graphics.as_ref().expect("session A is open");
                    graphics.device.create_material_pipeline(
                        mulciber::MaterialPipelineDescriptor {
                            shader: ShaderArtifact::new(MATERIAL_SHADER)?,
                            vertex_entry: "crystal_vertex",
                            fragment_entry: "crystal_fragment",
                            vertex_layout: MATERIAL_LAYOUT,
                            bindings: &MATERIAL_BINDINGS,
                            blend: BlendMode::PremultipliedTranslucent,
                            depth: DepthMode::Off,
                            instance_layout: None,
                        },
                    )?
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [&unorm_mip_texture, &unorm_texture];
                let overlay_textures = [&mip_texture, texture];
                let uniform = material_uniform();
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Mesh(
                        self.material_mesh.as_ref().expect("material mesh"),
                    ),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let overlay_records = [MaterialRecord {
                    pipeline: &overlay_pipeline,
                    geometry: GeometrySource::Mesh(
                        self.material_mesh.as_ref().expect("material mesh"),
                    ),
                    textures: &overlay_textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.render_and_present(
                    frame,
                    SceneSubmission {
                        content: SceneContent::Material(&records),
                        output: SceneOutput::Postprocessed {
                            pipeline: self
                                .postprocess_pipeline
                                .as_ref()
                                .expect("postprocess pipeline exists"),
                            targets: self
                                .postprocess_targets
                                .as_ref()
                                .expect("postprocess targets exist"),
                            uniform: &[],
                        },
                        shadow: None,
                        overlay: Some(&overlay_records),
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("postprocessed material presentation");
                self.pass("overlay records drawn after the postprocess resolve");
                self.pass("partial mip chain rejected");
                self.pass("mip level byte mismatch rejected");
                self.pass("mip-chained material presentation");
                self.pass("RGBA8 UNORM texture presentation");
                self.pass("mip-chained RGBA8 UNORM texture presentation");

                let graphics = self.graphics.as_ref().expect("session A is open");
                graphics.device.destroy_texture(mip_texture)?;
                graphics.device.destroy_texture(unorm_texture)?;
                graphics.device.destroy_texture(unorm_mip_texture)?;
                graphics
                    .device
                    .destroy_material_pipeline(overlay_pipeline)?;
                graphics.device.destroy_material_pipeline(
                    self.material_pipeline
                        .take()
                        .expect("material pipeline exists"),
                )?;
                graphics
                    .device
                    .destroy_mesh(self.material_mesh.take().expect("material mesh exists"))?;
                drop(graphics.device.create_material_pipeline(
                    mulciber::MaterialPipelineDescriptor {
                        shader: ShaderArtifact::new(MATERIAL_SHADER)?,
                        vertex_entry: "crystal_vertex",
                        fragment_entry: "crystal_fragment",
                        vertex_layout: MATERIAL_LAYOUT,
                        bindings: &MATERIAL_BINDINGS,
                        blend: BlendMode::Opaque,
                        depth: DepthMode::TestWrite,
                        instance_layout: None,
                    },
                )?);
                self.pass("material destruction and drop reclamation");
                self.step = 20;
                Ok(false)
            }
            // Shadow vocabulary creation: an out-of-range extent, out-of-range array layer
            // counts, and a shadow declaration with non-uniform bindings are rejected by name,
            // then the shadow map, the two-layer shadow map array, the depth-only pipeline
            // (consuming only the position attribute of the crystal layout), the
            // array-sampling lava pipeline, and their meshes are created for the cases below.
            20 => {
                let graphics = self.graphics.as_ref().expect("session A is open");
                expect_error(
                    graphics.device.create_shadow_map(0).map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "outside the supported",
                    "zero-extent shadow map rejected",
                )?;
                expect_error(
                    graphics.device.create_shadow_map_array(512, 0).map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "outside the supported",
                    "zero-layer shadow map array rejected",
                )?;
                expect_error(
                    graphics.device.create_shadow_map_array(512, 9).map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "outside the supported",
                    "over-limit shadow map array layers rejected",
                )?;
                expect_error(
                    graphics
                        .device
                        .create_shadow_pipeline(mulciber::ShadowPipelineDescriptor {
                            shader: ShaderArtifact::new(LAVA_SHADER)?,
                            vertex_entry: "lava_vertex",
                            vertex_layout: LAVA_LAYOUT,
                            bindings: &LAVA_BINDINGS,
                            fragment_entry: None,
                            instance_layout: None,
                        })
                        .map(|_| ()),
                    GraphicsErrorKind::Unsupported,
                    "shadow pipelines do not sample depth resources",
                    "non-uniform shadow binding rejected",
                )?;
                self.shadow_map = Some(graphics.device.create_shadow_map(512)?);
                self.shadow_map_array = Some(graphics.device.create_shadow_map_array(512, 2)?);
                self.shadow_pipeline = Some(graphics.device.create_shadow_pipeline(
                    mulciber::ShadowPipelineDescriptor {
                        shader: ShaderArtifact::new(SHADOW_SHADER)?,
                        vertex_entry: "shadow_vertex",
                        vertex_layout: MATERIAL_LAYOUT,
                        bindings: &[MaterialBinding::Uniform {
                            binding: 0,
                            size: 64,
                        }],
                        fragment_entry: None,
                        instance_layout: None,
                    },
                )?);
                self.shadowed_pipeline = Some(graphics.device.create_material_pipeline(
                    mulciber::MaterialPipelineDescriptor {
                        shader: ShaderArtifact::new(LAVA_SHADER)?,
                        vertex_entry: "lava_vertex",
                        fragment_entry: "lava_fragment",
                        vertex_layout: LAVA_LAYOUT,
                        bindings: &LAVA_BINDINGS,
                        blend: BlendMode::Opaque,
                        depth: DepthMode::TestWrite,
                        instance_layout: None,
                    },
                )?);
                self.floor_mesh = Some(graphics.device.create_mesh_with_layout(
                    LAVA_LAYOUT,
                    &floor_vertices(),
                    MeshIndices::U16(&[0, 1, 2, 0, 2, 3]),
                )?);
                self.material_pipeline = Some(graphics.device.create_material_pipeline(
                    mulciber::MaterialPipelineDescriptor {
                        shader: ShaderArtifact::new(MATERIAL_SHADER)?,
                        vertex_entry: "crystal_vertex",
                        fragment_entry: "crystal_fragment",
                        vertex_layout: MATERIAL_LAYOUT,
                        bindings: &MATERIAL_BINDINGS,
                        blend: BlendMode::Opaque,
                        depth: DepthMode::TestWrite,
                        instance_layout: None,
                    },
                )?);
                self.material_mesh = Some(graphics.device.create_mesh_with_layout(
                    MATERIAL_LAYOUT,
                    &material_triangle_vertices(),
                    MeshIndices::U32(&[0, 1, 2]),
                )?);
                self.pass("zero-extent shadow map rejected");
                self.pass("zero-layer shadow map array rejected");
                self.pass("over-limit shadow map array layers rejected");
                self.pass("non-uniform shadow binding rejected");
                self.step = 21;
                Ok(false)
            }
            // A record on a shadow-sampling pipeline must supply the source.
            21 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let lava_textures = [texture];
                let uniform = [0_u8; 80];
                let cascades = lava_cascades();
                let records = [MaterialRecord {
                    pipeline: self.shadowed_pipeline.as_ref().expect("shadowed pipeline"),
                    geometry: GeometrySource::Mesh(self.floor_mesh.as_ref().expect("floor mesh")),
                    textures: &lava_textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &cascades,
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "declares a depth-texture slot",
                    "missing shadow map supply rejected",
                )?;
                self.pass("missing shadow map supply rejected");
                self.step = 22;
                Ok(false)
            }
            // A record may not supply a map its pipeline never declared a slot for.
            22 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Mesh(
                        self.material_mesh.as_ref().expect("material mesh"),
                    ),
                    textures: &textures,
                    shadow_map: Some(ShadowSource::Map(
                        self.shadow_map.as_ref().expect("shadow map"),
                    )),
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "declares no depth-texture slot",
                    "undeclared shadow map supply rejected",
                )?;
                self.pass("undeclared shadow map supply rejected");
                self.step = 23;
                Ok(false)
            }
            // A record may not supply a single map to a pipeline declaring the array slot.
            23 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let lava_textures = [texture];
                let uniform = [0_u8; 80];
                let cascades = lava_cascades();
                let records = [MaterialRecord {
                    pipeline: self.shadowed_pipeline.as_ref().expect("shadowed pipeline"),
                    geometry: GeometrySource::Mesh(self.floor_mesh.as_ref().expect("floor mesh")),
                    textures: &lava_textures,
                    shadow_map: Some(ShadowSource::Map(
                        self.shadow_map.as_ref().expect("shadow map"),
                    )),
                    uniform: &uniform,
                    storage: &cascades,
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "declares a depth-texture-array slot",
                    "single-map source on array pipeline rejected",
                )?;
                self.pass("single-map source on array pipeline rejected");
                self.step = 24;
                Ok(false)
            }
            // Sampling an array no cascaded shadow pass has rendered is rejected rather than
            // reading undefined depth.
            24 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let lava_textures = [texture];
                let uniform = [0_u8; 80];
                let cascades = lava_cascades();
                let records = [MaterialRecord {
                    pipeline: self.shadowed_pipeline.as_ref().expect("shadowed pipeline"),
                    geometry: GeometrySource::Mesh(self.floor_mesh.as_ref().expect("floor mesh")),
                    textures: &lava_textures,
                    shadow_map: Some(ShadowSource::Array(
                        self.shadow_map_array.as_ref().expect("shadow map array"),
                    )),
                    uniform: &uniform,
                    storage: &cascades,
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "no cascaded shadow pass has rendered",
                    "unrendered shadow map sampling rejected",
                )?;
                self.pass("unrendered shadow map sampling rejected");
                self.step = 25;
                Ok(false)
            }
            // A cascaded pass must supply exactly one record list per layer of its map.
            25 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Mesh(
                        self.material_mesh.as_ref().expect("material mesh"),
                    ),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let shadow_uniform = matrix_bytes(IDENTITY);
                let shadow_records = [ShadowRecord {
                    pipeline: self.shadow_pipeline.as_ref().expect("shadow pipeline"),
                    mesh: self.material_mesh.as_ref().expect("material mesh"),
                    uniform: &shadow_uniform,
                    storage: &[],
                    textures: &[],
                    instances: &[],
                }];
                let cascades: [&[ShadowRecord]; 1] = [&shadow_records];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: Some(ShadowPrepass::Cascaded(CascadedShadowPass {
                                    map: self.shadow_map_array.as_ref().expect("shadow map array"),
                                    cascades: &cascades,
                                })),
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "cascade record lists",
                    "cascade list count mismatch rejected",
                )?;
                self.pass("cascade list count mismatch rejected");
                self.step = 26;
                Ok(false)
            }
            // A shadow record's uniform bytes must match its pipeline's declared size.
            26 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Mesh(
                        self.material_mesh.as_ref().expect("material mesh"),
                    ),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let short_uniform = [0_u8; 32];
                let shadow_records = [ShadowRecord {
                    pipeline: self.shadow_pipeline.as_ref().expect("shadow pipeline"),
                    mesh: self.material_mesh.as_ref().expect("material mesh"),
                    uniform: &short_uniform,
                    storage: &[],
                    textures: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: Some(ShadowPrepass::Single(ShadowPass {
                                    map: self.shadow_map.as_ref().expect("shadow map"),
                                    records: &shadow_records,
                                })),
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "uniform bytes",
                    "shadow uniform length mismatch rejected",
                )?;
                self.pass("shadow uniform length mismatch rejected");
                self.step = 27;
                Ok(false)
            }
            // Render-scale vocabulary: out-of-range percentages on both sides of the supported
            // range are rejected by name, and scaled postprocess targets create and destroy at
            // half the presentable extent.
            27 => {
                let graphics = self.graphics.as_ref().expect("session A is open");
                expect_error(
                    RenderScale::percent(24).map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "outside the supported",
                    "out-of-range render scale rejected",
                )?;
                expect_error(
                    RenderScale::percent(201).map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "outside the supported",
                    "out-of-range render scale rejected",
                )?;
                let half_scale = RenderScale::percent(50)?;
                let scaled_targets = graphics
                    .device
                    .create_scaled_postprocess_targets(graphics.surface.info()?, half_scale)?;
                graphics
                    .device
                    .destroy_postprocess_targets(scaled_targets)?;
                self.pass("out-of-range render scale rejected");
                self.pass("scaled postprocess targets created and destroyed");
                self.step = 28;
                Ok(false)
            }
            // Storage vocabulary creation: a second storage slot, an oversized declaration, and
            // a size that disagrees with the recorded WGSL type are rejected by name, then the
            // skinned pipeline pair (the shadow variant consuming a subset of the skinned
            // layout) and a skinned triangle are created for the cases below.
            28 => {
                let graphics = self.graphics.as_ref().expect("session A is open");
                expect_error(
                    graphics
                        .device
                        .create_material_pipeline(mulciber::MaterialPipelineDescriptor {
                            shader: ShaderArtifact::new(SKINNED_SHADER)?,
                            vertex_entry: "skinned_vertex",
                            fragment_entry: "skinned_fragment",
                            vertex_layout: SKINNED_LAYOUT,
                            bindings: &[
                                MaterialBinding::Uniform {
                                    binding: 0,
                                    size: 64,
                                },
                                MaterialBinding::Storage {
                                    binding: 1,
                                    size: PALETTE_SIZE,
                                },
                                MaterialBinding::Storage {
                                    binding: 2,
                                    size: 64,
                                },
                            ],
                            blend: BlendMode::Opaque,
                            depth: DepthMode::TestWrite,
                            instance_layout: None,
                        })
                        .map(|_| ()),
                    GraphicsErrorKind::Unsupported,
                    "at most one storage slot",
                    "second storage slot rejected",
                )?;
                expect_error(
                    graphics
                        .device
                        .create_material_pipeline(mulciber::MaterialPipelineDescriptor {
                            shader: ShaderArtifact::new(SKINNED_SHADER)?,
                            vertex_entry: "skinned_vertex",
                            fragment_entry: "skinned_fragment",
                            vertex_layout: SKINNED_LAYOUT,
                            bindings: &[
                                MaterialBinding::Uniform {
                                    binding: 0,
                                    size: 64,
                                },
                                MaterialBinding::Storage {
                                    binding: 1,
                                    size: 65_537,
                                },
                            ],
                            blend: BlendMode::Opaque,
                            depth: DepthMode::TestWrite,
                            instance_layout: None,
                        })
                        .map(|_| ()),
                    GraphicsErrorKind::Unsupported,
                    "outside the supported 1 through",
                    "oversized storage declaration rejected",
                )?;
                expect_error(
                    graphics
                        .device
                        .create_material_pipeline(mulciber::MaterialPipelineDescriptor {
                            shader: ShaderArtifact::new(SKINNED_SHADER)?,
                            vertex_entry: "skinned_vertex",
                            fragment_entry: "skinned_fragment",
                            vertex_layout: SKINNED_LAYOUT,
                            bindings: &[
                                MaterialBinding::Uniform {
                                    binding: 0,
                                    size: 64,
                                },
                                MaterialBinding::Storage {
                                    binding: 1,
                                    size: 320,
                                },
                            ],
                            blend: BlendMode::Opaque,
                            depth: DepthMode::TestWrite,
                            instance_layout: None,
                        })
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "storage slot 1 declares 320 bytes",
                    "storage size mismatch rejected",
                )?;
                self.skinned_pipeline = Some(graphics.device.create_material_pipeline(
                    mulciber::MaterialPipelineDescriptor {
                        shader: ShaderArtifact::new(SKINNED_SHADER)?,
                        vertex_entry: "skinned_vertex",
                        fragment_entry: "skinned_fragment",
                        vertex_layout: SKINNED_LAYOUT,
                        bindings: &SKINNED_BINDINGS,
                        blend: BlendMode::Opaque,
                        depth: DepthMode::TestWrite,
                        instance_layout: None,
                    },
                )?);
                self.skinned_shadow_pipeline = Some(graphics.device.create_shadow_pipeline(
                    mulciber::ShadowPipelineDescriptor {
                        shader: ShaderArtifact::new(SKINNED_SHADOW_SHADER)?,
                        vertex_entry: "skinned_shadow_vertex",
                        vertex_layout: SKINNED_LAYOUT,
                        bindings: &SKINNED_BINDINGS,
                        fragment_entry: None,
                        instance_layout: None,
                    },
                )?);
                self.skinned_mesh = Some(graphics.device.create_mesh_with_layout(
                    SKINNED_LAYOUT,
                    &skinned_triangle_vertices(),
                    MeshIndices::U16(&[0, 1, 2]),
                )?);
                self.pass("second storage slot rejected");
                self.pass("oversized storage declaration rejected");
                self.pass("storage size mismatch rejected");
                self.step = 29;
                Ok(false)
            }
            // A material record's storage bytes must match its pipeline's declared size.
            29 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let uniform = matrix_bytes(IDENTITY);
                let short_palette = [0_u8; 64];
                let records = [MaterialRecord {
                    pipeline: self.skinned_pipeline.as_ref().expect("skinned pipeline"),
                    geometry: GeometrySource::Mesh(
                        self.skinned_mesh.as_ref().expect("skinned mesh"),
                    ),
                    textures: &[],
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &short_palette,
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "supplies 64 storage bytes",
                    "material storage length mismatch rejected",
                )?;
                self.pass("material storage length mismatch rejected");
                self.step = 30;
                Ok(false)
            }
            // A shadow record's storage bytes must match its pipeline's declared size.
            30 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let uniform = matrix_bytes(IDENTITY);
                let palette = identity_palette();
                let records = [MaterialRecord {
                    pipeline: self.skinned_pipeline.as_ref().expect("skinned pipeline"),
                    geometry: GeometrySource::Mesh(
                        self.skinned_mesh.as_ref().expect("skinned mesh"),
                    ),
                    textures: &[],
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &palette,
                    instances: &[],
                }];
                let shadow_records = [ShadowRecord {
                    pipeline: self
                        .skinned_shadow_pipeline
                        .as_ref()
                        .expect("skinned shadow pipeline"),
                    mesh: self.skinned_mesh.as_ref().expect("skinned mesh"),
                    uniform: &uniform,
                    storage: &[],
                    textures: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: Some(ShadowPrepass::Single(ShadowPass {
                                    map: self.shadow_map.as_ref().expect("shadow map"),
                                    records: &shadow_records,
                                })),
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "shadow record supplies 0 storage bytes",
                    "shadow storage length mismatch rejected",
                )?;
                self.pass("shadow storage length mismatch rejected");
                self.step = 31;
                Ok(false)
            }
            // The skinned record renders with its palette flowing through both the shadow and
            // the material path, then the skinned resources destroy explicitly.
            31 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let uniform = matrix_bytes(IDENTITY);
                let palette = identity_palette();
                let records = [MaterialRecord {
                    pipeline: self.skinned_pipeline.as_ref().expect("skinned pipeline"),
                    geometry: GeometrySource::Mesh(
                        self.skinned_mesh.as_ref().expect("skinned mesh"),
                    ),
                    textures: &[],
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &palette,
                    instances: &[],
                }];
                let shadow_records = [ShadowRecord {
                    pipeline: self
                        .skinned_shadow_pipeline
                        .as_ref()
                        .expect("skinned shadow pipeline"),
                    mesh: self.skinned_mesh.as_ref().expect("skinned mesh"),
                    uniform: &uniform,
                    storage: &palette,
                    textures: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.render_and_present(
                    frame,
                    SceneSubmission {
                        content: SceneContent::Material(&records),
                        output: SceneOutput::Direct(self.targets.as_ref().expect("targets exist")),
                        shadow: Some(ShadowPrepass::Single(ShadowPass {
                            map: self.shadow_map.as_ref().expect("shadow map"),
                            records: &shadow_records,
                        })),
                        overlay: None,
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("skinned material presentation");
                let graphics = self.graphics.as_ref().expect("session A is open");
                graphics.device.destroy_material_pipeline(
                    self.skinned_pipeline.take().expect("skinned pipeline"),
                )?;
                graphics.device.destroy_shadow_pipeline(
                    self.skinned_shadow_pipeline
                        .take()
                        .expect("skinned shadow pipeline"),
                )?;
                graphics
                    .device
                    .destroy_mesh(self.skinned_mesh.take().expect("skinned mesh"))?;
                self.pass("skinned resource destruction");
                self.step = 32;
                Ok(false)
            }
            // A record supplying frame-transient geometry against the crystal pipeline's
            // declared layout presents validation-clean alongside an uploaded-mesh record, with
            // no Mesh handle backing the transient supply.
            32 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let transient_vertices = material_triangle_vertices();
                let records = [
                    MaterialRecord {
                        pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                        geometry: GeometrySource::Mesh(
                            self.material_mesh.as_ref().expect("material mesh"),
                        ),
                        textures: &textures,
                        shadow_map: None,
                        uniform: &uniform,
                        storage: &[],
                        instances: &[],
                    },
                    MaterialRecord {
                        pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                        geometry: GeometrySource::Transient(TransientGeometry {
                            vertices: &transient_vertices,
                            indices: MeshIndices::U16(&[0, 1, 2]),
                        }),
                        textures: &textures,
                        shadow_map: None,
                        uniform: &uniform,
                        storage: &[],
                        instances: &[],
                    },
                ];
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.render_and_present(
                    frame,
                    SceneSubmission {
                        content: SceneContent::Material(&records),
                        output: SceneOutput::Direct(self.targets.as_ref().expect("targets exist")),
                        shadow: None,
                        overlay: None,
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("transient geometry presentation");
                self.step = 33;
                Ok(false)
            }
            // The same transient supply presents through the 32-bit index path.
            33 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let transient_vertices = material_triangle_vertices();
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Transient(TransientGeometry {
                        vertices: &transient_vertices,
                        indices: MeshIndices::U32(&[0, 1, 2]),
                    }),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.render_and_present(
                    frame,
                    SceneSubmission {
                        content: SceneContent::Material(&records),
                        output: SceneOutput::Direct(self.targets.as_ref().expect("targets exist")),
                        shadow: None,
                        overlay: None,
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("u32-indexed transient geometry presentation");
                self.step = 34;
                Ok(false)
            }
            // Transient vertex bytes that are not a multiple of the pipeline's declared layout
            // stride are rejected.
            34 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let truncated = material_triangle_vertices();
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Transient(TransientGeometry {
                        vertices: &truncated[..truncated.len() - 4],
                        indices: MeshIndices::U16(&[0, 1, 2]),
                    }),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "multiple of its pipeline's declared layout stride",
                    "transient vertex stride mismatch rejected",
                )?;
                self.pass("transient vertex stride mismatch rejected");
                self.step = 35;
                Ok(false)
            }
            // A transient supply with no indices is rejected.
            35 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let transient_vertices = material_triangle_vertices();
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Transient(TransientGeometry {
                        vertices: &transient_vertices,
                        indices: MeshIndices::U16(&[]),
                    }),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "must supply at least one index",
                    "empty transient indices rejected",
                )?;
                self.pass("empty transient indices rejected");
                self.step = 36;
                Ok(false)
            }
            // A transient index past the supplied vertex count is rejected.
            36 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let transient_vertices = material_triangle_vertices();
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Transient(TransientGeometry {
                        vertices: &transient_vertices,
                        indices: MeshIndices::U16(&[0, 1, 3]),
                    }),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "transient geometry contains an out-of-range index",
                    "out-of-range transient index rejected",
                )?;
                self.pass("out-of-range transient index rejected");
                self.step = 37;
                Ok(false)
            }
            // A combined vertex-plus-index supply just over the transient limit is rejected:
            // 116,508 crystal-stride vertices occupy 4,194,288 bytes, and nine 16-bit indices
            // push the total to 4,194,306 — two bytes past the limit.
            37 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let oversized_vertices = vec![0_u8; 116_508 * 36];
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    geometry: GeometrySource::Transient(TransientGeometry {
                        vertices: &oversized_vertices,
                        indices: MeshIndices::U16(&[0; 9]),
                    }),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    &format!("exceeds the {TRANSIENT_GEOMETRY_SIZE_LIMIT}-byte supply limit"),
                    "over-limit transient supply rejected",
                )?;
                self.pass("over-limit transient supply rejected");
                self.step = 38;
                Ok(false)
            }
            // The cascaded depth-only pass renders the crystal-layout mesh into both layers of
            // the array, the floor record samples the array through the comparison sampler in
            // the same frame, and every shadow resource kind then destroys explicitly.
            38 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let lava_textures = [texture];
                let lava_uniform = [0_u8; 80];
                let lava_storage = lava_cascades();
                let crystal_textures = [texture, texture];
                let crystal_uniform = material_uniform();
                let records = [
                    MaterialRecord {
                        pipeline: self.shadowed_pipeline.as_ref().expect("shadowed pipeline"),
                        geometry: GeometrySource::Mesh(
                            self.floor_mesh.as_ref().expect("floor mesh"),
                        ),
                        textures: &lava_textures,
                        shadow_map: Some(ShadowSource::Array(
                            self.shadow_map_array.as_ref().expect("shadow map array"),
                        )),
                        uniform: &lava_uniform,
                        storage: &lava_storage,
                        instances: &[],
                    },
                    MaterialRecord {
                        pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                        geometry: GeometrySource::Mesh(
                            self.material_mesh.as_ref().expect("material mesh"),
                        ),
                        textures: &crystal_textures,
                        shadow_map: None,
                        uniform: &crystal_uniform,
                        storage: &[],
                        instances: &[],
                    },
                ];
                let shadow_uniform = matrix_bytes(IDENTITY);
                let shadow_records = [ShadowRecord {
                    pipeline: self.shadow_pipeline.as_ref().expect("shadow pipeline"),
                    mesh: self.material_mesh.as_ref().expect("material mesh"),
                    uniform: &shadow_uniform,
                    storage: &[],
                    textures: &[],
                    instances: &[],
                }];
                let cascades: [&[ShadowRecord]; 2] = [&shadow_records, &shadow_records];
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.render_and_present(
                    frame,
                    SceneSubmission {
                        content: SceneContent::Material(&records),
                        output: SceneOutput::Direct(self.targets.as_ref().expect("targets exist")),
                        shadow: Some(ShadowPrepass::Cascaded(CascadedShadowPass {
                            map: self.shadow_map_array.as_ref().expect("shadow map array"),
                            cascades: &cascades,
                        })),
                        overlay: None,
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("shadowed material presentation");
                let graphics = self.graphics.as_ref().expect("session A is open");
                graphics
                    .device
                    .destroy_shadow_map(self.shadow_map.take().expect("shadow map"))?;
                graphics.device.destroy_shadow_map_array(
                    self.shadow_map_array.take().expect("shadow map array"),
                )?;
                graphics.device.destroy_shadow_pipeline(
                    self.shadow_pipeline.take().expect("shadow pipeline"),
                )?;
                graphics.device.destroy_material_pipeline(
                    self.shadowed_pipeline.take().expect("shadowed pipeline"),
                )?;
                graphics
                    .device
                    .destroy_mesh(self.floor_mesh.take().expect("floor mesh"))?;
                graphics.device.destroy_material_pipeline(
                    self.material_pipeline.take().expect("material pipeline"),
                )?;
                graphics
                    .device
                    .destroy_mesh(self.material_mesh.take().expect("material mesh"))?;
                self.pass("shadow resource destruction");
                self.step = 39;
                Ok(false)
            }
            // Instance-layout and shadow-fragment declarations are enforced at creation.
            39 => {
                expect_error(
                    self.graphics
                        .as_ref()
                        .expect("session A is open")
                        .device
                        .create_shadow_pipeline(mulciber::ShadowPipelineDescriptor {
                            shader: ShaderArtifact::new(SPROUT_SHADOW_SHADER)?,
                            vertex_entry: "sprout_shadow_vertex",
                            fragment_entry: None,
                            vertex_layout: SPROUT_LAYOUT,
                            instance_layout: Some(SPROUT_INSTANCE_LAYOUT),
                            bindings: &SPROUT_BINDINGS,
                        })
                        .map(|_| ()),
                    GraphicsErrorKind::Unsupported,
                    "texture and sampler bindings only with a declared fragment entry",
                    "shadow texture bindings without a fragment entry rejected",
                )?;
                self.pass("shadow texture bindings without a fragment entry rejected");
                expect_error(
                    self.graphics
                        .as_ref()
                        .expect("session A is open")
                        .device
                        .create_material_pipeline(mulciber::MaterialPipelineDescriptor {
                            shader: ShaderArtifact::new(SPROUT_SHADER)?,
                            vertex_entry: "sprout_vertex",
                            fragment_entry: "sprout_fragment",
                            vertex_layout: SPROUT_LAYOUT,
                            instance_layout: Some(SPROUT_LAYOUT),
                            bindings: &SPROUT_BINDINGS,
                            blend: BlendMode::Cutout,
                            depth: DepthMode::TestWrite,
                        })
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "instance layout declares location 0 that the vertex layout already declares",
                    "overlapping instance locations rejected",
                )?;
                self.pass("overlapping instance locations rejected");
                let graphics = self.graphics.as_ref().expect("session A is open");
                self.sprout_pipeline = Some(graphics.device.create_material_pipeline(
                    mulciber::MaterialPipelineDescriptor {
                        shader: ShaderArtifact::new(SPROUT_SHADER)?,
                        vertex_entry: "sprout_vertex",
                        fragment_entry: "sprout_fragment",
                        vertex_layout: SPROUT_LAYOUT,
                        instance_layout: Some(SPROUT_INSTANCE_LAYOUT),
                        bindings: &SPROUT_BINDINGS,
                        blend: BlendMode::Cutout,
                        depth: DepthMode::TestWrite,
                    },
                )?);
                self.sprout_shadow_pipeline = Some(graphics.device.create_shadow_pipeline(
                    mulciber::ShadowPipelineDescriptor {
                        shader: ShaderArtifact::new(SPROUT_SHADOW_SHADER)?,
                        vertex_entry: "sprout_shadow_vertex",
                        fragment_entry: Some("sprout_shadow_fragment"),
                        vertex_layout: SPROUT_LAYOUT,
                        instance_layout: Some(SPROUT_INSTANCE_LAYOUT),
                        bindings: &SPROUT_BINDINGS,
                    },
                )?);
                self.sprout_mesh = Some(graphics.device.create_mesh_with_layout(
                    SPROUT_LAYOUT,
                    &sprout_triangle_vertices(),
                    MeshIndices::U16(&[0, 1, 2]),
                )?);
                self.sprout_shadow_map = Some(graphics.device.create_shadow_map(256)?);
                self.pass("instanced material and cutout shadow pipelines created");
                self.step = 40;
                Ok(false)
            }
            // A record must supply instance bytes exactly when its pipeline declares an
            // instance layout.
            40 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let sprout_textures = [texture];
                let uniform = matrix_bytes(IDENTITY);
                let records = [MaterialRecord {
                    pipeline: self.sprout_pipeline.as_ref().expect("sprout pipeline"),
                    geometry: GeometrySource::Mesh(self.sprout_mesh.as_ref().expect("sprout mesh")),
                    textures: &sprout_textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "must be a non-zero multiple of its pipeline's declared 64-byte instance",
                    "missing instance supply rejected",
                )?;
                self.pass("missing instance supply rejected");
                self.step = 41;
                Ok(false)
            }
            // An instance supply must be a whole number of declared instance strides.
            41 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let sprout_textures = [texture];
                let uniform = matrix_bytes(IDENTITY);
                let truncated = [0_u8; 96];
                let records = [MaterialRecord {
                    pipeline: self.sprout_pipeline.as_ref().expect("sprout pipeline"),
                    geometry: GeometrySource::Mesh(self.sprout_mesh.as_ref().expect("sprout mesh")),
                    textures: &sprout_textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &truncated,
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "must be a non-zero multiple of its pipeline's declared 64-byte instance",
                    "ragged instance supply rejected",
                )?;
                self.pass("ragged instance supply rejected");
                self.step = 42;
                Ok(false)
            }
            // Instanced material records present, with the caster's declared fragment stage
            // alpha-testing its texture inside the depth-only shadow pass.
            42 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let sprout_textures = [texture];
                let uniform = matrix_bytes(IDENTITY);
                let mut instances = matrix_bytes(IDENTITY);
                instances.extend_from_slice(&matrix_bytes(SHIFTED));
                let records = [MaterialRecord {
                    pipeline: self.sprout_pipeline.as_ref().expect("sprout pipeline"),
                    geometry: GeometrySource::Mesh(self.sprout_mesh.as_ref().expect("sprout mesh")),
                    textures: &sprout_textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &instances,
                }];
                let shadow_records = [ShadowRecord {
                    pipeline: self
                        .sprout_shadow_pipeline
                        .as_ref()
                        .expect("sprout shadow pipeline"),
                    mesh: self.sprout_mesh.as_ref().expect("sprout mesh"),
                    uniform: &uniform,
                    storage: &[],
                    textures: &sprout_textures,
                    instances: &instances,
                }];
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.render_and_present(
                    frame,
                    SceneSubmission {
                        content: SceneContent::Material(&records),
                        output: SceneOutput::Direct(self.targets.as_ref().expect("targets exist")),
                        shadow: Some(ShadowPrepass::Single(ShadowPass {
                            map: self.sprout_shadow_map.as_ref().expect("sprout shadow map"),
                            records: &shadow_records,
                        })),
                        overlay: None,
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("instanced records and cutout shadow presentation");
                let graphics = self.graphics.as_ref().expect("session A is open");
                graphics.device.destroy_material_pipeline(
                    self.sprout_pipeline.take().expect("sprout pipeline"),
                )?;
                graphics.device.destroy_shadow_pipeline(
                    self.sprout_shadow_pipeline
                        .take()
                        .expect("sprout shadow pipeline"),
                )?;
                graphics
                    .device
                    .destroy_mesh(self.sprout_mesh.take().expect("sprout mesh"))?;
                graphics.device.destroy_shadow_map(
                    self.sprout_shadow_map.take().expect("sprout shadow map"),
                )?;
                self.pass("instanced resource destruction");
                self.step = 43;
                Ok(false)
            }
            // The direct single-draw helper accepts exactly the declared postprocess uniform.
            43 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let uniform = [0_u8; 64];
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.draw_textured_postprocessed_and_present(
                    frame,
                    PostprocessedDraw {
                        mesh: self.mesh.as_ref().expect("mesh exists"),
                        texture: self.texture.as_ref().expect("texture exists"),
                        scene_pipeline: self.pipeline.as_ref().expect("pipeline exists"),
                        postprocess_pipeline: self
                            .postprocess_uniform_pipeline
                            .as_ref()
                            .expect("uniform postprocess pipeline exists"),
                        targets: self
                            .postprocess_targets
                            .as_ref()
                            .expect("postprocess targets exist"),
                        uniform: &uniform,
                        model_view_projection: IDENTITY,
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("exact-size postprocess uniform accepted by direct helper");
                self.step = 44;
                Ok(false)
            }
            // A declared postprocess uniform may not be omitted.
            44 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .draw_textured_postprocessed_and_present(
                            frame,
                            PostprocessedDraw {
                                mesh: self.mesh.as_ref().expect("mesh exists"),
                                texture: self.texture.as_ref().expect("texture exists"),
                                scene_pipeline: self.pipeline.as_ref().expect("pipeline exists"),
                                postprocess_pipeline: self
                                    .postprocess_uniform_pipeline
                                    .as_ref()
                                    .expect("uniform postprocess pipeline exists"),
                                targets: self
                                    .postprocess_targets
                                    .as_ref()
                                    .expect("postprocess targets exist"),
                                uniform: &[],
                                model_view_projection: IDENTITY,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "supplies no uniform bytes but its pipeline declares 64",
                    "missing postprocess uniform rejected",
                )?;
                self.pass("missing postprocess uniform rejected");
                self.step = 45;
                Ok(false)
            }
            // SceneSubmission rejects bytes supplied to a no-uniform postprocess pipeline.
            45 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let draws = scene_draws(
                    self.mesh.as_ref().expect("mesh exists"),
                    self.texture.as_ref().expect("texture exists"),
                    self.pipeline.as_ref().expect("pipeline exists"),
                );
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Textured(&draws),
                                output: SceneOutput::Postprocessed {
                                    pipeline: self
                                        .postprocess_pipeline
                                        .as_ref()
                                        .expect("postprocess pipeline exists"),
                                    targets: self
                                        .postprocess_targets
                                        .as_ref()
                                        .expect("postprocess targets exist"),
                                    uniform: &[1],
                                },
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "unexpected uniform bytes but its pipeline declares no uniform",
                    "unexpected postprocess uniform rejected",
                )?;
                self.pass("unexpected postprocess uniform rejected through SceneSubmission");
                self.step = 46;
                Ok(false)
            }
            // SceneSubmission also rejects a non-empty but wrong-size declared uniform.
            46 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let draws = scene_draws(
                    self.mesh.as_ref().expect("mesh exists"),
                    self.texture.as_ref().expect("texture exists"),
                    self.pipeline.as_ref().expect("pipeline exists"),
                );
                let uniform = [0_u8; 32];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Textured(&draws),
                                output: SceneOutput::Postprocessed {
                                    pipeline: self
                                        .postprocess_uniform_pipeline
                                        .as_ref()
                                        .expect("uniform postprocess pipeline exists"),
                                    targets: self
                                        .postprocess_targets
                                        .as_ref()
                                        .expect("postprocess targets exist"),
                                    uniform: &uniform,
                                },
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "supplies 32 uniform bytes but its pipeline declares 64",
                    "wrong-size postprocess uniform rejected",
                )?;
                self.pass("wrong-size postprocess uniform rejected through SceneSubmission");
                self.step = 47;
                Ok(false)
            }
            // The submission-side bound is independently observable for oversized data.
            47 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let oversized = [0_u8; 257];
                let graphics = self.graphics.as_mut().expect("session A is open");
                expect_error(
                    graphics
                        .queue
                        .draw_textured_postprocessed_and_present(
                            frame,
                            PostprocessedDraw {
                                mesh: self.mesh.as_ref().expect("mesh exists"),
                                texture: self.texture.as_ref().expect("texture exists"),
                                scene_pipeline: self.pipeline.as_ref().expect("pipeline exists"),
                                postprocess_pipeline: self
                                    .postprocess_uniform_pipeline
                                    .as_ref()
                                    .expect("uniform postprocess pipeline exists"),
                                targets: self
                                    .postprocess_targets
                                    .as_ref()
                                    .expect("postprocess targets exist"),
                                uniform: &oversized,
                                model_view_projection: IDENTITY,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "exceeding the 256-byte limit",
                    "oversized postprocess uniform rejected",
                )?;
                self.pass("oversized postprocess uniform rejected");
                self.step = 48;
                Ok(false)
            }
            // Session A shuts down cleanly; session B reopens the same window with the forced
            // one-sample path and keeps session-A handles for mixed-session cases.
            48 => {
                self.instanced_pipeline = None;
                self.postprocess_uniform_pipeline = None;
                self.postprocess_targets = None;
                let graphics = self.graphics.take().expect("session A is open");
                graphics.shutdown()?;
                self.pass("fallible shutdown succeeded");

                let reopened = OpenedGraphics::open(
                    self.window.surface_target(),
                    metrics,
                    DeviceRequest {
                        preferred_sample_count: SampleCount::One,
                    },
                )?;
                if reopened.selection.sample_count() != SampleCount::One {
                    return Err("forced one-sample selection was not observable".into());
                }
                self.pass("forced one-sample selection observable");

                self.mesh = Some(
                    reopened
                        .device
                        .create_mesh(&TRIANGLE_VERTICES, &[0, 1, 2])?,
                );
                self.texture = Some(reopened.device.create_rgba8_srgb_texture(
                    2,
                    2,
                    &[255_u8; 16],
                )?);
                self.pipeline = Some(
                    reopened
                        .device
                        .create_textured_pipeline(ShaderArtifact::new(SHADER)?)?,
                );
                self.targets = Some(
                    reopened
                        .device
                        .create_render_targets(reopened.surface.info()?)?,
                );
                self.postprocess_targets = Some(
                    reopened
                        .device
                        .create_postprocess_targets(reopened.surface.info()?)?,
                );
                self.graphics = Some(reopened);
                self.step = 49;
                Ok(false)
            }
            // A handle from the shut-down session is rejected by the new session.
            49 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let graphics = self.graphics.as_mut().expect("session B is open");
                let draw = TexturedDraw {
                    mesh: self.foreign_mesh.as_ref().expect("session A mesh kept"),
                    texture: self.texture.as_ref().expect("texture exists"),
                    pipeline: self.pipeline.as_ref().expect("pipeline exists"),
                    targets: self.targets.as_ref().expect("targets exist"),
                    model_view_projection: IDENTITY,
                    clear: ClearColor::BLACK,
                };
                expect_error(
                    graphics
                        .queue
                        .draw_textured_and_present(frame, draw)
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "mesh belongs to a different graphics session",
                    "mixed-session handles rejected",
                )?;
                self.pass("mixed-session handles rejected");
                self.step = 50;
                Ok(false)
            }
            // The mixed-session diagnostic also names the material pipeline handle kind.
            50 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("session B texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let records = [MaterialRecord {
                    pipeline: self
                        .foreign_material_pipeline
                        .as_ref()
                        .expect("session A material pipeline kept"),
                    geometry: GeometrySource::Mesh(
                        self.mesh.as_ref().expect("session B mesh exists"),
                    ),
                    textures: &textures,
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }];
                let graphics = self.graphics.as_mut().expect("session B is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Material(&records),
                                output: SceneOutput::Direct(
                                    self.targets.as_ref().expect("targets exist"),
                                ),
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "material pipeline belongs to a different graphics session",
                    "mixed-session material pipeline rejected",
                )?;
                self.pass("mixed-session material pipeline rejected");
                self.step = 51;
                Ok(false)
            }
            // Postprocess uniform validation does not mask a mixed-session pipeline error.
            51 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let draws = scene_draws(
                    self.mesh.as_ref().expect("session B mesh exists"),
                    self.texture.as_ref().expect("session B texture exists"),
                    self.pipeline.as_ref().expect("session B pipeline exists"),
                );
                let graphics = self.graphics.as_mut().expect("session B is open");
                expect_error(
                    graphics
                        .queue
                        .render_and_present(
                            frame,
                            SceneSubmission {
                                content: SceneContent::Textured(&draws),
                                output: SceneOutput::Postprocessed {
                                    pipeline: self
                                        .postprocess_pipeline
                                        .as_ref()
                                        .expect("session A postprocess pipeline kept"),
                                    targets: self
                                        .postprocess_targets
                                        .as_ref()
                                        .expect("session B postprocess targets exist"),
                                    uniform: &[1],
                                },
                                shadow: None,
                                overlay: None,
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "postprocess pipeline belongs to a different graphics session",
                    "mixed-session postprocess pipeline rejected before uniform data",
                )?;
                self.pass("mixed-session postprocess validation precedence retained");
                self.step = 52;
                Ok(false)
            }
            // The one-sample session presents and shuts down cleanly.
            _ => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let disposition = self.draw(frame, IDENTITY)?;
                assert_presented(disposition)?;
                self.pass("one-sample presentation");
                let graphics = self.graphics.take().expect("session B is open");
                graphics.shutdown()?;
                self.pass("second fallible shutdown succeeded");
                Ok(true)
            }
        }
    }

    fn acquire(
        &mut self,
        metrics: WindowMetrics,
    ) -> Result<Option<mulciber::Frame<'window>>, Box<dyn Error>> {
        let graphics = self.graphics.as_mut().expect("a session is open");
        match graphics.surface.acquire(metrics)? {
            FrameAcquire::Ready(frame) => {
                // Rebuild targets whenever the frame reports a newer generation than the current
                // set, except while a case is deliberately holding stale targets (step 3 handles
                // its own rebuild).
                if self.step != 3 {
                    let targets_info = self.targets.as_ref().expect("targets exist").info();
                    if frame.surface_info() != targets_info {
                        self.targets = Some(
                            graphics
                                .device
                                .create_render_targets(frame.surface_info())?,
                        );
                    }
                    if let Some(postprocess_targets) = self.postprocess_targets.as_ref()
                        && frame.surface_info() != postprocess_targets.info()
                    {
                        self.postprocess_targets = Some(
                            graphics
                                .device
                                .create_postprocess_targets(frame.surface_info())?,
                        );
                    }
                }
                Ok(Some(frame))
            }
            FrameAcquire::Unavailable(_) => Ok(None),
        }
    }

    fn draw(
        &mut self,
        frame: mulciber::Frame<'_>,
        transform: [[f32; 4]; 4],
    ) -> Result<mulciber::FrameDisposition, Box<dyn Error>> {
        let graphics = self.graphics.as_mut().expect("a session is open");
        let draw = TexturedDraw {
            mesh: self.mesh.as_ref().expect("mesh exists"),
            texture: self.texture.as_ref().expect("texture exists"),
            pipeline: self.pipeline.as_ref().expect("pipeline exists"),
            targets: self.targets.as_ref().expect("targets exist"),
            model_view_projection: transform,
            clear: ClearColor::BLACK,
        };
        Ok(graphics.queue.draw_textured_and_present(frame, draw)?)
    }
}

fn scene_draws<'resources>(
    mesh: &'resources Mesh,
    texture: &'resources mulciber::Texture,
    pipeline: &'resources mulciber::TexturedPipeline,
) -> [TexturedSceneDraw<'resources>; 2] {
    [
        TexturedSceneDraw {
            mesh,
            texture,
            pipeline,
            model_view_projection: IDENTITY,
        },
        TexturedSceneDraw {
            mesh,
            texture,
            pipeline,
            model_view_projection: SHIFTED,
        },
    ]
}

/// One triangle packed against the crystal layout: position, normal, texture coordinate, glow.
/// A quad in the lava layout: position and texture coordinate at stride 20.
fn floor_vertices() -> Vec<u8> {
    let vertices: [[f32; 5]; 4] = [
        [-1.0, -0.5, 1.0, 0.0, 1.0],
        [1.0, -0.5, 1.0, 1.0, 1.0],
        [1.0, -0.5, -1.0, 1.0, 0.0],
        [-1.0, -0.5, -1.0, 0.0, 0.0],
    ];
    let mut bytes = Vec::with_capacity(4 * 20);
    for vertex in vertices {
        for value in vertex {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
    }
    bytes
}

/// One column-major matrix as `ShadowParams` bytes.
fn matrix_bytes(matrix: [[f32; 4]; 4]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64);
    for column in matrix {
        for value in column {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
    }
    bytes
}

/// One triangle against the sprout module's recorded per-vertex interface: position and
/// texture coordinate.
fn sprout_triangle_vertices() -> Vec<u8> {
    let vertices: [[f32; 5]; 3] = [
        [-0.5, -0.5, 0.0, 0.0, 1.0],
        [0.5, -0.5, 0.0, 1.0, 1.0],
        [0.0, 0.5, 0.0, 0.5, 0.0],
    ];
    let mut bytes = Vec::with_capacity(3 * 20);
    for vertex in vertices {
        for value in vertex {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
    }
    bytes
}

fn material_triangle_vertices() -> Vec<u8> {
    let vertices: [[f32; 9]; 3] = [
        [-0.5, -0.5, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0],
        [0.5, -0.5, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.5],
        [0.0, 0.5, 0.0, 0.0, 0.0, 1.0, 0.5, 0.0, 0.0],
    ];
    let mut bytes = Vec::with_capacity(3 * 36);
    for vertex in vertices {
        for value in vertex {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
    }
    bytes
}

/// One triangle packed against the skinned layout: position, normal, then two blended bone
/// indices and their weights.
fn skinned_triangle_vertices() -> Vec<u8> {
    let vertices: [([f32; 6], [u32; 4], [f32; 4]); 3] = [
        (
            [-0.5, -0.5, 0.0, 0.0, 0.0, 1.0],
            [0, 1, 0, 0],
            [0.5, 0.5, 0.0, 0.0],
        ),
        (
            [0.5, -0.5, 0.0, 0.0, 0.0, 1.0],
            [2, 3, 0, 0],
            [0.75, 0.25, 0.0, 0.0],
        ),
        (
            [0.0, 0.5, 0.0, 0.0, 0.0, 1.0],
            [4, 5, 0, 0],
            [0.25, 0.75, 0.0, 0.0],
        ),
    ];
    let mut bytes = Vec::with_capacity(3 * 56);
    for (floats, joints, weights) in vertices {
        for value in floats {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        for value in joints {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        for value in weights {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
    }
    bytes
}

/// Six identity bone matrices as the skinned module's palette bytes.
fn identity_palette() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(384);
    for _ in 0..6 {
        bytes.extend_from_slice(&matrix_bytes(IDENTITY));
    }
    bytes
}

/// Three identity light-from-model matrices as the lava module's `LavaCascades` bytes.
fn lava_cascades() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(CASCADES_SIZE as usize);
    for _ in 0..3 {
        bytes.extend_from_slice(&matrix_bytes(IDENTITY));
    }
    bytes
}

/// `CrystalParams` bytes: identity model-view-projection and model, then zeroed pulse values.
fn material_uniform() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(144);
    for matrix in [IDENTITY, IDENTITY] {
        for column in matrix {
            for value in column {
                bytes.extend_from_slice(&value.to_ne_bytes());
            }
        }
    }
    bytes.extend_from_slice(&[0_u8; 16]);
    bytes
}

fn assert_presented(disposition: mulciber::FrameDisposition) -> Result<(), Box<dyn Error>> {
    match disposition {
        mulciber::FrameDisposition::Presented(_) => Ok(()),
        other => Err(format!("expected a presented disposition, got {other:?}").into()),
    }
}

fn expect_error(
    result: Result<(), mulciber::GraphicsError>,
    expected_kind: GraphicsErrorKind,
    needle: &str,
    case: &str,
) -> Result<(), Box<dyn Error>> {
    match result {
        Ok(()) => Err(format!("{case}: expected an error, the operation succeeded").into()),
        Err(error) => {
            if error.kind() != expected_kind {
                return Err(format!(
                    "{case}: expected {expected_kind:?}, got {:?}: {error}",
                    error.kind()
                )
                .into());
            }
            let message = error.to_string();
            if message.contains(needle) {
                Ok(())
            } else {
                Err(format!(
                    "{case}: diagnostic {message:?} does not identify the contract ({needle:?})"
                )
                .into())
            }
        }
    }
}
