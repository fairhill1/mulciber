//! Self-contained geometry, texture, and transform data for the cube.

use glam::{Mat4, Vec3, camera::rh::proj::directx};
use mulciber::{VertexAttribute, VertexFormat, VertexLayout};

/// Each vertex as `cube.wgsl` reads it: position, color, and texture coordinate at locations 0
/// through 2.
pub(crate) const CUBE_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 32,
    attributes: &[
        VertexAttribute {
            location: 0,
            format: VertexFormat::Float32x3,
            offset: 0,
        },
        VertexAttribute {
            location: 1,
            format: VertexFormat::Float32x3,
            offset: 12,
        },
        VertexAttribute {
            location: 2,
            format: VertexFormat::Float32x2,
            offset: 24,
        },
    ],
};

pub(crate) const CUBE_INDICES: [u16; 36] = [
    0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16, 17, 18,
    16, 18, 19, 20, 21, 22, 20, 22, 23,
];

/// The cube's 24 vertices, four per face, packed in [`CUBE_LAYOUT`].
pub(crate) fn cube_vertices() -> Vec<u8> {
    let n = -1.0;
    let p = 1.0;
    let faces: [([[f32; 3]; 4], [f32; 3]); 6] = [
        (
            [[n, n, p], [p, n, p], [p, p, p], [n, p, p]],
            [1.0, 0.45, 0.35],
        ),
        (
            [[p, n, n], [n, n, n], [n, p, n], [p, p, n]],
            [0.35, 0.75, 1.0],
        ),
        (
            [[n, n, n], [n, n, p], [n, p, p], [n, p, n]],
            [0.45, 1.0, 0.55],
        ),
        (
            [[p, n, p], [p, n, n], [p, p, n], [p, p, p]],
            [0.95, 0.85, 0.35],
        ),
        (
            [[n, p, p], [p, p, p], [p, p, n], [n, p, n]],
            [0.85, 0.45, 1.0],
        ),
        (
            [[n, n, n], [p, n, n], [p, n, p], [n, n, p]],
            [0.35, 0.95, 0.9],
        ),
    ];
    let uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
    let mut bytes = Vec::with_capacity(24 * 32);
    for (corners, color) in faces {
        for (position, uv) in corners.iter().zip(uvs) {
            for value in position.iter().chain(&color).chain(&uv) {
                bytes.extend_from_slice(&value.to_ne_bytes());
            }
        }
    }
    bytes
}

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

/// The cube's column-major model-view-projection matrix, as the bytes of `cube.wgsl`'s uniform.
pub(crate) fn transform(seconds: f32, aspect: f32) -> [u8; 64] {
    let model = Mat4::from_rotation_y(seconds * 0.85) * Mat4::from_rotation_x(seconds * 0.47);
    let view = Mat4::from_translation(Vec3::new(0.0, 0.0, -4.0));
    let projection = directx::perspective(55_f32.to_radians(), aspect, 0.1, 100.0);
    let mut bytes = [0_u8; 64];
    let matrix = (projection * view * model).to_cols_array();
    for (chunk, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(matrix) {
        *chunk = value.to_ne_bytes();
    }
    bytes
}
