//! Native evidence for 2D texture arrays through public material bindings: layer and per-layer mip
//! selection, in-layer orientation, the level and layer counts the shader reads, `RGBA16Float` and
//! `RGBA8Unorm` uploads with supplied chains, and a BC7 array baked by `mulciber-texture` into one
//! KTX 2.0 file and uploaded through `Ktx2Texture::descriptor`, which must sample as an
//! independent decoder (`bcdec`) decodes its blocks. Sampled through
//! `MaterialBinding::TextureArray` and read back from an HDR target.
use mulciber::{
    BlendMode, BlockCompression, ClearColor, DepthMode, DeviceRequest, FrameAcquire,
    GeometrySource, GraphicsErrorKind, HdrComposite, Ktx2Texture, MaterialBinding,
    MaterialPipelineDescriptor, MaterialRecord, MeshDescriptor, MeshIndices, MipChain,
    OpenedGraphics, Postprocess, PostprocessPipelineDescriptor, RenderScale, SampleCount,
    SamplerAddress, SamplerFilter, Scene, SceneColor, SceneOutput, SceneSubmission, ShaderArtifact,
    TexelData, Texture, TextureDescriptor, TextureDimension, TextureShape, VertexAttribute,
    VertexFormat, VertexLayout,
};
use mulciber_platform::{Application, LogicalSize, PumpStatus, WindowDescriptor, WindowEvent};
use std::{error::Error, time::Instant};

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
    MaterialBinding::TextureArray { binding: 1 },
    MaterialBinding::Sampler {
        binding: 2,
        filter: SamplerFilter::Nearest,
        address: SamplerAddress::ClampToEdge,
    },
];

