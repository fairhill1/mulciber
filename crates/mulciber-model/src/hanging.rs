//! What hangs from a skeleton and swings as it moves: a skirt's chains of bones from the hips to
//! the hem, a coat's tails, hair. [`Hanging`] takes the nodes' matrices as the animation leaves
//! them and moves its chains as weights hung from their roots would move: falling, lagging behind
//! as the root sets off, swinging and settling, pushed aside by the [`Capsule`]s a game puts round the
//! legs, and drawn gently back to where the animation holds them, so they keep their shape. Each
//! chain's nodes are then turned to point where its points went, and the palette
//! ([`crate::Skeleton::palette`]'s matrices, from the nodes) carries the vertices weighted to them.
//!
//! Each chain is a node and the line of single children under it: the root rides the animation,
//! and every node under it is a point that moves. Neighbouring chains are kept within
//! [`Hang::spread`] of how far apart they are at rest, so a skirt's cloth neither bunches nor tears
//! between them; a closed ring of chains, a skirt's, is kept so all round.
//!
//! The points move in the world, not the model's frame, so setting off, turning or stopping leaves
//! them swinging: the step takes where the model is placed. Their speed is damped against where the
//! animation carries them, so going on steadily they go along with it. A [`Hang::leash`] keeps each
//! within an angle of where the animation holds it. They move in fixed steps of
//! [`Hang::step`], however long the frame, the animation and the capsules met between where they
//! were the frame before and where they are now. A model put down somewhere new (further than
//! [`Hang::snap`] in a frame) starts again from its pose.
//!
//! A [`Hanging::ground`], if the game gives one, is a floor they lie on rather than fall through:
//! a skirt crouched in, or a body fallen.

use crate::skeleton::inverse;
use crate::{IDENTITY, Matrix, Skeleton, add, cross, dot, multiply, normalize, scale, sub};

/// A capsule the hanging points are kept out of: a line between two points in a node's frame, and
/// how far round it at each end, narrowing evenly from one to the other (a thigh to the knee).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Capsule {
    /// Index into [`Skeleton::nodes`]: what carries it.
    pub node: usize,
    /// Its line's ends, in the node's frame.
    pub ends: [[f32; 3]; 2],
    /// How far round the line at each end, in the world's units.
    pub radii: [f32; 2],
}

/// How the chains hang and move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hang {
    /// Which way they fall and how fast, in the world's units a second each second.
    pub gravity: [f32; 3],
    /// How much of their speed goes each second, as a rate: after a second, `exp(-damping)` of it
    /// is left. Their speed past where the animation carries them, not through the world: cloth's
    /// own stiffness, which carries it along with what wears it, not the air's drag, which left a
    /// skirt hanging back behind a walker in proportion to how fast she went.
    pub damping: f32,
    /// How quickly they're drawn back to where the animation holds them, as a rate: after a second,
    /// `exp(-stiffness)` of how far off they were is left.
    pub stiffness: f32,
    /// How near and how far neighbouring chains' points may come, as shares of how far apart they
    /// are at rest.
    pub spread: [f32; 2],
    /// The longest step they move in, seconds.
    pub step: f32,
    /// The most steps a frame takes: a longer frame moves them less far than it lasted.
    pub most_steps: u32,
    /// How many times a step meets the lengths, the spread and the capsules.
    pub iterations: u32,
    /// The most a link turns from the one above it, radians; a chain's first, from the way the
    /// animation holds it. Cloth bends easily, but kicked by a running knee a skirt's hem folded
    /// right back up over it.
    pub bend: f32,
    /// How far a point may swing from where the animation holds it, as an angle from its chain's
    /// root: a point a length down the chain is kept within that length times twice the sine of half
    /// of it. Cloth hiding what's cut away under it (a skirt over thighs left out of the mesh) mustn't
    /// lift far off it, as stairs and blows flung it. Half a turn or more, no limit.
    pub leash: f32,
    /// A root moving further than this in a frame starts the chains again from the pose, in the
    /// world's units.
    pub snap: f32,
}

impl Hang {
    /// Falling `gravity`, a skirt's damping and stiffness, steps of a 120th of a second.
    #[must_use]
    pub fn new(gravity: [f32; 3]) -> Self {
        Self {
            gravity,
            damping: 20.0,
            stiffness: 6.0,
            spread: [0.5, 1.1],
            step: 1.0 / 120.0,
            most_steps: 8,
            iterations: 4,
            bend: 100f32.to_radians(),
            leash: std::f32::consts::PI,
            snap: 1.0,
        }
    }
}

