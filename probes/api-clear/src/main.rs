//! Exercises validation-only controls around the public clear API.

use std::env;
use std::error::Error;

use mulciber::{ClearColor, ClearSurface, FrameAcquire};
use mulciber_platform::{
    Application, LogicalSize, PumpStatus, Window, WindowDescriptor, WindowEvent, WindowMetrics,
};

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
    let initial_metrics = wait_for_initial_metrics(&mut application, &window)?;
    let mut surface = ClearSurface::new(window.surface_target(), initial_metrics)?;
    let color = ClearColor::new(0.035, 0.14, 0.23, 1.0).expect("constant is normalized");
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
                        frame.clear_and_present(color)?;
                        presented += 1;
                    }
                    FrameAcquire::Unavailable(_) => {}
                    FrameAcquire::Reconfigured(info) => {
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
    surface.shutdown()?;
    println!("presented {presented} clear frame(s)");
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
