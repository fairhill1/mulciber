//! `MikkTSpace` tangents for a primitive that has none.
//!
//! `MikkTSpace` works on triangle corners, and a vertex shared by triangles either side of a UV
//! seam or a mirror can get a different tangent in each. The corners are read through the
//! indices, and each vertex is then split into as many as there are distinct tangents among its
//! corners: one, where the mesh is smooth in UV.
//!
//! glTF's v runs down the image, and its normal maps' green up it: the bitangent is the way up the
//! image. `MikkTSpace` is given v turned over, as Blender computes it before exporting with v
//! turned for glTF, so the handedness generated here is the one a file exported with tangents
//! states.

use std::collections::HashMap;

use bevy_mikktspace::{Geometry, TangentSpace, generate_tangents};

use crate::{Piece, cross, normalize};

struct Corners<'a> {
    piece: &'a Piece,
    tangents: Vec<[f32; 4]>,
}

impl Corners<'_> {
    fn vertex(&self, face: usize, vert: usize) -> usize {
        self.piece.indices[face * 3 + vert] as usize
    }
}

impl Geometry for Corners<'_> {
    fn num_faces(&self) -> usize {
        self.piece.indices.len() / 3
    }

    fn num_vertices_of_face(&self, _face: usize) -> usize {
        3
    }

    fn position(&self, face: usize, vert: usize) -> [f32; 3] {
        self.piece.positions[self.vertex(face, vert)]
    }

    fn normal(&self, face: usize, vert: usize) -> [f32; 3] {
        self.piece.normals[self.vertex(face, vert)]
    }

    fn tex_coord(&self, face: usize, vert: usize) -> [f32; 2] {
        let [u, v] = self.piece.uvs[self.vertex(face, vert)];
        [u, 1.0 - v]
    }

    fn set_tangent(&mut self, tangent_space: Option<TangentSpace>, face: usize, vert: usize) {
        let normal = self.normal(face, vert);
        self.tangents[face * 3 + vert] =
            tangent_space.map_or_else(|| across(normal), |t| t.tangent_encoded());
    }
}

/// Some unit tangent perpendicular to `normal`, for a corner `MikkTSpace` left without one (no
/// texture coordinates, or a degenerate triangle alone).
fn across(normal: [f32; 3]) -> [f32; 4] {
    let axis = if normal[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let [x, y, z] = normalize(cross(cross(normal, axis), normal));
    [x, y, z, 1.0]
}

/// Fills `piece.tangents`, splitting vertices whose corners' tangents differ.
pub(crate) fn generate(piece: &mut Piece) {
    let mut corners = Corners {
        piece,
        tangents: vec![[0.0; 4]; piece.indices.len()],
    };
    // The crate's error type has no variants: it can't fail.
    let _ = generate_tangents(&mut corners);
    let tangents = corners.tangents;

    let mut split: HashMap<(u32, [u32; 4]), u32> = HashMap::new();
    let mut out = Piece {
        positions: Vec::with_capacity(piece.positions.len()),
        normals: Vec::with_capacity(piece.positions.len()),
        tangents: Vec::with_capacity(piece.positions.len()),
        uvs: Vec::with_capacity(piece.positions.len()),
        indices: Vec::with_capacity(piece.indices.len()),
    };
    for (corner, &vertex) in piece.indices.iter().enumerate() {
        let tangent = tangents[corner];
        let index = *split
            .entry((vertex, tangent.map(f32::to_bits)))
            .or_insert_with(|| {
                let v = vertex as usize;
                out.positions.push(piece.positions[v]);
                out.normals.push(piece.normals[v]);
                out.uvs.push(piece.uvs[v]);
                out.tangents.push(tangent);
                u32::try_from(out.positions.len() - 1).expect("fits u32")
            });
        out.indices.push(index);
    }
    *piece = out;
}
