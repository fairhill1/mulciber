//! Exercises validation-only controls around the public textured-cube API.

#[path = "../../../examples/cube/src/scene.rs"]
mod scene;

use std::env;
use std::error::Error;
use std::time::Instant;

use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, OpenedGraphics, ShaderArtifact, TexturedDraw,
};
use mulciber_platform::{
    Application, LogicalSize, PumpStatus, Window, WindowDescriptor, WindowEvent, WindowMetrics,
};

use scene::{CUBE_INDICES, CUBE_VERTICES, checkerboard, transform};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cube.shaderbin"));

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
    let initial_metrics = wait_for_initial_metrics(&mut application, &window)?;
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
    let clear = ClearColor::new(0.025, 0.035, 0.055, 1.0).expect("constant is normalized");
    let started = Instant::now();
    let mut presented = 0_u64;
    let mut abandon_pending = options.abandon_once;
    let mut failure: Option<Box<dyn Error>> = None;

    loop {
        let status = application.pump_events(&window, |event| {
            if failure.is_some() {
                return;
            }
            let WindowEvent::RedrawRequested(metrics) = event else {
                return;
            };
            let result: Result<(), Box<dyn Error>> = (|| {
                match graphics.surface.acquire(metrics)? {
                    FrameAcquire::Ready(frame) if abandon_pending => {
                        frame.abandon()?;
                        targets = graphics
                            .device
                            .create_render_targets(graphics.surface.info()?)?;
                        abandon_pending = false;
                    }
                    FrameAcquire::Ready(frame) => {
                        let info = frame.surface_info();
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
                                clear,
                            },
                        )?;
                        presented += 1;
                    }
                    FrameAcquire::Unavailable(_) => {}
                    FrameAcquire::Reconfigured(info) => {
                        targets = graphics.device.create_render_targets(info)?;
                        println!(
                            "surface generation {} configured at {}x{}",
                            info.generation().get(),
                            info.extent().width(),
                            info.extent().height()
                        );
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                failure = Some(error);
            }
        })?;
        if let Some(error) = failure.take() {
            return Err(error);
        }
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

fn wait_for_initial_metrics(
    application: &mut Application,
    window: &Window,
) -> Result<WindowMetrics, Box<dyn Error>> {
    loop {
        if let Some(metrics) = window.rendering_metrics() {
            return Ok(metrics);
        }
        let mut initial_metrics = None;
        let status = application.pump_events(window, |event| {
            if let WindowEvent::RenderingResumed(metrics) | WindowEvent::RedrawRequested(metrics) =
                event
            {
                initial_metrics = Some(metrics);
            }
        })?;
        if let Some(metrics) = initial_metrics {
            return Ok(metrics);
        }
        if status == PumpStatus::Exit {
            return Err("window closed before drawable metrics became available".into());
        }
    }
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