/// One sampled point, layer and LOD on one uploaded array (or, with `query`, its level and layer
/// counts), with the RGBA it must read.
struct Case {
    texture: usize,
    what: String,
    uv: [f32; 2],
    layer: i32,
    lod: f32,
    query: bool,
    expected: [f32; 4],
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

/// Every texel of every level of `layers` square layers, one image per layer and level in that
/// order, from `texel(layer, level, column, row)`.
fn array<T>(
    layers: usize,
    size: usize,
    levels: usize,
    texel: impl Fn(usize, usize, usize, usize) -> T,
) -> Vec<Vec<T>> {
    let mut images = Vec::new();
    for layer in 0..layers {
        for level in 0..levels {
            let extent = (size >> level).max(1);
            images.push(
                (0..extent)
                    .flat_map(|row| (0..extent).map(|column| (column, row)).collect::<Vec<_>>())
                    .map(|(column, row)| texel(layer, level, column, row))
                    .collect(),
            );
        }
    }
    images
}

struct Uploads {
    textures: Vec<Texture>,
    cases: Vec<Case>,
}

fn counts(texture: usize, what: &str, levels: f32, layers: f32) -> Case {
    Case {
        texture,
        what: format!("{what} level and layer counts"),
        uv: [0.5, 0.5],
        layer: 0,
        lod: 0.0,
        query: true,
        expected: [levels, layers, 0.0, 1.0],
    }
}

#[allow(clippy::too_many_lines)]
fn upload(device: &mulciber::Device<'_>) -> Result<Uploads, Box<dyn Error>> {
    let mut textures = Vec::new();
    let mut cases = Vec::new();

    // 0: five layers of 4x4 with three levels each, every level of every layer holding its own
    // (layer, level). A wrong layer changes red, a wrong level green.
    let id = |layer: usize, level: usize| [small(layer) + 1.0, small(level) + 1.0, 0.5, 1.0];
    let chains = array(5, 4, 3, |layer, level, _, _| id(layer, level));
    let images: Vec<&[[f32; 4]]> = chains.iter().map(Vec::as_slice).collect();
    textures.push(device.create_texture(&TextureDescriptor {
        shape: TextureShape::D2Array {
            width: 4,
            height: 4,
            layers: 5,
        },
        mips: MipChain::Supplied,
        data: TexelData::Rgba16Float(&images),
    })?);
    for layer in 0..5 {
        for level in 0..3 {
            cases.push(Case {
                texture: 0,
                what: format!("float 4x4 layer {layer} level {level}"),
                uv: [0.5, 0.5],
                layer: i32::try_from(layer)?,
                lod: small(level),
                query: false,
                expected: id(layer, level),
            });
        }
    }
    cases.push(counts(0, "float 4x4", 3.0, 5.0));

    // 1: two 4x2 RGBA8 layers (three levels) whose base texels each name their layer, column and
    // row, sampled at every base texel centre of both: rows run down from the top in every layer.
    let oriented = |layer: usize, level: usize, column: usize, row: usize| -> [u8; 4] {
        if level > 0 {
            return [0, 0, 0, 255];
        }
        let byte = |value: usize| u8::try_from(value).expect("small");
        [
            byte(layer * 40 + 10),
            byte(column * 50),
            byte(row * 100),
            255,
        ]
    };
    // A 4x2 chain: 4x2, 2x1, 1x1.
    let mut images = Vec::new();
    for layer in 0..2 {
        for (level, (width, height)) in [(4, 2), (2, 1), (1, 1)].into_iter().enumerate() {
            let texels: Vec<u8> = (0..height)
                .flat_map(|row| {
                    (0..width).flat_map(move |column| oriented(layer, level, column, row))
                })
                .collect();
            images.push(texels);
        }
    }
    let views: Vec<&[u8]> = images.iter().map(Vec::as_slice).collect();
    textures.push(device.create_texture(&TextureDescriptor {
        shape: TextureShape::D2Array {
            width: 4,
            height: 2,
            layers: 2,
        },
        mips: MipChain::Supplied,
        data: TexelData::Rgba8Unorm(&views),
    })?);
    for layer in 0..2 {
        for row in 0..2 {
            for column in 0..4 {
                let texel = oriented(layer, 0, column, row);
                cases.push(Case {
                    texture: 1,
                    what: format!("RGBA8 4x2 layer {layer} texel ({column}, {row})"),
                    uv: [
                        (small(column) + 0.5) * 0.25,
                        (small(row) * 2.0 + 1.0) * 0.25,
                    ],
                    layer: i32::try_from(layer)?,
                    lod: 0.0,
                    query: false,
                    expected: texel.map(|channel| f32::from(channel) / 255.0),
                });
            }
        }
    }
    cases.push(counts(1, "RGBA8 4x2", 3.0, 2.0));

    // 2: three 8x8 layers of four levels, each level of each layer one colour, BC7-encoded by
    // mulciber-texture, written as one KTX 2.0 array and uploaded from it.
    let colour = |layer: usize, level: usize| -> [u8; 4] {
        let byte = |value: usize| u8::try_from(value).expect("small");
        [byte(30 + layer * 80), byte(20 + level * 60), 200, 255]
    };
    let layers: Vec<Vec<Vec<u8>>> = (0..3)
        .map(|layer| {
            let levels = (0..4)
                .map(|level| {
                    let extent = 8 >> level;
                    colour(layer, level).repeat(extent * extent)
                })
                .collect();
            mulciber_texture::encode_bc7(&mulciber_texture::Levels {
                width: 8,
                height: 8,
                levels,
            })
        })
        .collect();
    let bytes = mulciber_texture::ktx2_array_bytes(BlockCompression::Bc7Unorm, 8, 8, &layers, &[])?;
    let bake = Ktx2Texture::parse(&bytes)?;
    textures.push(device.create_texture(&bake.descriptor())?);
    for (layer, levels) in layers.iter().enumerate() {
        for (level, blocks) in levels.iter().enumerate() {
            let mut decoded = [0_u8; 4 * 4 * 4];
            bcdec_rs::bc7(&blocks[..16], &mut decoded, 16);
            cases.push(Case {
                texture: 2,
                what: format!("BC7 KTX2 8x8 layer {layer} level {level}"),
                uv: [
                    0.5 / f32::from(8_u8 >> level),
                    0.5 / f32::from(8_u8 >> level),
                ],
                layer: i32::try_from(layer)?,
                lod: small(level),
                query: false,
                expected: [decoded[0], decoded[1], decoded[2], decoded[3]]
                    .map(|channel| f32::from(channel) / 255.0),
            });
        }
    }
    cases.push(counts(2, "BC7 KTX2 8x8", 4.0, 3.0));

    for texture in &textures {
        assert_eq!(texture.dimension(), TextureDimension::D2Array);
    }
    Ok(Uploads { textures, cases })
}

/// Invalid array creation and the float update must refuse.
fn check_rejections(device: &mulciber::Device<'_>, array: &Texture) -> Result<(), Box<dyn Error>> {
    let texel = [[1.0_f32; 4]];
    let request = |layers: u32, images: &[&[[f32; 4]]]| {
        device
            .create_texture(&TextureDescriptor {
                shape: TextureShape::D2Array {
                    width: 1,
                    height: 1,
                    layers,
                },
                mips: MipChain::Supplied,
                data: TexelData::Rgba16Float(images),
            })
            .err()
    };
    let refused = request(0, &[]).ok_or("an array of no layers was accepted")?;
    if refused.kind() != GraphicsErrorKind::InvalidRequest {
        return Err(format!("no layers refused as {:?}", refused.kind()).into());
    }
    println!("texture-array: no layers refused: {refused}");
    let refused = request(3, &[&texel, &texel]).ok_or("a missing layer was accepted")?;
    if !refused.to_string().contains("takes 3 images") {
        return Err(format!("missing layer refusal does not count it: {refused}").into());
    }
    println!("texture-array: missing layer refused: {refused}");
    let nan = [[f32::NAN, 0.0, 0.0, 1.0]];
    let refused = request(2, &[&texel, &nan]).ok_or("a NaN texel was accepted")?;
    if !refused.to_string().contains("2D array layer 1") {
        return Err(format!("NaN refusal does not name the layer: {refused}").into());
    }
    println!("texture-array: NaN texel refused: {refused}");
    let refused = device
        .create_texture(&TextureDescriptor {
            shape: TextureShape::D2Array {
                width: 2,
                height: 2,
                layers: 2,
            },
            mips: MipChain::Generated,
            data: TexelData::Rgba8Unorm(&[&[0; 16], &[0; 16]]),
        })
        .err()
        .ok_or("generated mips for an array were accepted")?;
    if refused.kind() != GraphicsErrorKind::Unsupported {
        return Err(format!("generated mips refused as {:?}", refused.kind()).into());
    }
    println!("texture-array: generated mips refused: {refused}");
    let refused = device
        .update_texture(array, TexelData::Rgba16Float(&[&[[0.0; 4]]]))
        .err()
        .ok_or("a 2D array accepted a 2D replacement")?;
    println!("texture-array: float update refused: {refused}");
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let sample = include_bytes!(concat!(env!("OUT_DIR"), "/sample.shaderbin"));
    if sample.is_empty() {
        return Err(
            "no sample artifact for this target is checked in; build it with \
                    `mulciber-shader build src/sample.wgsl --target metal` on a Mac"
                .into(),
        );
    }
    let mut application = Application::new()?;
    let window = application.create_window(&WindowDescriptor::new(
        "Mulciber — 2D texture array validation",
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
    println!("texture-array: {:?}", graphics.selection);
    let Uploads { textures, cases } = upload(&graphics.device)?;
    check_rejections(&graphics.device, &textures[0])?;
    let flat = graphics.device.create_texture(&TextureDescriptor {
        shape: TextureShape::D2 {
            width: 1,
            height: 1,
        },
        mips: MipChain::Single,
        data: TexelData::Rgba16Float(&[&[[1.0, 1.0, 1.0, 1.0]]]),
    })?;
    let vertices: Vec<u8> = [-1.0_f32, -1.0, 3.0, -1.0, -1.0, 3.0]
        .iter()
        .flat_map(|v| v.to_ne_bytes())
        .collect();
    let mesh = graphics.device.create_mesh(&MeshDescriptor {
        layout: LAYOUT,
        vertices: &vertices,
        parts: &[MeshIndices::U16(&[0, 1, 2, 0, 2, 1])],
    })?;
    let shader = ShaderArtifact::new(sample)?;
    let descriptor = |bindings| MaterialPipelineDescriptor {
        shader,
        vertex_entry: "sample_vertex",
        fragment_entry: "sample_fragment",
        vertex_layout: LAYOUT,
        bindings,
        blend: BlendMode::Opaque,
        depth: DepthMode::Off,
        instance_layout: None,
        scene_color: SceneColor::Hdr,
    };
    // The artifact records `texture_2d_array<f32>` at binding 1, so a 2D declaration is refused.
    let as_flat = [
        BINDINGS[0],
        MaterialBinding::Texture { binding: 1 },
        BINDINGS[2],
    ];
    let refused = graphics
        .device
        .create_material_pipeline(descriptor(&as_flat))
        .err()
        .ok_or("a 2D declaration of a 2D array binding was accepted")?;
    println!("texture-array: 2D declaration refused: {refused}");
    let pipeline = graphics
        .device
        .create_material_pipeline(descriptor(&BINDINGS))?;
    let composite = graphics
        .device
        .create_postprocess_pipeline(PostprocessPipelineDescriptor {
            shader: ShaderArtifact::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/composite.shaderbin"
            )))?,
            uniform_size: None,
            hdr: Some(HdrComposite::default()),
        })?;
    let mut targets = None;
    let mut completed = 0;
    let mut refused_flat_record = false;
    let started = Instant::now();
    while completed < cases.len() {
        if started.elapsed().as_secs() > 60 {
            return Err("2D texture array validation exceeded 60 seconds".into());
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
                targets = Some(graphics.device.create_postprocess_targets(
                    info,
                    RenderScale::NATIVE,
                    SceneColor::Hdr,
                )?);
            }
            let target = targets.as_ref().expect("targets were just created");
            let case = &cases[completed];
            let mut uniform: Vec<u8> = [case.uv[0], case.uv[1], 0.0, case.lod]
                .iter()
                .flat_map(|v| v.to_ne_bytes())
                .collect();
            for selection in [case.layer, i32::from(case.query), 0, 0] {
                uniform.extend_from_slice(&selection.to_ne_bytes());
            }
            // On the first frame, a 2D texture in the array slot is refused before any native
            // work; the refused frame is abandoned and the next one renders.
            let texture = if refused_flat_record {
                &textures[case.texture]
            } else {
                &flat
            };
            let rendered = graphics.queue.render_and_present(
                frame,
                SceneSubmission {
                    scene: Scene {
                        timing_marks: &[],
                        records: &[MaterialRecord {
                            pipeline: &pipeline,
                            geometry: GeometrySource::Mesh(&mesh),
                            textures: &[texture],
                            shadow_map: None,
                            uniform: &uniform,
                            storage: &[],
                            instances: &[],
                        }],
                        shadow: None,
                        output: SceneOutput::Postprocessed {
                            postprocess: Postprocess {
                                pipeline: &composite,
                                targets: target,
                                uniform: &[],
                            },
                            foreground_start: None,
                            offscreen: &[],
                            overlay: &[],
                        },
                    },
                    clear: ClearColor::BLACK,
                },
            );
            if !refused_flat_record {
                let refused = rendered
                    .err()
                    .ok_or("a 2D texture in the 2D-array slot was accepted")?;
                println!("texture-array: 2D record refused: {refused}");
                refused_flat_record = true;
                return Ok(());
            }
            rendered?;
            let actual = mulciber::integration::read_hdr_validation_pixel(&graphics.queue, target)?;
            let reference = case.expected.map(reference_half);
            for channel in 0..4 {
                // A correct sample matches its reference to within the render target's rounding
                // and an 8-bit channel's; a wrong layer, level or texel is off by far more.
                if actual[channel].abs_diff(reference[channel]) > 2 {
                    return Err(format!(
                        "{}: uv={:?} layer={} lod={} channel={channel}: actual={actual:04x?} \
                         expected={reference:04x?}",
                        case.what, case.uv, case.layer, case.lod
                    )
                    .into());
                }
            }
            println!("texture-array: {} actual={actual:04x?} ok", case.what);
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
    println!("texture-array: {completed} sampled cases passed (2 half ULP tolerance)");
    Ok(())
}
