//! Asserted conformance cases for the public graphics slice.
//!
//! Unlike the interactive example and the finite validation probes, every case here asserts its
//! observable outcome and the process exits nonzero on the first divergence. The cases cover
//! invalid usage, deferred abandonment recovery, abandonment-driven surface generations, the
//! observable one-sample fallback, mixed-session rejection, and fallible shutdown.

use std::error::Error;
use std::time::Instant;

use mulciber::{
    ClearColor, DeviceRequest, FrameAcquire, Mesh, OpenedGraphics, SampleCount, ShaderArtifact,
    TexturedDraw, Vertex,
};
use mulciber_platform::{
    Application, LogicalSize, PumpStatus, Window, WindowDescriptor, WindowEvent, WindowMetrics,
};

const SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cube.shaderbin"));

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

const TRIANGLE_VERTICES: [Vertex; 3] = [
    Vertex {
        position: [-0.5, -0.5, 0.0],
        color: [1.0, 1.0, 1.0],
        uv: [0.0, 1.0],
    },
    Vertex {
        position: [0.5, -0.5, 0.0],
        color: [1.0, 1.0, 1.0],
        uv: [1.0, 1.0],
    },
    Vertex {
        position: [0.0, 0.5, 0.0],
        color: [1.0, 1.0, 1.0],
        uv: [0.5, 0.0],
    },
];

fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — public API conformance",
        LogicalSize::new(640, 400),
    ))?;
    let initial_metrics = wait_for_initial_metrics(&mut application, &window)?;
    let mut cases = Cases::new(&window, initial_metrics)?;
    let started = Instant::now();
    let mut outcome: Option<Result<u32, Box<dyn Error>>> = None;

    while outcome.is_none() {
        if started.elapsed().as_secs() > 60 {
            return Err("conformance run exceeded its 60 second budget".into());
        }
        let status = application.pump_events(&window, |event| {
            if outcome.is_some() {
                return;
            }
            let WindowEvent::RedrawRequested(metrics) = event else {
                return;
            };
            match cases.advance(metrics) {
                Ok(true) => outcome = Some(Ok(cases.passed)),
                Ok(false) => {}
                Err(error) => outcome = Some(Err(error)),
            }
        })?;
        if status == PumpStatus::Exit {
            return Err("window closed before the conformance cases completed".into());
        }
    }

    let passed = outcome.expect("loop exits only with an outcome")?;
    println!("conformance: {passed} case(s) passed");
    Ok(())
}

/// Sequential conformance state driven by one acquisition-consuming step per redraw.
struct Cases<'window> {
    step: u32,
    passed: u32,
    graphics: Option<OpenedGraphics<'window>>,
    window: &'window Window,
    mesh: Option<Mesh>,
    foreign_mesh: Option<Mesh>,
    texture: Option<mulciber::Texture>,
    pipeline: Option<mulciber::TexturedPipeline>,
    targets: Option<mulciber::RenderTargets>,
}

impl<'window> Cases<'window> {
    fn new(window: &'window Window, metrics: WindowMetrics) -> Result<Self, Box<dyn Error>> {
        let graphics =
            OpenedGraphics::open(window.surface_target(), metrics, DeviceRequest::default())?;
        Ok(Self {
            step: 0,
            passed: 0,
            graphics: Some(graphics),
            window,
            mesh: None,
            foreign_mesh: None,
            texture: None,
            pipeline: None,
            targets: None,
        })
    }

    fn pass(&mut self, name: &str) {
        self.passed += 1;
        println!("conformance: {name} ok");
    }

