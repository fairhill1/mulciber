#define_import_path mulciber::overlay

// Overlay geometry as mulciber-text's `Batch` builds it: text, images and flat shapes drawn over
// the scene from one atlas, in a material pipeline with `BlendMode::PremultipliedTranslucent` and
// `DepthMode::Off`. The game's shader declares the atlas and its sampler and passes through:
//
//     @vertex
//     fn overlay_vertex(vertex: OverlayVertex) -> OverlayRaster {
//         return overlay_raster(vertex);
//     }
//
//     @fragment
//     fn overlay_fragment(raster: OverlayRaster) -> @location(0) vec4<f32> {
//         return overlay_shade(textureSample(atlas, atlas_sampler, raster.uv), raster.color);
//     }

// One vertex, laid out as `mulciber_text::VERTEX_LAYOUT`.
struct OverlayVertex {
    // Clip space.
    @location(0) position: vec2<f32>,
    // Atlas texture coordinate.
    @location(1) uv: vec2<f32>,
    // Linear colour with straight alpha.
    @location(2) color: vec4<f32>,
}

struct OverlayRaster {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

fn overlay_raster(vertex: OverlayVertex) -> OverlayRaster {
    var raster: OverlayRaster;
    raster.clip_position = vec4<f32>(vertex.position, 0.0, 1.0);
    raster.uv = vertex.uv;
    raster.color = vertex.color;
    return raster;
}

// An atlas texel in `color`, premultiplied: glyphs and masks are white with their coverage in
// alpha, so they take the colour; images keep theirs, tinted by it.
fn overlay_shade(texel: vec4<f32>, color: vec4<f32>) -> vec4<f32> {
    let alpha = texel.a * color.a;
    return vec4<f32>(color.rgb * texel.rgb * alpha, alpha);
}
