//! Reports the target-selected native backend and its multisample capability selection.
//!
//! Opening a graphics session requires no shader artifact, pipeline, or presented frame, so this
//! is the smallest program that observes a capability decision: it opens the device with a
//! preferred sample count, prints the observable selection, and shuts down. Pass
//! `--force-one-sample` to request the one-sample path explicitly and exercise the same fallback
//! shape the probes force.

use std::error::Error;

use mulciber::{DeviceRequest, OpenedGraphics, SampleCount};
use mulciber_platform::{Application, LogicalSize, WindowDescriptor};

fn main() -> Result<(), Box<dyn Error>> {
    let force_one_sample = std::env::args().any(|argument| argument == "--force-one-sample");
    let requested = if force_one_sample {
        SampleCount::One
    } else {
        SampleCount::Four
    };

    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber capability report",
        LogicalSize::new(480, 270),
    ))?;
    let metrics = application.wait_for_first_metrics(&window)?;
    let graphics = OpenedGraphics::open(
        window.surface_target(),
        metrics,
        DeviceRequest {
            preferred_sample_count: requested,
        },
    )?;

    let selected = graphics.selection.sample_count();
    println!("backend: {}", graphics.selection.backend());
    println!("requested samples: {requested:?}");
    println!("selected samples: {selected:?}");
    if requested == SampleCount::Four && selected == SampleCount::One {
        println!("fallback: four-sample rendering is unsupported; one sample per pixel selected");
    }

    graphics.shutdown()?;
    Ok(())
}
