//! Exercises validation-only controls around the public clear API.

use std::env;
use std::error::Error;

use mulciber::{ClearColor, ClearSurface, FrameAcquire};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};

const COLOR: ClearColor = ClearColor::opaque(0.035, 0.14, 0.23);

struct Options {
    frame_limit: Option<u64>,
    abandon_once: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = options()?;
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber clear API validation",
        LogicalSize::new(960, 540),
    ))?;
    let initial_metrics = application.wait_for_first_metrics(&window)?;
    let mut surface = ClearSurface::new(window.surface_target(), initial_metrics)?;
    let mut presented = 0_u64;
    let mut last_generation = Some(surface.info().generation());
    let mut abandon_pending = options.abandon_once;

    loop {
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            let WindowEvent::RedrawRequested(metrics) = event else {
                return Ok(());
            };
            match surface.acquire(metrics)? {
                FrameAcquire::Ready(frame) if abandon_pending => {
                    let disposition = frame.abandon()?;
                    abandon_pending = false;
                    println!(
                        "abandoned generation {}; waiting for a recovery presentation",
                        disposition.generation().get()
                    );
                }
                FrameAcquire::Ready(frame) => {
                    let info = frame.surface_info();
                    if last_generation != Some(info.generation()) {
                        last_generation = Some(info.generation());
                        println!(
                            "surface generation {} configured at {}x{}",
                            info.generation().get(),
                            info.extent().width(),
                            info.extent().height()
                        );
                    }
                    frame.clear_and_present(COLOR)?;
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
    surface.shutdown()?;
    println!("presented {presented} clear frame(s)");
    Ok(())
}

fn options() -> Result<Options, Box<dyn Error>> {
    let mut frame_limit = None;
    let mut abandon_once = false;
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
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    Ok(Options {
        frame_limit,
        abandon_once,
    })
}
