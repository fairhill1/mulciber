//! Native evidence for presented-frame capture through the public surface API: request timing,
//! abandonment, and the final color of direct (multisample-resolved), HDR-composited and
//! overlaid frames, checked pixel by pixel against the four-quadrant scene that was drawn.
//! Pass `--force-one-sample` to render the direct path without a multisample resolve.
use mulciber::{
    BlendMode, ClearColor, DepthMode, DeviceRequest, FrameAcquire, FrameCapture, FrameDisposition,
    GeometrySource, MaterialPipeline, MaterialPipelineDescriptor, MaterialRecord, MeshIndices,
    OpenedGraphics, PostprocessPipeline, PostprocessTargets, RenderScale, RenderTargets,
    SampleCount, SceneContent, SceneOutput, SceneSubmission, ShaderArtifact, SurfaceInfo,
    TransientGeometry, VertexAttribute, VertexFormat, VertexLayout,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};
use std::{error::Error, time::Instant};

const HUD_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/hud.shaderbin"));
const COMPOSITE_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/composite.shaderbin"));
/// The material scene's HUD vertex: clip-space position and linear RGBA.
const LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 24,
    attributes: &[
        VertexAttribute {
            location: 0,
            format: VertexFormat::Float32x2,
            offset: 0,
        },
        VertexAttribute {
            location: 1,
            format: VertexFormat::Float32x4,
            offset: 8,
        },
    ],
};
/// The linear value whose sRGB encoding is one half: a captured 127 or 128 shows the bytes are
/// the encoded ones the display receives, not the linear value (54 or 55).
const HALF_ENCODED: f32 = 0.214_041_14;
/// Clip-space rectangles in top-left, top-right, bottom-left, bottom-right order, y up.
const QUADRANTS: [[f32; 4]; 4] = [
    [-1.0, 0.0, 0.0, 1.0],
    [0.0, 0.0, 1.0, 1.0],
    [-1.0, -1.0, 0.0, 0.0],
    [0.0, -1.0, 1.0, 0.0],
];

/// What one redraw does, in order.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// Present without any request; nothing may be captured.
    Unrequested,
    /// Request only after acquiring; that frame may not be captured.
    RequestAfterAcquire,
    /// Abandon a frame acquired under the pending request; nothing may be captured.
    Abandon,
    /// Present the first frame acquired under the still-pending request.
    Capture(Output),
    /// Request, then present and capture.
    RequestAndCapture(Output),
}

#[derive(Clone, Copy, Debug)]
enum Output {
    /// Material records straight into the presentable target, resolved when multisampled.
    Direct,
    /// HDR material records, then the composite postprocess pass.
    Hdr,
    /// The HDR path with a white overlay record over the bottom-right quadrant.
    HdrOverlay,
}

const STEPS: [Step; 10] = [
    Step::Unrequested,
    Step::RequestAfterAcquire,
    Step::Abandon,
    Step::Capture(Output::Direct),
    Step::RequestAndCapture(Output::Hdr),
    Step::RequestAndCapture(Output::HdrOverlay),
    Step::Unrequested,
    Step::RequestAndCapture(Output::Direct),
    Step::RequestAndCapture(Output::HdrOverlay),
    Step::Unrequested,
];

