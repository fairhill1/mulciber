// Bindingless overlay material: the application rebuilds its clip-space geometry every frame
// and supplies it as frame-transient bytes, so all animation lives in the vertex data itself.

struct HudVertex {
    @location(0) position: vec2<f32>,
    // Premultiplied linear color matching the pipeline's translucent blend mode.
    @location(1) color: vec4<f32>,
}

struct HudRaster {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn hud_vertex(input: HudVertex) -> HudRaster {
    var output: HudRaster;
    output.clip_position = vec4<f32>(input.position, 0.0, 1.0);
    output.color = input.color;
    return output;
}

@fragment
fn hud_fragment(input: HudRaster) -> @location(0) vec4<f32> {
    return input.color;
}
