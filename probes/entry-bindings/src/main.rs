//! Native evidence for per-entry-point resource bindings: one WGSL module holds a plain and a
//! skinned vertex entry point sharing one fragment entry point, and two material pipelines are
//! created from it, the plain one declaring no storage slot and the skinned one declaring the bone
//! palette it reads. Each draw is read back from an HDR target.
use mulciber::{
    BlendMode, ClearColor, DepthMode, DeviceRequest, FrameAcquire, GeometrySource, MaterialBinding,
    MaterialPipelineDescriptor, MaterialRecord, MeshIndices, OpenedGraphics, RenderScale,
    SampleCount, SceneContent, SceneOutput, SceneSubmission, ShaderArtifact, VertexAttribute,
    VertexFormat, VertexLayout,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};
use std::{error::Error, time::Instant};

const TINT: [f32; 4] = [2.0, 0.5, 4.0, 1.0];
const PROP_COLOR: [f32; 4] = [0.25, 0.5, 0.75, 1.0];
/// Bone indices as Source stores them, up to three bones a vertex; the fourth weight is zero.
const BONES: [u8; 4] = [2, 0, 3, 1];
const WEIGHTS: [u8; 4] = [128, 76, 51, 0];
/// Two bone palettes, so the readback shows the record's own storage bytes were read.
const PALETTES: [[[f32; 4]; 4]; 2] = [
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ],
    [
        [0.5, 0.25, 0.0, 1.0],
        [-1.0, -1.0, -1.0, -1.0],
        [0.0, 0.5, 1.0, 0.25],
        [1.0, 1.0, 0.0, 0.5],
    ],
];

const PROP_ATTRIBUTES: [VertexAttribute; 1] = [VertexAttribute {
    location: 0,
    format: VertexFormat::Float32x2,
    offset: 0,
}];
const SKINNED_ATTRIBUTES: [VertexAttribute; 3] = [
    PROP_ATTRIBUTES[0],
    VertexAttribute {
        location: 1,
        format: VertexFormat::Uint8x4,
        offset: 8,
    },
    VertexAttribute {
        location: 2,
        format: VertexFormat::Unorm8x4,
        offset: 12,
    },
];
const UNIFORM: MaterialBinding = MaterialBinding::Uniform {
    binding: 0,
    size: 16,
};
const PALETTE: MaterialBinding = MaterialBinding::Storage {
    binding: 1,
    size: 64,
};

fn bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

fn triangle(skinned: bool) -> Vec<u8> {
    let mut vertices = Vec::new();
    for position in [[-1.0_f32, -1.0], [3.0, -1.0], [-1.0, 3.0]] {
        vertices.extend(bytes(&position));
        if skinned {
            vertices.extend_from_slice(&BONES);
            vertices.extend_from_slice(&WEIGHTS);
        }
    }
    vertices
}

