//! Native evidence for offscreen passes into render textures through the public API: misuse
//! rejected by name, then a four-quadrant pattern rendered into a render texture with its own
//! depth (a later full-cover record at equal depth must lose the test), sampled by the scene in
//! the same frame, sampled again frames later without re-rendering, and re-rendered in a new
//! pattern while earlier frames may still be sampling it, each checked in a captured frame.
//! Pass `--force-one-sample` to render without the multisample resolve.
use mulciber::{
    BlendMode, ClearColor, DepthMode, DeviceRequest, FrameAcquire, FrameCapture, FrameDisposition,
    GeometrySource, MaterialBinding, MaterialPipeline, MaterialPipelineDescriptor, MaterialRecord,
    MeshIndices, OffscreenPass, OpenedGraphics, PostprocessPipeline, PostprocessTargets,
    RenderScale, RenderTexture, SampleCount, SamplerAddress, SamplerFilter, SceneContent,
    SceneOutput, SceneSubmission, ShaderArtifact, SurfaceInfo, TransientGeometry, VertexAttribute,
    VertexFormat, VertexLayout,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};
use std::{error::Error, time::Instant};

const HUD_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/hud.shaderbin"));
const SAMPLE_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sample.shaderbin"));
const COMPOSITE_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/composite.shaderbin"));
/// The HUD vertex: clip-space position and linear RGBA.
const HUD_LAYOUT: VertexLayout<'static> = VertexLayout {
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
/// The sampler's vertex: clip-space position alone.
const SAMPLE_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 8,
    attributes: &[VertexAttribute {
        location: 0,
        format: VertexFormat::Float32x2,
        offset: 0,
    }],
};
/// Uniform (texture coordinate, LOD, stage selector), the texture, and a sampler.
const SAMPLE_BINDINGS: [MaterialBinding; 3] = [
    MaterialBinding::Uniform {
        binding: 0,
        size: 16,
    },
    MaterialBinding::Texture { binding: 1 },
    MaterialBinding::Sampler {
        binding: 2,
        filter: SamplerFilter::Linear,
        address: SamplerAddress::ClampToEdge,
    },
];
/// The linear value whose sRGB encoding is one half.
const HALF_ENCODED: f32 = 0.214_041_14;
/// Clip-space rectangles in top-left, top-right, bottom-left, bottom-right order, y up.
const QUADRANTS: [[f32; 4]; 4] = [
    [-1.0, 0.0, 0.0, 1.0],
    [0.0, 0.0, 1.0, 1.0],
    [-1.0, -1.0, 0.0, 0.0],
    [0.0, -1.0, 1.0, 0.0],
];
/// The centre of each quadrant in texture coordinates: clip-space y up renders into rows from
/// the top, so the top-left quadrant is the texture's top-left.
const CENTRES: [[f32; 2]; 4] = [[0.25, 0.25], [0.75, 0.25], [0.25, 0.75], [0.75, 0.75]];
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
const ORANGE: [f32; 4] = [1.0, HALF_ENCODED, 0.0, 1.0];
/// The render texture's quadrants, first pattern and the pattern it is re-rendered in.
const FIRST: [[f32; 4]; 4] = [RED, GREEN, BLUE, ORANGE];
const SECOND: [[f32; 4]; 4] = [GREEN, RED, ORANGE, BLUE];

/// What one redraw does, in order.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// Sample the render texture before anything has rendered it.
    SampleUnrendered,
    /// An offscreen record samples the texture its own pass renders into.
    SampleOwnTarget,
    /// Two offscreen passes target the same texture.
    TargetTwice,
    /// An offscreen record uses a surface-format pipeline.
    SurfaceFormatPipeline,
    /// Render the pattern and sample it in the same frame; capture.
    Render([[f32; 4]; 4]),
    /// Sample what an earlier frame rendered; capture.
    Reuse([[f32; 4]; 4]),
    /// Sample without capturing, keeping frames in flight that read the texture.
    Busy,
}

const STEPS: [Step; 11] = [
    Step::SampleUnrendered,
    Step::SampleOwnTarget,
    Step::TargetTwice,
    Step::SurfaceFormatPipeline,
    Step::Render(FIRST),
    Step::Busy,
    Step::Busy,
    Step::Reuse(FIRST),
    Step::Render(SECOND),
    Step::Busy,
    Step::Reuse(SECOND),
];