/// Linear vertex colors per quadrant. Red and blue differ so a channel-order error shows; the
/// bottom-right alpha is one half so an opaque-composited surface shows its forced 255.
fn scene_colors(output: Output) -> [[f32; 4]; 4] {
    let first = if matches!(output, Output::Direct) {
        1.0
    } else {
        // The composite clamps to one, so an HDR value reads back as full intensity.
        4.0
    };
    [
        [first, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
        [1.0, HALF_ENCODED, 0.0, 0.5],
    ]
}

/// Expected RGB bytes per displayed quadrant, and whether its alpha may be the stored value
/// instead of 255.
///
/// The reused float-texture composite maps clip-space y up onto texture v up, so it presents the
/// scene upside down: displayed top rows come from the scene's bottom quadrants. The overlay is
/// drawn in presentable space after the composite and is not flipped, so the white rectangle
/// stays bottom right. Both are what the display shows, which is what a capture must report.
fn expected(output: Output) -> [([u8; 3], bool); 4] {
    let red = ([255, 0, 0], false);
    let green = ([0, 255, 0], false);
    let blue = ([0, 0, 255], false);
    match output {
        // Only the direct path stores the vertex alpha of one half.
        Output::Direct => [red, green, blue, ([255, 128, 0], true)],
        // The composite writes alpha one.
        Output::Hdr => [blue, ([255, 128, 0], false), red, green],
        Output::HdrOverlay => [blue, ([255, 128, 0], false), red, ([255, 255, 255], false)],
    }
}

/// Two triangles per rectangle, in both windings so culling cannot drop any.
fn geometry(rectangles: &[([f32; 4], [f32; 4])]) -> (Vec<u8>, Vec<u16>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (index, ([left, bottom, right, top], color)) in rectangles.iter().enumerate() {
        let base = u16::try_from(index * 4).expect("few rectangles");
        for [x, y] in [[left, bottom], [right, bottom], [right, top], [left, top]] {
            for value in [*x, *y].iter().chain(color) {
                vertices.extend_from_slice(&value.to_ne_bytes());
            }
        }
        for corner in [0, 1, 2, 0, 2, 3, 0, 2, 1, 0, 3, 2] {
            indices.push(base + corner);
        }
    }
    (vertices, indices)
}

/// Compares every pixel away from the quadrant edges with the quadrant's expected bytes, allowing
/// one step of sRGB encoding rounding. Returns the alpha seen in the bottom-right quadrant.
fn check_pixels(
    capture: &FrameCapture,
    info: SurfaceInfo,
    output: Output,
) -> Result<u8, Box<dyn Error>> {
    let (width, height) = (capture.width(), capture.height());
    let extent = info.extent();
    if (width, height) != (extent.width(), extent.height()) {
        return Err(format!(
            "capture is {width}x{height} but the frame was {}x{}",
            extent.width(),
            extent.height()
        )
        .into());
    }
    if capture.pixels().len() != usize::try_from(width * height * 4)? {
        return Err(format!("capture holds {} bytes", capture.pixels().len()).into());
    }
    let expected = expected(output);
    let mut seen_alpha = None;
    let mut checked = 0_u32;
    // Quadrant edges fall at half the extent; skip pixels whose centre lies within 1.5 pixels.
    let near = |position: u32, extent: u32| (2 * position + 1).abs_diff(extent) < 3;
    for (row, line) in capture
        .pixels()
        .chunks_exact(usize::try_from(width)? * 4)
        .enumerate()
    {
        let y = u32::try_from(row)?;
        if near(y, height) {
            continue;
        }
        for (column, pixel) in line.as_chunks::<4>().0.iter().enumerate() {
            let x = u32::try_from(column)?;
            if near(x, width) {
                continue;
            }
            let quadrant = usize::from(2 * y >= height) * 2 + usize::from(2 * x >= width);
            let (rgb, stored_alpha) = expected[quadrant];
            let alpha_ok = pixel[3] == 255 || (stored_alpha && pixel[3].abs_diff(128) <= 1);
            if !alpha_ok
                || rgb
                    .iter()
                    .zip(pixel)
                    .any(|(expected, actual)| expected.abs_diff(*actual) > 1)
            {
                return Err(format!(
                    "{output:?}: pixel ({x}, {y}) in quadrant {quadrant} is {pixel:?}, expected \
                     {rgb:?} with alpha 255{}",
                    if stored_alpha { " or 128" } else { "" }
                )
                .into());
            }
            if quadrant == 3 {
                seen_alpha = Some(pixel[3]);
            }
            checked += 1;
        }
    }
    let alpha = seen_alpha.ok_or("the bottom-right quadrant had no interior pixels")?;
    println!(
        "frame-capture: {output:?} frame {} {width}x{height}: {checked} pixels match, \
         bottom-right alpha {alpha}",
        capture.index()
    );
    Ok(alpha)
}

struct Pipelines {
    direct: MaterialPipeline,
    hdr: MaterialPipeline,
    composite: PostprocessPipeline,
}

#[derive(Default)]
struct Targets {
    direct: Option<RenderTargets>,
    hdr: Option<PostprocessTargets>,
}

fn render(
    graphics: &mut OpenedGraphics<'_>,
    pipelines: &Pipelines,
    targets: &mut Targets,
    frame: mulciber::Frame<'_>,
    output: Output,
) -> Result<FrameDisposition, Box<dyn Error>> {
    let info = frame.surface_info();
    let colors = scene_colors(output);
    let rectangles: Vec<_> = QUADRANTS.into_iter().zip(colors).collect();
    let (vertices, indices) = geometry(&rectangles);
    let (overlay_vertices, overlay_indices) = geometry(&[(QUADRANTS[3], [1.0; 4])]);
    let record = |pipeline, vertices, indices| MaterialRecord {
        pipeline,
        geometry: GeometrySource::Transient(TransientGeometry {
            vertices,
            indices: MeshIndices::U16(indices),
        }),
        textures: &[],
        shadow_map: None,
        uniform: &[],
        storage: &[],
        instances: &[],
    };
    let disposition = if matches!(output, Output::Direct) {
        if targets.direct.as_ref().is_none_or(|t| t.info() != info) {
            targets.direct = Some(graphics.device.create_render_targets(info)?);
        }
        graphics.queue.render_and_present(
            frame,
            SceneSubmission {
                content: SceneContent::Material(&[record(&pipelines.direct, &vertices, &indices)]),
                output: SceneOutput::Direct(targets.direct.as_ref().expect("created")),
                shadow: None,
                overlay: None,
                clear: ClearColor::BLACK,
            },
        )?
    } else {
        if targets.hdr.as_ref().is_none_or(|t| t.info() != info) {
            targets.hdr = Some(
                graphics
                    .device
                    .create_scaled_hdr_postprocess_targets(info, RenderScale::NATIVE)?,
            );
        }
        let overlay = [record(
            &pipelines.direct,
            &overlay_vertices,
            &overlay_indices,
        )];
        graphics.queue.render_and_present(
            frame,
            SceneSubmission {
                content: SceneContent::Material(&[record(&pipelines.hdr, &vertices, &indices)]),
                output: SceneOutput::Postprocessed {
                    pipeline: &pipelines.composite,
                    targets: targets.hdr.as_ref().expect("created"),
                    uniform: &[],
                },
                shadow: None,
                overlay: matches!(output, Output::HdrOverlay).then_some(&overlay[..]),
                clear: ClearColor::BLACK,
            },
        )?
    };
    Ok(disposition)
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — frame capture validation",
        LogicalSize::new(160, 120),
    ))?;
    let metrics = application.wait_for_first_metrics(&window)?;
    // Four samples, where available, make the direct path resolve into the presentable image;
    // `--force-one-sample` renders straight into it instead.
    let one_sample = std::env::args().any(|argument| argument == "--force-one-sample");
    let mut graphics = OpenedGraphics::open(
        window.surface_target(),
        metrics,
        DeviceRequest {
            preferred_sample_count: if one_sample {
                SampleCount::One
            } else {
                SampleCount::Four
            },
        },
    )?;
    println!("frame-capture: {:?}", graphics.selection);
    let descriptor = || -> Result<MaterialPipelineDescriptor<'static>, Box<dyn Error>> {
        Ok(MaterialPipelineDescriptor {
            shader: ShaderArtifact::new(HUD_SHADER)?,
            vertex_entry: "hud_vertex",
            fragment_entry: "hud_fragment",
            vertex_layout: LAYOUT,
            bindings: &[],
            blend: BlendMode::Opaque,
            depth: DepthMode::Off,
            instance_layout: None,
        })
    };
    let pipelines = Pipelines {
        direct: graphics.device.create_material_pipeline(descriptor()?)?,
        hdr: graphics
            .device
            .create_hdr_material_pipeline(descriptor()?)?,
        composite: graphics.device.create_hdr_composite_pipeline(
            ShaderArtifact::new(COMPOSITE_SHADER)?.into(),
            None,
            None,
        )?,
    };
    let mut targets = Targets::default();
    if graphics.surface.take_frame_capture()?.is_some() {
        return Err("a capture was reported before any request".into());
    }
    let mut step = 0;
    let mut presented = 0_u64;
    let mut captures = 0;
    let mut alphas = Vec::new();
    let started = Instant::now();
    while step < STEPS.len() {
        if started.elapsed().as_secs() > 60 {
            return Err("frame capture validation exceeded 60 seconds".into());
        }
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            let WindowEvent::RedrawRequested(metrics) = event else {
                return Ok(());
            };
            let Some(&current) = STEPS.get(step) else {
                return Ok(());
            };
            if let Step::RequestAndCapture(_) = current {
                graphics.surface.request_frame_capture()?;
            }
            let FrameAcquire::Ready(frame) = graphics.surface.acquire(metrics)? else {
                return Ok(());
            };
            let info = frame.surface_info();
            let output = match current {
                Step::Unrequested | Step::RequestAfterAcquire => Output::Direct,
                Step::Abandon => {
                    frame.abandon()?;
                    if graphics.surface.take_frame_capture()?.is_some() {
                        return Err("an abandoned frame was captured".into());
                    }
                    println!("frame-capture: abandoned frame not captured; request pending");
                    step += 1;
                    return Ok(());
                }
                Step::Capture(output) | Step::RequestAndCapture(output) => output,
            };
            if let Step::RequestAfterAcquire = current {
                graphics.surface.request_frame_capture()?;
            }
            if let Step::Capture(_) = current {
                // Repeating a pending request changes nothing.
                graphics.surface.request_frame_capture()?;
            }
            let disposition = render(&mut graphics, &pipelines, &mut targets, frame, output)?;
            if !matches!(disposition, FrameDisposition::Presented(_)) {
                return Err(format!("{current:?}: frame was not presented").into());
            }
            let index = presented;
            presented += 1;
            let capture = graphics.surface.take_frame_capture()?;
            match (current, capture) {
                (Step::Unrequested, None) => {
                    println!("frame-capture: unrequested frame not captured");
                }
                (Step::RequestAfterAcquire, None) => {
                    println!("frame-capture: frame acquired before the request not captured");
                }
                (Step::Capture(_) | Step::RequestAndCapture(_), Some(capture)) => {
                    if capture.index() != index {
                        return Err(format!(
                            "capture reports frame {} but frame {index} was presented",
                            capture.index()
                        )
                        .into());
                    }
                    alphas.push(check_pixels(&capture, info, output)?);
                    if graphics.surface.take_frame_capture()?.is_some() {
                        return Err("a taken capture was reported again".into());
                    }
                    captures += 1;
                }
                (current, capture) => {
                    return Err(format!(
                        "{current:?}: capture {}",
                        if capture.is_some() {
                            "was reported"
                        } else {
                            "is missing"
                        }
                    )
                    .into());
                }
            }
            step += 1;
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            return Err("window closed before validation completed".into());
        }
    }
    // A request still pending at shutdown holds no native storage.
    graphics.surface.request_frame_capture()?;
    graphics.shutdown()?;
    println!(
        "frame-capture: {captures} captures matched over {presented} presented frames; \
         bottom-right alpha per capture {alphas:?}"
    );
    Ok(())
}
