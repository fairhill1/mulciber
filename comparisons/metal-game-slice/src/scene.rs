//! Geometry, textures, camera, and transforms equivalent to Forge Run.

use glam::{
    Mat4, Quat, Vec2, Vec3,
    camera::rh::{proj::directx, view},
};

use crate::game::{Game, OBSTACLES};

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
    vertex([-1.0, -1.0, 1.0], [1.0, 0.55, 0.25], [0.0, 1.0]),
    vertex([1.0, -1.0, 1.0], [0.3, 0.9, 1.0], [1.0, 1.0]),
    vertex([1.0, -1.0, -1.0], [0.35, 1.0, 0.55], [1.0, 0.0]),
    vertex([-1.0, -1.0, -1.0], [0.95, 0.4, 1.0], [0.0, 0.0]),
    vertex([0.0, 1.0, 0.0], [1.0, 0.95, 0.35], [0.5, 0.0]),
];
pub const PYRAMID_INDICES: [u16; 18] = [0, 2, 1, 0, 3, 2, 0, 1, 4, 1, 2, 4, 2, 3, 4, 3, 0, 4];

pub struct SceneTransforms {
    pub ground: Vec<[[f32; 4]; 4]>,
    pub obstacles: Vec<[[f32; 4]; 4]>,
    pub player: Vec<[[f32; 4]; 4]>,
    pub pickups: Vec<[[f32; 4]; 4]>,
    pub hazards: Vec<[[f32; 4]; 4]>,
}

impl SceneTransforms {
    pub fn batches(&self) -> [&[[[f32; 4]; 4]]; 5] {
        [
            &self.ground,
            &self.obstacles,
            &self.player,
            &self.pickups,
            &self.hazards,
        ]
    }
}

#[allow(clippy::cast_precision_loss)]
pub fn transforms(game: &Game, aspect: f32, interpolation: f64) -> SceneTransforms {
    let state = game.render_state(interpolation);
    let player = state.player;
    let view = view::look_at_mat4(
        Vec3::new(player.x, 14.5, player.y + 9.5),
        Vec3::new(player.x, 0.0, player.y),
        Vec3::Y,
    );
    let projection = directx::perspective(48_f32.to_radians(), aspect, 0.1, 80.0);
    let vp = projection * view;
    let ground = vec![matrix(
        vp,
        Vec3::new(0.0, -0.65, 0.0),
        Vec3::new(9.7, 0.3, 9.7),
        Quat::IDENTITY,
    )];
    let obstacles = OBSTACLES
        .iter()
        .map(|&position| {
            matrix(
                vp,
                world(position, 0.72),
                Vec3::splat(0.72),
                Quat::from_rotation_y((position[0] * 0.31 + position[1]).sin()),
            )
        })
        .collect();
    let facing = state.facing;
    let player = vec![matrix(
        vp,
        Vec3::new(player.x, 0.52, player.y),
        Vec3::new(0.48, 0.42, 0.72),
        Quat::from_rotation_y(-facing.x.atan2(-facing.y)),
    )];
    let pickups = game
        .active_pickups()
        .enumerate()
        .map(|(index, position)| {
            let phase = index as f32 * 0.73;
            matrix(
                vp,
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
    let hazards = Game::hazards_at(state.world_seconds)
        .iter()
        .enumerate()
        .map(|(index, &position)| {
            matrix(
                vp,
                Vec3::new(position.x, 0.58, position.y),
                Vec3::splat(0.58),
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

fn matrix(vp: Mat4, translation: Vec3, scale: Vec3, rotation: Quat) -> [[f32; 4]; 4] {
    (vp * Mat4::from_scale_rotation_translation(scale, rotation, translation)).to_cols_array_2d()
}

fn world(position: [f32; 2], y: f32) -> Vec3 {
    let position = Vec2::from_array(position);
    Vec3::new(position.x, y, position.y)
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
