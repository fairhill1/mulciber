//! Exercises validation-only controls around the public textured-cube API.

#[path = "../../../examples/cube/src/scene.rs"]
mod scene;

use std::env;
use std::error::Error;
use std::time::{Duration, Instant};

use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, GpuTimingFeedback, GpuTimingScope, OpenedGraphics,
    PresentFeedback, ShaderArtifact, TexturedDraw,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};
use mulciber_runtime::PacingDiagnostics;

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
    graphics.queue.set_gpu_timing_enabled(true)?;
    println!(
        "backend: {}, samples: {:?}, GPU timing: {:?}",
        graphics.selection.backend(),
        graphics.selection.sample_count(),
        graphics.selection.gpu_timing_support()
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
    let mut pacing = PacingDiagnostics::new();
    let mut feedback_unsupported = false;
    let mut gpu_timing_unsupported = false;
    let mut gpu_timing_samples = 0_u64;
    let mut maximum_gpu_frame = Duration::ZERO;

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
            match graphics.surface.take_present_feedback()? {
                PresentFeedback::Reported(frames) => {
                    for frame in frames {
                        match frame.presented_at() {
                            Some(presented_at) => pacing.record_presented(presented_at),
                            None => pacing.record_untimed_presented(),
                        }
                    }
                }
                PresentFeedback::Unsupported => feedback_unsupported = true,
                _ => {}
            }
            match graphics.queue.take_gpu_timings()? {
                GpuTimingFeedback::Reported(frames) => {
                    gpu_timing_samples += u64::try_from(frames.len())?;
                    for frame in frames {
                        if let Some(scope) = frame
                            .scopes()
                            .iter()
                            .find(|scope| scope.scope() == GpuTimingScope::Frame)
                        {
                            maximum_gpu_frame = maximum_gpu_frame.max(scope.duration());
                        }
                    }
                }
                GpuTimingFeedback::Unsupported => gpu_timing_unsupported = true,
                GpuTimingFeedback::Disabled => {
                    return Err("GPU timing unexpectedly remained disabled".into());
                }
                _ => {}
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
    if !gpu_timing_unsupported && presented >= 2 && gpu_timing_samples == 0 {
        return Err("GPU timing produced no completed samples".into());
    }
    graphics.shutdown()?;
    println!("presented {presented} textured cube frame(s)");
    if feedback_unsupported {
        println!("presentation feedback: unsupported on this backend");
    } else {
        println!("presentation pacing: {}", pacing.report());
    }
    if gpu_timing_unsupported {
        println!("GPU timing: unsupported on the selected queue");
    } else {
        println!(
            "GPU timing: samples={gpu_timing_samples} maximum_frame={:.3} ms",
            maximum_gpu_frame.as_secs_f64() * 1_000.0
        );
    }
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
