//! Native numerical evidence for `RGBA16Float` uploads through public material bindings.
use mulciber::{
    BlendMode, ClearColor, DepthMode, DeviceRequest, FrameAcquire, GeometrySource, MaterialBinding,
    MaterialPipelineDescriptor, MaterialRecord, MeshIndices, OpenedGraphics, RenderScale,
    SampleCount, SamplerAddress, SamplerFilter, SceneContent, SceneOutput, SceneSubmission,
    ShaderArtifact, VertexAttribute, VertexFormat, VertexLayout,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};
use std::{error::Error, time::Instant};

const BASE: [[f32; 4]; 4] = [
    [0.0, -2.0, 1e-5, 1e-3],
    [4.0, 8.0, 1e-4, -1e-3],
    [-8.0, 16.0, 3e-5, 3e-4],
    [2.0, -4.0, -1e-5, 0.0],
];
const MIP: [[f32; 4]; 1] = [[10.0, -12.0, 7e-5, 7e-4]];
const QUERIES: [[f32; 3]; 10] = [
    [0.25, 0.25, 0.0],
    [0.75, 0.25, 0.0],
    [0.25, 0.75, 0.0],
    [0.75, 0.75, 0.0],
    [0.5, 0.25, 0.0],
    [0.5, 0.5, 0.0],
    [0.5, 0.5, 1.0],
    [0.5, 0.5, 0.5],
    [0.375, 0.625, 0.25],
    [0.25, 0.25, 8.0],
];
const LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 8,
    attributes: &[VertexAttribute {
        location: 0,
        offset: 0,
        format: VertexFormat::Float32x2,
    }],
};
const BINDINGS: [MaterialBinding; 3] = [
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

// Independent arithmetic oracle: enumerate representable values instead of reusing upload packing.
fn decode(bits: u16) -> f32 {
    let exponent = (bits >> 10) & 31;
    let mantissa = bits & 1023;
    let value = if exponent == 0 {
        f32::from(mantissa) * 2.0_f32.powi(-24)
    } else {
        (1.0 + f32::from(mantissa) / 1024.0) * 2.0_f32.powi(i32::from(exponent) - 15)
    };
    if bits & 0x8000 == 0 { value } else { -value }
}
#[allow(clippy::float_cmp)] // Exact equality detects representable midpoint ties.
fn reference_half(value: f32) -> u16 {
    let magnitude = value.abs();
    let mut best = 0;
    let mut error = f32::INFINITY;
    for bits in 0_u16..=0x7bff {
        let distance = (decode(bits) - magnitude).abs();
        if distance < error || (distance == error && bits & 1 == 0) {
            best = bits;
            error = distance;
        }
    }
    best | if value.is_sign_negative() { 0x8000 } else { 0 }
}
fn expected(query: [f32; 3], mips: bool) -> [u16; 4] {
    let x = (query[0] * 2.0 - 0.5).clamp(0.0, 1.0);
    let y = (query[1] * 2.0 - 0.5).clamp(0.0, 1.0);
    let lod = if mips { query[2].clamp(0.0, 1.0) } else { 0.0 };
    std::array::from_fn(|channel| {
        let base = BASE.map(|texel| decode(reference_half(texel[channel])));
        let top = base[0] * (1.0 - x) + base[1] * x;
        let bottom = base[2] * (1.0 - x) + base[3] * x;
        let interpolated = (top * (1.0 - y) + bottom * y) * (1.0 - lod)
            + decode(reference_half(MIP[0][channel])) * lod;
        reference_half(interpolated * if channel >= 2 { 1024.0 } else { 1.0 })
    })
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — float texture numerical validation",
        LogicalSize::new(128, 128),
    ))?;
    let metrics = application.wait_for_first_metrics(&window)?;
    let mut graphics = OpenedGraphics::open(
        window.surface_target(),
        metrics,
        DeviceRequest {
            preferred_sample_count: SampleCount::One,
        },
    )?;
    println!("float-texture: {:?}", graphics.selection);
    let textures = [
        graphics.device.create_rgba16_float_texture(2, 2, &BASE)?,
        graphics
            .device
            .create_rgba16_float_texture_with_mips(2, 2, &[&BASE, &MIP])?,
    ];
    let vertices: Vec<u8> = [-1.0_f32, -1.0, 3.0, -1.0, -1.0, 3.0]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect();
    let mesh = graphics.device.create_mesh_with_layout(
        LAYOUT,
        &vertices,
        MeshIndices::U16(&[0, 1, 2, 0, 2, 1]),
    )?;
    let pipeline = graphics
        .device
        .create_hdr_material_pipeline(MaterialPipelineDescriptor {
            shader: ShaderArtifact::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/sample.shaderbin"
            )))?,
            vertex_entry: "sample_vertex",
            fragment_entry: "sample_fragment",
            vertex_layout: LAYOUT,
            bindings: &BINDINGS,
            blend: BlendMode::Opaque,
            depth: DepthMode::Off,
            instance_layout: None,
        })?;
    let composite = graphics.device.create_hdr_composite_pipeline(
        ShaderArtifact::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/composite.shaderbin"
        )))?
        .into(),
        None,
        None,
    )?;
    let mut targets = None;
    let mut completed = 0;
    let started = Instant::now();
    while completed < QUERIES.len() * 4 {
        if started.elapsed().as_secs() > 60 {
            return Err("float texture validation exceeded 60 seconds".into());
        }
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            if completed >= QUERIES.len() * 4 { return Ok(()); }
            let WindowEvent::RedrawRequested(metrics) = event else { return Ok(()); };
            let FrameAcquire::Ready(frame) = graphics.surface.acquire(metrics)? else { return Ok(()); };
            let info = frame.surface_info();
            if targets.as_ref().is_none_or(|target: &mulciber::PostprocessTargets| target.info() != info) {
                targets = Some(graphics.device.create_scaled_hdr_postprocess_targets(info, RenderScale::NATIVE)?);
            }
            let target = targets.as_ref().unwrap();
            let mips = completed / (QUERIES.len() * 2) != 0;
            let fragment = !(completed / QUERIES.len()).is_multiple_of(2);
            let query = QUERIES[completed % QUERIES.len()];
            let uniform: Vec<u8> = [query[0], query[1], query[2], f32::from(fragment)].iter().flat_map(|v| v.to_ne_bytes()).collect();
            graphics.queue.render_and_present(frame, SceneSubmission {
                content: SceneContent::Material(&[MaterialRecord {
                    pipeline: &pipeline, geometry: GeometrySource::Mesh(&mesh),
                    textures: &[&textures[usize::from(mips)]], shadow_map: None, uniform: &uniform,
                    storage: &[], instances: &[],
                }]),
                output: SceneOutput::Postprocessed { pipeline: &composite, targets: target, uniform: &[] },
                shadow: None, overlay: None, clear: ClearColor::BLACK,
            })?;
            let actual = mulciber::integration::read_hdr_validation_pixel(&graphics.queue, target)?;
            let reference = expected(query, mips);
            for channel in 0..4 {
                // Permit two half ULPs for native filtering and final half render-target rounding.
                // Small channels were amplified exactly, so a flushed coefficient cannot pass.
                let same_zero = matches!(actual[channel], 0 | 0x8000) && matches!(reference[channel], 0 | 0x8000);
                if !same_zero && (actual[channel] & 0x8000 != reference[channel] & 0x8000
                    || actual[channel].abs_diff(reference[channel]) > 2) {
                    return Err(format!("mips={mips} fragment={fragment} query={query:?} channel={channel}: actual={actual:04x?} expected={reference:04x?}").into());
                }
            }
            println!("float-texture: mips={mips} fragment={fragment} query={query:?} actual={actual:04x?} expected={reference:04x?} ok");
            completed += 1;
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            return Err("window closed before validation completed".into());
        }
    }
    for texture in textures {
        graphics.device.destroy_texture(texture)?;
    }
    graphics.shutdown()?;
    println!("float-texture: {completed} numerical cases passed (2 half ULP tolerance)");
    Ok(())
}
