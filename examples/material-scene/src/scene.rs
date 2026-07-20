//! Self-contained geometry bytes, procedural textures, and uniform packing for the material
//! scene. Vertex and uniform data are packed by the application; the crate only sees bytes
//! against declared layouts.

use glam::{Mat4, Vec3, Vec4, camera::rh::proj::directx, camera::rh::view::look_at_mat4};
use mulciber::{VertexAttribute, VertexFormat, VertexLayout};

/// Shadow cascades rendered into the layered map, ordered near to far.
pub(crate) const CASCADE_COUNT: usize = 3;

/// Extent of every cascade layer; cascade fitting quality is relative to this resolution.
pub(crate) const SHADOW_MAP_SIZE: u32 = 1024;

/// View-space depth range each cascade's light volume is fitted to. The scene's visible
/// depth ends around the floor's far corners (~18 units from the camera), so the last band
/// runs past that and everything beyond it stays lit.
const CASCADE_BOUNDS: [(f32, f32); CASCADE_COUNT] = [(0.1, 6.0), (6.0, 12.0), (12.0, 40.0)];

/// World-space distance the light volume extends behind each cascade slice so casters
/// between the light and the slice still write depth.
const CASTER_MARGIN: f32 = 12.0;

/// The fixed directional light's position; its direction points at the scene center.
const LIGHT_EYE: Vec3 = Vec3::new(4.0, 7.0, 3.0);

/// Crystal vertices carry position, normal, texture coordinate, and a per-vertex glow weight the
/// fixed vocabulary cannot express.
pub(crate) const CRYSTAL_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 36,
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
        VertexAttribute {
            location: 3,
            format: VertexFormat::Float32,
            offset: 32,
        },
    ],
};

/// The lava floor uses a second, tighter layout: position and texture coordinate only.
pub(crate) const LAVA_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 20,
    attributes: &[
        VertexAttribute {
            location: 0,
            format: VertexFormat::Float32x3,
            offset: 0,
        },
        VertexAttribute {
            location: 1,
            format: VertexFormat::Float32x2,
            offset: 12,
        },
    ],
};

/// Bones in the kelp strand's palette, matching the WGSL `array<mat4x4<f32>, 6>`.
pub(crate) const KELP_BONES: usize = 6;

/// Vertical span one bone governs, from its pivot to the next.
const KELP_SEGMENT_HEIGHT: f32 = 0.75;

const KELP_RADIUS: f32 = 0.32;

/// Sides of the strand's tube cross-section.
const KELP_SIDES: usize = 6;

/// Kelp vertices carry position, normal, two blended bone indices, and their weights.
pub(crate) const SKINNED_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 56,
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
            format: VertexFormat::Uint32x4,
            offset: 24,
        },
        VertexAttribute {
            location: 3,
            format: VertexFormat::Float32x4,
            offset: 40,
        },
    ],
};

/// HUD vertices carry a clip-space position and a premultiplied linear color; the overlay's
/// geometry is rebuilt every frame and submitted as frame-transient bytes.
pub(crate) const HUD_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: 24,
    attributes: &[
        VertexAttribute {
            location: 0,
            format: VertexFormat::Float32x2,
            offset: 0,
        },
        VertexAttribute {
            location: 1,
            format: VertexFormat::Float32x4,
            offset: 8,
        },
    ],
};

pub(crate) const CUBE_INDICES: [u16; 36] = [
    0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7, 8, 9, 10, 8, 10, 11, 12, 13, 14, 12, 14, 15, 16, 17, 18,
    16, 18, 19, 20, 21, 22, 20, 22, 23,
];

pub(crate) const FLOOR_INDICES: [u16; 6] = [0, 1, 2, 0, 2, 3];

fn push_f32s(bytes: &mut Vec<u8>, values: &[f32]) {
    for value in values {
        bytes.extend_from_slice(&value.to_ne_bytes());
    }
}

fn push_u32s(bytes: &mut Vec<u8>, values: &[u32]) {
    for value in values {
        bytes.extend_from_slice(&value.to_ne_bytes());
    }
}

