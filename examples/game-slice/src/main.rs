//! A small playable collect-and-avoid loop using the extracted platform and graphics slices.

mod game;
mod scene;

use std::error::Error;
use std::time::Instant;

use game::Game;
use mulciber::{
    ClearColor, Device, DeviceRequest, Frame, FrameAcquire, GpuTimingFeedback,
    InstancedTexturedPipeline, Mesh, OpenedGraphics, PostprocessPipeline, PostprocessTargets,
    PresentFeedback, Queue, SceneContent, SceneOutput, SceneSubmission, ShaderArtifact, Texture,
    TexturedInstanceBatch,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};
use mulciber_runtime::{Runtime, RuntimeConfig};
use scene::{
    CUBE_INDICES, CUBE_VERTICES, PYRAMID_INDICES, PYRAMID_VERTICES, checkerboard, transforms,
};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/game.shaderbin"));
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
    scene_pipeline: InstancedTexturedPipeline,
    postprocess_pipeline: PostprocessPipeline,
    targets: PostprocessTargets,
}

impl Resources {
    fn new(graphics: &OpenedGraphics<'_>) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            cube: graphics.device.create_mesh(&CUBE_VERTICES, &CUBE_INDICES)?,
            pyramid: graphics
                .device
                .create_mesh(&PYRAMID_VERTICES, &PYRAMID_INDICES)?,
            ground_texture: texture(&graphics.device, [30, 42, 58], [55, 72, 82], 1)?,
            obstacle_texture: texture(&graphics.device, [100, 105, 120], [42, 48, 65], 2)?,
            player_texture: texture(&graphics.device, [35, 230, 190], [20, 90, 145], 2)?,
            pickup_texture: texture(&graphics.device, [255, 195, 45], [245, 80, 30], 1)?,
            hazard_texture: texture(&graphics.device, [245, 45, 85], [100, 20, 135], 2)?,
            scene_pipeline: graphics
                .device
                .create_instanced_textured_pipeline(ShaderArtifact::new(SHADER)?)?,
            postprocess_pipeline: graphics
                .device
                .create_postprocess_pipeline(ShaderArtifact::new(SHADER)?)?,
            targets: graphics
                .device
                .create_postprocess_targets(graphics.surface.info()?)?,
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
            self.targets = device.create_postprocess_targets(info)?;
        }
        let aspect = info.extent().width() as f32 / info.extent().height() as f32;
        let scene = transforms(game, aspect, interpolation);
        let mut batches = vec![
            TexturedInstanceBatch {
                mesh: &self.cube,
                texture: &self.ground_texture,
                pipeline: &self.scene_pipeline,
                model_view_projections: &scene.ground,
            },
            TexturedInstanceBatch {
                mesh: &self.cube,
                texture: &self.obstacle_texture,
                pipeline: &self.scene_pipeline,
                model_view_projections: &scene.obstacles,
            },
            TexturedInstanceBatch {
                mesh: &self.cube,
                texture: &self.player_texture,
                pipeline: &self.scene_pipeline,
                model_view_projections: &scene.player,
            },
        ];
        if !scene.pickups.is_empty() {
            batches.push(TexturedInstanceBatch {
                mesh: &self.pyramid,
                texture: &self.pickup_texture,
                pipeline: &self.scene_pipeline,
                model_view_projections: &scene.pickups,
            });
        }
        batches.push(TexturedInstanceBatch {
            mesh: &self.pyramid,
            texture: &self.hazard_texture,
            pipeline: &self.scene_pipeline,
            model_view_projections: &scene.hazards,
        });
        queue.render_and_present(
            frame,
            SceneSubmission {
                content: SceneContent::Instanced(&batches),
                output: SceneOutput::Postprocessed {
                    pipeline: &self.postprocess_pipeline,
                    targets: &self.targets,
                    uniform: &[],
                },
                shadow: None,
                overlay: None,
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
            runtime.handle_window_event(event);
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
    Ok(device.create_rgba8_srgb_texture(8, 8, &checkerboard(first, second, scale))?)
}
