//! Geometry, textures, camera, and render transforms for the playable slice.

use glam::{
    Mat4, Quat, Vec2, Vec3,
    camera::rh::{proj::directx, view},
};
use mulciber::{VertexAttribute, VertexFormat, VertexLayout};

use crate::game::{Game, OBSTACLES};

/// Each vertex as `forge.wgsl` reads it: position, color, and texture coordinate at locations 0
/// through 2.
pub const VERTEX_LAYOUT: VertexLayout<'static> = VertexLayout {
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

/// Each instance as `forge.wgsl` reads it: a column-major model-view-projection matrix, a column
/// per location from 3 through 6.
pub const INSTANCE_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 64,
    attributes: &[
        VertexAttribute {
            location: 3,
            format: VertexFormat::Float32x4,
            offset: 0,
        },
        VertexAttribute {
            location: 4,
            format: VertexFormat::Float32x4,
            offset: 16,
        },
        VertexAttribute {
            location: 5,
            format: VertexFormat::Float32x4,
            offset: 32,
        },
        VertexAttribute {
            location: 6,
            format: VertexFormat::Float32x4,
            offset: 48,
        },
    ],
};

pub const CUBE_INDICES: [u16; 36] = [
    0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16, 17, 18,
    16, 18, 19, 20, 21, 22, 20, 22, 23,
];
pub const PYRAMID_INDICES: [u16; 18] = [
    0, 2, 1, 0, 3, 2, // base
    0, 1, 4, 1, 2, 4, 2, 3, 4, 3, 0, 4,
];

/// The cube's 24 vertices, four per face, packed in [`VERTEX_LAYOUT`].
pub fn cube_vertices() -> Vec<u8> {
    let n = -1.0;
    let p = 1.0;
    let faces = [
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
    let mut vertices = Vec::with_capacity(24);
    for (corners, color) in faces {
        for (position, uv) in corners.into_iter().zip(uvs) {
            vertices.push((position, color, uv));
        }
    }
    pack_vertices(&vertices)
}

/// The pyramid's five vertices, packed in [`VERTEX_LAYOUT`].
pub fn pyramid_vertices() -> Vec<u8> {
    pack_vertices(&[
        ([-1.0, -1.0, 1.0], [1.0, 0.55, 0.25], [0.0, 1.0]),
        ([1.0, -1.0, 1.0], [0.3, 0.9, 1.0], [1.0, 1.0]),
        ([1.0, -1.0, -1.0], [0.35, 1.0, 0.55], [1.0, 0.0]),
        ([-1.0, -1.0, -1.0], [0.95, 0.4, 1.0], [0.0, 0.0]),
        ([0.0, 1.0, 0.0], [1.0, 0.95, 0.35], [0.5, 0.0]),
    ])
}

fn pack_vertices(vertices: &[([f32; 3], [f32; 3], [f32; 2])]) -> Vec<u8> {
    vertices
        .iter()
        .flat_map(|(position, color, uv)| position.iter().chain(color).chain(uv))
        .flat_map(|value| value.to_ne_bytes())
        .collect()
}

/// Each kind of shape's instances this frame, packed in [`INSTANCE_LAYOUT`].
pub struct SceneTransforms {
    pub ground: Vec<u8>,
    pub obstacles: Vec<u8>,
    pub player: Vec<u8>,
    pub pickups: Vec<u8>,
    pub hazards: Vec<u8>,
}

#[allow(clippy::cast_precision_loss)]
pub fn transforms(game: &Game, aspect: f32, interpolation: f64) -> SceneTransforms {
    let state = game.render_state(interpolation);
    let player = state.player;
    let eye = Vec3::new(player.x, 14.5, player.y + 9.5);
    let target = Vec3::new(player.x, 0.0, player.y);
    let view = view::look_at_mat4(eye, target, Vec3::Y);
    let projection = directx::perspective(48_f32.to_radians(), aspect, 0.1, 80.0);
    let view_projection = projection * view;

    let ground = matrix(
        view_projection,
        Vec3::new(0.0, -0.65, 0.0),
        Vec3::new(9.7, 0.3, 9.7),
        Quat::IDENTITY,
    )
    .collect();
    let obstacles = OBSTACLES
        .iter()
        .flat_map(|&position| {
            matrix(
                view_projection,
                world(position, 0.72),
                Vec3::new(0.72, 0.72, 0.72),
                Quat::from_rotation_y((position[0] * 0.31 + position[1]).sin()),
            )
        })
        .collect();
    let facing = state.facing;
    let player_yaw = -facing.x.atan2(-facing.y);
    let player = matrix(
        view_projection,
        Vec3::new(player.x, 0.52, player.y),
        Vec3::new(0.48, 0.42, 0.72),
        Quat::from_rotation_y(player_yaw),
    )
    .collect();
    let pickups = game
        .active_pickups()
        .enumerate()
        .flat_map(|(index, position)| {
            let phase = index as f32 * 0.73;
            matrix(
                view_projection,
                Vec3::new(
                    position.x,
                    0.72 + (state.visual_seconds * 2.2 + phase).sin() * 0.18,
                    position.y,
                ),
                Vec3::splat(0.38),
                Quat::from_rotation_y(state.visual_seconds * 1.4 + phase),
            )
        })
        .collect();
    let hazard_positions = Game::hazards_at(state.world_seconds);
    let hazards = hazard_positions
        .iter()
        .enumerate()
        .flat_map(|(index, &position)| {
            matrix(
                view_projection,
                Vec3::new(position.x, 0.58, position.y),
                Vec3::new(0.58, 0.58, 0.58),
                Quat::from_rotation_y(-state.world_seconds * 1.8 + index as f32),
            )
        })
        .collect();

    SceneTransforms {
        ground,
        obstacles,
        player,
        pickups,
        hazards,
    }
}

pub fn checkerboard(a: [u8; 3], b: [u8; 3], scale: usize) -> [u8; 8 * 8 * 4] {
    let mut texels = [0_u8; 8 * 8 * 4];
    for y in 0..8 {
        for x in 0..8 {
            let color = if (x / scale + y / scale).is_multiple_of(2) {
                a
            } else {
                b
            };
            let offset = (y * 8 + x) * 4;
            texels[offset..offset + 3].copy_from_slice(&color);
            texels[offset + 3] = 255;
        }
    }
    texels
}

/// One instance's matrix as its bytes in [`INSTANCE_LAYOUT`].
fn matrix(
    view_projection: Mat4,
    translation: Vec3,
    scale: Vec3,
    rotation: Quat,
) -> impl Iterator<Item = u8> {
    (view_projection * Mat4::from_scale_rotation_translation(scale, rotation, translation))
        .to_cols_array()
        .into_iter()
        .flat_map(f32::to_ne_bytes)
}

fn world(position: [f32; 2], y: f32) -> Vec3 {
    let position = Vec2::from_array(position);
    Vec3::new(position.x, y, position.y)
}