/// A unit cube with per-face normals; the top face glows fully, the sides partially, and the
/// bottom barely.
pub(crate) fn crystal_vertices() -> Vec<u8> {
    let n = -1.0;
    let p = 1.0;
    let faces: [([f32; 3], [[f32; 3]; 4], f32); 6] = [
        (
            [0.0, 0.0, 1.0],
            [[n, n, p], [p, n, p], [p, p, p], [n, p, p]],
            0.55,
        ),
        (
            [0.0, 0.0, -1.0],
            [[p, n, n], [n, n, n], [n, p, n], [p, p, n]],
            0.55,
        ),
        (
            [-1.0, 0.0, 0.0],
            [[n, n, n], [n, n, p], [n, p, p], [n, p, n]],
            0.55,
        ),
        (
            [1.0, 0.0, 0.0],
            [[p, n, p], [p, n, n], [p, p, n], [p, p, p]],
            0.55,
        ),
        (
            [0.0, 1.0, 0.0],
            [[n, p, p], [p, p, p], [p, p, n], [n, p, n]],
            1.0,
        ),
        (
            [0.0, -1.0, 0.0],
            [[n, n, n], [p, n, n], [p, n, p], [n, n, p]],
            0.15,
        ),
    ];
    let corner_uvs = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
    let mut bytes = Vec::with_capacity(24 * 36);
    for (normal, corners, glow) in faces {
        for (position, uv) in corners.into_iter().zip(corner_uvs) {
            push_f32s(&mut bytes, &position);
            push_f32s(&mut bytes, &normal);
            push_f32s(&mut bytes, &uv);
            push_f32s(&mut bytes, &[glow]);
        }
    }
    bytes
}

/// A large quad at y = -1.6 whose texture coordinates tile the lava pattern.
pub(crate) fn floor_vertices() -> Vec<u8> {
    let e = 9.0;
    let y = -1.6;
    let corners = [
        ([-e, y, e], [0.0, 6.0]),
        ([e, y, e], [6.0, 6.0]),
        ([e, y, -e], [6.0, 0.0]),
        ([-e, y, -e], [0.0, 0.0]),
    ];
    let mut bytes = Vec::with_capacity(4 * 20);
    for (position, uv) in corners {
        push_f32s(&mut bytes, &position);
        push_f32s(&mut bytes, &uv);
    }
    bytes
}

pub(crate) fn crystal_base_texture() -> [u8; 8 * 8 * 4] {
    let mut texels = [0_u8; 8 * 8 * 4];
    for y in 0..8 {
        for x in 0..8 {
            let bright = (x / 2 + y / 2) % 2 == 0;
            let color = if bright {
                [70, 190, 200, 255]
            } else {
                [110, 60, 205, 255]
            };
            let offset = (y * 8 + x) * 4;
            texels[offset..offset + 4].copy_from_slice(&color);
        }
    }
    texels
}

/// A radial gradient so the emissive pulse concentrates at each face's center.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub(crate) fn crystal_glow_texture() -> [u8; 8 * 8 * 4] {
    let mut texels = [0_u8; 8 * 8 * 4];
    for y in 0..8 {
        for x in 0..8 {
            let dx = x as f32 - 3.5;
            let dy = y as f32 - 3.5;
            let intensity = (1.0 - (dx * dx + dy * dy).sqrt() / 5.0).clamp(0.0, 1.0);
            let offset = (y * 8 + x) * 4;
            texels[offset..offset + 4].copy_from_slice(&[
                (250.0 * intensity) as u8,
                (120.0 * intensity) as u8,
                (255.0 * intensity) as u8,
                255,
            ]);
        }
    }
    texels
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub(crate) fn lava_texture() -> [u8; 16 * 16 * 4] {
    let mut texels = [0_u8; 16 * 16 * 4];
    for y in 0..16 {
        for x in 0..16 {
            let band = ((x as f32 * 0.9).sin() + (y as f32 * 0.7).cos() + 2.0) / 4.0;
            let heat = band.powi(2);
            let offset = (y * 16 + x) * 4;
            texels[offset..offset + 4].copy_from_slice(&[
                (140.0 + 115.0 * heat) as u8,
                (35.0 + 130.0 * heat) as u8,
                (20.0 + 35.0 * heat) as u8,
                255,
            ]);
        }
    }
    texels
}

