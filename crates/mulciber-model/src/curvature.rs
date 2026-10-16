//! How curved a mesh is at each vertex, for shading that depends on it (pre-integrated skin).

use std::collections::HashMap;

/// How curved a mesh is at each vertex, per metre (one over the radius of the sphere it bends
/// like), clamped to `most`: how far the normals turn between a vertex and its neighbours over how
/// far apart they are, then evened out over its neighbourhood four times, so it follows the shape
/// (a cheek, a nose, an ear) rather than the triangles. Copies of a vertex at a seam in its texture
/// (the same position, to a hundredth of a millimetre) take one value.
///
/// For `mulciber-shader`'s skin table, pass its `SKIN_MOST_CURVATURE` as `most`. `positions` and
/// `normals` are one per vertex, and `indices` a triangle list, as a [`Part`](crate::Part) has them.
///
/// # Panics
///
/// If `normals` is shorter than `positions`, or an index is out of range.
#[must_use]
pub fn vertex_curvature(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    indices: &[u32],
    most: f32,
) -> Vec<f32> {
    let mut groups = HashMap::new();
    let mut remap = Vec::with_capacity(positions.len());
    let mut points = Vec::new();
    let mut directions: Vec<[f32; 3]> = Vec::new();
    for (position, normal) in positions.iter().zip(normals) {
        #[allow(clippy::cast_possible_truncation)]
        let key = position.map(|v| (v * 100_000.0).round() as i64);
        let group = *groups.entry(key).or_insert_with(|| {
            points.push(*position);
            directions.push([0.0; 3]);
            points.len() - 1
        });
        remap.push(group);
        let direction = &mut directions[group];
        for axis in 0..3 {
            direction[axis] += normal[axis];
        }
    }
    for direction in &mut directions {
        *direction = normalize_or_zero(*direction);
    }
    let mut neighbours = vec![Vec::new(); points.len()];
    for face in indices.as_chunks::<3>().0 {
        for edge in 0..3 {
            let a = remap[face[edge] as usize];
            let b = remap[face[(edge + 1) % 3] as usize];
            if a != b {
                neighbours[a].push(b);
                neighbours[b].push(a);
            }
        }
    }
    for adjacent in &mut neighbours {
        adjacent.sort_unstable();
        adjacent.dedup();
    }
    let mut values: Vec<f32> = neighbours
        .iter()
        .enumerate()
        .map(|(i, adjacent)| {
            let (mut apart, mut turned) = (0.0, 0.0);
            for &j in adjacent {
                apart += distance(points[i], points[j]);
                turned += distance(directions[i], directions[j]);
            }
            (turned / f32::max(apart, 1.0e-5)).clamp(0.0, most)
        })
        .collect();
    for _ in 0..4 {
        values = neighbours
            .iter()
            .enumerate()
            .map(|(i, adjacent)| {
                if adjacent.is_empty() {
                    return values[i];
                }
                #[allow(clippy::cast_precision_loss)]
                let mean = adjacent.iter().map(|&j| values[j]).sum::<f32>() / adjacent.len() as f32;
                0.5 * values[i] + 0.5 * mean
            })
            .collect();
    }
    remap.into_iter().map(|i| values[i]).collect()
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

/// `v` scaled to unit length, or zero when it has none to scale.
fn normalize_or_zero(v: [f32; 3]) -> [f32; 3] {
    let reciprocal = 1.0 / (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if reciprocal.is_finite() && reciprocal > 0.0 {
        v.map(|c| c * reciprocal)
    } else {
        [0.0; 3]
    }
}

#[cfg(test)]
mod tests {
    use super::vertex_curvature;

    const MOST: f32 = 500.0;

    /// A flat mesh isn't curved; a ball is curved one over its radius everywhere.
    #[test]
    fn flat_is_not_curved_and_a_ball_is_one_over_its_radius() {
        let points = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]];
        assert_eq!(
            vertex_curvature(&points, &[[0., 0., 1.]; 3], &[0, 1, 2], MOST),
            [0.; 3]
        );
        let normals: [[f32; 3]; 6] = [
            [1., 0., 0.],
            [-1., 0., 0.],
            [0., 1., 0.],
            [0., -1., 0.],
            [0., 0., 1.],
            [0., 0., -1.],
        ];
        let indices = [
            0, 2, 4, 2, 1, 4, 1, 3, 4, 3, 0, 4, 2, 0, 5, 1, 2, 5, 3, 1, 5, 0, 3, 5,
        ];
        for radius in [0.01, 0.05, 0.2_f32] {
            let positions = normals.map(|n| n.map(|c| c * radius));
            for k in vertex_curvature(&positions, &normals, &indices, MOST) {
                assert!((k - 1.0 / radius).abs() < 0.001, "{radius}: {k}");
            }
        }
        // Tighter than `most` is held at it.
        let positions = normals.map(|n| n.map(|c| c * 0.001));
        assert!(
            vertex_curvature(&positions, &normals, &indices, MOST)
                .iter()
                .all(|k| (k - MOST).abs() < 1.0e-3)
        );
    }

    /// The copies of a vertex either side of a seam in the texture are as curved as each other.
    #[test]
    fn a_seams_copies_are_as_curved() {
        let p = [
            [0., 0., 0.],
            [1., 0., 0.],
            [0., 1., 0.],
            [0., 0., 0.],
            [-1., 0., 0.],
        ];
        let n = [
            [0., 0., 1.],
            [0.2, 0., 0.98],
            [0., 0.1, 0.99],
            [0., 0., 1.],
            [-0.1, 0., 0.99],
        ];
        let k = vertex_curvature(&p, &n, &[0, 1, 2, 3, 2, 4], MOST);
        assert_eq!(k[0], k[3]);
        assert!(k.iter().all(|k| k.is_finite() && *k > 0.0));
    }
}
