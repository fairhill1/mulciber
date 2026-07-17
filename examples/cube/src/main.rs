//! Renders a spinning textured cube through Mulciber's target-selected native backend.

mod scene;

use std::error::Error;
use std::time::Instant;

use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, OpenedGraphics, SampleCount, ShaderArtifact,
    TexturedDraw,
};
use mulciber_platform::{
    Application, LogicalSize, PumpStatus, Window, WindowDescriptor, WindowEvent, WindowMetrics,
};

use scene::{CUBE_INDICES, CUBE_VERTICES, checkerboard, transform};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cube.shaderbin"));

#[allow(clippy::cast_precision_loss)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — same-source textured cube",
        LogicalSize::new(960, 540),
    ))?;
    let initial_metrics = wait_for_initial_metrics(&mut application, &window)?;
    let mut graphics = OpenedGraphics::open(
        window.surface_target(),
        initial_metrics,
        DeviceRequest {
            preferred_sample_count: SampleCount::Four,
        },
    )?;
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
                loop {
                    match graphics.surface.acquire(metrics)? {
                        FrameAcquire::Ready(frame) => {
                            let info = frame.surface_info();
                            let aspect =
                                info.extent().width() as f32 / info.extent().height() as f32;
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
                            return Ok(());
                        }
                        FrameAcquire::Unavailable(_) => return Ok(()),
                        // Rebuild extent-dependent targets and acquire again in the same redraw so
                        // every reconfigured size presents immediately; deferring to the next pump
                        // leaves the committed window contents trailing a live resize.
                        FrameAcquire::Reconfigured(info) => {
                            targets = graphics.device.create_render_targets(info)?;
                        }
                    }
                }
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

    graphics.shutdown()?;
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
