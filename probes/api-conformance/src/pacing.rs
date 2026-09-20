//! Native pacing transitions under repeatable light/CPU-bound workloads; no asset loading.
use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, OpenedGraphics, PresentFeedback, PresentationMode,
    TexturedDraw,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};
use std::{
    error::Error,
    time::{Duration, Instant},
};

#[allow(clippy::too_many_lines)]
pub(super) fn run() -> Result<(), Box<dyn Error>> {
    let mut app = Application::new()?;
    let window = app.create_window(&WindowDescriptor::new(
        "Mulciber presentation pacing",
        LogicalSize::new(640, 360),
    ))?;
    let metrics = app.wait_for_first_metrics(&window)?;
    println!("display,{:?}", metrics.display_timing());
    let mut graphics =
        OpenedGraphics::open(window.surface_target(), metrics, DeviceRequest::default())?;
    let mesh = graphics
        .device
        .create_mesh(&super::TRIANGLE_VERTICES, &[0, 1, 2])?;
    let texture = graphics
        .device
        .create_rgba8_srgb_texture(1, 1, &[255, 255, 255, 255])?;
    let shader = mulciber::ShaderArtifact::new(super::SHADER)?;
    let pipeline = graphics.device.create_textured_pipeline(shader)?;
    let mut targets = graphics
        .device
        .create_render_targets(graphics.surface.info()?)?;
    let phases = [
        ("adaptive-light", PresentationMode::Adaptive, false, 180),
        ("adaptive-slow", PresentationMode::Adaptive, true, 180),
        ("adaptive-recovery", PresentationMode::Adaptive, false, 240),
        ("strict-light", PresentationMode::Strict, false, 240),
        ("strict-slow", PresentationMode::Strict, true, 180),
        ("strict-recovery", PresentationMode::Strict, false, 240),
    ];
    let base = Instant::now();
    let mut previous = base;
    let mut index = 0;
    for (name, mode, slow, count) in phases {
        if !graphics.surface.supports_presentation_mode(mode)? {
            println!("unsupported,{name}");
            continue;
        }
        graphics.surface.set_presentation_mode(mode)?;
        let mut rendered = 0;
        while rendered < count {
            let status = app.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
                let WindowEvent::RedrawRequested(metrics) = event else {
                    return Ok(());
                };
                if let PresentFeedback::Reported(frames) =
                    graphics.surface.take_present_feedback()?
                {
                    for frame in frames {
                        if let Some(at) = frame.presented_at() {
                            println!(
                                "present,{},{}",
                                frame.index(),
                                at.saturating_duration_since(base).as_secs_f64()
                            );
                        }
                    }
                }
                let FrameAcquire::Ready(frame) = graphics.surface.acquire(metrics)? else {
                    return Ok(());
                };
                if frame.surface_info() != targets.info() {
                    targets = graphics
                        .device
                        .create_render_targets(frame.surface_info())?;
                }
                if slow {
                    std::thread::sleep(Duration::from_millis(22));
                }
                graphics.queue.draw_textured_and_present(
                    frame,
                    TexturedDraw {
                        mesh: &mesh,
                        texture: &texture,
                        pipeline: &pipeline,
                        targets: &targets,
                        model_view_projection: super::IDENTITY,
                        clear: ClearColor::BLACK,
                    },
                )?;
                let now = Instant::now();
                println!(
                    "frame,{index},{name},{:.6},{:?}",
                    now.duration_since(previous).as_secs_f64() * 1000.0,
                    graphics.surface.active_presentation_mode()?
                );
                previous = now;
                index += 1;
                rendered += 1;
                Ok(())
            })?;
            if status == PumpStatus::Exit {
                return Err("pacing probe closed before completing".into());
            }
        }
    }
    graphics.shutdown()?;
    Ok(())
}