/// A tapered tube of stacked rings: each ring blends the bone below and above its height, so
/// the strand bends smoothly when the palette sways. The tip closes with a small fan.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub(crate) fn kelp_vertices() -> Vec<u8> {
    let mut bytes = Vec::with_capacity((KELP_SIDES * (KELP_BONES + 1) + 1) * 56);
    for ring in 0..=KELP_BONES {
        let y = ring as f32 * KELP_SEGMENT_HEIGHT;
        let radius = KELP_RADIUS * (1.0 - 0.55 * ring as f32 / KELP_BONES as f32);
        let lower = ring.saturating_sub(1) as u32;
        let upper = ring.min(KELP_BONES - 1) as u32;
        for side in 0..KELP_SIDES {
            let angle = side as f32 * core::f32::consts::TAU / KELP_SIDES as f32;
            let (sin, cos) = angle.sin_cos();
            push_f32s(&mut bytes, &[radius * cos, y, radius * sin]);
            push_f32s(&mut bytes, &[cos, 0.0, sin]);
            push_u32s(&mut bytes, &[lower, upper, 0, 0]);
            push_f32s(&mut bytes, &[0.5, 0.5, 0.0, 0.0]);
        }
    }
    let top_bone = (KELP_BONES - 1) as u32;
    push_f32s(
        &mut bytes,
        &[0.0, KELP_BONES as f32 * KELP_SEGMENT_HEIGHT, 0.0],
    );
    push_f32s(&mut bytes, &[0.0, 1.0, 0.0]);
    push_u32s(&mut bytes, &[top_bone, top_bone, 0, 0]);
    push_f32s(&mut bytes, &[0.5, 0.5, 0.0, 0.0]);
    bytes
}

/// Outward-wound side quads between consecutive rings, then the tip fan.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn kelp_indices() -> Vec<u16> {
    let mut indices = Vec::with_capacity(KELP_BONES * KELP_SIDES * 6 + KELP_SIDES * 3);
    for ring in 0..KELP_BONES {
        for side in 0..KELP_SIDES {
            let a = (ring * KELP_SIDES + side) as u16;
            let b = (ring * KELP_SIDES + (side + 1) % KELP_SIDES) as u16;
            let c = a + KELP_SIDES as u16;
            let d = b + KELP_SIDES as u16;
            indices.extend_from_slice(&[a, d, b, a, c, d]);
        }
    }
    let top_ring = (KELP_BONES * KELP_SIDES) as u16;
    let center = top_ring + KELP_SIDES as u16;
    for side in 0..KELP_SIDES {
        indices.extend_from_slice(&[
            center,
            top_ring + ((side + 1) % KELP_SIDES) as u16,
            top_ring + side as u16,
        ]);
    }
    indices
}

/// Packs the strand's bone palette: an accumulated pivot chain, each bone swaying a little
/// later than the one below, with the strand's floor anchor baked into every matrix. The
/// matrices of neighboring bones agree exactly at their shared pivot, so blending two bones
/// per ring stays continuous across segment boundaries.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn kelp_bone_palette(seconds: f32) -> Vec<u8> {
    let mut current = Mat4::from_translation(Vec3::new(2.4, -1.6, -1.4));
    let mut bytes = Vec::with_capacity(KELP_BONES * 64);
    for bone in 0..KELP_BONES {
        let pivot = Vec3::new(0.0, bone as f32 * KELP_SEGMENT_HEIGHT, 0.0);
        let sway = 0.16 * (seconds * 1.1 + bone as f32 * 0.85).sin();
        let drift = 0.11 * (seconds * 0.7 + bone as f32 * 0.6).cos();
        current = current
            * Mat4::from_translation(pivot)
            * Mat4::from_rotation_z(sway)
            * Mat4::from_rotation_x(drift)
            * Mat4::from_translation(-pivot);
        push_f32s(&mut bytes, &current.to_cols_array());
    }
    bytes
}

