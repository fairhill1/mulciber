//! What hangs from a skeleton and swings as it moves: a skirt's chains of bones from the hips to
//! the hem, a coat's tails, hair. [`Hanging`] takes the nodes' matrices as the animation leaves
//! them and moves its chains as weights hung from their roots would move: falling, lagging behind
//! as the root sets off, swinging and settling, pushed aside by the [`Capsule`]s a game puts round the
//! legs, and drawn gently back to where the animation holds them, so they keep their shape. Each
//! chain's nodes are then turned to point where its points went, and the palette
//! ([`crate::Skeleton::palette`]'s matrices, from the nodes) carries the vertices weighted to them.
//!
//! Each chain is a node and the line of single children under it: the root rides the animation,
//! and every node under it is a point that moves. Neighbouring chains are woven together, the cloth
//! between them straight across and corner to corner drawn toward its width at rest as by springs
//! ([`Hang::weave`]) and kept within [`Hang::spread`] of it; a closed ring of chains, a skirt's,
//! all round. That cloth is kept out of the capsules too, so a knee raised or stepped out pushes it
//! along rather than coming out between the chains. Whatever a capsule meets is pushed back out the
//! side it came from, so a leg sweeping through can't fling it out the far side, and where it
//! touches, friction holds it ([`Hang::friction`]).
//!
//! The points move in the world, not the model's frame, so setting off, turning or stopping leaves
//! them swinging: the step takes where the model is placed. Their speed is damped against where the
//! animation carries them, so going on steadily they go along with it. A [`Hang::leash`] reels each
//! back within an angle of where the animation holds it. They move in fixed steps of
//! [`Hang::step`], however long the frame, the animation and the capsules met between where they
//! were the frame before and where they are now. A model put down somewhere new (further than
//! [`Hang::snap`] in a frame) starts again from its pose.
//!
//! A [`Hanging::ground`], if the game gives one, is a floor they lie on rather than fall through:
//! a skirt crouched in, or a body fallen. Capsules carried by nothing of the skeleton, in the world,
//! may be given each frame too ([`Around`], [`Hanging::step_among`]): someone walking into a rope
//! hung from a ceiling.

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

/// A capsule in the world the hanging points are kept out of, carried by none of the skeleton's
/// nodes: someone walking into a rope hung from a ceiling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Around {
    /// Its line's ends this frame, in the world.
    pub ends: [[f32; 3]; 2],
    /// How far round the line at each end, narrowing evenly from one to the other, in the world's
    /// units.
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
    /// How stiffly the cloth between neighbouring chains keeps its width at rest, as a spring's
    /// stiffness a unit of weight at each point, per second squared: straight across and corner to
    /// corner, so it stretches round a knee and gathers, as cloth does, rather than being held to
    /// the width at rest; hard limits on it were met against the capsules each step and shook.
    pub weave: f32,
    /// How near and how far neighbouring chains' points may come at most, as shares of how far
    /// apart they are at rest.
    pub spread: [f32; 2],
    /// How quickly a point touching a capsule loses its speed along it, as a rate: after a second,
    /// `exp(-friction)` of it is left.
    pub friction: f32,
    /// The step they move in, seconds: always the same, what's left of a frame carried on to the
    /// next, so they move alike at any frame rate. Each frame cut into steps to fit it, a skirt
    /// moved otherwise at 75 frames a second than at 60: crouched, a shin swept into the slim
    /// gown's hem caught it at one and passed through it at the other.
    pub step: f32,
    /// The most steps a frame takes: a longer frame moves them less far than it lasted.
    pub most_steps: u32,
    /// How many times a step meets the lengths, the spread and the capsules.
    pub iterations: u32,
    /// The most a link turns from the one above it, radians; a chain's first, from the way the
    /// animation holds it. Cloth bends easily, but kicked by a running knee a skirt's hem folded
    /// right back up over it. It may always turn as far as hanging straight down.
    pub bend: f32,
    /// How far a point may swing from where the animation holds it, as an angle from its chain's
    /// root (a point a length down the chain, that length times twice the sine of half of it):
    /// further, it goes no further away and is reeled back in ([`Hang::reel`]). Cloth hiding what's
    /// cut away under it (a skirt over thighs left out of the mesh) mustn't lift far off it, as
    /// stairs and blows flung it. Half a turn or more, no limit. Not a wall it's held to: a capsule
    /// may push it further, crouched a thigh the gown's back, which, pulled back into the thigh
    /// and pushed out of it again each step, shook and sprang loose.
    pub leash: f32,
    /// How quickly a point beyond its leash is reeled back in, as a rate: after a second,
    /// `exp(-reel)` of how far beyond it was is left, if nothing else holds it there. Gently: at 15
    /// or more, the gown's back, which a crouched thigh holds out past its leash, sprang about.
    pub reel: f32,
    /// A root moving further than this in a frame starts the chains again from the pose, in the
    /// world's units.
    pub snap: f32,
    /// How much of the way each point's lengths moved the point below it is taken back out of its
    /// speed, 0 to 1 (Müller et al.'s follow-the-leader correction, "Fast Simulation of Inextensible
    /// Hair and Fur", 2012). Each link's length is kept by moving its lower point alone, and with
    /// light damping that fed a long chain more speed than it lost, until a rope of ten links
    /// swung on its own: 0.9 or so takes it back. None by default, as a damped skirt needs none.
    pub follow: f32,
}

