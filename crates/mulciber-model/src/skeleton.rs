//! A model's node hierarchy as it moves: [`Skeleton`], the [`Pose`] it is in, and the bone
//! matrices ([`Skeleton::palette`]) a skinned vertex shader blends each vertex's joints by.

use crate::{IDENTITY, Matrix, multiply};

/// A translation, rotation and scale, applied scale first: glTF's node transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// Where the node's origin is in its parent's space.
    pub translation: [f32; 3],
    /// A unit quaternion, `[x, y, z, w]`.
    pub rotation: [f32; 4],
    /// Along each axis.
    pub scale: [f32; 3],
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
        }
    }
}

impl Transform {
    /// As a matrix: translation × rotation × scale.
    #[must_use]
    pub fn matrix(&self) -> Matrix {
        let [x, y, z, w] = self.rotation;
        let [sx, sy, sz] = self.scale;
        let [tx, ty, tz] = self.translation;
        [
            [
                (1.0 - 2.0 * (y * y + z * z)) * sx,
                2.0 * (x * y + z * w) * sx,
                2.0 * (x * z - y * w) * sx,
                0.0,
            ],
            [
                2.0 * (x * y - z * w) * sy,
                (1.0 - 2.0 * (x * x + z * z)) * sy,
                2.0 * (y * z + x * w) * sy,
                0.0,
            ],
            [
                2.0 * (x * z + y * w) * sz,
                2.0 * (y * z - x * w) * sz,
                (1.0 - 2.0 * (x * x + y * y)) * sz,
                0.0,
            ],
            [tx, ty, tz, 1.0],
        ]
    }

    /// `t` of the way from `self` to `other`: translation and scale straight, the rotation along
    /// the shorter arc ([`slerp`]).
    #[must_use]
    pub fn lerp(&self, other: &Self, t: f32) -> Self {
        Self {
            translation: lerp3(self.translation, other.translation, t),
            rotation: slerp(self.rotation, other.rotation, t),
            scale: lerp3(self.scale, other.scale, t),
        }
    }
}

/// A node of the model's scene.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// The node's name, when it has one: how a game finds a hand to hold something in.
    pub name: Option<String>,
    /// Index into [`Skeleton::nodes`], always before this one; `None` for a root of the scene.
    pub parent: Option<usize>,
    /// Where it stands when nothing moves it.
    pub rest: Transform,
}

/// One bone matrix of the palette: the node whose movement it follows, and what takes a vertex
/// from the model as it was bound into that node's space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Joint {
    /// Index into [`Skeleton::nodes`]; `None` for vertices that don't move (the identity).
    pub node: Option<usize>,
    /// glTF's inverse bind matrix, or the inverse of the node's rest transform for a mesh carried
    /// rigidly by it.
    pub inverse_bind: Matrix,
}

/// Every node of a model's scene, parents before children, and the joints its vertices are bound
/// to (each [`crate::Part::joints`] entry indexes [`Skeleton::joints`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Skeleton {
    /// Every node of the scene, parents before children.
    pub nodes: Vec<Node>,
    /// The palette's joints, as the meshes first meet them: each skin's, in the skin's order, a
    /// joint for each node carrying rigid meshes, and one that never moves.
    pub joints: Vec<Joint>,
    /// What [`crate::Model::transform`] has moved the model by: the nodes' transforms stay in the
    /// file's frame, and their matrices are moved by it on the way out.
    pub root: Matrix,
}

/// Each node's transform, by [`Skeleton::nodes`] index: where an animation (or a game) has put
/// it.
#[derive(Clone, Debug, PartialEq)]
pub struct Pose {
    /// Each node's transform in its parent's space.
    pub locals: Vec<Transform>,
}

impl Pose {
    /// `t` of the way from this pose to `other`, node by node ([`Transform::lerp`]): a cross-fade
    /// from one animation to the next, or a walk mixed with a run by speed.
    ///
    /// # Panics
    ///
    /// When the poses are of skeletons with different numbers of nodes.
    pub fn blend(&mut self, other: &Self, t: f32) {
        assert_eq!(
            self.locals.len(),
            other.locals.len(),
            "poses of one skeleton"
        );
        for (a, b) in self.locals.iter_mut().zip(&other.locals) {
            *a = a.lerp(b, t);
        }
    }

