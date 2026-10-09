//! A model's animations: keyframes moving its nodes, sampled into a [`Pose`] at any time.

use crate::skeleton::{Pose, lerp3, normalize4, slerp};

/// One of the file's animations.
#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    /// The animation's name, when it has one ("Walk", "Idle").
    pub name: Option<String>,
    /// Seconds to its last key.
    pub duration: f32,
    /// What it moves, a channel a node's translation, rotation or scale.
    pub channels: Vec<Channel>,
}

/// One property of one node, keyed over time.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    /// Index into [`crate::Skeleton::nodes`].
    pub node: usize,
    /// How it goes between keys.
    pub interpolation: Interpolation,
    /// Each key's time, seconds, rising.
    pub times: Vec<f32>,
    /// The keys' values: one per time, or for [`Interpolation::CubicSpline`] three (the in
    /// tangent, the value, the out tangent).
    pub keys: Keys,
}

/// How a channel goes between keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    /// Holds each key until the next.
    Step,
    /// Straight, rotations along the shorter arc.
    Linear,
    /// Hermite, with each key's tangents.
    CubicSpline,
}

/// What a channel keys.
#[derive(Clone, Debug, PartialEq)]
pub enum Keys {
    /// Where the node is in its parent's space.
    Translation(Vec<[f32; 3]>),
    /// Unit quaternions, `[x, y, z, w]`.
    Rotation(Vec<[f32; 4]>),
    /// Along each axis.
    Scale(Vec<[f32; 3]>),
}

impl Keys {
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Translation(v) | Self::Scale(v) => v.len(),
            Self::Rotation(v) => v.len(),
        }
    }
}

impl Animation {
    /// Sets the nodes it moves in `pose` to where they are `time` seconds in. Before the first key
    /// a channel holds its first value, after the last its last; looping is the caller's
    /// (`time % duration`). Nodes it doesn't move are left as they are, so animations can be
    /// layered.
    pub fn sample(&self, time: f32, pose: &mut Pose) {
        for channel in &self.channels {
            let Some(local) = pose.locals.get_mut(channel.node) else {
                continue;
            };
            match &channel.keys {
                Keys::Translation(v) => local.translation = channel.sample3(v, time),
                Keys::Scale(v) => local.scale = channel.sample3(v, time),
                Keys::Rotation(v) => local.rotation = channel.sample4(v, time),
            }
        }
    }
}

impl Channel {
    /// The key before `time` (clamped to the first and last), the one after, and how far between
    /// them `time` is, with the gap's length in seconds.
    fn span(&self, time: f32) -> (usize, usize, f32, f32) {
        let last = self.times.len() - 1;
        let next = self.times.partition_point(|&t| t <= time);
        if next == 0 {
            return (0, 0, 0.0, 0.0);
        }
        if next > last {
            return (last, last, 0.0, 0.0);
        }
        let (a, b) = (self.times[next - 1], self.times[next]);
        let gap = b - a;
        let t = if gap > 0.0 { (time - a) / gap } else { 0.0 };
        (next - 1, next, t, gap)
    }

    fn sample3(&self, values: &[[f32; 3]], time: f32) -> [f32; 3] {
        let (a, b, t, gap) = self.span(time);
        match self.interpolation {
            Interpolation::Step => values[a],
            Interpolation::Linear => lerp3(values[a], values[b], t),
            Interpolation::CubicSpline => {
                std::array::from_fn(|k| hermite(values, a, b, t, gap, |v: &[f32; 3]| v[k]))
            }
        }
    }

    fn sample4(&self, values: &[[f32; 4]], time: f32) -> [f32; 4] {
        let (a, b, t, gap) = self.span(time);
        match self.interpolation {
            Interpolation::Step => values[a],
            Interpolation::Linear => slerp(values[a], values[b], t),
            Interpolation::CubicSpline => normalize4(std::array::from_fn(|k| {
                hermite(values, a, b, t, gap, |v: &[f32; 4]| v[k])
            })),
        }
    }
}

/// glTF's cubic spline between keys `a` and `b`, `t` of the way along a `gap` of seconds: each key
/// is three values, in tangent, value, out tangent. Keys `a` and `b` the same (outside the keys),
/// the value itself.
fn hermite<V>(values: &[V], a: usize, b: usize, t: f32, gap: f32, c: impl Fn(&V) -> f32) -> f32 {
    let (p0, m0) = (c(&values[3 * a + 1]), c(&values[3 * a + 2]) * gap);
    if a == b {
        return p0;
    }
    let (p1, m1) = (c(&values[3 * b + 1]), c(&values[3 * b]) * gap);
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * p0
        + (t3 - 2.0 * t2 + t) * m0
        + (-2.0 * t3 + 3.0 * t2) * p1
        + (t3 - t2) * m1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton::Transform;

    fn pose() -> Pose {
        Pose {
            locals: vec![Transform::default()],
        }
    }

    fn moving(interpolation: Interpolation, times: Vec<f32>, values: Vec<[f32; 3]>) -> Animation {
        Animation {
            name: None,
            duration: *times.last().unwrap(),
            channels: vec![Channel {
                node: 0,
                interpolation,
                times,
                keys: Keys::Translation(values),
            }],
        }
    }

    #[test]
    fn keys_hold_outside_and_step_or_run_straight_between() {
        let walk = moving(
            Interpolation::Linear,
            vec![1.0, 2.0],
            vec![[0.0; 3], [10.0, 0.0, 0.0]],
        );
        let mut p = pose();
        for (time, x) in [(0.0, 0.0), (1.25, 2.5), (2.0, 10.0), (9.0, 10.0)] {
            walk.sample(time, &mut p);
            assert!((p.locals[0].translation[0] - x).abs() < 1e-5, "{time}");
        }
        let step = moving(
            Interpolation::Step,
            vec![1.0, 2.0],
            vec![[0.0; 3], [10.0, 0.0, 0.0]],
        );
        step.sample(1.9, &mut p);
        assert_eq!(p.locals[0].translation[0], 0.0);
    }

    #[test]
    fn a_cubic_spline_meets_its_keys_and_follows_its_tangents() {
        // From 0 to 1 over 2 s, flat at both ends: an ease in and out, halfway at the middle.
        let flat = [0.0; 3];
        let ease = moving(
            Interpolation::CubicSpline,
            vec![0.0, 2.0],
            vec![flat, [0.0; 3], flat, flat, [1.0, 0.0, 0.0], flat],
        );
        let mut p = pose();
        let x = |time: f32, p: &mut Pose| {
            ease.sample(time, p);
            p.locals[0].translation[0]
        };
        assert!((x(1.0, &mut p) - 0.5).abs() < 1e-5);
        assert!(x(0.5, &mut p) < 0.25, "eases in");
        assert!((x(2.0, &mut p) - 1.0).abs() < 1e-5);
        // A slope of 1 a second at the start: twice as far in the first moment.
        let slope = [1.0, 0.0, 0.0];
        let pushed = moving(
            Interpolation::CubicSpline,
            vec![0.0, 2.0],
            vec![flat, [0.0; 3], slope, flat, [1.0, 0.0, 0.0], flat],
        );
        pushed.sample(0.01, &mut p);
        assert!((p.locals[0].translation[0] - 0.01).abs() < 1e-3);
    }
}