/// A chain of nodes, its root first.
#[derive(Clone, Debug, PartialEq)]
struct Chain {
    nodes: Vec<usize>,
    /// Where its root's point is among [`Hanging`]'s, the rest following.
    first: usize,
}

/// Chains of nodes hanging from a skeleton, and where their points are and how fast they're going.
#[derive(Clone, Debug, PartialEq)]
pub struct Hanging {
    /// How they hang.
    pub hang: Hang,
    /// A plane the points are kept on the upper side of, in the world: its normal, of length 1 and
    /// pointing up from it, and how far along the normal from the world's origin it lies.
    pub ground: Option<[f32; 4]>,
    chains: Vec<Chain>,
    closed: bool,
    capsules: Vec<Capsule>,
    /// Each chain's points, the roots' among them, in the world.
    points: Vec<[f32; 3]>,
    velocities: Vec<[f32; 3]>,
    /// How far apart each point and the same of the next chain are at rest, in the model's frame;
    /// the last chain's from the first's when closed, nothing past the last otherwise.
    apart: Vec<Option<f32>>,
    /// Where the animation held each point and the capsules' ends last frame, in the world.
    held: Vec<[f32; 3]>,
    ends: Vec<[[f32; 3]; 2]>,
    ready: bool,
}

impl Hanging {
    /// The chains from each of `roots` (indices into [`Skeleton::nodes`]) down their lines of
    /// single children, in order round, `closed` if the last is the first's neighbour too, kept out
    /// of `capsules`.
    ///
    /// # Panics
    ///
    /// When a root isn't a node of the skeleton.
    #[must_use]
    pub fn new(
        skeleton: &Skeleton,
        roots: &[usize],
        closed: bool,
        capsules: Vec<Capsule>,
        hang: Hang,
    ) -> Self {
        let mut chains = Vec::with_capacity(roots.len());
        let mut first = 0;
        for &root in roots {
            assert!(root < skeleton.nodes.len(), "a node of the skeleton");
            let mut nodes = vec![root];
            while let Some(child) = skeleton
                .nodes
                .iter()
                .position(|n| n.parent == nodes.last().copied())
            {
                nodes.push(child);
            }
            let count = nodes.len();
            chains.push(Chain { nodes, first });
            first += count;
        }
        let rest = skeleton.node_matrices(&skeleton.rest_pose());
        let mut apart = vec![None; first];
        for (k, chain) in chains.iter().enumerate() {
            let next = match chains.get(k + 1) {
                Some(next) => next,
                None if closed && chains.len() > 2 => &chains[0],
                None => continue,
            };
            for (j, (&a, &b)) in chain.nodes.iter().zip(&next.nodes).enumerate() {
                apart[chain.first + j] = Some(length(sub(head(&rest[a]), head(&rest[b]))));
            }
        }
        Self {
            hang,
            ground: None,
            chains,
            closed,
            capsules,
            points: vec![[0.0; 3]; first],
            velocities: vec![[0.0; 3]; first],
            apart,
            held: vec![[0.0; 3]; first],
            ends: Vec::new(),
            ready: false,
        }
    }

    /// How many chains hang.
    #[must_use]
    pub fn chains(&self) -> usize {
        self.chains.len()
    }

    /// Where each chain's points are now, in the world, its root's first.
    pub fn points(&self) -> impl Iterator<Item = &[[f32; 3]]> {
        self.chains
            .iter()
            .map(|c| &self.points[c.first..c.first + c.nodes.len()])
    }

    /// Starts the chains again from the next pose, at rest.
    pub fn reset(&mut self) {
        self.ready = false;
    }

