#import mulciber::overlay::{OverlayVertex, OverlayRaster, overlay_raster, overlay_shade}

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
