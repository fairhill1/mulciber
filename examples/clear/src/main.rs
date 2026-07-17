//! Clears and presents through Mulciber's target-selected native graphics backend.

use std::error::Error;

use mulciber::{ClearColor, ClearSurface, FrameAcquire};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};

const COLOR: ClearColor = ClearColor::opaque(0.035, 0.14, 0.23);

fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber clear slice",
        LogicalSize::new(960, 540),
    ))?;
    let initial_metrics = application.wait_for_first_metrics(&window)?;
    let mut surface = ClearSurface::new(window.surface_target(), initial_metrics)?;

    loop {
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            let WindowEvent::RedrawRequested(metrics) = event else {
                return Ok(());
            };
            match surface.acquire(metrics)? {
                FrameAcquire::Ready(frame) => {
                    frame.clear_and_present(COLOR)?;
                }
                FrameAcquire::Unavailable(_) => {}
            }
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            break;
        }
    }

    surface.shutdown()?;
    Ok(())
}
