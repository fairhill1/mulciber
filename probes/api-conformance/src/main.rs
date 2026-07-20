//! Asserted conformance cases for the public graphics slice.
//!
//! Unlike the interactive example and the finite validation probes, every case here asserts its
//! observable outcome and the process exits nonzero on the first divergence. The cases cover
//! invalid usage, deferred abandonment recovery, abandonment-driven surface generations, the
//! observable one-sample fallback, explicit and drop-driven resource reclamation, mixed-session
//! rejection, material declaration/interface validation, and fallible shutdown.

use std::error::Error;
use std::time::Instant;

use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, GraphicsErrorKind, MaterialBinding, MaterialRecord,
    Mesh, OpenedGraphics, PostprocessedScene, SampleCount, SceneContent, SceneOutput,
    SceneSubmission, ShaderArtifact, TexturedDraw, TexturedInstanceBatch, TexturedScene,
    TexturedSceneDraw, Vertex, VertexAttribute, VertexFormat, VertexLayout,
};
use mulciber_platform::{
    Application, LogicalSize, PumpStatus, Window, WindowDescriptor, WindowEvent, WindowMetrics,
};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cube.shaderbin"));
const INSTANCED_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/instanced.shaderbin"));
const MATERIAL_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/material.shaderbin"));

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
    MaterialBinding::Sampler { binding: 3 },
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
    postprocess_targets: Option<mulciber::PostprocessTargets>,
    material_pipeline: Option<mulciber::MaterialPipeline>,
    foreign_material_pipeline: Option<mulciber::MaterialPipeline>,
    material_mesh: Option<Mesh>,
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
            postprocess_targets: None,
            material_pipeline: None,
            foreign_material_pipeline: None,
            material_mesh: None,
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
                        },
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
                                    MaterialBinding::Sampler { binding: 3 },
                                ],
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
                                &[0, 1, 2],
                            )
                            .map(|_| ()),
                        GraphicsErrorKind::InvalidRequest,
                        "multiple of the layout stride",
                        "vertex byte stride mismatch rejected",
                    )?;
                }
                self.pass("material uniform size mismatch rejected");
                self.pass("missing entry point rejected");
                self.pass("undeclared vertex input rejected");
                self.pass("undeclared binding slot rejected");
                self.pass("vertex byte stride mismatch rejected");

                let graphics = self.graphics.as_ref().expect("session A is open");
                self.material_pipeline = Some(graphics.device.create_material_pipeline(
                    mulciber::MaterialPipelineDescriptor {
                        shader,
                        vertex_entry: "crystal_vertex",
                        fragment_entry: "crystal_fragment",
                        vertex_layout: MATERIAL_LAYOUT,
                        bindings: &MATERIAL_BINDINGS,
                    },
                )?);
                self.foreign_material_pipeline = Some(graphics.device.create_material_pipeline(
                    mulciber::MaterialPipelineDescriptor {
                        shader,
                        vertex_entry: "crystal_vertex",
                        fragment_entry: "crystal_fragment",
                        vertex_layout: MATERIAL_LAYOUT,
                        bindings: &MATERIAL_BINDINGS,
                    },
                )?);
                self.material_mesh = Some(graphics.device.create_mesh_with_layout(
                    MATERIAL_LAYOUT,
                    &material_triangle_vertices(),
                    &[0, 1, 2],
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
                    mesh: self.material_mesh.as_ref().expect("material mesh"),
                    textures: &textures,
                    uniform: &short_uniform,
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
                    mesh: self.material_mesh.as_ref().expect("material mesh"),
                    textures: &textures,
                    uniform: &uniform,
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
                    mesh: self.mesh.as_ref().expect("fixed-layout mesh exists"),
                    textures: &textures,
                    uniform: &uniform,
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
            // Two material records with application-packed uniform bytes present directly.
            14 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let records = [
                    MaterialRecord {
                        pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                        mesh: self.material_mesh.as_ref().expect("material mesh"),
                        textures: &textures,
                        uniform: &uniform,
                    },
                    MaterialRecord {
                        pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                        mesh: self.material_mesh.as_ref().expect("material mesh"),
                        textures: &textures,
                        uniform: &uniform,
                    },
                ];
                let graphics = self.graphics.as_mut().expect("session A is open");
                let disposition = graphics.queue.render_and_present(
                    frame,
                    SceneSubmission {
                        content: SceneContent::Material(&records),
                        output: SceneOutput::Direct(self.targets.as_ref().expect("targets exist")),
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("material presentation");
                self.step = 15;
                Ok(false)
            }
            // The material path remains valid through resolved color and post-processing, and the
            // new handle kind supports explicit destruction plus drop-driven reclamation.
            15 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let texture = self.texture.as_ref().expect("texture exists");
                let textures = [texture, texture];
                let uniform = material_uniform();
                let records = [MaterialRecord {
                    pipeline: self.material_pipeline.as_ref().expect("material pipeline"),
                    mesh: self.material_mesh.as_ref().expect("material mesh"),
                    textures: &textures,
                    uniform: &uniform,
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
                        },
                        clear: ClearColor::BLACK,
                    },
                )?;
                assert_presented(disposition)?;
                self.pass("postprocessed material presentation");

                let graphics = self.graphics.as_ref().expect("session A is open");
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
                    },
                )?);
                self.pass("material destruction and drop reclamation");
                self.step = 16;
                Ok(false)
            }
            // Session A shuts down cleanly; session B reopens the same window with the forced
            // one-sample path and keeps a session-A handle for the mixed-session case.
            16 => {
                self.instanced_pipeline = None;
                self.postprocess_pipeline = None;
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
                self.graphics = Some(reopened);
                self.step = 17;
                Ok(false)
            }
            // A handle from the shut-down session is rejected by the new session.
            17 => {
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
                self.step = 18;
                Ok(false)
            }
            // The mixed-session diagnostic also names the material pipeline handle kind.
            18 => {
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
                    mesh: self.mesh.as_ref().expect("session B mesh exists"),
                    textures: &textures,
                    uniform: &uniform,
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
                                clear: ClearColor::BLACK,
                            },
                        )
                        .map(|_| ()),
                    GraphicsErrorKind::InvalidRequest,
                    "material pipeline belongs to a different graphics session",
                    "mixed-session material pipeline rejected",
                )?;
                self.pass("mixed-session material pipeline rejected");
                self.step = 19;
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
