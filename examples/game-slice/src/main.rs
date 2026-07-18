//! A small playable collect-and-avoid loop using the extracted platform and graphics slices.

mod game;
mod scene;

use std::error::Error;
use std::time::Instant;

use game::Game;
use mulciber::{
    ClearColor, Device, DeviceRequest, Frame, FrameAcquire, InstancedTexturedPipeline, Mesh,
    OpenedGraphics, PostprocessPipeline, PostprocessTargets, Queue, SceneContent, SceneOutput,
    SceneSubmission, ShaderArtifact, Texture, TexturedInstanceBatch,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};
use scene::{
    CUBE_INDICES, CUBE_VERTICES, PYRAMID_INDICES, PYRAMID_VERTICES, checkerboard, transforms,
};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/game.shaderbin"));
const CLEAR: ClearColor = ClearColor::opaque(0.008, 0.014, 0.026);

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
    ) -> Result<(), Box<dyn Error>> {
        let info = frame.surface_info();
        if info != self.targets.info() {
            self.targets = device.create_postprocess_targets(info)?;
        }
        let aspect = info.extent().width() as f32 / info.extent().height() as f32;
        let scene = transforms(game, aspect);
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
    println!(
        "backend: {}, samples: {:?}",
        graphics.selection.backend(),
        graphics.selection.sample_count()
    );
    println!("forge run: W/A/S/D or arrows move; recover eight crystals, avoid sentries, R resets");

    let mut resources = Resources::new(&graphics)?;
    let started = Instant::now();
    let mut game = Game::default();

    loop {
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            let metrics = match event {
                WindowEvent::Input(input) => {
                    game.handle_input(input);
                    return Ok(());
                }
                WindowEvent::RedrawRequested(metrics) => metrics,
                _ => return Ok(()),
            };
            let FrameAcquire::Ready(frame) = graphics.surface.acquire(metrics)? else {
                return Ok(());
            };
            game.update(started.elapsed().as_secs_f32());
            resources.render(&graphics.device, &mut graphics.queue, frame, &game)?;
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            break;
        }
    }

    graphics.shutdown()?;
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
