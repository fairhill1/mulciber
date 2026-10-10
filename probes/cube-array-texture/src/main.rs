//! Native evidence for `RGBA16Float` cube texture arrays through public material bindings: layer,
//! face, in-face orientation and per-face mip selection in every cube, the `_from_bits` upload, and
//! the level and cube counts the shader reads, sampled through `MaterialBinding::CubeTextureArray`
//! and read back from an HDR target.
use mulciber::{
    BlendMode, ClearColor, DepthMode, DeviceRequest, FrameAcquire, GeometrySource,
    GraphicsErrorKind, MaterialBinding, MaterialPipelineDescriptor, MaterialRecord, MeshIndices,
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
        size: 32,
    },
    MaterialBinding::CubeTextureArray { binding: 1 },
    MaterialBinding::Sampler {
        binding: 2,
        filter: SamplerFilter::Nearest,
        address: SamplerAddress::ClampToEdge,
    },
];

/// One sampled direction, cube and LOD on one uploaded array (or, with `query`, its level and
/// cube counts), with the RGBA it must read.
struct Case {
    texture: usize,
    what: String,
    direction: [f32; 3],
    layer: i32,
    lod: f32,
    query: bool,
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

fn small(value: usize) -> f32 {
    f32::from(u8::try_from(value).expect("probe indices fit u8"))
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

/// Every texel of every face at every level of a cube array of `layers` cubes, `layer`-major
/// then face then level, from `texel(layer, face, level, column, row)`.
fn cube_array(
    layers: usize,
    size: usize,
    levels: usize,
    texel: impl Fn(usize, usize, usize, usize, usize) -> [f32; 4],
) -> Vec<[Vec<Vec<[f32; 4]>>; 6]> {
    (0..layers)
        .map(|layer| {
            std::array::from_fn(|face| {
                (0..levels)
                    .map(|level| {
                        let extent = (size >> level).max(1);
                        (0..extent)
                            .flat_map(|row| {
                                (0..extent).map({
                                    let texel = &texel;
                                    move |column| texel(layer, face, level, column, row)
                                })
                            })
                            .collect()
                    })
                    .collect()
            })
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn upload(device: &mulciber::Device<'_>) -> Result<Uploads, Box<dyn Error>> {
    let mut textures = Vec::new();
    let mut cases = Vec::new();

    // 0: three cubes of 4x4 faces with three levels each; every level of every face of every cube
    // holds its own (cube, face, level), sampled at each face centre and level through a
    // direction that is not normalized. A wrong cube changes red, a wrong face green, a wrong
    // level blue.
    let id = |layer: usize, face: usize, level: usize| {
        [
            small(layer) + 1.0,
            small(face) + 1.0,
            small(level) + 1.0,
            1.0,
        ]
    };
    let chains = cube_array(3, 4, 3, |layer, face, level, _, _| id(layer, face, level));
    let views: Vec<[Vec<&[[f32; 4]]>; 6]> = chains
        .iter()
        .map(|cube| {
            cube.each_ref()
                .map(|levels| levels.iter().map(Vec::as_slice).collect())
        })
        .collect();
    let layers: Vec<[&[&[[f32; 4]]]; 6]> = views
        .iter()
        .map(|cube| cube.each_ref().map(Vec::as_slice))
        .collect();
    textures.push(device.create_rgba16_float_cube_array_texture_with_mips(4, &layers)?);
    for layer in 0..3 {
        for (face, name) in FACES.iter().enumerate() {
            for level in 0..3 {
                cases.push(Case {
                    texture: 0,
                    what: format!("4x4 cube {layer} face {name} level {level}"),
                    direction: direction(face, 0.5, 0.5).map(|axis| axis * 3.0),
                    layer: i32::try_from(layer)?,
                    lod: small(level),
                    query: false,
                    expected: id(layer, face, level),
                });
            }
        }
    }
    cases.push(Case {
        texture: 0,
        what: "4x4 array level and cube counts".to_owned(),
        direction: [1.0, 0.0, 0.0],
        layer: 0,
        lod: 0.0,
        query: true,
        expected: [3.0, 3.0, 0.0, 1.0],
    });

    // 1: two cubes of 2x2 faces (two levels) whose base texels each name their cube, face,
    // column and row, sampled at every texel centre of both cubes: in-face orientation is the
    // single cube's in every layer.
    let oriented = |layer: usize, face: usize, level: usize, column: usize, row: usize| {
        if level == 1 {
            return [-1.0, -1.0, -1.0, 1.0];
        }
        [
            small(layer) * 8.0 + small(face) + 1.0,
            small(column),
            small(row),
            1.0,
        ]
    };
    let chains = cube_array(2, 2, 2, oriented);
    let views: Vec<[Vec<&[[f32; 4]]>; 6]> = chains
        .iter()
        .map(|cube| {
            cube.each_ref()
                .map(|levels| levels.iter().map(Vec::as_slice).collect())
        })
        .collect();
    let layers: Vec<[&[&[[f32; 4]]]; 6]> = views
        .iter()
        .map(|cube| cube.each_ref().map(Vec::as_slice))
        .collect();
    textures.push(device.create_rgba16_float_cube_array_texture_with_mips(2, &layers)?);
    for layer in 0..2 {
        for (face, name) in FACES.iter().enumerate() {
            for (column, row) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let centre = |index: usize| if index == 0 { 0.25 } else { 0.75 };
                cases.push(Case {
                    texture: 1,
                    what: format!("2x2 cube {layer} face {name} texel ({column}, {row})"),
                    direction: direction(face, centre(column), centre(row)),
                    layer: i32::try_from(layer)?,
                    lod: 0.0,
                    query: false,
                    expected: oriented(layer, face, 0, column, row),
                });
            }
        }
    }

    // 2: one cube of 1x1 faces from binary16 bits, signed values outside 0..1.
    let bits = |face: usize| -> [u16; 4] {
        // -2.5, face + 0.5 (exact halves), 1024, 1.
        let positive = reference_half(small(face) + 0.5);
        [0xc100, positive, 0x6400, 0x3c00]
    };
    let faces: [[[u16; 4]; 1]; 6] = std::array::from_fn(|face| [bits(face)]);
    let chains: [[&[[u16; 4]]; 1]; 6] = faces.each_ref().map(|face| [&face[..]]);
    let cube: [&[&[[u16; 4]]]; 6] = chains.each_ref().map(|chain| &chain[..]);
    textures.push(device.create_rgba16_float_cube_array_texture_with_mips_from_bits(1, &[cube])?);
    for (face, name) in FACES.iter().enumerate() {
        cases.push(Case {
            texture: 2,
            what: format!("1x1 bits cube 0 face {name}"),
            direction: direction(face, 0.5, 0.5),
            layer: 0,
            lod: 0.0,
            query: false,
            expected: [-2.5, small(face) + 0.5, 1024.0, 1.0],
        });
    }
    cases.push(Case {
        texture: 2,
        what: "1x1 bits array level and cube counts".to_owned(),
        direction: [1.0, 0.0, 0.0],
        layer: 0,
        lod: 0.0,
        query: true,
        expected: [1.0, 1.0, 0.0, 1.0],
    });

    for texture in &textures {
        assert_eq!(texture.dimension(), TextureDimension::CubeArray);
    }
    Ok(Uploads { textures, cases })
}

/// Input the cube array constructors and the float update must refuse.
fn check_rejections(device: &mulciber::Device<'_>, array: &Texture) -> Result<(), Box<dyn Error>> {
    let refused = device
        .create_rgba16_float_cube_array_texture_with_mips(1, &[])
        .err()
        .ok_or("an array of no cubes was accepted")?;
    if refused.kind() != GraphicsErrorKind::InvalidRequest {
        return Err(format!("no cubes refused as {:?}", refused.kind()).into());
    }
    println!("cube-array-texture: no layers refused: {refused}");
    let texel = [[1.0_f32; 4]];
    let chain: [&[[f32; 4]]; 1] = [&texel];
    let good: [&[&[[f32; 4]]]; 6] = [&chain; 6];
    let mut short = good;
    let empty: [&[[f32; 4]]; 0] = [];
    short[5] = &empty;
    let refused = device
        .create_rgba16_float_cube_array_texture_with_mips(1, &[good, short])
        .err()
        .ok_or("a -Z face without levels in cube 1 was accepted")?;
    if !refused
        .to_string()
        .contains("cube array layer 1: cube face -Z")
    {
        return Err(format!("short face refusal does not name it: {refused}").into());
    }
    println!("cube-array-texture: short face refused: {refused}");
    let nan = [[f32::NAN, 0.0, 0.0, 1.0]];
    let bad_chain: [&[[f32; 4]]; 1] = [&nan];
    let mut bad = good;
    bad[2] = &bad_chain;
    let refused = device
        .create_rgba16_float_cube_array_texture_with_mips(1, &[bad])
        .err()
        .ok_or("a NaN texel was accepted")?;
    if !refused
        .to_string()
        .contains("cube array layer 0: cube face +Y")
    {
        return Err(format!("NaN refusal does not name the face: {refused}").into());
    }
    println!("cube-array-texture: NaN texel refused: {refused}");
    let refused = device
        .update_rgba16_float_texture(array, 1, 1, &[[0.0; 4]])
        .err()
        .ok_or("a cube array accepted a 2D replacement")?;
    println!("cube-array-texture: float update refused: {refused}");
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — cube texture array validation",
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
    println!("cube-array-texture: {:?}", graphics.selection);
    let Uploads { textures, cases } = upload(&graphics.device)?;
    check_rejections(&graphics.device, &textures[0])?;
    let single_cube = graphics
        .device
        .create_rgba16_float_cube_texture(1, [&[[1.0, 1.0, 1.0, 1.0]][..]; 6])?;
    let vertices: Vec<u8> = [-1.0_f32, -1.0, 3.0, -1.0, -1.0, 3.0]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect();
    let mesh = graphics.device.create_mesh_with_layout(
        LAYOUT,
        &vertices,
        MeshIndices::U16(&[0, 1, 2, 0, 2, 1]),
    )?;
    let shader = ShaderArtifact::new(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/sample.shaderbin"
    )))?;
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
    // The artifact records `texture_cube_array<f32>` at binding 1, so a cube declaration is refused.
    let as_cube = [
        BINDINGS[0],
        MaterialBinding::CubeTexture { binding: 1 },
        BINDINGS[2],
    ];
    let refused = graphics
        .device
        .create_hdr_material_pipeline(descriptor(&as_cube))
        .err()
        .ok_or("a cube declaration of a cube array binding was accepted")?;
    println!("cube-array-texture: cube declaration refused: {refused}");
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
    let mut refused_cube_record = false;
    let started = Instant::now();
    while completed < cases.len() {
        if started.elapsed().as_secs() > 60 {
            return Err("cube texture array validation exceeded 60 seconds".into());
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
            let mut uniform: Vec<u8> = [
                case.direction[0],
                case.direction[1],
                case.direction[2],
                case.lod,
            ]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
            for selection in [case.layer, i32::from(case.query), 0, 0] {
                uniform.extend_from_slice(&selection.to_ne_bytes());
            }
            // On the first frame, a single cube in the cube-array slot is refused before any
            // native work; the refused frame is abandoned and the next one renders.
            let texture = if refused_cube_record {
                &textures[case.texture]
            } else {
                &single_cube
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
                    offscreen: &[],
                    clear: ClearColor::BLACK,
                },
            );
            if !refused_cube_record {
                let refused = rendered
                    .err()
                    .ok_or("a cube texture in the cube-array slot was accepted")?;
                println!("cube-array-texture: cube record refused: {refused}");
                refused_cube_record = true;
                return Ok(());
            }
            rendered?;
            let actual = mulciber::integration::read_hdr_validation_pixel(&graphics.queue, target)?;
            let reference = case.expected.map(reference_half);
            for channel in 0..4 {
                // The uploads are exact binary16 values, so a correct sample matches its reference
                // to within the render target's rounding; a wrong cube, face, level or texel is off
                // by far more.
                let same_zero = matches!(actual[channel], 0 | 0x8000)
                    && matches!(reference[channel], 0 | 0x8000);
                if !same_zero
                    && (actual[channel] & 0x8000 != reference[channel] & 0x8000
                        || actual[channel].abs_diff(reference[channel]) > 2)
                {
                    return Err(format!(
                        "{}: direction={:?} cube={} lod={} channel={channel}: actual={actual:04x?} \
                         expected={reference:04x?}",
                        case.what, case.direction, case.layer, case.lod
                    )
                    .into());
                }
            }
            println!("cube-array-texture: {} actual={actual:04x?} ok", case.what);
            completed += 1;
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            return Err("window closed before validation completed".into());
        }
    }
    graphics.device.destroy_texture(single_cube)?;
    for texture in textures {
        graphics.device.destroy_texture(texture)?;
    }
    graphics.shutdown()?;
    println!("cube-array-texture: {completed} sampled cases passed (2 half ULP tolerance)");
    Ok(())
}
