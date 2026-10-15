//! The README's overlay shader and pipeline declaration, compiled and checked against each other
//! as `Device::create_material_pipeline` checks them, without a device.

use mulciber::{
    BlendMode, DepthMode, MaterialBinding, MaterialPipelineDescriptor, SamplerAddress,
    SamplerFilter, SceneColor, ShaderArtifact,
};
use mulciber_shader::{ShaderModules, ShaderTarget};

/// The overlay shader as the README gives it.
const OVERLAY_WGSL: &str = r"#import mulciber::overlay::{OverlayVertex, OverlayRaster, overlay_raster, overlay_shade}

@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(0) @binding(1) var atlas_sampler: sampler;

@vertex
fn overlay_vertex(vertex: OverlayVertex) -> OverlayRaster {
    return overlay_raster(vertex);
}

@fragment
fn overlay_fragment(raster: OverlayRaster) -> @location(0) vec4<f32> {
    return overlay_shade(textureSample(atlas, atlas_sampler, raster.uv), raster.color);
}
";

#[test]
fn the_readme_overlay_shader_matches_its_pipeline_declaration() {
    assert!(
        include_str!("../README.md").contains(OVERLAY_WGSL.trim_end()),
        "the README's overlay shader differs from this test's"
    );
    let directory = std::env::temp_dir().join(format!("mulciber-text-{}", std::process::id()));
    let artifact = directory.join("overlay.shaderbin");
    ShaderModules::new()
        .shader_source("overlay.wgsl", OVERLAY_WGSL)
        .compile_wgsl(&artifact, ShaderTarget::Vulkan)
        .expect("the overlay shader compiles (needs spirv-val on PATH)");
    let bytes = std::fs::read(&artifact).unwrap();
    let _ = std::fs::remove_dir_all(&directory);
    let descriptor = MaterialPipelineDescriptor {
        shader: ShaderArtifact::new(&bytes).expect("an artifact"),
        vertex_entry: "overlay_vertex",
        fragment_entry: "overlay_fragment",
        vertex_layout: mulciber_text::VERTEX_LAYOUT,
        instance_layout: None,
        bindings: &[
            MaterialBinding::Texture { binding: 0 },
            MaterialBinding::Sampler {
                binding: 1,
                filter: SamplerFilter::Nearest,
                address: SamplerAddress::ClampToEdge,
            },
        ],
        blend: BlendMode::PremultipliedTranslucent,
        depth: DepthMode::Off,
        scene_color: SceneColor::Surface,
    };
    descriptor
        .validate()
        .expect("VERTEX_LAYOUT and the bindings match the compiled overlay shader");
}