impl Step {
    /// The expected rejection's wording, for steps that must be refused.
    const fn refusal(self) -> Option<&'static str> {
        match self {
            Self::SampleUnrendered => Some("no offscreen pass has rendered"),
            Self::SampleOwnTarget => Some("samples the render texture its pass renders into"),
            Self::TargetTwice => Some("target the same render texture"),
            Self::SurfaceFormatPipeline => Some("HDR formats disagree"),
            _ => None,
        }
    }

    const fn captured(self) -> Option<[[f32; 4]; 4]> {
        match self {
            Self::Render(pattern) | Self::Reuse(pattern) => Some(pattern),
            _ => None,
        }
    }
}

/// Two triangles per rectangle in both windings, so culling cannot drop any, laid out per
/// vertex as clip-space position followed by `extra`.
fn geometry(rectangles: &[([f32; 4], &[f32])]) -> (Vec<u8>, Vec<u16>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (index, ([left, bottom, right, top], extra)) in rectangles.iter().enumerate() {
        let base = u16::try_from(index * 4).expect("few rectangles");
        for [x, y] in [[left, bottom], [right, bottom], [right, top], [left, top]] {
            for value in [*x, *y].iter().chain(extra.iter()) {
                vertices.extend_from_slice(&value.to_ne_bytes());
            }
        }
        for corner in [0, 1, 2, 0, 2, 3, 0, 2, 1, 0, 3, 2] {
            indices.push(base + corner);
        }
    }
    (vertices, indices)
}

/// Expected RGB bytes per displayed quadrant for a render texture holding `pattern`.
///
/// The sampler amplifies blue by 1024 and the composite clamps to one, so blue reads back full
/// and the others keep their channels; the composite also presents the scene upside down, so
/// displayed top rows come from the scene's bottom quadrants, each of which sampled the same
/// quadrant of the texture.
fn expected(pattern: [[f32; 4]; 4]) -> [[u8; 3]; 4] {
    let bytes = |color: [f32; 4]| -> [u8; 3] {
        let encode = |linear: f32| -> u8 {
            let encoded = if linear <= 0.003_130_8 {
                linear * 12.92
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            };
            // Clamped to 0..=255 before the cast.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let byte = (encoded.clamp(0.0, 1.0) * 255.0).round() as u8;
            byte
        };
        [
            encode(color[0]),
            encode(color[1]),
            encode(color[2].min(1.0) * 1024.0),
        ]
    };
    [
        bytes(pattern[2]),
        bytes(pattern[3]),
        bytes(pattern[0]),
        bytes(pattern[1]),
    ]
}

/// Compares every pixel away from the quadrant edges with the quadrant's expected bytes,
/// allowing one step of sRGB encoding rounding, and returns how many were checked.
fn check_pixels(
    capture: &FrameCapture,
    info: SurfaceInfo,
    pattern: [[f32; 4]; 4],
) -> Result<u32, Box<dyn Error>> {
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
    let expected = expected(pattern);
    let mut checked = 0_u32;
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
            let rgb = expected[quadrant];
            if rgb
                .iter()
                .zip(pixel)
                .any(|(expected, actual)| expected.abs_diff(*actual) > 1)
            {
                return Err(format!(
                    "pixel ({x}, {y}) in quadrant {quadrant} is {pixel:?}, expected {rgb:?}"
                )
                .into());
            }
            checked += 1;
        }
    }
    Ok(checked)
}

struct Pipelines {
    /// Depth-tested HDR vertex colors, for the offscreen passes.
    hud: MaterialPipeline,
    /// The same at the surface format, which offscreen passes must refuse.
    surface_hud: MaterialPipeline,
    /// HDR texel reads, for the scene.
    sample: MaterialPipeline,
    composite: PostprocessPipeline,
}

/// Transient bytes for one submission, kept alive while its records borrow them.
struct Supplies {
    pattern: (Vec<u8>, Vec<u16>),
    cover: (Vec<u8>, Vec<u16>),
    quadrants: [(Vec<u8>, Vec<u16>); 4],
    requests: [Vec<u8>; 4],
}