    /// Moves the chains on `dt` seconds, `nodes` the skeleton's matrices as the animation has them
    /// ([`Skeleton::node_matrices`]) and `place` what takes the model's frame to the world, and
    /// turns the chains' nodes in `nodes` to point where their points have gone.
    ///
    /// # Panics
    ///
    /// When `nodes` aren't the skeleton's this was made for.
    pub fn step(&mut self, nodes: &mut [Matrix], place: &Matrix, dt: f32) {
        let held: Vec<[f32; 3]> = self
            .chains
            .iter()
            .flat_map(|c| &c.nodes)
            .map(|&n| point(place, head(&nodes[n])))
            .collect();
        let ends: Vec<[[f32; 3]; 2]> = self
            .capsules
            .iter()
            .map(|c| {
                let carry = multiply(place, &nodes[c.node]);
                c.ends.map(|e| point(&carry, e))
            })
            .collect();
        let moved = self
            .chains
            .iter()
            .any(|c| length(sub(held[c.first], self.held[c.first])) > self.hang.snap);
        if !self.ready || moved || held.len() != self.held.len() {
            self.points.clone_from(&held);
            self.velocities.fill([0.0; 3]);
            self.held.clone_from(&held);
            self.ends.clone_from(&ends);
            self.ready = true;
        }
        let hang = self.hang;
        let wanted = (dt / hang.step).ceil();
        // At least one step, at most most_steps (a cast: wanted is whole and at most most_steps).
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let steps = (wanted.clamp(1.0, f32::from(u16::MAX)) as u32).min(hang.most_steps.max(1));
        #[allow(clippy::cast_precision_loss)]
        let h = (dt / steps as f32).min(hang.step).max(1e-6);
        let lengths: Vec<f32> = (0..held.len())
            .map(|k| {
                if self.chains.iter().any(|c| c.first == k) {
                    0.0
                } else {
                    length(sub(held[k], held[k - 1]))
                }
            })
            .collect();
        // How fast the animation carries each point, through the frame.
        let carried: Vec<[f32; 3]> = self
            .held
            .iter()
            .zip(&held)
            .map(|(&a, &b)| scale(sub(b, a), 1.0 / dt.max(1e-6)))
            .collect();
        for s in 1..=steps {
            #[allow(clippy::cast_precision_loss)]
            let t = s as f32 / steps as f32;
            let now: Vec<[f32; 3]> = self
                .held
                .iter()
                .zip(&held)
                .map(|(&a, &b)| mix(a, b, t))
                .collect();
            let capsules: Vec<([[f32; 3]; 2], [f32; 2])> = self
                .ends
                .iter()
                .zip(&ends)
                .zip(&self.capsules)
                .map(|((a, b), c)| ([mix(a[0], b[0], t), mix(a[1], b[1], t)], c.radii))
                .collect();
            self.move_by(h, &now, &carried, &lengths, &capsules);
        }
        self.held = held;
        self.ends = ends;
        self.pose(nodes, place);
    }

    /// One step of `h` seconds, the animation holding the points at `now` and carrying them at
    /// `carried`.
    fn move_by(
        &mut self,
        h: f32,
        now: &[[f32; 3]],
        carried: &[[f32; 3]],
        lengths: &[f32],
        capsules: &[([[f32; 3]; 2], [f32; 2])],
    ) {
        let hang = self.hang;
        let before = self.points.clone();
        let (kept, drawn) = ((-hang.damping * h).exp(), 1.0 - (-hang.stiffness * h).exp());
        for chain in &self.chains {
            let k = chain.first;
            self.points[k] = now[k];
            self.velocities[k] = [0.0; 3];
            let moving = k + 1..k + chain.nodes.len();
            for ((p, &v), &along) in self.points[moving.clone()]
                .iter_mut()
                .zip(&self.velocities[moving.clone()])
                .zip(&carried[moving])
            {
                let past = sub(add(v, scale(hang.gravity, h)), along);
                *p = add(*p, scale(add(along, scale(past, kept)), h));
            }
        }
        for _ in 0..hang.iterations {
            self.keep_lengths(lengths, now);
            self.keep_apart(hang.spread);
            self.keep_near(lengths, now);
            self.keep_out(capsules, now);
            self.keep_above();
        }
        for (k, v) in self.velocities.iter_mut().enumerate() {
            *v = scale(sub(self.points[k], before[k]), 1.0 / h);
        }
        // Drawn back to the animation's shape after their speed is taken, so it eases them there
        // rather than springing them past it, then the lengths and the capsules met again.
        for (p, &at) in self.points.iter_mut().zip(now) {
            *p = mix(*p, at, drawn);
        }
        self.keep_lengths(lengths, now);
        self.keep_near(lengths, now);
        // Again, as a push out of one capsule may push into another where they meet.
        for _ in 0..3 {
            self.keep_out(capsules, now);
        }
        self.keep_above();
    }

    /// Every point within [`Hang::leash`] of where the animation, `now`, holds it, `lengths` each
    /// link's.
    fn keep_near(&mut self, lengths: &[f32], now: &[[f32; 3]]) {
        if self.hang.leash >= std::f32::consts::PI {
            return;
        }
        let chord = 2.0 * (self.hang.leash.max(0.0) / 2.0).sin();
        for chain in &self.chains {
            let mut down = 0.0;
            for k in chain.first + 1..chain.first + chain.nodes.len() {
                down += lengths[k];
                let off = sub(self.points[k], now[k]);
                let (far, most) = (length(off), down * chord);
                if far > most {
                    self.points[k] = add(now[k], scale(off, most / far));
                }
            }
        }
    }

