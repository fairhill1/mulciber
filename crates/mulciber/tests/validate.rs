//! Postprocess and shadow pipeline declarations checked against compiled shaders without a
//! device, as `Device::create_postprocess_pipeline` and `Device::create_shadow_pipeline` check
//! them. Compiling needs `spirv-val` on PATH.

use mulciber::{
    BloomShaders, GraphicsErrorKind, HdrComposite, MaterialBinding, PostprocessPipelineDescriptor,
    SamplerAddress, SamplerFilter, ShaderArtifact, ShadowPipelineDescriptor, VertexAttribute,
    VertexFormat, VertexLayout,
};
use mulciber_shader::{ShaderModules, ShaderTarget};

const COMPOSITE: &str = "
struct Grade { exposure: vec4<f32> }
@group(0) @binding(0) var<uniform> grade: Grade;
@group(0) @binding(1) var scene: texture_2d<f32>;
@group(0) @binding(2) var scene_sampler: sampler;
struct Raster { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn post_vertex(@builtin(vertex_index) index: u32) -> Raster {
    var positions = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    var out: Raster;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = positions[index] * 0.5 + 0.5;
    return out;
}
@fragment fn post_fragment(in: Raster) -> @location(0) vec4<f32> {
    let colour = textureSampleLevel(scene, scene_sampler, in.uv, 0.0).rgb * grade.exposure.x;
    return vec4<f32>(colour, 1.0);
}
";

const SHADOW: &str = "
struct ShadowParams { light_from_model: mat4x4<f32> }
@group(0) @binding(0) var<uniform> params: ShadowParams;
@group(0) @binding(1) var leaves: texture_2d<f32>;
@group(0) @binding(2) var leaves_sampler: sampler;
struct Raster { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn shadow_vertex(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>) -> Raster {
    return Raster(params.light_from_model * vec4<f32>(position, 1.0), uv);
}
@fragment fn shadow_fragment(in: Raster) {
    if textureSample(leaves, leaves_sampler, in.uv).a < 0.5 {
        discard;
    }
}
";

const LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 20,
    attributes: &[
        VertexAttribute {
            location: 0,
            offset: 0,
            format: VertexFormat::Float32x3,
        },
        VertexAttribute {
            location: 1,
            offset: 12,
            format: VertexFormat::Float32x2,
        },
    ],
};

fn compile(name: &str, source: &str) -> Vec<u8> {
    let directory = std::env::temp_dir().join(format!("mulciber-validate-{}", std::process::id()));
    let artifact = directory.join(format!("{name}.shaderbin"));
    ShaderModules::new()
        .shader_source(format!("{name}.wgsl"), source)
        .compile_wgsl(&artifact, ShaderTarget::Vulkan)
        .expect("the shader compiles (needs spirv-val on PATH)");
    let bytes = std::fs::read(&artifact).unwrap();
    let _ = std::fs::remove_file(&artifact);
    bytes
}

#[test]
fn a_postprocess_declaration_is_checked_without_a_device() {
    let bytes = compile("composite", COMPOSITE);
    let shader = ShaderArtifact::new(&bytes).expect("an artifact");
    let declare = |uniform_size, hdr| PostprocessPipelineDescriptor {
        shader,
        uniform_size,
        hdr,
    };
    declare(Some(16), None)
        .validate()
        .expect("the composite's uniform is 16 bytes");
    declare(Some(16), Some(HdrComposite::default()))
        .validate()
        .expect("an HDR composite without bloom or volume");

    let wrong_size = declare(Some(32), None).validate().expect_err("32 bytes");
    assert_eq!(wrong_size.kind(), GraphicsErrorKind::InvalidRequest);
    // The shader reads a uniform the pipeline would not bind, with or without an HDR composite.
    for hdr in [None, Some(HdrComposite::default())] {
        let undeclared = declare(None, hdr)
            .validate()
            .expect_err("no uniform declared");
        assert_eq!(undeclared.kind(), GraphicsErrorKind::InvalidRequest);
        assert!(
            undeclared.to_string().contains("declares no uniform"),
            "{undeclared}"
        );
    }
    // Bloom needs the composite to sample the bloom chain, which this one does not.
    let bloom = BloomShaders {
        prefilter: shader,
        downsample: shader,
        upsample: None,
    };
    let no_bloom_binding = declare(
        Some(16),
        Some(HdrComposite {
            bloom: Some(bloom),
            volume: None,
        }),
    )
    .validate()
    .expect_err("bloom without the composite's bloom binding");
    assert_eq!(no_bloom_binding.kind(), GraphicsErrorKind::InvalidRequest);
}

#[test]
fn a_module_shared_with_the_scene_is_held_only_to_its_fullscreen_pass() {
    // Binding 0 and binding 3 belong to the scene's entry point, not the fullscreen pass.
    let source = "
struct Tint { colour: vec4<f32> }
@group(0) @binding(0) var<uniform> tint: Tint;
@group(0) @binding(1) var scene: texture_2d<f32>;
@group(0) @binding(2) var scene_sampler: sampler;
@group(0) @binding(3) var pattern: texture_2d<f32>;
struct Raster { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn post_vertex(@builtin(vertex_index) index: u32) -> Raster {
    var positions = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return Raster(vec4<f32>(positions[index], 0.0, 1.0), positions[index] * 0.5 + 0.5);
}
@fragment fn post_fragment(in: Raster) -> @location(0) vec4<f32> {
    return textureSampleLevel(scene, scene_sampler, in.uv, 0.0);
}
@fragment fn scene_fragment(in: Raster) -> @location(0) vec4<f32> {
    return tint.colour * textureSampleLevel(pattern, scene_sampler, in.uv, 0.0);
}
";
    let bytes = compile("shared", source);
    PostprocessPipelineDescriptor {
        shader: ShaderArtifact::new(&bytes).expect("an artifact"),
        uniform_size: None,
        hdr: None,
    }
    .validate()
    .expect("the fullscreen pass uses only bindings 1 and 2");
}

#[test]
fn a_composite_without_hdr_takes_no_bindings_past_the_scene_sampler() {
    let source = COMPOSITE
        .replace(
            "@group(0) @binding(2) var scene_sampler: sampler;",
            "@group(0) @binding(2) var scene_sampler: sampler;\n\
             @group(0) @binding(3) var dust: texture_2d<f32>;",
        )
        .replace(
            "* grade.exposure.x;",
            "* grade.exposure.x + textureSampleLevel(dust, scene_sampler, in.uv, 0.0).rgb;",
        );
    let bytes = compile("dusty", &source);
    let refused = PostprocessPipelineDescriptor {
        shader: ShaderArtifact::new(&bytes).expect("an artifact"),
        uniform_size: Some(16),
        hdr: None,
    }
    .validate()
    .expect_err("binding 3 has no native slot without bloom");
    assert_eq!(refused.kind(), GraphicsErrorKind::InvalidRequest);
    assert!(refused.to_string().contains("binding 3"), "{refused}");
}

#[test]
fn a_shadow_declaration_is_checked_without_a_device() {
    let bytes = compile("shadow", SHADOW);
    let shader = ShaderArtifact::new(&bytes).expect("an artifact");
    let bindings = [
        MaterialBinding::Uniform {
            binding: 0,
            size: 64,
        },
        MaterialBinding::Texture { binding: 1 },
        MaterialBinding::Sampler {
            binding: 2,
            filter: SamplerFilter::Anisotropic { max: 8 },
            address: SamplerAddress::Repeat,
        },
    ];
    let declare = |bindings, fragment_entry| ShadowPipelineDescriptor {
        shader,
        vertex_entry: "shadow_vertex",
        fragment_entry,
        vertex_layout: LAYOUT,
        instance_layout: None,
        bindings,
    };
    declare(&bindings, Some("shadow_fragment"))
        .validate()
        .expect("the cutout caster's declaration matches its shader");

    let mut wrong_size = bindings;
    wrong_size[0] = MaterialBinding::Uniform {
        binding: 0,
        size: 80,
    };
    let refused = declare(&wrong_size, Some("shadow_fragment"))
        .validate()
        .expect_err("an 80-byte uniform");
    assert_eq!(refused.kind(), GraphicsErrorKind::InvalidRequest);
    let refused = declare(&bindings, Some("missing_fragment"))
        .validate()
        .expect_err("a missing fragment entry point");
    assert_eq!(refused.kind(), GraphicsErrorKind::InvalidRequest);
    let mut zero_anisotropy = bindings;
    zero_anisotropy[2] = MaterialBinding::Sampler {
        binding: 2,
        filter: SamplerFilter::Anisotropic { max: 0 },
        address: SamplerAddress::Repeat,
    };
    let refused = declare(&zero_anisotropy, Some("shadow_fragment"))
        .validate()
        .expect_err("an anisotropy of zero");
    assert_eq!(refused.kind(), GraphicsErrorKind::InvalidRequest);
}