/// The colour each case draws, computed from the inputs as the shader blends them.
fn expected(case: usize) -> (&'static str, [f32; 4]) {
    let color = if case == 0 {
        PROP_COLOR
    } else {
        let palette = PALETTES[case - 1];
        std::array::from_fn(|channel| {
            BONES
                .iter()
                .zip(WEIGHTS)
                .map(|(&bone, weight)| {
                    palette[usize::from(bone)][channel] * f32::from(weight) / 255.0
                })
                .sum()
        })
    };
    let label = [
        "prop_vertex without storage",
        "skinned_vertex palette A",
        "skinned_vertex palette B",
    ][case];
    (
        label,
        std::array::from_fn(|channel| color[channel] * TINT[channel]),
    )
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
    let skinning_artifact = include_bytes!(concat!(env!("OUT_DIR"), "/skinning.shaderbin"));
    if skinning_artifact.is_empty() {
        return Err(
            "the entry-point binding probe's Metal skinning artifact is not generated yet; build \
             it with mulciber-shader on macOS (see docs/per-entry-point-bindings.md)"
                .into(),
        );
    }
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — per-entry-point binding validation",
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
    println!("entry-bindings: {:?}", graphics.selection);
    let shader = ShaderArtifact::new(skinning_artifact)?;
    let prop_layout = VertexLayout {
        stride: 8,
        attributes: &PROP_ATTRIBUTES,
    };
    let skinned_layout = VertexLayout {
        stride: 16,
        attributes: &SKINNED_ATTRIBUTES,
    };
    let descriptor = |vertex_entry, vertex_layout, bindings| MaterialPipelineDescriptor {
        shader,
        vertex_entry,
        fragment_entry: "prop_fragment",
        vertex_layout,
        bindings,
        blend: BlendMode::Opaque,
        depth: DepthMode::Off,
        instance_layout: None,
    };
    // The plain pair never reads the palette, so declaring it is refused, as is leaving it out
    // of the skinned pair that does.
    let refused = graphics
        .device
        .create_hdr_material_pipeline(descriptor("prop_vertex", prop_layout, &[UNIFORM, PALETTE]))
        .err()
        .ok_or("the plain pipeline accepted a storage slot its entry points never use")?;
    println!("entry-bindings: unused storage refused: {refused}");
    let refused = graphics
        .device
        .create_hdr_material_pipeline(descriptor("skinned_vertex", skinned_layout, &[UNIFORM]))
        .err()
        .ok_or("the skinned pipeline was created without the bone palette it reads")?;
    println!("entry-bindings: missing storage refused: {refused}");
    let plain = graphics.device.create_hdr_material_pipeline(descriptor(
        "prop_vertex",
        prop_layout,
        &[UNIFORM],
    ))?;
    let skinned = graphics.device.create_hdr_material_pipeline(descriptor(
        "skinned_vertex",
        skinned_layout,
        &[UNIFORM, PALETTE],
    ))?;
    let prop_mesh = graphics.device.create_mesh_with_layout(
        prop_layout,
        &triangle(false),
        MeshIndices::U16(&[0, 1, 2]),
    )?;
    let skinned_mesh = graphics.device.create_mesh_with_layout(
        skinned_layout,
        &triangle(true),
        MeshIndices::U16(&[0, 1, 2]),
    )?;
    let composite = graphics.device.create_hdr_composite_pipeline(
        ShaderArtifact::new(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/composite.shaderbin"
        )))?
        .into(),
        None,
        None,
    )?;
    let uniform = bytes(&TINT);
    let palettes = PALETTES.map(|palette| bytes(palette.as_flattened()));
    let cases = 3;
    let mut targets = None;
    let mut completed = 0;
    let started = Instant::now();
    while completed < cases {
        if started.elapsed().as_secs() > 60 {
            return Err("entry-point binding validation exceeded 60 seconds".into());
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
            let record = if completed == 0 {
                MaterialRecord {
                    pipeline: &plain,
                    geometry: GeometrySource::Mesh(&prop_mesh),
                    textures: &[],
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &[],
                    instances: &[],
                }
            } else {
                MaterialRecord {
                    pipeline: &skinned,
                    geometry: GeometrySource::Mesh(&skinned_mesh),
                    textures: &[],
                    shadow_map: None,
                    uniform: &uniform,
                    storage: &palettes[completed - 1],
                    instances: &[],
                }
            };
            graphics.queue.render_and_present(
                frame,
                SceneSubmission {
                    content: SceneContent::Material(&[record]),
                    output: SceneOutput::Postprocessed {
                        pipeline: &composite,
                        targets: target,
                        uniform: &[],
                    },
                    shadow: None,
                    overlay: None,
                    clear: ClearColor::BLACK,
                },
            )?;
            let actual = mulciber::integration::read_hdr_validation_pixel(&graphics.queue, target)?;
            let (what, values) = expected(completed);
            let reference = values.map(reference_half);
            for channel in 0..4 {
                // Normalized weights may round by an ULP in the fetch and another in the half
                // render target; the palette and tint are exact.
                if actual[channel].abs_diff(reference[channel]) > 2 {
                    return Err(format!(
                        "{what} channel {channel}: actual={actual:04x?} expected={reference:04x?}"
                    )
                    .into());
                }
            }
            println!("entry-bindings: {what} actual={actual:04x?} ok");
            completed += 1;
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            return Err("window closed before validation completed".into());
        }
    }
    graphics.device.destroy_mesh(prop_mesh)?;
    graphics.device.destroy_mesh(skinned_mesh)?;
    graphics.shutdown()?;
    println!(
        "entry-bindings: {completed} draws from one module matched (2 half ULP tolerance); \
         both refusals fired"
    );
    Ok(())
}