impl Supplies {
    fn new(pattern: [[f32; 4]; 4]) -> Self {
        let rectangles: Vec<([f32; 4], &[f32])> = QUADRANTS
            .iter()
            .zip(&pattern)
            .map(|(&rectangle, color)| (rectangle, &color[..]))
            .collect();
        Self {
            pattern: geometry(&rectangles),
            cover: geometry(&[([-1.0, -1.0, 1.0, 1.0], &[1.0; 4])]),
            quadrants: QUADRANTS.map(|rectangle| geometry(&[(rectangle, &[])])),
            requests: CENTRES.map(|[u, v]| {
                [u, v, 0.0, 1.0]
                    .iter()
                    .flat_map(|value| value.to_ne_bytes())
                    .collect()
            }),
        }
    }
}

fn record<'a>(
    pipeline: &'a MaterialPipeline,
    (vertices, indices): &'a (Vec<u8>, Vec<u16>),
    textures: &'a [&'a mulciber::Texture],
    uniform: &'a [u8],
) -> MaterialRecord<'a> {
    MaterialRecord {
        pipeline,
        geometry: GeometrySource::Transient(TransientGeometry {
            vertices,
            indices: MeshIndices::U16(indices),
        }),
        textures,
        shadow_map: None,
        uniform,
        storage: &[],
        instances: &[],
    }
}