impl Hang {
    /// Falling `gravity`, a skirt's damping and stiffness, steps of a 120th of a second.
    #[must_use]
    pub fn new(gravity: [f32; 3]) -> Self {
        Self {
            gravity,
            damping: 20.0,
            stiffness: 6.0,
            weave: 2000.0,
            spread: [0.3, 2.0],
            friction: 20.0,
            step: 1.0 / 120.0,
            most_steps: 8,
            iterations: 4,
            bend: 100f32.to_radians(),
            leash: std::f32::consts::PI,
            reel: 5.0,
            snap: 1.0,
            follow: 0.0,
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
    /// Where they were at the step's start, and the capsules then.
    start: Vec<[f32; 3]>,
    last: Vec<Kept>,
    /// Each point touching a capsule this step: the way out of it, and how fast the capsule goes
    /// there.
    touching: Vec<Option<([f32; 3], [f32; 3])>>,
    /// The cloth between neighbouring chains: two points, and how far apart they are at rest.
    woven: Vec<(usize, usize, f32)>,
    /// How far apart each point and the same of the next chain are at rest, in the model's frame;
    /// the last chain's from the first's when closed, nothing past the last otherwise.
    apart: Vec<Option<f32>>,
    /// Where the animation held each point and the capsules' ends last frame, in the world.
    held: Vec<[f32; 3]>,
    ends: Vec<[[f32; 3]; 2]>,
    /// The seconds of the frames so far not yet stepped.
    leftover: f32,
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
        let mut woven = Vec::new();
        for (k, chain) in chains.iter().enumerate() {
            let next = match chains.get(k + 1) {
                Some(next) => next,
                None if closed && chains.len() > 2 => &chains[0],
                None => continue,
            };
            for (j, (&a, &b)) in chain.nodes.iter().zip(&next.nodes).enumerate() {
                apart[chain.first + j] = Some(length(sub(head(&rest[a]), head(&rest[b]))));
            }
            let depth = chain.nodes.len().min(next.nodes.len());
            let apart = |a: usize, b: usize| length(sub(head(&rest[a]), head(&rest[b])));
            for j in 0..depth {
                let (a, b) = (chain.nodes[j], next.nodes[j]);
                if j > 0 {
                    woven.push((chain.first + j, next.first + j, apart(a, b)));
                }
                if j + 1 < depth {
                    let (below_a, below_b) = (chain.nodes[j + 1], next.nodes[j + 1]);
                    woven.push((chain.first + j, next.first + j + 1, apart(a, below_b)));
                    woven.push((next.first + j, chain.first + j + 1, apart(b, below_a)));
                }
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
            start: Vec::new(),
            last: Vec::new(),
            touching: vec![None; first],
            woven,
            apart,
            held: vec![[0.0; 3]; first],
            ends: Vec::new(),
            leftover: 0.0,
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
        self.step_among(nodes, place, dt, &[]);
    }

    /// As [`Hanging::step`], the chains kept out of the capsules `around` in the world too, each
    /// where it is now through the whole frame.
    ///
    /// # Panics
    ///
    /// When `nodes` aren't the skeleton's this was made for.
    pub fn step_among(&mut self, nodes: &mut [Matrix], place: &Matrix, dt: f32, around: &[Around]) {
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
            self.last.clear();
            self.held.clone_from(&held);
            self.ends.clone_from(&ends);
            self.leftover = 0.0;
            self.ready = true;
        }
        let hang = self.hang;
        let h = hang.step.max(1e-6);
        let total = self.leftover + dt.max(0.0);
        let wanted = (total / h).floor();
        let most = hang.most_steps.max(1);
        // At most most_steps (a cast: wanted is whole and not below none).
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let steps = (wanted.min(f32::from(u16::MAX)) as u32).min(most);
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
            // How far through the frame this step ends.
            #[allow(clippy::cast_precision_loss)]
            let t = ((s as f32 * h - self.leftover) / dt.max(1e-6)).clamp(0.0, 1.0);
            let now: Vec<[f32; 3]> = self
                .held
                .iter()
                .zip(&held)
                .map(|(&a, &b)| mix(a, b, t))
                .collect();
            let capsules: Vec<Kept> = self
                .ends
                .iter()
                .zip(&ends)
                .zip(&self.capsules)
                .map(|((a, b), c)| ([mix(a[0], b[0], t), mix(a[1], b[1], t)], c.radii))
                .chain(around.iter().map(|a| (a.ends, a.radii)))
                .collect();
            self.move_by(h, &now, &carried, &lengths, &capsules);
        }
        // A longer frame than most_steps take moves them less far than it lasted.
        #[allow(clippy::cast_precision_loss)]
        let past = total - steps as f32 * h;
        self.leftover = if steps == most && past >= h {
            0.0
        } else {
            past
        };
        // Drawn as they'd be now, moved on from the last step at their speed for what's left:
        // drawn as they were at the last step, a frame that took two steps went twice as far as one
        // that took one, and at 75 frames a second the skirt shook.
        let drawn: Vec<[f32; 3]> = self
            .points
            .iter()
            .zip(&self.velocities)
            .map(|(&p, &v)| add(p, scale(v, self.leftover)))
            .collect();
        self.held = held;
        self.ends = ends;
        self.pose(nodes, place, &drawn);
    }

    /// One step of `h` seconds, the animation holding the points at `now` and carrying them at
    /// `carried`, the capsules at `capsules` (at the step before, `self.last`).
    ///
    /// The points move on at their speed, then every constraint is met in turn a few times: drawn
    /// toward where the animation holds them and the cloth between neighbouring chains toward its
    /// width at rest, both as springs (extended position-based dynamics: Macklin et al., "XPBD",
    /// 2016), each link's length and bend, the leash, the capsules and the ground. Their speed is
    /// then how far they went, everything met: taken before the constraints were met for the last
    /// time, as it once was, a constraint that couldn't be met with another (the slim gown's hem
    /// round both knees crouched) pushed a point back and forth each step and that went into its
    /// speed, until it sprang loose.
    fn move_by(
        &mut self,
        h: f32,
        now: &[[f32; 3]],
        carried: &[[f32; 3]],
        lengths: &[f32],
        capsules: &[Kept],
    ) {
        let hang = self.hang;
        self.start.clone_from(&self.points);
        if self.last.len() != capsules.len() {
            self.last = capsules.to_vec();
        }
        self.touching.fill(None);
        let mut pulled = vec![[0.0; 3]; self.points.len()];
        let kept = (-hang.damping * h).exp();
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
        // Drawn back as by a spring that, damped as they are, leaves exp(-stiffness) of how far
        // off they were after a second: stiffness times damping, a unit of weight at each point.
        let give = |rate: f32| 1.0 / (rate * h * h).max(1e-12);
        let (held_give, woven_give) = (give(hang.stiffness * hang.damping), give(hang.weave));
        let mut held = vec![[0.0; 3]; self.points.len()];
        let mut woven = vec![0.0; self.woven.len()];
        #[allow(clippy::cast_precision_loss)]
        let reeled = 1.0 - (-hang.reel * h / hang.iterations.max(1) as f32).exp();
        for _ in 0..hang.iterations {
            self.keep_held(now, held_give, &mut held);
            self.keep_woven(woven_give, &mut woven);
            self.keep_apart(hang.spread);
            self.keep_lengths(lengths, now, &mut pulled);
            self.keep_near(lengths, now, reeled);
            self.keep_out(capsules, now, h);
            self.keep_above();
        }
        for (k, v) in self.velocities.iter_mut().enumerate() {
            *v = scale(sub(self.points[k], self.start[k]), 1.0 / h);
        }
        for chain in &self.chains {
            for k in chain.first + 1..chain.first + chain.nodes.len() - 1 {
                let back = scale(pulled[k + 1], hang.follow / h);
                self.velocities[k] = sub(self.velocities[k], back);
            }
        }
        self.stop_leaving(lengths, now, carried);
        // Where a point meets a capsule it goes no further into it, and slides along it only as
        // far as the friction lets it: cloth draped over a knee crouched slid off it and sprang
        // back each time she shifted.
        let grip = (-hang.friction * h).exp();
        for (v, touch) in self.velocities.iter_mut().zip(&self.touching) {
            let Some((out, moving)) = *touch else {
                continue;
            };
            let past = sub(*v, moving);
            let toward = dot(past, out).max(0.0);
            let across = sub(past, scale(out, dot(past, out)));
            *v = add(moving, add(scale(out, toward), scale(across, grip)));
        }
        self.last = capsules.to_vec();
    }

    /// Every point but the roots drawn toward where the animation, `now`, holds it, as by a spring
    /// of compliance `give` (a step's, over its length squared); `held` what's been drawn so far
    /// this step, each point's.
    fn keep_held(&mut self, now: &[[f32; 3]], give: f32, held: &mut [[f32; 3]]) {
        for chain in &self.chains {
            for k in chain.first + 1..chain.first + chain.nodes.len() {
                let off = sub(self.points[k], now[k]);
                let more = scale(add(off, scale(held[k], give)), -1.0 / (1.0 + give));
                self.points[k] = add(self.points[k], more);
                held[k] = add(held[k], more);
            }
        }
    }

    /// The cloth between neighbouring chains, from each point to the same of the next chain and
    /// corner to corner to the one below it, drawn toward its length at rest as by a spring of
    /// compliance `give`; `woven` what's been drawn so far this step, each line's. A root stays
    /// where the animation has it.
    fn keep_woven(&mut self, give: f32, woven: &mut [f32]) {
        for (&(a, b, rest), drawn) in self.woven.iter().zip(woven) {
            let (wa, wb) = (self.weight(a), self.weight(b));
            let way = sub(self.points[b], self.points[a]);
            let d = length(way);
            if d < 1e-9 || wa + wb == 0.0 {
                continue;
            }
            let more = (rest - d - give * *drawn) / (wa + wb + give);
            *drawn += more;
            let push = scale(way, more / d);
            self.points[a] = sub(self.points[a], scale(push, wa));
            self.points[b] = add(self.points[b], scale(push, wb));
        }
    }

    /// How freely point `k` moves: not at all if it's a root.
    fn weight(&self, k: usize) -> f32 {
        if self.chains.iter().any(|c| c.first == k) {
            0.0
        } else {
            1.0
        }
    }

    /// How far from where the animation, `now`, holds it each point may swing ([`Hang::leash`]),
    /// `lengths` each link's: a point a length down its chain, that length times twice the sine of
    /// half the leash; none, if the leash is half a turn or more.
    fn leashes(&self, lengths: &[f32]) -> Option<Vec<f32>> {
        if self.hang.leash >= std::f32::consts::PI {
            return None;
        }
        let chord = 2.0 * (self.hang.leash.max(0.0) / 2.0).sin();
        let mut most = vec![0.0; lengths.len()];
        for chain in &self.chains {
            let mut down = 0.0;
            for k in chain.first + 1..chain.first + chain.nodes.len() {
                down += lengths[k];
                most[k] = down * chord;
            }
        }
        Some(most)
    }

    /// Every point further than its leash from where the animation, `now`, holds it drawn `share`
    /// of the way back to it, `lengths` each link's.
    fn keep_near(&mut self, lengths: &[f32], now: &[[f32; 3]], share: f32) {
        let Some(leashes) = self.leashes(lengths) else {
            return;
        };
        for (k, most) in leashes.into_iter().enumerate() {
            let off = sub(self.points[k], now[k]);
            let far = length(off);
            if far > most && most > 0.0 {
                self.points[k] = add(self.points[k], scale(off, (most / far - 1.0) * share));
            }
        }
    }

    /// Every point further than its leash from where the animation, `now`, holds it going no
    /// further from it than the animation carries it (`carried`), `lengths` each link's.
    fn stop_leaving(&mut self, lengths: &[f32], now: &[[f32; 3]], carried: &[[f32; 3]]) {
        let Some(leashes) = self.leashes(lengths) else {
            return;
        };
        for (k, most) in leashes.into_iter().enumerate() {
            let off = sub(self.points[k], now[k]);
            let far = length(off);
            if far > most && most > 0.0 {
                let out = scale(off, 1.0 / far);
                let away = dot(sub(self.velocities[k], carried[k]), out);
                if away > 0.0 {
                    self.velocities[k] = sub(self.velocities[k], scale(out, away));
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
    /// holds it) unless it hangs lower than that one; how far each point is moved so added to `pulled`.
    // Each point is met with the one above it, so by index.
    #[allow(clippy::needless_range_loop)]
    fn keep_lengths(&mut self, lengths: &[f32], now: &[[f32; 3]], pulled: &mut [[f32; 3]]) {
        let bend = self.hang.bend;
        let fall = normalize_or(self.hang.gravity, [0.0, 0.0, -1.0]);
        for chain in &self.chains {
            let mut before = normalize(sub(now[chain.first + 1], now[chain.first]));
            for k in chain.first + 1..chain.first + chain.nodes.len() {
                let above = self.points[k - 1];
                let way = normalize_or(sub(self.points[k], above), before);
                // Never held up by it: it may always turn as far as hanging straight down. Held,
                // the gown's back folded up crouched stayed up as she stood, each link out level
                // from the one above it pointing up.
                let way = within(
                    way,
                    before,
                    bend.max(dot(before, fall).clamp(-1.0, 1.0).acos()),
                );
                let at = add(above, scale(way, lengths[k]));
                pulled[k] = add(pulled[k], sub(at, self.points[k]));
                self.points[k] = at;
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

    /// Every chain's lines between its points out of the capsules, and a closed ring's lines from
    /// each point to the same of the next chain and to the one below it, the cloth between them,
    /// straight across and corner to corner but at the hem (see [`push_out`]); each point pushed
    /// marked touching, with the way out and how fast the capsule goes there, the step `h` seconds
    /// long. A chain's first link, from its root, keeps only its lower point out: the root may be
    /// in a capsule (where a thigh meets the hips), and pushed out along the line, its point was
    /// pushed further every step, as the line could never leave it, until it stood out level.
    // Each point is met with the one above it, so by index.
    #[allow(clippy::needless_range_loop)]
    fn keep_out(&mut self, capsules: &[Kept], now: &[[f32; 3]], h: f32) {
        let count = self.chains.len();
        let ring = (self.closed && count > 2).then(|| {
            // Its middle at the roots, and which way it hangs from there to the hem's middle.
            #[allow(clippy::cast_precision_loss)]
            let share = 1.0 / count as f32;
            let middle = |j: fn(&Chain) -> usize| {
                self.chains
                    .iter()
                    .fold([0.0; 3], |sum, c| add(sum, scale(now[j(c)], share)))
            };
            let top = middle(|c| c.first);
            let hem = middle(|c| c.first + c.nodes.len() - 1);
            (top, normalize_or(sub(hem, top), [0.0, 0.0, -1.0]))
        });
        let mut lines = Vec::new();
        for (c, chain) in self.chains.iter().enumerate() {
            for k in chain.first + 1..chain.first + chain.nodes.len() {
                lines.push(((k - 1 != chain.first).then_some(k - 1), k, now[k]));
            }
            if ring.is_none() {
                continue;
            }
            let next = &self.chains[(c + 1) % count];
            let depth = chain.nodes.len().min(next.nodes.len());
            for j in 1..depth {
                let (a, b) = (chain.first + j, next.first + j);
                let across = [(a, b), (a, b + 1), (b, a + 1)];
                for &(a, b) in &across[..if j + 1 < depth { 3 } else { 1 }] {
                    lines.push((Some(a), b, mix(now[a], now[b], 0.5)));
                }
            }
        }
        for (from, to, held) in lines {
            for (&capsule, &last) in capsules.iter().zip(&self.last) {
                let Some((out, along)) = push_out(
                    &mut self.points,
                    &self.start,
                    from,
                    to,
                    capsule,
                    last,
                    held,
                    ring,
                ) else {
                    continue;
                };
                let ([c0, c1], _) = capsule;
                let ([l0, l1], _) = last;
                let moving = scale(sub(mix(c0, c1, along), mix(l0, l1, along)), 1.0 / h);
                for k in from.into_iter().chain([to]) {
                    self.touching[k] = Some((out, moving));
                }
            }
        }
    }

    /// The chains' nodes in `nodes` turned from how the animation has them to point at `points`, each
    /// at its own point; a chain's last node turned as the one above it.
    fn pose(&self, nodes: &mut [Matrix], place: &Matrix, points: &[[f32; 3]]) {
        let unplace = inverse(place);
        for chain in &self.chains {
            let held: Vec<[f32; 3]> = chain.nodes.iter().map(|&n| head(&nodes[n])).collect();
            let at: Vec<[f32; 3]> = (0..chain.nodes.len())
                .map(|j| point(&unplace, points[chain.first + j]))
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

/// A capsule as a step meets it: its line's ends in the world and how far round each.
type Kept = ([[f32; 3]; 2], [f32; 2]);

/// The line from `points[from]` (or `points[to]` alone, if none) to `points[to]` pushed out of
/// `capsule` (its line's ends and how far round each), if it's in it: the way out and how far along
/// the capsule's line it was met.
///
/// It's pushed back out the side it was on at the step's start (`start`, `last` the capsule then),
/// as far as it takes along that way to leave: pushed to whichever side was nearer now, a line a
/// leg swept into was pushed out through its far side, the cloth flung from one side of a running
/// leg to the other in a step. One that was in it then too goes to the nearer side, or the way
/// `held` is from the capsule's line when it's on it; in a closed ring, `ring` its middle at the
/// roots and which way it hangs, never in toward the middle, where the legs are, but mirrored
/// across the level way out from there.
#[allow(clippy::too_many_arguments)]
fn push_out(
    points: &mut [[f32; 3]],
    start: &[[f32; 3]],
    from: Option<usize>,
    to: usize,
    ([c0, c1], [r0, r1]): Kept,
    last: Kept,
    held: [f32; 3],
    ring: Option<([f32; 3], [f32; 3])>,
) -> Option<([f32; 3], f32)> {
    let meet = |p: &[[f32; 3]], [c0, c1]: [[f32; 3]; 2]| {
        if let Some(from) = from {
            closest(p[from], p[to], c0, c1)
        } else {
            let (_, along, _, on_axis) = closest(p[to], p[to], c0, c1);
            (1.0, along, p[to], on_axis)
        }
    };
    let (u, along, on_line, on_axis) = meet(points, [c0, c1]);
    let radius = r0 + (r1 - r0) * along;
    let off = sub(on_line, on_axis);
    let d = length(off);
    if d >= radius {
        return None;
    }
    let (_, was_along, was_on, was_axis) = meet(start, last.0);
    let was = sub(was_on, was_axis);
    let [w0, w1] = last.1;
    let way = if length(was) >= 0.95 * (w0 + (w1 - w0) * was_along) {
        normalize(was)
    } else if let Some((top, down)) = ring {
        let from_middle = sub(on_line, top);
        let out = normalize_or(sub(from_middle, scale(down, dot(from_middle, down))), off);
        let inward = dot(off, out).min(0.0);
        normalize_or(sub(off, scale(out, 2.0 * inward)), out)
    } else {
        normalize_or(off, sub(held, on_axis))
    };
    // In a closed ring, never out the side of a leg that faces the middle, toward the other leg:
    // the gown's back, fallen between her calves crouched, was held there between her legs as she
    // stood, pushed back out each step the side it was on.
    let way = match ring {
        Some((top, down)) => {
            let level = |v: [f32; 3]| sub(v, scale(down, dot(v, down)));
            let side = level(sub(on_axis, top));
            if length(side) > 0.5 * radius && dot(way, side) < 0.0 {
                let side = normalize(side);
                normalize_or(
                    sub(way, scale(side, dot(way, side))),
                    normalize_or(level(sub(on_line, top)), way),
                )
            } else {
                way
            }
        }
        None => way,
    };
    // Off straight out from the axis, it leaves further than the radius: halved toward where it's
    // out.
    let inside = |p: [f32; 3]| {
        let (_, t, _, at) = closest(p, p, c0, c1);
        length(sub(p, at)) < r0 + (r1 - r0) * t
    };
    let (mut within, mut out_at) = (0.0, 2.0 * r0.max(r1) + length(sub(c1, c0)));
    for _ in 0..16 {
        let half = f32::midpoint(within, out_at);
        if inside(add(on_line, scale(way, half))) {
            within = half;
        } else {
            out_at = half;
        }
    }
    let push = scale(way, out_at);
    match from {
        Some(from) => {
            let (wa, wb) = (1.0 - u, u);
            let by = 1.0 / (wa * wa + wb * wb);
            points[from] = add(points[from], scale(push, by * wa));
            points[to] = add(points[to], scale(push, by * wb));
        }
        None => points[to] = add(points[to], push),
    }
    Some((way, along))
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
    use std::f32::consts::PI;

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
    fn a_leashed_chain_trails_little_past_its_leash_and_is_reeled_back_in() {
        let (skeleton, roots) = skirt(1, 5);
        let mut hang = Hang::new(DOWN);
        hang.leash = 20f32.to_radians();
        let mut hanging = Hanging::new(&skeleton, &roots, false, Vec::new(), hang);
        let mut nodes = rest(&skeleton);
        hanging.step(&mut nodes, &IDENTITY, 1.0 / 60.0);
        // How far each point is past its leash, the chain's root at `x`.
        let past = |hanging: &Hanging, x: f32| {
            let points = hanging.points().next().unwrap().to_vec();
            (1..5)
                .map(|j| {
                    #[allow(clippy::cast_precision_loss)]
                    let down = 0.1 * j as f32;
                    let most = down * 2.0 * 10f32.to_radians().sin();
                    length(sub(points[j], [x + 0.2, 0.0, 1.0 - down])) / most
                })
                .fold(0.0, f32::max)
        };
        // Snatched along +x at four metres a second for a quarter of a second: each point trails,
        // and goes little past its leash.
        let mut x = 0.0;
        for frame in 0..15 {
            x += 4.0 / 60.0;
            let mut nodes = rest(&skeleton);
            hanging.step(&mut nodes, &moved([x, 0.0, 0.0]), 1.0 / 60.0);
            assert!(
                past(&hanging, x) < 1.5,
                "{:.2} of its leash",
                past(&hanging, x)
            );
            if frame == 2 {
                let tip = hanging.points().next().unwrap()[4];
                assert!(tip[0] < x + 0.2 - 0.05, "it trails: {tip:?}");
            }
        }
        // Stopped, within it again in a second.
        for _ in 0..60 {
            let mut nodes = rest(&skeleton);
            hanging.step(&mut nodes, &moved([x, 0.0, 0.0]), 1.0 / 60.0);
        }
        assert!(
            past(&hanging, x) < 1.0,
            "{:.2} of its leash",
            past(&hanging, x)
        );
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
    fn a_capsule_in_the_world_pushes_the_chain_aside_and_it_swings_back() {
        let (skeleton, roots) = skirt(1, 5);
        let mut hanging = Hanging::new(&skeleton, &roots, false, Vec::new(), Hang::new(DOWN));
        // Someone stood up through where the chain hangs, a little to its side.
        let body = Around {
            ends: [[0.25, 0.0, 0.0], [0.25, 0.0, 2.0]],
            radii: [0.1, 0.1],
        };
        for _ in 0..60 {
            let mut nodes = rest(&skeleton);
            hanging.step_among(&mut nodes, &IDENTITY, 1.0 / 60.0, &[body]);
        }
        let points = hanging.points().next().unwrap().to_vec();
        for pair in points.windows(2).skip(1) {
            let (_, _, on_line, on_axis) = closest(pair[0], pair[1], body.ends[0], body.ends[1]);
            assert!(length(sub(on_line, on_axis)) > 0.1 - 1e-3, "{points:?}");
        }
        assert!(points[4][0] < 0.2 - 0.04, "pushed aside: {points:?}");
        // Gone, it comes back to hang where it did.
        for _ in 0..240 {
            let mut nodes = rest(&skeleton);
            hanging.step_among(&mut nodes, &IDENTITY, 1.0 / 60.0, &[]);
        }
        let points = hanging.points().next().unwrap().to_vec();
        assert!(near(points[4], [0.2, 0.0, 0.6], 0.005), "{points:?}");
    }

    #[test]
    fn a_long_lightly_damped_chain_swings_and_settles_with_follow() {
        // Ten links, damped as a rope is, knocked by its root carried aside for a few frames.
        let (skeleton, roots) = skirt(1, 10);
        let knocked = |follow: f32| {
            let hang = Hang {
                damping: 0.6,
                stiffness: 0.1,
                follow,
                ..Hang::new(DOWN)
            };
            let mut hanging = Hanging::new(&skeleton, &roots, false, Vec::new(), hang);
            let mut tips = Vec::new();
            for frame in 0..900 {
                let x = if (5..10).contains(&frame) { 0.05 } else { 0.0 };
                let mut nodes = rest(&skeleton);
                hanging.step(&mut nodes, &moved([x, 0.0, 0.0]), 1.0 / 60.0);
                tips.push(hanging.points().next().unwrap()[9]);
            }
            tips
        };
        // Without, it swings on, more and more; with, it swings and settles back under its root.
        let wild = knocked(0.0);
        assert!(wild[600..].iter().any(|t| (t[0] - 0.2).abs() > 0.1));
        let tips = knocked(0.9);
        assert!(
            tips[..120].iter().any(|t| (t[0] - 0.2).abs() > 1e-3),
            "it swings"
        );
        assert!(
            tips[600..].iter().all(|t| near(*t, [0.2, 0.0, 0.1], 0.005)),
            "{:?}",
            tips[899]
        );
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
    fn a_knee_pushed_out_between_two_chains_of_a_ring_stays_inside_the_cloth_between_them() {
        let (mut skeleton, roots) = skirt(8, 5);
        // A thigh 0.3 m long, its knee level with the third links, pushed out from the middle
        // between the first two chains until its knee is 0.3 m out, half again as far as they hang.
        let knee = skeleton.nodes.len();
        skeleton.nodes.push(Node {
            name: Some("Thigh".into()),
            parent: None,
            rest: Transform::default(),
            extras: None,
        });
        let way = [(PI / 8.0).cos(), (PI / 8.0).sin(), 0.0];
        let thigh = Capsule {
            node: knee,
            ends: [[0.0; 3], scale(way, 0.3)],
            radii: [0.05; 2],
        };
        let mut hanging = Hanging::new(&skeleton, &roots, true, vec![thigh], Hang::new(DOWN));
        let at = |out: f32| add([0.0, 0.0, 0.75], scale(way, out - 0.3));
        let mut last: Vec<[f32; 3]> = Vec::new();
        for frame in 0..240u8 {
            let mut nodes = rest(&skeleton);
            nodes[knee] = moved(at(0.3 * f32::from(frame.min(60)) / 60.0));
            hanging.step(&mut nodes, &IDENTITY, 1.0 / 60.0);
            // Held still, it settles within a few seconds and stays still: pushed to one side by
            // one line and back by another, the cloth across a knee shook on.
            let now: Vec<[f32; 3]> = hanging.points().flatten().copied().collect();
            if frame > 200 {
                let moved = now
                    .iter()
                    .zip(&last)
                    .map(|(&a, &b)| length(sub(a, b)))
                    .fold(0.0, f32::max);
                assert!(
                    moved < 1e-4,
                    "a point moved {moved:.4} m in a frame at {frame}"
                );
            }
            last = now;
        }
        let (c0, c1) = (at(0.0), at(0.3));
        let chains: Vec<Vec<[f32; 3]>> = hanging.points().map(<[_]>::to_vec).collect();
        let (a, b) = (&chains[0], &chains[1]);
        // The cloth between them: straight across and corner to corner, below the roots.
        for j in 1..5 {
            let mut lines = vec![(a[j], b[j])];
            if j + 1 < 5 {
                lines.extend([(a[j], b[j + 1]), (b[j], a[j + 1])]);
            }
            for (p, q) in lines {
                let (_, _, on_line, on_axis) = closest(p, q, c0, c1);
                let off = length(sub(on_line, on_axis));
                assert!(
                    off > 0.05 - 0.01,
                    "the cloth between them {off:.3} m from the knee's line at link {j}"
                );
            }
        }
    }

    #[test]
    fn a_rings_cloth_is_pushed_out_of_a_capsule_outward_only() {
        let (mut skeleton, roots) = skirt(8, 5);
        // A shin lying level across the first chain just outside it, between its second and third
        // links: the chain's nearer side of it is inward, toward the ring's middle.
        let shin = skeleton.nodes.len();
        skeleton.nodes.push(Node {
            name: Some("Shin".into()),
            parent: None,
            rest: Transform::default(),
            extras: None,
        });
        let capsule = Capsule {
            node: shin,
            ends: [[0.22, -0.05, 0.75], [0.22, 0.05, 0.75]],
            radii: [0.06; 2],
        };
        let mut hanging = Hanging::new(&skeleton, &roots, true, vec![capsule], Hang::new(DOWN));
        for _ in 0..120 {
            let mut nodes = rest(&skeleton);
            hanging.step(&mut nodes, &IDENTITY, 1.0 / 60.0);
        }
        let chain = hanging.points().next().unwrap();
        // Over it on the outside, not caught between it and the middle.
        assert!(chain[3][0] > 0.22, "the third link at x {:.3}", chain[3][0]);
        for j in 1..5 {
            let (_, _, on_line, on_axis) =
                closest(chain[j - 1], chain[j], capsule.ends[0], capsule.ends[1]);
            let off = length(sub(on_line, on_axis));
            assert!(
                off > 0.06 - 0.01,
                "link {j} {off:.3} m from the shin's line"
            );
        }
    }

    #[test]
    fn a_capsule_holding_a_ring_past_its_leash_leaves_it_still() {
        let (mut skeleton, roots) = skirt(8, 5);
        // A thigh lying level across the first chain's lower links, holding them out further than
        // their leash lets them swing, as a crouch holds a gown's back out.
        let thigh = skeleton.nodes.len();
        skeleton.nodes.push(Node {
            name: Some("Thigh".into()),
            parent: None,
            rest: Transform::default(),
            extras: None,
        });
        let capsule = Capsule {
            node: thigh,
            ends: [[0.13, -0.05, 0.7], [0.13, 0.05, 0.7]],
            radii: [0.12; 2],
        };
        let hang = Hang {
            leash: 5f32.to_radians(),
            ..Hang::new(DOWN)
        };
        let mut hanging = Hanging::new(&skeleton, &roots, true, vec![capsule], hang);
        let mut last: Vec<[f32; 3]> = Vec::new();
        for frame in 0..240u8 {
            let mut nodes = rest(&skeleton);
            hanging.step(&mut nodes, &IDENTITY, 1.0 / 60.0);
            let now: Vec<[f32; 3]> = hanging.points().flatten().copied().collect();
            if frame > 120 {
                let moved = now
                    .iter()
                    .zip(&last)
                    .map(|(&a, &b)| length(sub(a, b)))
                    .fold(0.0, f32::max);
                assert!(
                    moved < 1e-4,
                    "a point moved {moved:.4} m in a frame at {frame}"
                );
            }
            last = now;
        }
        let chain = hanging.points().next().unwrap();
        for j in 1..5 {
            let (_, _, on_line, on_axis) =
                closest(chain[j - 1], chain[j], capsule.ends[0], capsule.ends[1]);
            let off = length(sub(on_line, on_axis));
            assert!(
                off > 0.12 - 0.01,
                "link {j} {off:.3} m from the thigh's line"
            );
        }
        assert!(chain[3][0] > 0.24, "held out: {:?}", chain[3]);
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