    /// Every point but the roots on the upper side of the ground, if there is one.
    fn keep_above(&mut self) {
        let Some([nx, ny, nz, d]) = self.ground else {
            return;
        };
        let normal = [nx, ny, nz];
        for chain in &self.chains {
            for p in &mut self.points[chain.first + 1..chain.first + chain.nodes.len()] {
                let below = d - dot(normal, *p);
                if below > 0.0 {
                    *p = add(*p, scale(normal, below));
                }
            }
        }
    }

    /// Each point as far from the one above as at rest, down from the root, its link turned no
    /// more than [`Hang::bend`] from the one above (the first, from where the animation, `now`,
    /// holds it).
    // Each point is met with the one above it, so by index.
    #[allow(clippy::needless_range_loop)]
    fn keep_lengths(&mut self, lengths: &[f32], now: &[[f32; 3]]) {
        let bend = self.hang.bend;
        for chain in &self.chains {
            let mut before = normalize(sub(now[chain.first + 1], now[chain.first]));
            for k in chain.first + 1..chain.first + chain.nodes.len() {
                let above = self.points[k - 1];
                let way = normalize_or(sub(self.points[k], above), before);
                let way = within(way, before, bend);
                self.points[k] = add(above, scale(way, lengths[k]));
                before = way;
            }
        }
    }

    /// Neighbouring chains' points within the spread of how far apart they are at rest; roots
    /// stay where the animation has them.
    fn keep_apart(&mut self, spread: [f32; 2]) {
        let count = self.chains.len();
        for (c, chain) in self.chains.iter().enumerate() {
            let next = if c + 1 < count {
                &self.chains[c + 1]
            } else if self.closed {
                &self.chains[0]
            } else {
                continue;
            };
            let depth = chain.nodes.len().min(next.nodes.len());
            for j in 1..depth {
                let (a, b) = (chain.first + j, next.first + j);
                let Some(rest) = self.apart[a] else { continue };
                let way = sub(self.points[b], self.points[a]);
                let d = length(way);
                let goal = d.clamp(rest * spread[0], rest * spread[1]);
                if d < 1e-9 || (goal - d).abs() < 1e-9 {
                    continue;
                }
                let half = scale(way, (d - goal) / d * 0.5);
                self.points[a] = add(self.points[a], half);
                self.points[b] = sub(self.points[b], half);
            }
        }
    }

    /// Every chain's lines between its points out of the capsules. A chain's first, from its root,
    /// keeps only its lower point out: the root may be in a capsule (where a thigh meets the hips),
    /// and pushed out along the line, its point was pushed further every step, as the line could
    /// never leave it, until it stood out level.
    #[allow(clippy::needless_range_loop)]
    fn keep_out(&mut self, capsules: &[([[f32; 3]; 2], [f32; 2])], now: &[[f32; 3]]) {
        for chain in &self.chains {
            for k in chain.first + 1..chain.first + chain.nodes.len() {
                let pinned = k - 1 == chain.first;
                for &([c0, c1], [r0, r1]) in capsules {
                    let (a, b) = (self.points[k - 1], self.points[k]);
                    let (u, along, on_line, on_axis) = if pinned {
                        let (_, along, _, on_axis) = closest(b, b, c0, c1);
                        (1.0, along, b, on_axis)
                    } else {
                        closest(a, b, c0, c1)
                    };
                    let radius = r0 + (r1 - r0) * along;
                    let off = sub(on_line, on_axis);
                    let d = length(off);
                    if d >= radius {
                        continue;
                    }
                    // Out the way the animation holds the point from the axis when it's on it.
                    let out = normalize_or(off, sub(now[k], on_axis));
                    let push = radius - d;
                    if pinned {
                        self.points[k] = add(b, scale(out, push));
                    } else {
                        let (wa, wb) = (1.0 - u, u);
                        let by = push / (wa * wa + wb * wb);
                        self.points[k - 1] = add(a, scale(out, by * wa));
                        self.points[k] = add(b, scale(out, by * wb));
                    }
                }
            }
        }
    }