fn render(
    graphics: &mut OpenedGraphics<'_>,
    pipelines: &Pipelines,
    targets: &mut Option<PostprocessTargets>,
    render_texture: &RenderTexture,
    frame: mulciber::Frame<'_>,
    step: Step,
) -> Result<FrameDisposition, mulciber::GraphicsError> {
    let info = frame.surface_info();
    if targets.as_ref().is_none_or(|t| t.info() != info) {
        *targets = Some(
            graphics
                .device
                .create_scaled_hdr_postprocess_targets(info, RenderScale::NATIVE)?,
        );
    }
    let pattern = match step {
        Step::Render(pattern) => pattern,
        _ => SECOND,
    };
    let supplies = Supplies::new(pattern);
    let sampled = [render_texture.texture()];
    let scene: Vec<MaterialRecord<'_>> = supplies
        .quadrants
        .iter()
        .zip(&supplies.requests)
        .map(|(quadrant, request)| record(&pipelines.sample, quadrant, &sampled, request))
        .collect();
    let hud = match step {
        Step::SurfaceFormatPipeline => &pipelines.surface_hud,
        _ => &pipelines.hud,
    };
    // The pattern first, then a white cover at the same depth that the depth test must reject.
    let drawn = [
        record(hud, &supplies.pattern, &[], &[]),
        record(hud, &supplies.cover, &[], &[]),
    ];
    let own = [
        drawn[0],
        record(
            &pipelines.sample,
            &supplies.quadrants[0],
            &sampled,
            &supplies.requests[0],
        ),
    ];
    let pass = |records| OffscreenPass {
        target: render_texture,
        records,
        clear: ClearColor::BLACK,
    };
    let passes: Vec<OffscreenPass<'_>> = match step {
        Step::SampleUnrendered | Step::Reuse(_) | Step::Busy => Vec::new(),
        Step::SampleOwnTarget => vec![pass(&own[..])],
        Step::TargetTwice => vec![pass(&drawn[..]), pass(&drawn[..])],
        Step::SurfaceFormatPipeline | Step::Render(_) => vec![pass(&drawn[..])],
    };
    graphics.queue.render_and_present(
        frame,
        SceneSubmission {
            content: SceneContent::Material(&scene),
            output: SceneOutput::Postprocessed {
                pipeline: &pipelines.composite,
                targets: targets.as_ref().expect("created"),
                uniform: &[],
            },
            shadow: None,
            overlay: None,
            offscreen: &passes,
            clear: ClearColor::BLACK,
        },
    )
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — render texture validation",
        LogicalSize::new(160, 120),
    ))?;
    let metrics = application.wait_for_first_metrics(&window)?;
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
    println!("render-texture: {:?}", graphics.selection);
    let hud = || -> Result<MaterialPipelineDescriptor<'static>, Box<dyn Error>> {
        Ok(MaterialPipelineDescriptor {
            shader: ShaderArtifact::new(HUD_SHADER)?,
            vertex_entry: "hud_vertex",
            fragment_entry: "hud_fragment",
            vertex_layout: HUD_LAYOUT,
            bindings: &[],
            blend: BlendMode::Opaque,
            depth: DepthMode::TestWrite,
            instance_layout: None,
        })
    };
    let pipelines = Pipelines {
        hud: graphics.device.create_hdr_material_pipeline(hud()?)?,
        surface_hud: graphics.device.create_material_pipeline(hud()?)?,
        sample: graphics
            .device
            .create_hdr_material_pipeline(MaterialPipelineDescriptor {
                shader: ShaderArtifact::new(SAMPLE_SHADER)?,
                vertex_entry: "sample_vertex",
                fragment_entry: "sample_fragment",
                vertex_layout: SAMPLE_LAYOUT,
                bindings: &SAMPLE_BINDINGS,
                blend: BlendMode::Opaque,
                depth: DepthMode::Off,
                instance_layout: None,
            })?,
        composite: graphics.device.create_hdr_composite_pipeline(
            ShaderArtifact::new(COMPOSITE_SHADER)?.into(),
            None,
            None,
        )?,
    };
    // An odd extent, so a texture-coordinate slip shows as a wrong quadrant.
    let render_texture = graphics.device.create_hdr_render_texture(48, 40)?;
    for (width, height) in [(0, 8), (8, mulciber::RENDER_TEXTURE_SIZE_LIMIT + 1)] {
        if graphics
            .device
            .create_hdr_render_texture(width, height)
            .is_ok()
        {
            return Err(format!("a {width}x{height} render texture was created").into());
        }
    }
    let refused = graphics.device.update_rgba16_float_texture(
        render_texture.texture(),
        48,
        40,
        &vec![[0.0; 4]; 48 * 40],
    );
    match refused {
        Err(error) if error.to_string().contains("offscreen passes") => {
            println!("render-texture: CPU update refused: {error}");
        }
        other => return Err(format!("CPU update of a render texture: {other:?}").into()),
    }
    let mut targets = None;
    let mut step = 0;
    let mut captures = 0;
    let started = Instant::now();
    while step < STEPS.len() {
        if started.elapsed().as_secs() > 60 {
            return Err("render texture validation exceeded 60 seconds".into());
        }
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            let WindowEvent::RedrawRequested(metrics) = event else {
                return Ok(());
            };
            let Some(&current) = STEPS.get(step) else {
                return Ok(());
            };
            if current.captured().is_some() {
                graphics.surface.request_frame_capture()?;
            }
            let FrameAcquire::Ready(frame) = graphics.surface.acquire(metrics)? else {
                return Ok(());
            };
            let info = frame.surface_info();
            let result = render(
                &mut graphics,
                &pipelines,
                &mut targets,
                &render_texture,
                frame,
                current,
            );
            match (current.refusal(), result) {
                (Some(wording), Err(error)) if error.to_string().contains(wording) => {
                    println!("render-texture: {current:?} refused: {error}");
                }
                (Some(_), outcome) => {
                    return Err(format!("{current:?}: expected a refusal, got {outcome:?}").into());
                }
                (None, Ok(FrameDisposition::Presented(_))) => {}
                (None, outcome) => {
                    return Err(format!("{current:?}: frame was not presented: {outcome:?}").into());
                }
            }
            if let Some(pattern) = current.captured() {
                let capture = graphics
                    .surface
                    .take_frame_capture()?
                    .ok_or_else(|| format!("{current:?}: capture is missing"))?;
                let checked = check_pixels(&capture, info, pattern)
                    .map_err(|error| format!("{current:?}: {error}"))?;
                println!("render-texture: {current:?}: {checked} pixels match");
                captures += 1;
            }
            step += 1;
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            return Err("window closed before validation completed".into());
        }
    }
    graphics.device.destroy_render_texture(render_texture)?;
    graphics.shutdown()?;
    println!("render-texture: {captures} captures matched");
    Ok(())
}
