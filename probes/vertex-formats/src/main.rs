//! Native evidence for packed vertex attribute formats: 8- and 16-bit unsigned integer and
//! normalized attributes fetched from a mesh, forwarded by the vertex stage, and read back from an
//! HDR target.
use mulciber::{
    BlendMode, ClearColor, DepthMode, DeviceRequest, FrameAcquire, GeometrySource, MaterialBinding,
    MaterialPipelineDescriptor, MaterialRecord, MeshIndices, OpenedGraphics, RenderScale,
    SampleCount, SceneContent, SceneOutput, SceneSubmission, ShaderArtifact, VertexAttribute,
    VertexFormat, VertexLayout,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};
use std::{error::Error, time::Instant};

const BONES: [u8; 4] = [3, 17, 200, 255];
const WEIGHTS: [u8; 4] = [0, 51, 204, 255];
const PAIR: [u16; 2] = [1000, 2047];
const QUAD: [u16; 4] = [1, 515, 2048, 4096];
const UNIT_PAIR: [u16; 2] = [16384, 65535];
const UNIT_QUAD: [u16; 4] = [0, 32768, 49152, 65535];

const fn attribute(location: u32, offset: u32, format: VertexFormat) -> VertexAttribute {
    VertexAttribute {
        location,
        format,
        offset,
    }
}

const ATTRIBUTES: [VertexAttribute; 7] = [
    attribute(0, 0, VertexFormat::Float32x2),
    attribute(1, 8, VertexFormat::Uint8x4),
    attribute(2, 12, VertexFormat::Unorm8x4),
    attribute(3, 16, VertexFormat::Uint16x2),
    attribute(4, 20, VertexFormat::Uint16x4),
    attribute(5, 28, VertexFormat::Unorm16x2),
    attribute(6, 32, VertexFormat::Unorm16x4),
];
const LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 40,
    attributes: &ATTRIBUTES,
};
const BINDINGS: [MaterialBinding; 1] = [MaterialBinding::Uniform {
    binding: 0,
    size: 16,
}];

fn vertices() -> Vec<u8> {
    let mut bytes = Vec::new();
    for position in [[-1.0_f32, -1.0], [3.0, -1.0], [-1.0, 3.0]] {
        bytes.extend(position.iter().flat_map(|v| v.to_ne_bytes()));
        bytes.extend_from_slice(&BONES);
        bytes.extend_from_slice(&WEIGHTS);
        for values in [&PAIR[..], &QUAD, &UNIT_PAIR, &UNIT_QUAD] {
            bytes.extend(values.iter().flat_map(|v| v.to_ne_bytes()));
        }
    }
    assert_eq!(bytes.len(), 3 * 40);
    bytes
}