    /// The chains' nodes in `nodes` turned from how the animation has them to point at where their
    /// points are, each at its own point; a chain's last node turned as the one above it.
    fn pose(&self, nodes: &mut [Matrix], place: &Matrix) {
        let unplace = inverse(place);
        for chain in &self.chains {
            let held: Vec<[f32; 3]> = chain.nodes.iter().map(|&n| head(&nodes[n])).collect();
            let at: Vec<[f32; 3]> = (0..chain.nodes.len())
                .map(|j| point(&unplace, self.points[chain.first + j]))
                .collect();
            let mut turn = IDENTITY;
            for (j, &n) in chain.nodes.iter().enumerate() {
                if j + 1 < chain.nodes.len() {
                    turn = turning(sub(held[j + 1], held[j]), sub(at[j + 1], at[j]));
                }
                let here = multiply(
                    &moved(at[j]),
                    &multiply(&turn, &moved(scale(held[j], -1.0))),
                );
                nodes[n] = multiply(&here, &nodes[n]);
            }
        }
    }
}

/// Where a matrix puts its origin.
fn head(m: &Matrix) -> [f32; 3] {
    [m[3][0], m[3][1], m[3][2]]
}

/// `p` as `m` carries it.
fn point(m: &Matrix, p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|r| m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r])
}

fn moved(by: [f32; 3]) -> Matrix {
    let mut m = IDENTITY;
    m[3] = [by[0], by[1], by[2], 1.0];
    m
}

fn length(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    add(a, scale(sub(b, a), t))
}

/// `a` made unit, or `or` when `a` has no length.
fn normalize_or(a: [f32; 3], or: [f32; 3]) -> [f32; 3] {
    if dot(a, a) > 1e-18 {
        normalize(a)
    } else {
        normalize(or)
    }
}

/// The unit way `way`, turned toward the unit way `from` until it's no more than `most` radians from
/// it.
fn within(way: [f32; 3], from: [f32; 3], most: f32) -> [f32; 3] {
    let cos = dot(way, from);
    if cos >= most.cos() {
        return way;
    }
    // Across `from` toward `way`; any way across when `way` is right back along it.
    let across = sub(way, scale(from, cos));
    let across = if dot(across, across) > 1e-12 {
        normalize(across)
    } else {
        let other = if from[0].abs() < 0.9 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        normalize(cross(from, other))
    };
    add(scale(from, most.cos()), scale(across, most.sin()))
}

