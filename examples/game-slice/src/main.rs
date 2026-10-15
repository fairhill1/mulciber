//! Forge Run, a small playable collect-and-avoid game: the fixed-step loop of `mulciber-runtime`,
//! one instanced material for every shape, and a postprocess grade, with its WGSL compiled by
//! `build.rs` from shaders that import a shared module and one of Mulciber's own.

mod game;
mod scene;

use std::error::Error;
use std::time::Instant;

use game::Game;
use mulciber::{
    BlendMode, ClearColor, DepthMode, Device, DeviceRequest, Frame, FrameAcquire, GeometrySource,
    GpuTimingFeedback, MaterialBinding, MaterialPipeline, MaterialPipelineDescriptor,
    MaterialRecord, Mesh, MeshDescriptor, MeshIndices, MipChain, OpenedGraphics, Postprocess,
    PostprocessPipeline, PostprocessTargets, PresentFeedback, Queue, RenderScale, SamplerAddress,
    SamplerFilter, Scene, SceneColor, SceneOutput, SceneSubmission, ShaderArtifact, TexelData,
    Texture, TextureDescriptor, TextureShape,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};
use mulciber_runtime::{Runtime, RuntimeConfig};
use scene::{
    CUBE_INDICES, INSTANCE_LAYOUT, PYRAMID_INDICES, VERTEX_LAYOUT, checkerboard, cube_vertices,
    pyramid_vertices, transforms,
};

const FORGE_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/forge.shaderbin"));
const GRADE_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/grade.shaderbin"));
const CLEAR: ClearColor = ClearColor::opaque(0.008, 0.014, 0.026);
const GPU_HITCH_SECONDS: f64 = 0.020;

struct Resources {
    cube: Mesh,
    pyramid: Mesh,
    ground_texture: Texture,
    obstacle_texture: Texture,
    player_texture: Texture,
    pickup_texture: Texture,
    hazard_texture: Texture,
    scene_pipeline: MaterialPipeline,
    postprocess_pipeline: PostprocessPipeline,
    targets: PostprocessTargets,
}