    /// Runs the next step. Returns `Ok(true)` when every case has passed.
    #[allow(clippy::too_many_lines)]
    fn advance(&mut self, metrics: WindowMetrics) -> Result<bool, Box<dyn Error>> {
        match self.step {
            // Creation-time validation and one draw-time validation failure, all on session A.
            0 => {
                {
                    let graphics = self.graphics.as_ref().expect("session A is open");
                    expect_error(
                        graphics.device.create_mesh(&[], &[]).map(|_| ()),
                        "non-empty",
                        "empty mesh rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_mesh(&TRIANGLE_VERTICES, &[0, 1, 3])
                            .map(|_| ()),
                        "out-of-range",
                        "out-of-range index rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_rgba8_srgb_texture(4, 4, &[0_u8; 4])
                            .map(|_| ()),
                        "does not match",
                        "texture byte mismatch rejected",
                    )?;
                    expect_error(
                        graphics
                            .device
                            .create_rgba8_srgb_texture(0, 4, &[])
                            .map(|_| ()),
                        "does not match",
                        "zero-dimension texture rejected",
                    )?;
                }
                self.pass("empty mesh rejected");
                self.pass("out-of-range index rejected");
                self.pass("texture byte mismatch rejected");
                self.pass("zero-dimension texture rejected");

                let graphics = self.graphics.as_ref().expect("session A is open");
                let mesh = graphics.device.create_mesh(&TRIANGLE_VERTICES, &[0, 1, 2])?;
                let texture = graphics
                    .device
                    .create_rgba8_srgb_texture(2, 2, &[255_u8; 16])?;
                let pipeline = graphics
                    .device
                    .create_textured_pipeline(ShaderArtifact::new(SHADER)?)?;
                let targets = graphics
                    .device
                    .create_render_targets(graphics.surface.info()?)?;
                self.mesh = Some(mesh);
                self.texture = Some(texture);
                self.pipeline = Some(pipeline);
                self.targets = Some(targets);
                self.step = 1;
                Ok(false)
            }
            // A non-finite transform is rejected at draw time; the consumed frame's deferred
            // abandonment must not poison the session.
            1 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let graphics = self.graphics.as_mut().expect("session A is open");
                let mut transform = IDENTITY;
                transform[3][3] = f32::NAN;
                let draw = TexturedDraw {
                    mesh: self.mesh.as_ref().expect("mesh exists"),
                    texture: self.texture.as_ref().expect("texture exists"),
                    pipeline: self.pipeline.as_ref().expect("pipeline exists"),
                    targets: self.targets.as_ref().expect("targets exist"),
                    model_view_projection: transform,
                    clear: ClearColor::BLACK,
                };
                expect_error(
                    graphics
                        .queue
                        .draw_textured_and_present(frame, draw)
                        .map(|_| ()),
                    "finite",
                    "non-finite transform rejected",
                )?;
                self.pass("non-finite transform rejected");
                self.step = 2;
                Ok(false)
            }
            // Explicit abandonment succeeds and the session recovers.
            2 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                frame.abandon()?;
                self.pass("acquired frame abandoned");
                self.step = 3;
                Ok(false)
            }
            // If abandonment replaced the generation, stale targets must be rejected before a
            // rebuilt set presents; otherwise the original targets still present. Either branch
            // must end in one successful presentation.
            3 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let info = frame.surface_info();
                let stale = self.targets.expect("targets exist");
                if info == stale.info() {
                    let disposition = self.draw(frame, IDENTITY)?;
                    assert_presented(disposition)?;
                    self.pass("presentation after abandonment (stable generation)");
                    self.step = 5;
                } else {
                    let graphics = self.graphics.as_mut().expect("session A is open");
                    let draw = TexturedDraw {
                        mesh: self.mesh.as_ref().expect("mesh exists"),
                        texture: self.texture.as_ref().expect("texture exists"),
                        pipeline: self.pipeline.as_ref().expect("pipeline exists"),
                        targets: &stale,
                        model_view_projection: IDENTITY,
                        clear: ClearColor::BLACK,
                    };
                    expect_error(
                        graphics
                            .queue
                            .draw_textured_and_present(frame, draw)
                            .map(|_| ()),
                        "stale",
                        "superseded-generation targets rejected",
                    )?;
                    self.targets = Some(graphics.device.create_render_targets(info)?);
                    self.pass("superseded-generation targets rejected");
                    self.step = 4;
                }
                Ok(false)
            }
            // Present with the rebuilt targets after the stale rejection.
            4 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let disposition = self.draw(frame, IDENTITY)?;
                assert_presented(disposition)?;
                self.pass("presentation after generation replacement");
                self.step = 5;
                Ok(false)
            }
            // Session A shuts down cleanly; session B reopens the same window with the forced
            // one-sample path and keeps a session-A handle for the mixed-session case.
            5 => {
                self.foreign_mesh = self.mesh.take();
                let graphics = self.graphics.take().expect("session A is open");
                graphics.shutdown()?;
                self.pass("fallible shutdown succeeded");

                let reopened = OpenedGraphics::open(
                    self.window.surface_target(),
                    metrics,
                    DeviceRequest {
                        preferred_sample_count: SampleCount::One,
                    },
                )?;
                if reopened.selection.sample_count() != SampleCount::One {
                    return Err("forced one-sample selection was not observable".into());
                }
                self.pass("forced one-sample selection observable");

                self.mesh = Some(
                    reopened
                        .device
                        .create_mesh(&TRIANGLE_VERTICES, &[0, 1, 2])?,
                );
                self.texture = Some(reopened.device.create_rgba8_srgb_texture(
                    2,
                    2,
                    &[255_u8; 16],
                )?);
                self.pipeline = Some(
                    reopened
                        .device
                        .create_textured_pipeline(ShaderArtifact::new(SHADER)?)?,
                );
                self.targets = Some(
                    reopened
                        .device
                        .create_render_targets(reopened.surface.info()?)?,
                );
                self.graphics = Some(reopened);
                self.step = 6;
                Ok(false)
            }
            // A handle from the shut-down session is rejected by the new session.
            6 => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let graphics = self.graphics.as_mut().expect("session B is open");
                let draw = TexturedDraw {
                    mesh: self.foreign_mesh.as_ref().expect("session A mesh kept"),
                    texture: self.texture.as_ref().expect("texture exists"),
                    pipeline: self.pipeline.as_ref().expect("pipeline exists"),
                    targets: self.targets.as_ref().expect("targets exist"),
                    model_view_projection: IDENTITY,
                    clear: ClearColor::BLACK,
                };
                expect_error(
                    graphics
                        .queue
                        .draw_textured_and_present(frame, draw)
                        .map(|_| ()),
                    "different sessions",
                    "mixed-session handles rejected",
                )?;
                self.pass("mixed-session handles rejected");
                self.step = 7;
                Ok(false)
            }
            // The one-sample session presents and shuts down cleanly.
            _ => {
                let Some(frame) = self.acquire(metrics)? else {
                    return Ok(false);
                };
                let disposition = self.draw(frame, IDENTITY)?;
                assert_presented(disposition)?;
                self.pass("one-sample presentation");
                let graphics = self.graphics.take().expect("session B is open");
                graphics.shutdown()?;
                self.pass("second fallible shutdown succeeded");
                Ok(true)
            }
        }
    }

    fn acquire(
        &mut self,
        metrics: WindowMetrics,
    ) -> Result<Option<mulciber::Frame<'window>>, Box<dyn Error>> {
        let graphics = self.graphics.as_mut().expect("a session is open");
        match graphics.surface.acquire(metrics)? {
            FrameAcquire::Ready(frame) => {
                // Rebuild targets whenever the frame reports a newer generation than the current
                // set, except while a case is deliberately holding stale targets (step 3 handles
                // its own rebuild).
                if self.step != 3 {
                    let targets = self.targets.expect("targets exist");
                    if frame.surface_info() != targets.info() {
                        self.targets = Some(
                            graphics
                                .device
                                .create_render_targets(frame.surface_info())?,
                        );
                    }
                }
                Ok(Some(frame))
            }
            FrameAcquire::Unavailable(_) => Ok(None),
        }
    }

    fn draw(
        &mut self,
        frame: mulciber::Frame<'_>,
        transform: [[f32; 4]; 4],
    ) -> Result<mulciber::FrameDisposition, Box<dyn Error>> {
        let graphics = self.graphics.as_mut().expect("a session is open");
        let draw = TexturedDraw {
            mesh: self.mesh.as_ref().expect("mesh exists"),
            texture: self.texture.as_ref().expect("texture exists"),
            pipeline: self.pipeline.as_ref().expect("pipeline exists"),
            targets: self.targets.as_ref().expect("targets exist"),
            model_view_projection: transform,
            clear: ClearColor::BLACK,
        };
        Ok(graphics.queue.draw_textured_and_present(frame, draw)?)
    }
}

fn assert_presented(disposition: mulciber::FrameDisposition) -> Result<(), Box<dyn Error>> {
    match disposition {
        mulciber::FrameDisposition::Presented(_) => Ok(()),
        other => Err(format!("expected a presented disposition, got {other:?}").into()),
    }
}

fn expect_error(
    result: Result<(), mulciber::GraphicsError>,
    needle: &str,
    case: &str,
) -> Result<(), Box<dyn Error>> {
    match result {
        Ok(()) => Err(format!("{case}: expected an error, the operation succeeded").into()),
        Err(error) => {
            let message = error.to_string();
            if message.contains(needle) {
                Ok(())
            } else {
                Err(format!(
                    "{case}: diagnostic {message:?} does not identify the contract ({needle:?})"
                )
                .into())
            }
        }
    }
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
