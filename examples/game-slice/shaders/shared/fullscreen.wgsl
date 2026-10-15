#define_import_path forge::fullscreen

// One triangle covering the screen, drawn from vertex indices 0 to 2 with no vertex buffer.

struct FullscreenPoint {
    @builtin(position) clip_position: vec4<f32>,
    // Texture coordinate of the point on the screen: (0, 0) top left, (1, 1) bottom right.
    @location(0) uv: vec2<f32>,
}

fn fullscreen_point(index: u32) -> FullscreenPoint {
    let corners = array(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    let corner = corners[index];
    var point: FullscreenPoint;
    point.clip_position = vec4<f32>(corner, 0.0, 1.0);
    point.uv = vec2<f32>(corner.x * 0.5 + 0.5, 0.5 - corner.y * 0.5);
    return point;
}
