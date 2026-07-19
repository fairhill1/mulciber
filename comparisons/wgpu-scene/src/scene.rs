//! Geometry, texture, and transform data equivalent to `mulciber-scene`.

use glam::{
    Mat4, Quat, Vec3,
    camera::rh::{proj::directx, view},
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub color: [f32; 3],
    pub uv: [f32; 2],
}

pub const CUBE_VERTICES: [Vertex; 24] = cube_vertices();
pub const CUBE_INDICES: [u16; 36] = [
    0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16, 17, 18,
    16, 18, 19, 20, 21, 22, 20, 22, 23,
];
pub const PYRAMID_VERTICES: [Vertex; 5] = [
    vertex([-1.0, -1.0, 1.0], [1.0, 0.5, 0.35], [0.0, 1.0]),
    vertex([1.0, -1.0, 1.0], [0.35, 0.8, 1.0], [1.0, 1.0]),
    vertex([1.0, -1.0, -1.0], [0.4, 1.0, 0.55], [1.0, 0.0]),
    vertex([-1.0, -1.0, -1.0], [0.9, 0.45, 1.0], [0.0, 0.0]),
    vertex([0.0, 1.0, 0.0], [1.0, 0.9, 0.4], [0.5, 0.0]),
];
pub const PYRAMID_INDICES: [u16; 18] = [
    0, 2, 1, 0, 3, 2, // base
    0, 1, 4, 1, 2, 4, 2, 3, 4, 3, 0, 4,
];

pub fn checkerboard(a: [u8; 3], b: [u8; 3]) -> [u8; 8 * 8 * 4] {
    let mut texels = [0_u8; 8 * 8 * 4];
    for y in 0..8 {
        for x in 0..8 {
            let color = if (x / 2 + y / 2) % 2 == 0 { a } else { b };
            let offset = (y * 8 + x) * 4;
            texels[offset..offset + 3].copy_from_slice(&color);
            texels[offset + 3] = 255;
        }
    }
    texels
}

#[allow(clippy::cast_precision_loss)]
pub fn transforms(seconds: f32, aspect: f32) -> Vec<[[f32; 4]; 4]> {
    const WIDTH: usize = 10;
    const DEPTH: usize = 10;
    let orbit = seconds * 0.13;
    let eye = Vec3::new(orbit.sin() * 22.0, 15.0, orbit.cos() * 22.0);
    let view = view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y);
    let projection = directx::perspective(52_f32.to_radians(), aspect, 0.1, 100.0);
    let mut transforms = Vec::with_capacity(WIDTH * DEPTH);
    for z in 0..DEPTH {
        for x in 0..WIDTH {
            let phase = (x * 17 + z * 11) as f32 * 0.09;
            let translation = Vec3::new(
                (x as f32 - (WIDTH - 1) as f32 * 0.5) * 2.35,
                (seconds * 1.7 + phase).sin() * 0.45,
                (z as f32 - (DEPTH - 1) as f32 * 0.5) * 2.35,
            );
            let rotation = Quat::from_rotation_y(seconds * 0.7 + phase)
                * Quat::from_rotation_x(seconds * 0.31 + phase * 0.5);
            let scale = if (x + z) % 2 == 0 { 0.72 } else { 0.88 };
            let model =
                Mat4::from_scale_rotation_translation(Vec3::splat(scale), rotation, translation);
            transforms.push((projection * view * model).to_cols_array_2d());
        }
    }
    transforms
}

const fn vertex(position: [f32; 3], color: [f32; 3], uv: [f32; 2]) -> Vertex {
    Vertex {
        position,
        color,
        uv,
    }
}

const fn cube_vertices() -> [Vertex; 24] {
    let n = -1.0;
    let p = 1.0;
    [
        vertex([n, n, p], [1.0, 0.45, 0.35], [0.0, 1.0]),
        vertex([p, n, p], [1.0, 0.45, 0.35], [1.0, 1.0]),
        vertex([p, p, p], [1.0, 0.45, 0.35], [1.0, 0.0]),
        vertex([n, p, p], [1.0, 0.45, 0.35], [0.0, 0.0]),
        vertex([p, n, n], [0.35, 0.75, 1.0], [0.0, 1.0]),
        vertex([n, n, n], [0.35, 0.75, 1.0], [1.0, 1.0]),
        vertex([n, p, n], [0.35, 0.75, 1.0], [1.0, 0.0]),
        vertex([p, p, n], [0.35, 0.75, 1.0], [0.0, 0.0]),
        vertex([n, n, n], [0.45, 1.0, 0.55], [0.0, 1.0]),
        vertex([n, n, p], [0.45, 1.0, 0.55], [1.0, 1.0]),
        vertex([n, p, p], [0.45, 1.0, 0.55], [1.0, 0.0]),
        vertex([n, p, n], [0.45, 1.0, 0.55], [0.0, 0.0]),
        vertex([p, n, p], [0.95, 0.85, 0.35], [0.0, 1.0]),
        vertex([p, n, n], [0.95, 0.85, 0.35], [1.0, 1.0]),
        vertex([p, p, n], [0.95, 0.85, 0.35], [1.0, 0.0]),
        vertex([p, p, p], [0.95, 0.85, 0.35], [0.0, 0.0]),
        vertex([n, p, p], [0.85, 0.45, 1.0], [0.0, 1.0]),
        vertex([p, p, p], [0.85, 0.45, 1.0], [1.0, 1.0]),
        vertex([p, p, n], [0.85, 0.45, 1.0], [1.0, 0.0]),
        vertex([n, p, n], [0.85, 0.45, 1.0], [0.0, 0.0]),
        vertex([n, n, n], [0.35, 0.95, 0.9], [0.0, 1.0]),
        vertex([p, n, n], [0.35, 0.95, 0.9], [1.0, 1.0]),
        vertex([p, n, p], [0.35, 0.95, 0.9], [1.0, 0.0]),
        vertex([n, n, p], [0.35, 0.95, 0.9], [0.0, 0.0]),
    ]
}