/// The least turn that takes the way `from` to the way `to`, about the origin.
// The formula's own names.
#[allow(clippy::many_single_char_names)]
fn turning(from: [f32; 3], to: [f32; 3]) -> Matrix {
    let (from, to) = (normalize(from), normalize(to));
    let c = dot(from, to);
    let axis = cross(from, to);
    let s = length(axis);
    let axis = if s > 1e-7 {
        scale(axis, 1.0 / s)
    } else if c > 0.0 {
        return IDENTITY;
    } else {
        // Half a turn, about any line across `from`.
        let across = if from[0].abs() < 0.9 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        normalize(cross(from, across))
    };
    // Rodrigues: c I + s [axis]x + (1 - c) axis axisᵀ, column by column.
    let [x, y, z] = axis;
    let t = 1.0 - c;
    [
        [t * x * x + c, t * x * y + s * z, t * x * z - s * y, 0.0],
        [t * x * y - s * z, t * y * y + c, t * y * z + s * x, 0.0],
        [t * x * z + s * y, t * y * z - s * x, t * z * z + c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// The closest points of the lines from `a` to `b` and from `c` to `d`: how far along each its
/// point is (0 at `a` or `c`, 1 at `b` or `d`), and the two points.
#[allow(clippy::many_single_char_names)]
fn closest(a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]) -> (f32, f32, [f32; 3], [f32; 3]) {
    let (u, v, w) = (sub(b, a), sub(d, c), sub(a, c));
    let (uu, vv, uv, uw, vw) = (dot(u, u), dot(v, v), dot(u, v), dot(u, w), dot(v, w));
    let along_second = |s: f32| ((uv * s + vw) / vv.max(1e-12)).clamp(0.0, 1.0);
    let along_first = |t: f32| ((uv * t - uw) / uu.max(1e-12)).clamp(0.0, 1.0);
    let denominator = uu * vv - uv * uv;
    let s = if denominator > 1e-12 {
        ((uv * vw - vv * uw) / denominator).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let t = along_second(s);
    let s = along_first(t);
    let t = along_second(s);
    (s, t, add(a, scale(u, s)), add(c, scale(v, t)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Node, Transform};

    const DOWN: [f32; 3] = [0.0, 0.0, -9.81];

    /// Hips at a metre up, and under them `chains` chains of `links` nodes, each a tenth of a metre
    /// below the one above, the chains' roots round a circle of radius 0.2.
    fn skirt(chains: usize, links: usize) -> (Skeleton, Vec<usize>) {
        let mut nodes = vec![Node {
            name: Some("Hips".into()),
            parent: None,
            rest: Transform {
                translation: [0.0, 0.0, 1.0],
                ..Transform::default()
            },
            extras: None,
        }];
        let mut roots = Vec::new();
        for c in 0..chains {
            #[allow(clippy::cast_precision_loss)]
            let angle = c as f32 / chains as f32 * std::f32::consts::TAU;
            for l in 0..links {
                let translation = if l == 0 {
                    [0.2 * angle.cos(), 0.2 * angle.sin(), 0.0]
                } else {
                    [0.0, 0.0, -0.1]
                };
                if l == 0 {
                    roots.push(nodes.len());
                }
                let parent = if l == 0 { 0 } else { nodes.len() - 1 };
                nodes.push(Node {
                    name: None,
                    parent: Some(parent),
                    rest: Transform {
                        translation,
                        ..Transform::default()
                    },
                    extras: None,
                });
            }
        }
        let skeleton = Skeleton {
            nodes,
            joints: Vec::new(),
            root: IDENTITY,
        };
        (skeleton, roots)
    }

    fn rest(skeleton: &Skeleton) -> Vec<Matrix> {
        skeleton.node_matrices(&skeleton.rest_pose())
    }

    fn near(a: [f32; 3], b: [f32; 3], within: f32) -> bool {
        length(sub(a, b)) < within
    }

    #[test]
    fn a_chain_hangs_still_where_the_animation_holds_it() {
        let (skeleton, roots) = skirt(1, 4);
        let mut hanging = Hanging::new(&skeleton, &roots, false, Vec::new(), Hang::new(DOWN));
        for _ in 0..120 {
            let mut nodes = rest(&skeleton);
            hanging.step(&mut nodes, &IDENTITY, 1.0 / 60.0);
            for (n, m) in nodes.iter().zip(rest(&skeleton)) {
                assert!(near(head(n), head(&m), 1e-4));
            }
        }
    }

    #[test]
    fn a_chain_lags_as_its_root_sets_off_then_goes_along_with_it() {
        let (skeleton, roots) = skirt(1, 4);
        let mut hanging = Hanging::new(&skeleton, &roots, false, Vec::new(), Hang::new(DOWN));
        let tip = |hanging: &Hanging| hanging.points().next().unwrap()[3];
        let mut nodes = rest(&skeleton);
        hanging.step(&mut nodes, &IDENTITY, 1.0 / 60.0);
        // Carried along +x at a metre and a half a second: setting off, the tip trails; going on
        // steadily, it comes back under the root, carried along as cloth is by what wears it.
        let mut x = 0.0;
        for frame in 0..60 {
            x += 1.5 / 60.0;
            let mut nodes = rest(&skeleton);
            hanging.step(&mut nodes, &moved([x, 0.0, 0.0]), 1.0 / 60.0);
            if frame == 2 {
                let trailing = tip(&hanging);
                assert!(trailing[0] < x + 0.2 - 0.02, "the tip trails: {trailing:?}");
            }
        }
        assert!(
            near(tip(&hanging), [x + 0.2, 0.0, 0.7], 0.01),
            "going along: {:?}",
            tip(&hanging)
        );
        // Every link keeps its length.
        let points = hanging.points().next().unwrap().to_vec();
        for pair in points.windows(2) {
            assert!((length(sub(pair[1], pair[0])) - 0.1).abs() < 1e-3);
        }
        // Stopped, it comes back under the root.
        for _ in 0..240 {
            let mut nodes = rest(&skeleton);
            hanging.step(&mut nodes, &moved([x, 0.0, 0.0]), 1.0 / 60.0);
        }
        assert!(near(tip(&hanging), [x + 0.2, 0.0, 0.7], 0.005));
    }

    #[test]
    fn a_leashed_chain_trails_no_further_than_its_leash() {
        let (skeleton, roots) = skirt(1, 5);
        let mut hang = Hang::new(DOWN);
        hang.leash = 20f32.to_radians();
        let mut hanging = Hanging::new(&skeleton, &roots, false, Vec::new(), hang);
        let mut nodes = rest(&skeleton);
        hanging.step(&mut nodes, &IDENTITY, 1.0 / 60.0);
        // Snatched along +x at four metres a second: each point trails, but within the leash.
        let mut x = 0.0;
        for frame in 0..15 {
            x += 4.0 / 60.0;
            let mut nodes = rest(&skeleton);
            hanging.step(&mut nodes, &moved([x, 0.0, 0.0]), 1.0 / 60.0);
            let points = hanging.points().next().unwrap().to_vec();
            for (j, p) in points.iter().enumerate().skip(1) {
                #[allow(clippy::cast_precision_loss)]
                let down = 0.1 * j as f32;
                let most = down * 2.0 * 10f32.to_radians().sin();
                let held = [x + 0.2, 0.0, 1.0 - down];
                assert!(length(sub(*p, held)) < most + 1e-4, "{j}: {p:?}");
            }
            if frame == 2 {
                let tip = hanging.points().next().unwrap()[4];
                assert!(tip[0] < x + 0.2 - 0.05, "it trails: {tip:?}");
            }
        }
    }

    #[test]
    fn a_chain_lies_on_the_ground_rather_than_falling_through() {
        let (skeleton, roots) = skirt(1, 5);
        let mut hang = Hang::new(DOWN);
        hang.stiffness = 0.0;
        let mut hanging = Hanging::new(&skeleton, &roots, false, Vec::new(), hang);
        // A floor a quarter of a metre under the root, the chain's four links four tenths long.
        hanging.ground = Some([0.0, 0.0, 1.0, 0.75]);
        // Its root carried aside a little as it starts, as nothing falls quite straight.
        let mut nodes = rest(&skeleton);
        for frame in 0..240 {
            #[allow(clippy::cast_precision_loss)]
            let x = (frame.min(10) as f32) * 0.005;
            nodes = rest(&skeleton);
            hanging.step(&mut nodes, &moved([x, 0.0, 0.0]), 1.0 / 60.0);
        }
        let points = hanging.points().next().unwrap().to_vec();
        assert!(points.iter().all(|p| p[2] > 0.75 - 1e-4), "{points:?}");
        // Lying along it, its links their lengths, and the nodes where the points are (in the
        // model's frame, put down half a tenth along).
        assert!(points[4][2] < 0.75 + 1e-3, "{points:?}");
        for pair in points.windows(2) {
            assert!(
                (length(sub(pair[1], pair[0])) - 0.1).abs() < 2e-3,
                "{points:?}"
            );
        }
        for (j, n) in (roots[0]..roots[0] + 5).enumerate() {
            assert!(near(
                add(head(&nodes[n]), [0.05, 0.0, 0.0]),
                points[j],
                1e-4
            ));
        }
    }

    #[test]
    fn capsules_push_the_chain_aside() {
        let (skeleton, roots) = skirt(1, 5);
        // A leg's capsule through where the chain hangs, from the hips' frame.
        let leg = Capsule {
            node: 0,
            ends: [[0.15, 0.0, -0.15], [0.25, 0.0, -0.45]],
            radii: [0.08, 0.06],
        };
        let mut hanging = Hanging::new(&skeleton, &roots, false, vec![leg], Hang::new(DOWN));
        for _ in 0..60 {
            let mut nodes = rest(&skeleton);
            hanging.step(&mut nodes, &IDENTITY, 1.0 / 60.0);
        }
        let axis = [[0.15, 0.0, 0.85], [0.25, 0.0, 0.55]];
        let points = hanging.points().next().unwrap().to_vec();
        for pair in points.windows(2) {
            let (_, t, on_line, on_axis) = closest(pair[0], pair[1], axis[0], axis[1]);
            let radius = 0.08 - 0.02 * t;
            assert!(length(sub(on_line, on_axis)) > radius - 1e-3, "{points:?}");
        }
    }

    #[test]
    fn a_root_inside_a_capsule_leaves_its_chain_hanging() {
        let (skeleton, roots) = skirt(1, 5);
        // A thigh's top round the chain's root and down along it, a little to its side.
        let thigh = Capsule {
            node: 0,
            ends: [[0.25, 0.0, 0.05], [0.25, 0.0, -0.4]],
            radii: [0.1, 0.08],
        };
        let mut hanging = Hanging::new(&skeleton, &roots, false, vec![thigh], Hang::new(DOWN));
        for _ in 0..120 {
            let mut nodes = rest(&skeleton);
            hanging.step(&mut nodes, &IDENTITY, 1.0 / 60.0);
        }
        // Pushed aside, not flung up: every point well below the one above it.
        let points = hanging.points().next().unwrap().to_vec();
        for pair in points.windows(2) {
            assert!(pair[0][2] - pair[1][2] > 0.05, "{points:?}");
        }
    }

    #[test]
    fn neighbours_keep_within_their_spread() {
        let (skeleton, roots) = skirt(8, 4);
        let hang = Hang::new(DOWN);
        let mut hanging = Hanging::new(&skeleton, &roots, true, Vec::new(), hang);
        let mut nodes = rest(&skeleton);
        hanging.step(&mut nodes, &IDENTITY, 1.0 / 60.0);
        // Two neighbouring tips pulled far apart come back within the spread.
        let rest_apart = 2.0 * 0.2 * (std::f32::consts::PI / 8.0).sin();
        hanging.points[3][0] += 0.3;
        for _ in 0..20 {
            hanging.keep_apart(hang.spread);
        }
        let apart = length(sub(hanging.points[3], hanging.points[7]));
        assert!(apart <= rest_apart * hang.spread[1] + 1e-5, "{apart}");
    }

    #[test]
    fn a_model_put_down_elsewhere_starts_again() {
        let (skeleton, roots) = skirt(1, 3);
        let mut hanging = Hanging::new(&skeleton, &roots, false, Vec::new(), Hang::new(DOWN));
        let mut nodes = rest(&skeleton);
        hanging.step(&mut nodes, &IDENTITY, 1.0 / 60.0);
        let mut nodes = rest(&skeleton);
        hanging.step(&mut nodes, &moved([10.0, 0.0, 0.0]), 1.0 / 60.0);
        let points = hanging.points().next().unwrap().to_vec();
        assert!(near(points[2], [10.2, 0.0, 0.8], 1e-4), "{points:?}");
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn the_nodes_point_where_their_points_went_and_stay_rigid() {
        let (skeleton, roots) = skirt(1, 4);
        let mut hanging = Hanging::new(&skeleton, &roots, false, Vec::new(), Hang::new(DOWN));
        let mut nodes = rest(&skeleton);
        hanging.step(&mut nodes, &IDENTITY, 1.0 / 60.0);
        let mut x = 0.0;
        for _ in 0..10 {
            x += 0.03;
            nodes = rest(&skeleton);
            hanging.step(&mut nodes, &moved([x, 0.0, 0.0]), 1.0 / 60.0);
        }
        // In the model's frame, as the nodes are.
        let unplace = inverse(&moved([x, 0.0, 0.0]));
        let points: Vec<[f32; 3]> = hanging
            .points()
            .next()
            .unwrap()
            .iter()
            .map(|&p| point(&unplace, p))
            .collect();
        assert!(points[3][0] < 0.2 - 0.01, "the tip trails: {points:?}");
        for (j, &n) in hanging.chains[0].nodes.iter().enumerate() {
            let m = nodes[n];
            assert!(near(head(&m), points[j], 1e-4));
            // Still a turn: unit columns at right angles.
            let [a, b, c] = [0, 1, 2].map(|k| [m[k][0], m[k][1], m[k][2]]);
            for (p, q) in [(a, b), (b, c), (c, a)] {
                assert!(dot(p, q).abs() < 1e-4);
            }
            for p in [a, b, c] {
                assert!((length(p) - 1.0).abs() < 1e-4);
            }
            // Its own -z, the way to the next node at rest, points at the next point.
            if j + 1 < points.len() {
                let way = normalize(sub(points[j + 1], points[j]));
                assert!(dot(scale(c, -1.0), way) > 0.9999);
            }
        }
    }

    #[test]
    fn a_link_turns_no_further_than_the_bend_allows() {
        let down = [0.0, 0.0, -1.0];
        // Straight back up is turned to a right angle and a bit, toward any side.
        let up = within([0.0, 0.0, 1.0], down, 100f32.to_radians());
        assert!((dot(up, down).acos().to_degrees() - 100.0).abs() < 1e-3);
        // A way within it is left as it is.
        let aside = normalize([1.0, 0.0, -1.0]);
        assert_eq!(within(aside, down, 100f32.to_radians()), aside);
        // Turned toward the side it leans to.
        let back = within(normalize([1.0, 0.0, 0.2]), down, 60f32.to_radians());
        assert!(back[0] > 0.0 && (dot(back, down).acos().to_degrees() - 60.0).abs() < 1e-3);
    }

    #[test]
    fn closest_points_of_crossing_lines() {
        let (u, _, p, q) = closest(
            [-1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, -1.0, 1.0],
            [0.0, 1.0, 1.0],
        );
        assert!((u - 0.5).abs() < 1e-6);
        assert!(near(p, [0.0, 0.0, 0.0], 1e-6) && near(q, [0.0, 0.0, 1.0], 1e-6));
        // Past an end.
        let (u, _, p, _) = closest(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [3.0, -1.0, 0.0],
            [3.0, 1.0, 0.0],
        );
        assert!((u - 1.0).abs() < 1e-6 && near(p, [1.0, 0.0, 0.0], 1e-6));
    }
}