/// Packs `SkinnedParams`: the camera's view-projection; the bones place the strand.
pub(crate) fn skinned_uniform(aspect: f32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64);
    push_f32s(&mut bytes, &view_projection(aspect).to_cols_array());
    bytes
}

/// Packs `SkinnedShadowParams`: one cascade's light view-projection over the same palette.
pub(crate) fn skinned_shadow_uniform(light_view_projection: Mat4) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64);
    push_f32s(&mut bytes, &light_view_projection.to_cols_array());
    bytes
}

fn camera_view() -> Mat4 {
    Mat4::from_rotation_x(0.32) * Mat4::from_translation(Vec3::new(0.0, -0.6, -7.5))
}

fn view_projection(aspect: f32) -> Mat4 {
    directx::perspective(55_f32.to_radians(), aspect, 0.1, 100.0) * camera_view()
}

/// One directional-light view-projection per shadow cascade, each fitted to its view-frustum
/// slice. Cascade policy — split distances, fitting, texel snapping — is application code;
/// the crate only ever sees the packed matrices and the layered map.
///
/// Each slice's eight frustum corners are wrapped in a bounding sphere, which keeps the
/// orthographic volume's size independent of camera rotation, and the sphere's center is
/// snapped to shadow-texel increments on the light's image plane so shadow edges do not
/// shimmer as the fitted volume moves. NDC z spans zero through one, matching the shadow
/// map's depth range.
pub(crate) fn cascade_light_view_projections(aspect: f32) -> [Mat4; CASCADE_COUNT] {
    #[allow(clippy::cast_precision_loss)]
    let map_extent = SHADOW_MAP_SIZE as f32;
    let light_direction = (Vec3::ZERO - LIGHT_EYE).normalize();
    let light_orientation = look_at_mat4(-light_direction, Vec3::ZERO, Vec3::Y);
    CASCADE_BOUNDS.map(|(near, far)| {
        let slice = directx::perspective(55_f32.to_radians(), aspect, near, far) * camera_view();
        let inverse = slice.inverse();
        let mut corners = [Vec3::ZERO; 8];
        let mut center = Vec3::ZERO;
        for (index, corner) in corners.iter_mut().enumerate() {
            let ndc = Vec4::new(
                if index & 1 == 0 { -1.0 } else { 1.0 },
                if index & 2 == 0 { -1.0 } else { 1.0 },
                if index & 4 == 0 { 0.0 } else { 1.0 },
                1.0,
            );
            let world = inverse * ndc;
            *corner = world.truncate() / world.w;
            center += *corner;
        }
        center /= 8.0;
        let radius = corners
            .iter()
            .map(|corner| corner.distance(center))
            .fold(0.0, f32::max);
        let texel = 2.0 * radius / map_extent;
        let on_light_plane = light_orientation.transform_point3(center);
        let snapped = Vec3::new(
            (on_light_plane.x / texel).floor() * texel,
            (on_light_plane.y / texel).floor() * texel,
            on_light_plane.z,
        );
        let center = light_orientation.inverse().transform_point3(snapped);
        let view = look_at_mat4(
            center - light_direction * (radius + CASTER_MARGIN),
            center,
            Vec3::Y,
        );
        let projection = directx::orthographic(
            -radius,
            radius,
            -radius,
            radius,
            0.0,
            2.0 * radius + CASTER_MARGIN,
        );
        projection * view
    })
}

fn crystal_model(seconds: f32, phase: f32, offset: Vec3) -> Mat4 {
    Mat4::from_translation(offset)
        * Mat4::from_rotation_y(seconds * 0.6 + phase)
        * Mat4::from_rotation_x(seconds * 0.35 + phase * 0.5)
}

/// Packs `CrystalParams`: model-view-projection, model, then seconds and pulse strength. The
/// application owns this WGSL memory layout.
pub(crate) fn crystal_uniform(seconds: f32, aspect: f32, phase: f32, offset: Vec3) -> Vec<u8> {
    let model = crystal_model(seconds, phase, offset);
    let mut bytes = Vec::with_capacity(144);
    push_f32s(
        &mut bytes,
        &(view_projection(aspect) * model).to_cols_array(),
    );
    push_f32s(&mut bytes, &model.to_cols_array());
    push_f32s(&mut bytes, &[seconds, 0.9, 0.0, 0.0]);
    bytes
}

