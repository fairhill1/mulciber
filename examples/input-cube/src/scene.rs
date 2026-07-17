//! Self-contained geometry, texture, and transform data for the cube.

use glam::{Mat4, Quat, Vec3, camera::rh::proj::directx};
use mulciber::Vertex;

pub(crate) const CUBE_VERTICES: [Vertex; 24] = cube_vertices();

pub(crate) const CUBE_INDICES: [u16; 36] = [
    0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16, 17, 18,
    16, 18, 19, 20, 21, 22, 20, 22, 23,
];

pub(crate) fn checkerboard() -> [u8; 8 * 8 * 4] {
    let mut texels = [0_u8; 8 * 8 * 4];
    for y in 0..8 {
        for x in 0..8 {
            let bright = (x / 2 + y / 2) % 2 == 0;
            let color = if bright {
                [245, 170, 45, 255]
            } else {
                [35, 95, 210, 255]
            };
            let offset = (y * 8 + x) * 4;
            texels[offset..offset + 4].copy_from_slice(&color);
        }
    }
    texels
}

pub(crate) fn interactive_transform(
    seconds: f32,
    aspect: f32,
    orientation: Quat,
    distance: f32,
) -> [[f32; 4]; 4] {
    let automatic = Quat::from_rotation_y(seconds * 0.85) * Quat::from_rotation_x(seconds * 0.47);
    let model = Mat4::from_quat(orientation * automatic);
    let view = Mat4::from_translation(Vec3::new(0.0, 0.0, -distance));
    let projection = directx::perspective(55_f32.to_radians(), aspect, 0.1, 100.0);
    (projection * view * model).to_cols_array_2d()
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
