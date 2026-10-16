#import mulciber::skin::skin_diffuse

@group(0) @binding(0) var skin_table: texture_2d<f32>;
@group(0) @binding(1) var skin_sampler: sampler;

struct SkinRaster {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) n_dot_l: f32,
    @location(1) curvature: f32,
}

@vertex
fn skin_vertex(@location(0) position: vec3<f32>, @location(1) n_dot_l: f32, @location(2) curvature: f32) -> SkinRaster {
    return SkinRaster(vec4<f32>(position, 1.0), n_dot_l, curvature);
}

@fragment
fn skin_fragment(raster: SkinRaster) -> @location(0) vec4<f32> {
    return vec4<f32>(skin_diffuse(skin_table, skin_sampler, raster.n_dot_l, raster.curvature), 1.0);
}