impl Resources {
    fn new(graphics: &OpenedGraphics<'_>) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            cube: graphics.device.create_mesh(&MeshDescriptor {
                layout: VERTEX_LAYOUT,
                vertices: &cube_vertices(),
                parts: &[MeshIndices::U16(&CUBE_INDICES)],
            })?,
            pyramid: graphics.device.create_mesh(&MeshDescriptor {
                layout: VERTEX_LAYOUT,
                vertices: &pyramid_vertices(),
                parts: &[MeshIndices::U16(&PYRAMID_INDICES)],
            })?,
            ground_texture: texture(&graphics.device, [30, 42, 58], [55, 72, 82], 1)?,
            obstacle_texture: texture(&graphics.device, [100, 105, 120], [42, 48, 65], 2)?,
            player_texture: texture(&graphics.device, [35, 230, 190], [20, 90, 145], 2)?,
            pickup_texture: texture(&graphics.device, [255, 195, 45], [245, 80, 30], 1)?,
            hazard_texture: texture(&graphics.device, [245, 45, 85], [100, 20, 135], 2)?,
            scene_pipeline: forge_pipeline(&graphics.device)?,
            postprocess_pipeline: graphics
                .device
                .create_postprocess_pipeline(ShaderArtifact::new(GRADE_SHADER)?)?,
            targets: graphics.device.create_postprocess_targets(
                graphics.surface.info()?,
                RenderScale::NATIVE,
                SceneColor::Surface,
            )?,
        })
    }

    #[allow(clippy::cast_precision_loss)]
    fn render(
        &mut self,
        device: &Device<'_>,
        queue: &mut Queue<'_>,
        frame: Frame<'_>,
        game: &Game,
        interpolation: f64,
    ) -> Result<(), Box<dyn Error>> {
        let info = frame.surface_info();
        if info != self.targets.info() {
            self.targets = device.create_postprocess_targets(
                info,
                RenderScale::NATIVE,
                SceneColor::Surface,
            )?;
        }
        let aspect = info.extent().width() as f32 / info.extent().height() as f32;
        let scene = transforms(game, aspect, interpolation);
        // A record per kind of shape, drawn once per instance it supplies; the pickups' is left
        // out once the last one is collected, as a record must supply at least one instance.
        let shapes: [(&Mesh, [&Texture; 1], &[u8]); 5] = [
            (&self.cube, [&self.ground_texture], &scene.ground),
            (&self.cube, [&self.obstacle_texture], &scene.obstacles),
            (&self.cube, [&self.player_texture], &scene.player),
            (&self.pyramid, [&self.pickup_texture], &scene.pickups),
            (&self.pyramid, [&self.hazard_texture], &scene.hazards),
        ];
        let records: Vec<MaterialRecord<'_>> = shapes
            .iter()
            .filter(|(_, _, instances)| !instances.is_empty())
            .map(|(mesh, textures, instances)| MaterialRecord {
                pipeline: &self.scene_pipeline,
                geometry: GeometrySource::Mesh(mesh),
                textures,
                shadow_map: None,
                uniform: &[],
                storage: &[],
                instances,
            })
            .collect();
        queue.render_and_present(
            frame,
            SceneSubmission {
                scene: Scene {
                    records: &records,
                    shadow: None,
                    output: SceneOutput::Postprocessed {
                        postprocess: Postprocess {
                            pipeline: &self.postprocess_pipeline,
                            targets: &self.targets,
                            uniform: &[],
                        },
                        foreground_start: None,
                        offscreen: &[],
                        overlay: &[],
                    },
                },
                clear: CLEAR,
            },
        )?;
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — Forge Run",
        LogicalSize::new(1100, 700),
    ))?;
    let initial_metrics = application.wait_for_first_metrics(&window)?;
    let mut graphics = OpenedGraphics::open(
        window.surface_target(),
        initial_metrics,
        DeviceRequest::default(),
    )?;
    graphics.queue.set_gpu_timing_enabled(true)?;
    println!(
        "backend: {}, samples: {:?}",
        graphics.selection.backend(),
        graphics.selection.sample_count()
    );
    println!("forge run: W/A/S/D or arrows move; recover eight crystals, avoid sentries, R resets");

    let mut resources = Resources::new(&graphics)?;
    let mut game = Game::default();
    let mut runtime = Runtime::new(RuntimeConfig::fixed_hz(60)?, Instant::now());
    let mut feedback_unsupported = false;

    loop {
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            runtime.handle_window_event(event, Instant::now());
            let WindowEvent::RedrawRequested(metrics) = event else {
                return Ok(());
            };
            match graphics.surface.take_present_feedback()? {
                PresentFeedback::Reported(frames) => {
                    for presented in frames {
                        match presented.presented_at() {
                            Some(presented_at) => runtime.record_presented(presented_at),
                            None => runtime.record_untimed_presented(),
                        }
                    }
                }
                PresentFeedback::Unsupported => feedback_unsupported = true,
                _ => {}
            }

            let FrameAcquire::Ready(frame) = graphics.surface.acquire(metrics)? else {
                return Ok(());
            };
            if let GpuTimingFeedback::Reported(frames) = graphics.queue.take_gpu_timings()? {
                for timing in frames {
                    let Some(frame_scope) = timing.scopes().first() else {
                        continue;
                    };
                    if frame_scope.duration().as_secs_f64() >= GPU_HITCH_SECONDS {
                        eprintln!(
                            "GPU hitch: frame={} total={:.3} ms scopes={:?}",
                            timing.frame_index(),
                            frame_scope.duration().as_secs_f64() * 1_000.0,
                            timing.scopes()
                        );
                    }
                }
            }
            let runtime_frame = runtime.begin_frame(Instant::now());
            let plan = runtime_frame.plan();
            if plan.fixed_steps() != 0 {
                game.handle_frame_input(runtime_frame.input());
            }
            for _ in 0..plan.fixed_steps() {
                game.fixed_update(runtime_frame.input(), plan.fixed_step().as_secs_f32());
            }
            game.variable_update(plan.frame_delta().as_secs_f32());
            resources.render(
                &graphics.device,
                &mut graphics.queue,
                frame,
                &game,
                plan.interpolation(),
            )?;
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            break;
        }
    }

    graphics.shutdown()?;
    if feedback_unsupported {
        println!("presentation feedback: unsupported on this backend");
    }
    println!("presentation pacing: {}", runtime.pacing_report());
    Ok(())
}

fn texture(
    device: &Device<'_>,
    first: [u8; 3],
    second: [u8; 3],
    scale: usize,
) -> Result<Texture, Box<dyn Error>> {
    Ok(device.create_texture(&TextureDescriptor {
        shape: TextureShape::D2 {
            width: 8,
            height: 8,
        },
        mips: MipChain::Single,
        data: TexelData::Rgba8Srgb(&[&checkerboard(first, second, scale)]),
    })?)
}

/// The material every shape is drawn with: `forge.wgsl`, a vertex buffer of shape vertices and an
/// instance buffer of transforms.
fn forge_pipeline(device: &Device<'_>) -> Result<MaterialPipeline, Box<dyn Error>> {
    Ok(device.create_material_pipeline(MaterialPipelineDescriptor {
        shader: ShaderArtifact::new(FORGE_SHADER)?,
        vertex_entry: "forge_vertex",
        fragment_entry: "forge_fragment",
        vertex_layout: VERTEX_LAYOUT,
        instance_layout: Some(INSTANCE_LAYOUT),
        bindings: &[
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
    })?)
}