/// Packs `ShadowParams` for one crystal in one cascade: that cascade's light view-projection
/// times the crystal's model transform.
pub(crate) fn crystal_shadow_uniform(
    light_view_projection: Mat4,
    seconds: f32,
    phase: f32,
    offset: Vec3,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64);
    push_f32s(
        &mut bytes,
        &(light_view_projection * crystal_model(seconds, phase, offset)).to_cols_array(),
    );
    bytes
}

/// Packs `LavaParams`: model-view-projection, then seconds.
pub(crate) fn lava_uniform(seconds: f32, aspect: f32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(80);
    push_f32s(&mut bytes, &view_projection(aspect).to_cols_array());
    push_f32s(&mut bytes, &[seconds, 0.0, 0.0, 0.0]);
    bytes
}

/// Packs `LavaCascades` read-only storage: one light-from-model matrix per cascade (the
/// floor's model is the identity), near to far.
pub(crate) fn lava_cascades(lights: &[Mat4; CASCADE_COUNT]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(CASCADE_COUNT * 64);
    for light in lights {
        push_f32s(&mut bytes, &light.to_cols_array());
    }
    bytes
}

/// Appends one axis-aligned HUD quad with a left-to-right color gradient; colors are supplied
/// straight (with alpha) and packed premultiplied for the overlay's translucent blend mode.
fn push_hud_quad(
    vertices: &mut Vec<u8>,
    indices: &mut Vec<u16>,
    min: (f32, f32),
    max: (f32, f32),
    left: Vec4,
    right: Vec4,
) {
    let base = u16::try_from(vertices.len() / HUD_LAYOUT.stride as usize)
        .expect("HUD overlay stays within 16-bit indexing");
    let corners = [
        (min.0, min.1, left),
        (max.0, min.1, right),
        (max.0, max.1, right),
        (min.0, max.1, left),
    ];
    for (x, y, color) in corners {
        push_f32s(
            vertices,
            &[
                x,
                y,
                color.x * color.w,
                color.y * color.w,
                color.z * color.w,
                color.w,
            ],
        );
    }
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Builds the frame's energy-gauge overlay in clip space: a translucent backplate, a fill bar
/// whose width breathes with time, and one divider tick per filled tenth, so both the vertex
/// and index counts genuinely change from frame to frame.
pub(crate) fn hud_geometry(seconds: f32) -> (Vec<u8>, Vec<u16>) {
    const LEFT: f32 = -0.92;
    const RIGHT: f32 = -0.22;
    const TOP: f32 = 0.90;
    const BOTTOM: f32 = 0.82;
    const PADDING: f32 = 0.012;
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    push_hud_quad(
        &mut vertices,
        &mut indices,
        (LEFT - PADDING, BOTTOM - PADDING),
        (RIGHT + PADDING, TOP + PADDING),
        Vec4::new(0.02, 0.05, 0.09, 0.6),
        Vec4::new(0.02, 0.05, 0.09, 0.6),
    );
    let energy = 0.5 + 0.5 * (seconds * 0.7).sin();
    let fill_right = LEFT + (RIGHT - LEFT) * energy.max(0.02);
    push_hud_quad(
        &mut vertices,
        &mut indices,
        (LEFT, BOTTOM),
        (fill_right, TOP),
        Vec4::new(0.05, 0.7, 0.55, 0.85),
        Vec4::new(0.5, 0.95, 0.6, 0.85),
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let filled_tenths = (energy * 10.0) as usize;
    for tenth in 1..=filled_tenths {
        #[allow(clippy::cast_precision_loss)]
        let x = LEFT + (RIGHT - LEFT) * (tenth as f32 / 10.0);
        push_hud_quad(
            &mut vertices,
            &mut indices,
            (x - 0.002, BOTTOM),
            (x + 0.002, TOP),
            Vec4::new(0.0, 0.0, 0.0, 0.7),
            Vec4::new(0.0, 0.0, 0.0, 0.7),
        );
    }
    (vertices, indices)
}