/// The value each selector forwards, widened as WGSL reads the attribute.
fn expected(selector: usize) -> (&'static str, [f32; 4]) {
    let widen = |values: &[f32]| std::array::from_fn(|i| values.get(i).copied().unwrap_or(0.0));
    let unit8 = |value: u8| f32::from(value) / 255.0;
    let unit16 = |value: u16| f32::from(value) / 65535.0;
    match selector {
        0 => ("Uint8x4 as vec4<u32>", BONES.map(f32::from)),
        1 => ("Unorm8x4 as vec4<f32>", WEIGHTS.map(unit8)),
        2 => ("Uint16x2 as vec2<u32>", widen(&PAIR.map(f32::from))),
        3 => ("Uint16x4 as vec4<u32>", QUAD.map(f32::from)),
        4 => ("Unorm16x2 as vec2<f32>", widen(&UNIT_PAIR.map(unit16))),
        _ => ("Unorm16x4 as vec4<f32>", UNIT_QUAD.map(unit16)),
    }
}

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

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let fetch_artifact = include_bytes!(concat!(env!("OUT_DIR"), "/fetch.shaderbin"));
    if fetch_artifact.is_empty() {
        return Err(
            "the vertex format probe's Metal fetch artifact is not generated yet; build \
                    it with mulciber-shader on macOS (see docs/vertex-formats.md)"
                .into(),
        );
    }
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — packed vertex format validation",
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
    println!("vertex-formats: {:?}", graphics.selection);
    let vertices = vertices();
    let mesh =
        graphics
            .device
            .create_mesh_with_layout(LAYOUT, &vertices, MeshIndices::U16(&[0, 1, 2]))?;
    let shader = ShaderArtifact::new(fetch_artifact)?;
    let descriptor = |vertex_layout| MaterialPipelineDescriptor {
        shader,
        vertex_entry: "fetch_vertex",
        fragment_entry: "fetch_fragment",
        vertex_layout,
        bindings: &BINDINGS,
        blend: BlendMode::Opaque,
        depth: DepthMode::Off,
        instance_layout: None,
    };
    // Bone indices declared as normalized bytes do not satisfy the recorded vec4<u32>.
    let mut as_unorm = ATTRIBUTES;
    as_unorm[1].format = VertexFormat::Unorm8x4;
    let refused = graphics
        .device
        .create_hdr_material_pipeline(descriptor(VertexLayout {
            stride: 40,
            attributes: &as_unorm,
        }))
        .err()
        .ok_or("Unorm8x4 was accepted for a vec4<u32> input")?;
    println!("vertex-formats: mismatched format refused: {refused}");
    let mut unaligned = ATTRIBUTES;
    unaligned[2].offset = 13;
    let refused = graphics
        .device
        .create_mesh_with_layout(
            VertexLayout {
                stride: 40,
                attributes: &unaligned,
            },
            &vertices,
            MeshIndices::U16(&[0, 1, 2]),
        )
        .err()
        .ok_or("an attribute at offset 13 was accepted")?;
    println!("vertex-formats: unaligned offset refused: {refused}");
    let pipeline = graphics
        .device
        .create_hdr_material_pipeline(descriptor(LAYOUT))?;
    let composite = graphics.device.create_hdr_composite_pipeline(
        ShaderArtifact::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/composite.shaderbin"
        )))?
        .into(),
        None,
        None,
    )?;
    let cases = 6;
    let mut targets = None;
    let mut completed = 0;
    let started = Instant::now();
    while completed < cases {
        if started.elapsed().as_secs() > 60 {
            return Err("vertex format validation exceeded 60 seconds".into());
        }
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            if completed >= cases {
                return Ok(());
            }
            let WindowEvent::RedrawRequested(metrics) = event else {
                return Ok(());
            };
            let FrameAcquire::Ready(frame) = graphics.surface.acquire(metrics)? else {
                return Ok(());
            };
            let info = frame.surface_info();
            if targets
                .as_ref()
                .is_none_or(|target: &mulciber::PostprocessTargets| target.info() != info)
            {
                targets = Some(
                    graphics
                        .device
                        .create_scaled_hdr_postprocess_targets(info, RenderScale::NATIVE)?,
                );
            }
            let target = targets.as_ref().expect("targets were just created");
            let selector = f32::from(u8::try_from(completed).expect("fits u8"));
            let uniform: Vec<u8> = [selector, 0.0, 0.0, 0.0]
                .iter()
                .flat_map(|v| v.to_ne_bytes())
                .collect();
            graphics.queue.render_and_present(
                frame,
                SceneSubmission {
                    content: SceneContent::Material(&[MaterialRecord {
                        pipeline: &pipeline,
                        geometry: GeometrySource::Mesh(&mesh),
                        textures: &[],
                        shadow_map: None,
                        uniform: &uniform,
                        storage: &[],
                        instances: &[],
                    }]),
                    output: SceneOutput::Postprocessed {
                        pipeline: &composite,
                        targets: target,
                        uniform: &[],
                    },
                    shadow: None,
                    overlay: None,
                    offscreen: &[],
                    clear: ClearColor::BLACK,
                },
            )?;
            let actual = mulciber::integration::read_hdr_validation_pixel(&graphics.queue, target)?;
            let (what, values) = expected(completed);
            let reference = values.map(reference_half);
            for channel in 0..4 {
                // Integers up to 4096 are exact in half; normalized values may round by an ULP
                // in the fetch and another in the half render target.
                if actual[channel].abs_diff(reference[channel]) > 2 {
                    return Err(format!(
                        "{what} channel {channel}: actual={actual:04x?} expected={reference:04x?}"
                    )
                    .into());
                }
            }
            println!("vertex-formats: {what} actual={actual:04x?} ok");
            completed += 1;
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            return Err("window closed before validation completed".into());
        }
    }
    graphics.device.destroy_mesh(mesh)?;
    graphics.shutdown()?;
    println!("vertex-formats: {completed} packed formats fetched exactly (2 half ULP tolerance)");
    Ok(())
}
