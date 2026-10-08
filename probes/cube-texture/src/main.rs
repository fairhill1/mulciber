//! Native evidence for cube texture uploads through public material bindings: face order,
//! per-face orientation and per-face mip chains in RGBA8, BC1 and `RGBA16Float`, sampled
//! through `MaterialBinding::CubeTexture` and read back from an HDR target.
use mulciber::{
    BlendMode, BlockCompression, ClearColor, DepthMode, DeviceRequest, FrameAcquire,
    GeometrySource, MaterialBinding, MaterialPipelineDescriptor, MaterialRecord, MeshIndices,
    OpenedGraphics, RenderScale, SampleCount, SamplerAddress, SamplerFilter, SceneContent,
    SceneOutput, SceneSubmission, ShaderArtifact, Texture, TextureDimension, VertexAttribute,
    VertexFormat, VertexLayout,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};
use std::{error::Error, time::Instant};

const FACES: [&str; 6] = ["+X", "-X", "+Y", "-Y", "+Z", "-Z"];
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
    MaterialBinding::CubeTexture { binding: 1 },
    MaterialBinding::Sampler {
        binding: 2,
        filter: SamplerFilter::Nearest,
        address: SamplerAddress::ClampToEdge,
    },
];

/// One sampled direction and LOD on one uploaded cube, with the RGBA it must read.
struct Case {
    texture: usize,
    what: String,
    direction: [f32; 3],
    lod: f32,
    expected: [f32; 4],
}

/// The direction through the point (s, t) of a face, inverting the cube map face selection
/// table Vulkan, Metal and Direct3D share. (s, t) = (0, 0) is the first texel of the face's
/// first row as uploaded.
fn direction(face: usize, s: f32, t: f32) -> [f32; 3] {
    let (sc, tc) = (2.0 * s - 1.0, 2.0 * t - 1.0);
    match face {
        0 => [1.0, -tc, -sc],
        1 => [-1.0, -tc, sc],
        2 => [sc, 1.0, tc],
        3 => [sc, -1.0, -tc],
        4 => [sc, -tc, 1.0],
        _ => [-sc, -tc, -1.0],
    }
}

/// Seven colours whose channels are all zero or one, exact in sRGB, UNORM and BC1's 5:6:5.
/// Black is left out so an unwritten level cannot pass as one.
fn palette(index: usize) -> [u8; 3] {
    let index = index + 1;
    [index & 1, (index >> 1) & 1, (index >> 2) & 1].map(|bit| if bit == 1 { 255 } else { 0 })
}

/// A distinct colour for every face and level: no two faces share a level's colour and no
/// face repeats a colour across its four levels, so a wrong face, a wrong level or a missing
/// upload shows.
fn level_colour(face: usize, level: usize) -> [u8; 3] {
    palette((face + 3 * level) % 7)
}

fn unit(colour: [u8; 3]) -> [f32; 4] {
    let [r, g, b] = colour.map(|channel| f32::from(channel) / 255.0);
    [r, g, b, 1.0]
}

fn solid_rgba8(colour: [u8; 3], extent: usize) -> Vec<u8> {
    [colour[0], colour[1], colour[2], 255].repeat(extent * extent)
}