    /// As [`Pose::blend`], but only the nodes `mask` holds ([`Skeleton::subtree`]): a swing's
    /// upper body over a walk's legs. Blending twice, a little at the spine and wholly from the
    /// chest up, feathers the seam.
    ///
    /// # Panics
    ///
    /// When the poses or the mask are of skeletons with different numbers of nodes.
    pub fn blend_masked(&mut self, other: &Self, t: f32, mask: &[bool]) {
        assert_eq!(
            (self.locals.len(), mask.len()),
            (other.locals.len(), other.locals.len()),
            "poses and a mask of one skeleton"
        );
        for ((a, b), _) in self
            .locals
            .iter_mut()
            .zip(&other.locals)
            .zip(mask)
            .filter(|(_, m)| **m)
        {
            *a = a.lerp(b, t);
        }
    }
}

impl Skeleton {
    /// Every node at rest.
    #[must_use]
    pub fn rest_pose(&self) -> Pose {
        Pose {
            locals: self.nodes.iter().map(|n| n.rest).collect(),
        }
    }

    /// Which nodes are `node` or under it, by [`Skeleton::nodes`] index: the mask for
    /// [`Pose::blend_masked`].
    #[must_use]
    pub fn subtree(&self, node: usize) -> Vec<bool> {
        let mut under = vec![false; self.nodes.len()];
        // Parents come first, so each node's parent is settled before it.
        for k in 0..self.nodes.len() {
            under[k] = k == node || self.nodes[k].parent.is_some_and(|p| under[p]);
        }
        under
    }

    /// The first node called `name`.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<usize> {
        self.nodes
            .iter()
            .position(|n| n.name.as_deref() == Some(name))
    }

    /// Each node's matrix in `pose`, in the model's frame (after [`crate::Model::transform`]): what
    /// takes a point in the node's own space to the model's. For attaching things to a node.
    ///
    /// # Panics
    ///
    /// When `pose` isn't of this skeleton.
    #[must_use]
    pub fn node_matrices(&self, pose: &Pose) -> Vec<Matrix> {
        assert_eq!(
            pose.locals.len(),
            self.nodes.len(),
            "a pose of this skeleton"
        );
        let mut globals: Vec<Matrix> = Vec::with_capacity(self.nodes.len());
        for (node, local) in self.nodes.iter().zip(&pose.locals) {
            let parent = node.parent.map_or(&self.root, |p| &globals[p]);
            globals.push(multiply(parent, &local.matrix()));
        }
        globals
    }

    /// The bone matrices for `pose`, one per [`Skeleton::joints`] entry: what takes a vertex as
    /// loaded (and transformed) to where the pose puts it, blended by the vertex's weights.
    ///
    /// # Panics
    ///
    /// When `pose` isn't of this skeleton.
    #[must_use]
    pub fn palette(&self, pose: &Pose) -> Vec<Matrix> {
        let nodes = self.node_matrices(pose);
        let unroot = inverse(&self.root);
        self.joints
            .iter()
            .map(|joint| match joint.node {
                Some(n) => multiply(&multiply(&nodes[n], &joint.inverse_bind), &unroot),
                None => IDENTITY,
            })
            .collect()
    }
}

/// The inverse of an affine matrix (its last row 0, 0, 0, 1); the identity for one that flattens
/// space.
#[must_use]
pub fn inverse(matrix: &Matrix) -> Matrix {
    let [x, y, z] = [0, 1, 2].map(|k| [matrix[k][0], matrix[k][1], matrix[k][2]]);
    let det = crate::dot(x, crate::cross(y, z));
    if det.abs() < 1e-12 {
        return IDENTITY;
    }
    // The inverse's rows are the cofactors, over the determinant.
    let rows = [crate::cross(y, z), crate::cross(z, x), crate::cross(x, y)]
        .map(|row| crate::scale(row, 1.0 / det));
    let offset = [matrix[3][0], matrix[3][1], matrix[3][2]];
    let moved = rows.map(|row| -crate::dot(row, offset));
    [
        [rows[0][0], rows[1][0], rows[2][0], 0.0],
        [rows[0][1], rows[1][1], rows[2][1], 0.0],
        [rows[0][2], rows[1][2], rows[2][2], 0.0],
        [moved[0], moved[1], moved[2], 1.0],
    ]
}

