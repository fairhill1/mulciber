//! Clears and presents through Mulciber's target-selected native graphics backend.

use std::error::Error;

use mulciber::{ClearColor, ClearSurface, FrameAcquire};
use mulciber_platform::{
    Application, LogicalSize, PumpStatus, Window, WindowDescriptor, WindowEvent, WindowMetrics,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber clear slice",
        LogicalSize::new(960, 540),
    ))?;
    let initial_metrics = wait_for_initial_metrics(&mut application, &window)?;
    let mut surface = ClearSurface::new(window.surface_target(), initial_metrics)?;
    let color = ClearColor::new(0.035, 0.14, 0.23, 1.0).expect("constant is normalized");
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
                    FrameAcquire::Ready(frame) => {
                        frame.clear_and_present(color)?;
                    }
                    FrameAcquire::Unavailable(_) => {}
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
        if status == PumpStatus::Exit {
            break;
        }
    }

    surface.shutdown()?;
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
