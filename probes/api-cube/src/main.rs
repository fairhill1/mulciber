//! Exercises validation-only controls around the public textured-cube API.

#[path = "../../../examples/cube/src/scene.rs"]
mod scene;

use std::env;
use std::error::Error;
use std::time::Instant;

use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, OpenedGraphics, ShaderArtifact, TexturedDraw,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};

use scene::{CUBE_INDICES, CUBE_VERTICES, checkerboard, transform};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cube.shaderbin"));
const CLEAR: ClearColor = ClearColor::opaque(0.025, 0.035, 0.055);

struct Options {
    frame_limit: Option<u64>,
    abandon_once: bool,
    force_one_sample: bool,
}

#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let options = options()?;
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — public API cube validation",
        LogicalSize::new(960, 540),
    ))?;
    let initial_metrics = application.wait_for_first_metrics(&window)?;
    let request = DeviceRequest {
        preferred_sample_count: if options.force_one_sample {
            mulciber::SampleCount::One
        } else {
            mulciber::SampleCount::Four
        },
    };
    let mut graphics = OpenedGraphics::open(window.surface_target(), initial_metrics, request)?;
    println!(
        "backend: {}, samples: {:?}",
        graphics.selection.backend(),
        graphics.selection.sample_count()
    );

    let mesh = graphics.device.create_mesh(&CUBE_VERTICES, &CUBE_INDICES)?;
    let texture = graphics
        .device
        .create_rgba8_srgb_texture(8, 8, &checkerboard())?;
    let pipeline = graphics
        .device
        .create_textured_pipeline(ShaderArtifact::new(SHADER)?)?;
    let mut targets = graphics
        .device
        .create_render_targets(graphics.surface.info()?)?;
    let started = Instant::now();
    let mut presented = 0_u64;
    let mut abandon_pending = options.abandon_once;

    loop {
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            let WindowEvent::RedrawRequested(metrics) = event else {
                return Ok(());
            };
            match graphics.surface.acquire(metrics)? {
                FrameAcquire::Ready(frame) if abandon_pending => {
                    frame.abandon()?;
                    abandon_pending = false;
                }
                FrameAcquire::Ready(frame) => {
                    let info = frame.surface_info();
                    if info != targets.info() {
                        targets = graphics.device.create_render_targets(info)?;
                        println!(
                            "surface generation {} configured at {}x{}",
                            info.generation().get(),
                            info.extent().width(),
                            info.extent().height()
                        );
                    }
                    let aspect = info.extent().width() as f32 / info.extent().height() as f32;
                    graphics.queue.draw_textured_and_present(
                        frame,
                        TexturedDraw {
                            mesh: &mesh,
                            texture: &texture,
                            pipeline: &pipeline,
                            targets: &targets,
                            model_view_projection: transform(
                                started.elapsed().as_secs_f32(),
                                aspect,
                            ),
                            clear: CLEAR,
                        },
                    )?;
                    presented += 1;
                }
                FrameAcquire::Unavailable(_) => {}
            }
            Ok(())
        })?;
        if status == PumpStatus::Exit
            || options
                .frame_limit
                .is_some_and(|frame_limit| presented >= frame_limit)
        {
            break;
        }
    }

    if abandon_pending {
        return Err("requested abandonment never acquired a frame".into());
    }
    graphics.shutdown()?;
    println!("presented {presented} textured cube frame(s)");
    Ok(())
}

fn options() -> Result<Options, Box<dyn Error>> {
    let mut frame_limit = None;
    let mut abandon_once = false;
    let mut force_one_sample = false;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--frames" => {
                let value = arguments.next().ok_or("--frames requires a value")?;
                let value = value.parse::<u64>()?;
                if value == 0 {
                    return Err("--frames must be greater than zero".into());
                }
                frame_limit = Some(value);
            }
            "--abandon-acquired-frame-once" => abandon_once = true,
            "--force-one-sample" => force_one_sample = true,
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    Ok(Options {
        frame_limit,
        abandon_once,
        force_one_sample,
    })
}
