//! Native evidence for anisotropic sampler filtering through public material bindings. A 64×64
//! `RGBA16Float` texture whose every texel of level n holds n in red is sampled with explicit
//! gradients through a trilinear sampler and `SamplerFilter::Anisotropic` samplers of 4 and 16
//! samples, so the red read back from an HDR target is the level each chose. An isotropic
//! footprint must read the same level through all three; a footprint stretched along one axis must
//! read a sharper level through the anisotropic samplers than through the trilinear one, sharper
//! still with more samples. Out-of-range sample counts must be refused.
use mulciber::{
    BlendMode, ClearColor, DepthMode, DeviceRequest, FrameAcquire, GeometrySource,
    GraphicsErrorKind, HdrComposite, MaterialBinding, MaterialPipelineDescriptor, MaterialRecord,
    MeshDescriptor, MeshIndices, MipChain, OpenedGraphics, Postprocess,
    PostprocessPipelineDescriptor, RenderScale, SAMPLER_ANISOTROPY_LIMIT, SampleCount,
    SamplerAddress, SamplerFilter, Scene, SceneColor, SceneOutput, SceneSubmission, ShaderArtifact,
    TexelData, TextureDescriptor, TextureShape, VertexAttribute, VertexFormat, VertexLayout,
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
const SIZE: u16 = 64;
const LEVELS: usize = 7;

const fn sampler(binding: u32, filter: SamplerFilter) -> MaterialBinding {
    MaterialBinding::Sampler {
        binding,
        filter,
        address: SamplerAddress::Repeat,
    }
}

const BINDINGS: [MaterialBinding; 5] = [
    MaterialBinding::Uniform {
        binding: 0,
        size: 32,
    },
    MaterialBinding::Texture { binding: 1 },
    sampler(2, SamplerFilter::Linear),
    sampler(3, SamplerFilter::Anisotropic { max: 4 }),
    sampler(4, SamplerFilter::Anisotropic { max: 16 }),
];
const FILTERS: [&str; 3] = ["trilinear", "anisotropic 4x", "anisotropic 16x"];

/// A footprint `across` base texels wide and `along` texels long, sampled through every filter.
struct Footprint {
    what: &'static str,
    across: f32,
    along: f32,
}

const FOOTPRINTS: [Footprint; 4] = [
    Footprint {
        what: "point",
        across: 0.0,
        along: 0.0,
    },
    Footprint {
        what: "isotropic 8x8",
        across: 8.0,
        along: 8.0,
    },
    Footprint {
        what: "stretched 1x8",
        across: 1.0,
        along: 8.0,
    },
    Footprint {
        what: "stretched 1x32",
        across: 1.0,
        along: 32.0,
    },
];

/// Decodes binary16 bits independently of the upload's packing.
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

/// The level the specification's ideal filter reads: trilinear takes the long axis, an
/// anisotropic filter of `samples` divides it by as many samples as the stretch allows.
fn ideal_level(footprint: &Footprint, samples: f32) -> f32 {
    let long = footprint.across.max(footprint.along);
    let short = footprint.across.min(footprint.along);
    if long <= 1.0 {
        return 0.0;
    }
    let taken = if short > 0.0 {
        (long / short).min(samples)
    } else {
        1.0
    };
    (long / taken).log2().clamp(0.0, 6.0)
}

/// Out-of-range sample counts are refused when the pipeline is created.
fn check_rejections(
    device: &mulciber::Device<'_>,
    shader: ShaderArtifact<'_>,
) -> Result<(), Box<dyn Error>> {
    for max in [0, SAMPLER_ANISOTROPY_LIMIT + 1] {
        let mut bindings = BINDINGS;
        bindings[4] = sampler(4, SamplerFilter::Anisotropic { max });
        let refused = device
            .create_material_pipeline(descriptor(shader, &bindings))
            .err()
            .ok_or_else(|| format!("an anisotropy of {max} samples was accepted"))?;
        if refused.kind() != GraphicsErrorKind::InvalidRequest {
            return Err(format!("{max} samples refused as {:?}", refused.kind()).into());
        }
        println!("anisotropic-filter: {max} samples refused: {refused}");
    }
    Ok(())
}

fn descriptor<'a>(
    shader: ShaderArtifact<'a>,
    bindings: &'a [MaterialBinding],
) -> MaterialPipelineDescriptor<'a> {
    MaterialPipelineDescriptor {
        shader,
        vertex_entry: "sample_vertex",
        fragment_entry: "sample_fragment",
        vertex_layout: LAYOUT,
        bindings,
        blend: BlendMode::Opaque,
        depth: DepthMode::Off,
        instance_layout: None,
        scene_color: SceneColor::Hdr,
    }
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
        "Mulciber — anisotropic filtering validation",
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
    println!("anisotropic-filter: {:?}", graphics.selection);
    let chain: Vec<Vec<[f32; 4]>> = (0..LEVELS)
        .map(|level| {
            let extent = usize::from(SIZE >> level);
            let value = f32::from(u8::try_from(level).expect("seven levels"));
            vec![[value, 0.0, 0.0, 1.0]; extent * extent]
        })
        .collect();
    let images: Vec<&[[f32; 4]]> = chain.iter().map(Vec::as_slice).collect();
    let texture = graphics.device.create_texture(&TextureDescriptor {
        shape: TextureShape::D2 {
            width: SIZE.into(),
            height: SIZE.into(),
        },
        mips: MipChain::Supplied,
        data: TexelData::Rgba16Float(&images),
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
    check_rejections(&graphics.device, shader)?;
    let pipeline = graphics
        .device
        .create_material_pipeline(descriptor(shader, &BINDINGS))?;
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
    let cases: Vec<(usize, usize)> = (0..FOOTPRINTS.len())
        .flat_map(|footprint| (0..FILTERS.len()).map(move |filter| (footprint, filter)))
        .collect();
    let mut read = vec![[0.0_f32; 3]; FOOTPRINTS.len()];
    let mut targets = None;
    let mut completed = 0;
    let started = Instant::now();
    while completed < cases.len() {
        if started.elapsed().as_secs() > 60 {
            return Err("anisotropic filtering validation exceeded 60 seconds".into());
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
            let (footprint, filter) = cases[completed];
            let texel = 1.0 / f32::from(SIZE);
            let shape = &FOOTPRINTS[footprint];
            let selection = [0.0_f32, 1.0, 2.0][filter];
            let uniform: Vec<u8> = [
                0.5,
                0.5,
                shape.across * texel,
                0.0,
                0.0,
                shape.along * texel,
                selection,
                0.0,
            ]
            .iter()
            .flat_map(|v| v.to_ne_bytes())
            .collect();
            graphics.queue.render_and_present(
                frame,
                SceneSubmission {
                    scene: Scene {
                        records: &[MaterialRecord {
                            pipeline: &pipeline,
                            geometry: GeometrySource::Mesh(&mesh),
                            textures: &[&texture],
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
            )?;
            let actual = mulciber::integration::read_hdr_validation_pixel(&graphics.queue, target)?;
            let level = decode(actual[0]);
            let samples = [1.0, 4.0, 16.0][filter];
            println!(
                "anisotropic-filter: {} through {}: level {level:.3} (ideal {:.3})",
                shape.what,
                FILTERS[filter],
                ideal_level(shape, samples)
            );
            read[footprint][filter] = level;
            completed += 1;
            Ok(())
        })?;
        if status == PumpStatus::Exit {
            return Err("window closed before validation completed".into());
        }
    }
    graphics.device.destroy_texture(texture)?;
    graphics.shutdown()?;

    // How a GPU approximates the anisotropic footprint is its own, so the checks are what any
    // anisotropic filter must do rather than exact levels.
    let [point, isotropic, short, long] = [0, 1, 2, 3].map(|footprint| read[footprint]);
    let near = |a: f32, b: f32, within: f32| (a - b).abs() <= within;
    let mut failures = Vec::new();
    if !point.iter().all(|&level| near(level, 0.0, 0.01)) {
        failures.push(format!("a point footprint read {point:?}, not level 0"));
    }
    if !isotropic.iter().all(|&level| near(level, 3.0, 0.25)) {
        failures.push(format!(
            "the isotropic footprint read {isotropic:?}, not level 3"
        ));
    }
    for (what, levels, trilinear) in [("1x8", short, 3.0), ("1x32", long, 5.0)] {
        if !near(levels[0], trilinear, 0.25) {
            failures.push(format!(
                "trilinear read level {} of {what}, not {trilinear}",
                levels[0]
            ));
        }
        if levels[1] > levels[0] - 1.0 {
            failures.push(format!(
                "4 samples read level {} of {what}, not a level sharper than trilinear's {}",
                levels[1], levels[0]
            ));
        }
        if levels[2] > levels[1] - 0.5 {
            failures.push(format!(
                "16 samples read level {} of {what}, not sharper than 4 samples' {}",
                levels[2], levels[1]
            ));
        }
    }
    if !failures.is_empty() {
        return Err(failures.join("; ").into());
    }
    println!("anisotropic-filter: {completed} sampled cases passed");
    Ok(())
}
