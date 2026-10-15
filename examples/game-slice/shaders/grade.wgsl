// The postprocess pass: a cool, slightly desaturated grade and a vignette over the resolved scene.

#import forge::fullscreen::{FullscreenPoint, fullscreen_point}
#import mulciber::colorspace::{luminance}

// Mulciber binds the resolved scene color and its sampler here for every postprocess pipeline.
@group(0) @binding(1) var scene_color: texture_2d<f32>;
@group(0) @binding(2) var scene_sampler: sampler;

@vertex
fn post_vertex(@builtin(vertex_index) index: u32) -> FullscreenPoint {
    return fullscreen_point(index);
}

@fragment
fn post_fragment(input: FullscreenPoint) -> @location(0) vec4<f32> {
    let source = textureSample(scene_color, scene_sampler, input.uv);
    let graded = mix(source.rgb, vec3<f32>(luminance(source.rgb)), 0.35) * vec3<f32>(1.08, 0.98, 1.12);
    let centered = input.uv * 2.0 - vec2<f32>(1.0);
    let vignette = 1.0 - 0.22 * dot(centered, centered);
    return vec4<f32>(graded * vignette, source.a);
}