/// One BC1 block whose two endpoints are the colour and whose sixteen indices select the
/// first endpoint, so it decodes to exactly that colour.
fn solid_bc1(colour: [u8; 3], extent: usize) -> Vec<u8> {
    let [r, g, b] = colour.map(u16::from);
    let packed = ((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3);
    let mut block = Vec::with_capacity(8);
    block.extend_from_slice(&packed.to_le_bytes());
    block.extend_from_slice(&packed.to_le_bytes());
    block.extend_from_slice(&[0; 4]);
    block.repeat(extent.div_ceil(4).pow(2))
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

struct Uploads {
    textures: Vec<Texture>,
    cases: Vec<Case>,
}

#[allow(clippy::too_many_lines)]
fn upload(device: &mulciber::Device<'_>) -> Result<Uploads, Box<dyn Error>> {
    let mut textures = Vec::new();
    let mut cases = Vec::new();

    // 0: a 2x2 RGBA8 UNORM face per layer whose every texel names its face, column and row,
    // sampled at each texel centre: face order and in-face orientation.
    let texel = |face: usize, column: usize, row: usize| {
        [
            u8::try_from((face + 1) * 36).expect("fits u8"),
            if column == 1 { 255 } else { 0 },
            if row == 1 { 255 } else { 0 },
            255,
        ]
    };
    let oriented: Vec<Vec<u8>> = (0..6)
        .map(|face| {
            (0..2)
                .flat_map(|row| (0..2).flat_map(move |column| texel(face, column, row)))
                .collect()
        })
        .collect();
    textures.push(device.create_rgba8_unorm_cube_texture(
        2,
        std::array::from_fn(|face| oriented[face].as_slice()),
    )?);
    for (face, name) in FACES.iter().enumerate() {
        for (column, row) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let centre = |index: usize| if index == 0 { 0.25 } else { 0.75 };
            cases.push(Case {
                texture: 0,
                what: format!("rgba8 unorm 2x2 face {name} texel ({column}, {row})"),
                direction: direction(face, centre(column), centre(row)),
                lod: 0.0,
                expected: texel(face, column, row).map(|channel| f32::from(channel) / 255.0),
            });
        }
    }

    // 1: RGBA8 sRGB, 4x4 faces with three levels each, sampled at every face centre and level
    // through a direction that is not normalized.
    let srgb: Vec<Vec<Vec<u8>>> = (0..6)
        .map(|face| {
            (0..3)
                .map(|level| solid_rgba8(level_colour(face, level), 4 >> level))
                .collect()
        })
        .collect();
    let srgb_levels: Vec<Vec<&[u8]>> = srgb
        .iter()
        .map(|levels| levels.iter().map(Vec::as_slice).collect())
        .collect();
    textures.push(device.create_rgba8_srgb_cube_texture_with_mips(
        4,
        std::array::from_fn(|face| srgb_levels[face].as_slice()),
    )?);
    // 2: BC1 UNORM, 8x8 faces with four levels each, the tail levels one block apiece.
    let bc1: Vec<Vec<Vec<u8>>> = (0..6)
        .map(|face| {
            (0..4)
                .map(|level| solid_bc1(level_colour(face, level), 8 >> level))
                .collect()
        })
        .collect();
    let bc1_levels: Vec<Vec<&[u8]>> = bc1
        .iter()
        .map(|levels| levels.iter().map(Vec::as_slice).collect())
        .collect();
    textures.push(device.create_block_compressed_cube_texture_with_mips(
        BlockCompression::Bc1Unorm,
        8,
        std::array::from_fn(|face| bc1_levels[face].as_slice()),
    )?);
    for (texture, label, levels) in [(1, "rgba8 srgb 4x4", 3), (2, "bc1 unorm 8x8", 4)] {
        for (face, name) in FACES.iter().enumerate() {
            for level in 0..levels {
                cases.push(Case {
                    texture,
                    what: format!("{label} face {name} level {level}"),
                    direction: direction(face, 0.5, 0.5).map(|axis| axis * 3.0),
                    lod: f32::from(u8::try_from(level).expect("fits u8")),
                    expected: unit(level_colour(face, level)),
                });
            }
        }
    }

    // 3: RGBA16Float 1x1 faces holding signed values outside 0..1.
    let float_texel = |face: usize| {
        let face = f32::from(u8::try_from(face).expect("fits u8"));
        [face + 0.5, -0.25 * face, 4.0 - face, 1.0]
    };
    let float_faces: [[[f32; 4]; 1]; 6] = std::array::from_fn(|face| [float_texel(face)]);
    textures.push(
        device.create_rgba16_float_cube_texture(1, float_faces.each_ref().map(|face| &face[..]))?,
    );
    for (face, name) in FACES.iter().enumerate() {
        cases.push(Case {
            texture: 3,
            what: format!("rgba16 float 1x1 face {name}"),
            direction: direction(face, 0.5, 0.5),
            lod: 0.0,
            expected: float_texel(face),
        });
    }

    for texture in &textures {
        assert_eq!(texture.dimension(), TextureDimension::Cube);
    }
    Ok(Uploads { textures, cases })
}

/// Input that every cube constructor and the float update must refuse.
fn check_rejections(
    device: &mulciber::Device<'_>,
    float_cube: &Texture,
) -> Result<(), Box<dyn Error>> {
    let face = [0_u8; 16];
    let short = [0_u8; 12];
    let mut faces = [&face[..]; 6];
    faces[4] = &short;
    let refused = device
        .create_rgba8_unorm_cube_texture(2, faces)
        .err()
        .ok_or("a short +Z face was accepted")?;
    println!("cube-texture: short face refused: {refused}");
    let refused = device
        .create_block_compressed_cube_texture(BlockCompression::Bc3Srgb, 4, [&face[..8]; 6])
        .err()
        .ok_or("eight-byte BC3 faces were accepted")?;
    println!("cube-texture: BC3 sizing refused: {refused}");
    let refused = device
        .update_rgba16_float_texture(float_cube, 1, 1, &[[0.0; 4]])
        .err()
        .ok_or("a float cube accepted a 2D replacement")?;
    println!("cube-texture: float update refused: {refused}");
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let sample_artifact = include_bytes!(concat!(env!("OUT_DIR"), "/sample.shaderbin"));
    if sample_artifact.is_empty() {
        return Err(
            "the cube probe's Metal sample artifact is not generated yet; build it with \
                    mulciber-shader on macOS (see docs/cube-textures.md)"
                .into(),
        );
    }
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — cube texture validation",
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
    println!("cube-texture: {:?}", graphics.selection);
    let Uploads { textures, cases } = upload(&graphics.device)?;
    check_rejections(&graphics.device, &textures[3])?;
    let flat = graphics
        .device
        .create_rgba8_unorm_texture(1, 1, &[255, 255, 255, 255])?;
    let vertices: Vec<u8> = [-1.0_f32, -1.0, 3.0, -1.0, -1.0, 3.0]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect();
    let mesh = graphics.device.create_mesh_with_layout(
        LAYOUT,
        &vertices,
        MeshIndices::U16(&[0, 1, 2, 0, 2, 1]),
    )?;
    let shader = ShaderArtifact::new(sample_artifact)?;
    let descriptor = |bindings| MaterialPipelineDescriptor {
        shader,
        vertex_entry: "sample_vertex",
        fragment_entry: "sample_fragment",
        vertex_layout: LAYOUT,
        bindings,
        blend: BlendMode::Opaque,
        depth: DepthMode::Off,
        instance_layout: None,
    };
    // The artifact records `texture_cube<f32>` at binding 1, so a 2D declaration is refused.
    let as_2d = [
        BINDINGS[0],
        MaterialBinding::Texture { binding: 1 },
        BINDINGS[2],
    ];
    let refused = graphics
        .device
        .create_hdr_material_pipeline(descriptor(&as_2d))
        .err()
        .ok_or("a 2D declaration of a cube binding was accepted")?;
    println!("cube-texture: 2D declaration refused: {refused}");
    let pipeline = graphics
        .device
        .create_hdr_material_pipeline(descriptor(&BINDINGS))?;
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
    let mut refused_2d_record = false;
    let started = Instant::now();
    while completed < cases.len() {
        if started.elapsed().as_secs() > 60 {
            return Err("cube texture validation exceeded 60 seconds".into());
        }
        let status = application.pump_events(&window, |event| -> Result<(), Box<dyn Error>> {
            if completed >= cases.len() {
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
            let case = &cases[completed];
            let uniform: Vec<u8> = [
                case.direction[0],
                case.direction[1],
                case.direction[2],
                case.lod,
            ]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
            // On the first frame, a 2D texture in the cube slot is refused before any native
            // work; the refused frame is abandoned and the next one renders.
            let texture = if refused_2d_record {
                &textures[case.texture]
            } else {
                &flat
            };
            let rendered = graphics.queue.render_and_present(
                frame,
                SceneSubmission {
                    content: SceneContent::Material(&[MaterialRecord {
                        pipeline: &pipeline,
                        geometry: GeometrySource::Mesh(&mesh),
                        textures: &[texture],
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
                    clear: ClearColor::BLACK,
                },
            );
            if !refused_2d_record {
                let refused = rendered
                    .err()
                    .ok_or("a 2D texture in the cube slot was accepted")?;
                println!("cube-texture: 2D record refused: {refused}");
                refused_2d_record = true;
                return Ok(());
            }
            rendered?;
            let actual = mulciber::integration::read_hdr_validation_pixel(&graphics.queue, target)?;
            let reference = case.expected.map(reference_half);
            for channel in 0..4 {
                // Permit two half ULPs for UNORM-to-float conversion and half render-target
                // rounding; a wrong face, level or texel is off by far more.
                let same_zero = matches!(actual[channel], 0 | 0x8000)
                    && matches!(reference[channel], 0 | 0x8000);
                if !same_zero
                    && (actual[channel] & 0x8000 != reference[channel] & 0x8000
                        || actual[channel].abs_diff(reference[channel]) > 2)
                {
                    return Err(format!(
                        "{}: direction={:?} lod={} channel={channel}: actual={actual:04x?} \
                         expected={reference:04x?}",
                        case.what, case.direction, case.lod
                    )
                    .into());
                }
            }
            println!("cube-texture: {} actual={actual:04x?} ok", case.what);
            completed += 1;
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            return Err("window closed before validation completed".into());
        }
    }
    graphics.device.destroy_texture(flat)?;
    for texture in textures {
        graphics.device.destroy_texture(texture)?;
    }
    graphics.shutdown()?;
    println!("cube-texture: {completed} sampled cases passed (2 half ULP tolerance)");
    Ok(())
}