pub(crate) fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t)
}

/// `t` of the way from unit quaternion `a` to `b` along the shorter arc, unit.
#[must_use]
pub fn slerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut cos = (0..4).map(|k| a[k] * b[k]).sum::<f32>();
    let b = if cos < 0.0 {
        cos = -cos;
        b.map(|c| -c)
    } else {
        b
    };
    // Nearly the same rotation: straight, then made unit.
    let (wa, wb) = if cos > 0.9995 {
        (1.0 - t, t)
    } else {
        let angle = cos.acos();
        let sin = angle.sin();
        (((1.0 - t) * angle).sin() / sin, (t * angle).sin() / sin)
    };
    normalize4(std::array::from_fn(|k| a[k] * wa + b[k] * wb))
}

pub(crate) fn normalize4(q: [f32; 4]) -> [f32; 4] {
    let length = q.iter().map(|c| c * c).sum::<f32>().sqrt();
    if length > 1e-12 {
        q.map(|c| c / length)
    } else {
        [0.0, 0.0, 0.0, 1.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &Matrix, b: &Matrix) -> bool {
        a.iter()
            .flatten()
            .zip(b.iter().flatten())
            .all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn a_transform_scales_then_turns_then_moves() {
        // A quarter turn about z.
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let t = Transform {
            translation: [5.0, 0.0, 0.0],
            rotation: [0.0, 0.0, half, half],
            scale: [2.0, 1.0, 1.0],
        };
        let m = t.matrix();
        // +x, doubled, turned to +y, then moved.
        assert!(close(
            &[m[0], m[1], m[2], m[3]],
            &[
                [0.0, 2.0, 0.0, 0.0],
                [-1.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [5.0, 0.0, 0.0, 1.0],
            ]
        ));
        assert!(close(&multiply(&m, &inverse(&m)), &IDENTITY));
    }

    #[test]
    fn a_masked_blend_takes_only_the_subtree() {
        // Hips, with legs under them and a spine under them with an arm.
        let node = |parent: Option<usize>| Node {
            name: None,
            parent,
            rest: Transform::default(),
        };
        let skeleton = Skeleton {
            nodes: vec![node(None), node(Some(0)), node(Some(0)), node(Some(2))],
            joints: Vec::new(),
            root: IDENTITY,
        };
        let upper = skeleton.subtree(2);
        assert_eq!(upper, [false, false, true, true]);
        // A walk moves everything one way; a swing everything another; the swing's upper body
        // goes over the walk's legs.
        let pose = |x: f32| Pose {
            locals: vec![
                Transform {
                    translation: [x, 0.0, 0.0],
                    ..Transform::default()
                };
                4
            ],
        };
        let mut walk = pose(1.0);
        walk.blend_masked(&pose(5.0), 1.0, &upper);
        let x: Vec<f32> = walk.locals.iter().map(|t| t.translation[0]).collect();
        assert_eq!(x, [1.0, 1.0, 5.0, 5.0]);
        // Half in, at the seam.
        walk.blend_masked(&pose(3.0), 0.5, &skeleton.subtree(3));
        assert_eq!(walk.locals[3].translation[0], 4.0);
    }

    #[test]
    fn slerp_takes_the_shorter_arc_at_an_even_pace() {
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let quarter = [0.0, 0.0, half, half];
        // An eighth of a turn, halfway to a quarter.
        let eighth = slerp([0.0, 0.0, 0.0, 1.0], quarter, 0.5);
        let angle = 2.0 * eighth[2].atan2(eighth[3]);
        assert!((angle - std::f32::consts::FRAC_PI_4).abs() < 1e-5);
        // The same quarter turn written negated is still a quarter turn away, not three.
        let negated = slerp([0.0, 0.0, 0.0, 1.0], quarter.map(|c| -c), 0.5);
        assert!((2.0 * negated[2].atan2(negated[3]) - std::f32::consts::FRAC_PI_4).abs() < 1e-5);
    }
}
