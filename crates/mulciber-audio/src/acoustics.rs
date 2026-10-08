//! Room acoustics: sizing the reverb from the world around the listener.
//!
//! The game supplies the world as a ray cast, so the same probe serves voxels, heightfields,
//! BSP and collider soups.

use crate::math::Vec3;

/// The reverb's room, each 0..1: `wet` is the tail level, `room` its decay length and `damp`
/// how quickly its highs die (soft, absorbent rooms are higher).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoomAcoustics {
    /// Tail level; 0 turns the reverb off.
    pub wet: f32,
    /// Decay length, from a tight slap at 0 to a many-second bloom at 1.
    pub room: f32,
    /// High-frequency absorption: 0 is bright stone, 1 dark and soft.
    pub damp: f32,
}

impl RoomAcoustics {
    /// No reverb: open air, menus, underwater.
    pub const DRY: Self = Self {
        wet: 0.0,
        room: 0.0,
        damp: 0.5,
    };

    fn eased_toward(self, target: Self, amount: f32) -> Self {
        Self {
            wet: self.wet + (target.wet - self.wet) * amount,
            room: self.room + (target.room - self.room) * amount,
            damp: self.damp + (target.damp - self.damp) * amount,
        }
    }

    fn differs_from(self, other: Self, threshold: f32) -> bool {
        (self.wet - other.wet).abs() > threshold
            || (self.room - other.room).abs() > threshold
            || (self.damp - other.damp).abs() > threshold
    }
}

impl Default for RoomAcoustics {
    fn default() -> Self {
        Self::DRY
    }
}

/// What a probe ray struck.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    /// Distance from the ray's origin to the surface, in world units.
    pub distance: f32,
    /// How well the surface reflects, 0..1: about 1 for stone, rock and plaster, around 0.5 for
    /// timber, and near 0 for earth, turf, cloth and snow. Hard rooms ring bright; soft ones
    /// swallow their highs.
    pub hardness: f32,
}

/// How [`probe_room`] samples the space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProbeSettings {
    /// Rays cast over a Fibonacci sphere.
    pub rays: usize,
    /// Rays that travel this far without a hit count as open sky.
    pub max_distance: f32,
}

impl Default for ProbeSettings {
    fn default() -> Self {
        Self {
            rays: 32,
            max_distance: 40.0,
        }
    }
}

/// Enclosure below which the space reads as open: on open ground about half the sphere hits
/// the ground, so the tail only fades in past this.
const OPEN_ENCLOSURE: f32 = 0.70;
/// Enclosure span over which the tail fades in to full.
const ENCLOSURE_SPAN: f32 = 0.25;
/// Mean free path (world units, metre scale) at which the room is smallest...
const SMALLEST_PATH: f32 = 3.0;
/// ...and the span to the largest.
const PATH_SPAN: f32 = 22.0;

/// Sizes the acoustic space around `origin` by casting rays in all directions through the
/// game's world, and returns the reverb settings for it.
///
/// `cast(origin, direction, max_distance)` traces one ray (`direction` is unit length) and
/// returns the first surface within `max_distance`, or `None` when the ray escapes. Leave out
/// what sound passes through, such as foliage: a forest canopy is not a roof.
///
/// The share of rays that hit is the enclosure, which sets the wet level: open ground stays dry
/// and a sealed room is fully wet. The mean free path (escaped rays count as `max_distance`)
/// sets the decay, so a cramped cell answers tight and a great hall blooms, and the mean
/// hardness of what was hit sets the damping. The constants are tuned for metre-scale worlds.
///
/// Feed the result to [`RoomTracker`] rather than straight to the mixer, so the room eases as
/// the listener moves.
#[must_use]
pub fn probe_room(
    origin: impl Into<[f32; 3]>,
    settings: ProbeSettings,
    mut cast: impl FnMut([f32; 3], [f32; 3], f32) -> Option<RayHit>,
) -> RoomAcoustics {
    let origin: Vec3 = origin.into();
    let rays = settings.rays.max(1);
    let max_distance = settings.max_distance.max(f32::EPSILON);
    let golden_angle = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
    #[allow(clippy::cast_precision_loss)]
    let count = rays as f32;

    let mut hits = 0_usize;
    let mut hardness = 0.0_f32;
    let mut total_path = 0.0_f32;
    for ray in 0..rays {
        #[allow(clippy::cast_precision_loss)]
        let index = ray as f32;
        let y = 1.0 - 2.0 * (index + 0.5) / count;
        let ring = (1.0 - y * y).max(0.0).sqrt();
        let theta = golden_angle * index;
        let direction = [ring * theta.cos(), y, ring * theta.sin()];
        let path = match cast(origin, direction, max_distance) {
            Some(hit) if hit.distance.is_finite() && hit.distance <= max_distance => {
                hits += 1;
                hardness += hit.hardness.clamp(0.0, 1.0);
                hit.distance.max(0.0)
            }
            _ => max_distance,
        };
        total_path += path;
    }

    #[allow(clippy::cast_precision_loss)]
    let enclosure = hits as f32 / count;
    let mean_path = total_path / count;
    let closed = ((enclosure - OPEN_ENCLOSURE) / ENCLOSURE_SPAN).clamp(0.0, 1.0);
    let room = ((mean_path - SMALLEST_PATH) / PATH_SPAN).clamp(0.0, 1.0);
    let wet = closed * (0.35 + 0.65 * room);
    #[allow(clippy::cast_precision_loss)]
    let hard = if hits > 0 {
        hardness / hits as f32
    } else {
        0.0
    };
    RoomAcoustics {
        wet,
        room,
        damp: 0.75 - 0.5 * hard,
    }
}

/// Keeps the reverb sized to the space around the listener: re-probes on an interval, eases the
/// live room toward the latest probe, and says when the change is big enough to send.
///
/// ```
/// use mulciber_audio::{Mixer, ProbeSettings, RoomTracker, probe_room};
///
/// let (_mixer, controls) = Mixer::new(48_000);
/// let mut tracker = RoomTracker::new();
/// let listener = [0.0, 1.7, 0.0];
/// // Every frame:
/// let dt = 1.0 / 60.0;
/// if let Some(room) = tracker.update(dt, || {
///     // `None` (menus, underwater) fades to dry.
///     Some(probe_room(listener, ProbeSettings::default(), |_, _, _| None))
/// }) {
///     controls.set_room(room);
/// }
/// // Nothing around the listener: open sky, no tail.
/// assert_eq!(tracker.current().wet, 0.0);
/// ```
#[derive(Clone, Debug)]
pub struct RoomTracker {
    interval: f32,
    smoothing_seconds: f32,
    threshold: f32,
    timer: f32,
    current: RoomAcoustics,
    target: RoomAcoustics,
    sent: Option<RoomAcoustics>,
    /// The next probe is taken as is rather than eased toward.
    snap: bool,
}

impl Default for RoomTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RoomTracker {
    /// A tracker that probes every 0.3 s and follows the probe with a 0.8 s time constant, so
    /// walking out of a cave mouth fades the tail instead of cutting it.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_timing(0.3, 0.8)
    }

    /// A tracker that probes every `interval` seconds and eases with a `smoothing_seconds` time
    /// constant.
    #[must_use]
    pub const fn with_timing(interval: f32, smoothing_seconds: f32) -> Self {
        Self {
            interval,
            smoothing_seconds,
            threshold: 0.002,
            timer: 0.0,
            current: RoomAcoustics::DRY,
            target: RoomAcoustics::DRY,
            sent: Some(RoomAcoustics::DRY),
            snap: false,
        }
    }

    /// Advances by `dt` seconds. When a probe is due, calls `probe` for the space around the
    /// listener; `None` means no room (menus, underwater) and eases toward dry. Returns the
    /// eased room when it has moved far enough from the last one returned to be worth sending
    /// to [`Controls::set_room`](crate::Controls::set_room).
    pub fn update(
        &mut self,
        dt: f32,
        probe: impl FnOnce() -> Option<RoomAcoustics>,
    ) -> Option<RoomAcoustics> {
        self.timer -= dt;
        if self.timer <= 0.0 {
            self.timer = self.interval;
            self.target = probe().unwrap_or(RoomAcoustics::DRY);
        }
        let amount = if std::mem::take(&mut self.snap) || self.smoothing_seconds <= 0.0 {
            1.0
        } else {
            1.0 - (-dt.max(0.0) / self.smoothing_seconds).exp()
        };
        self.current = self.current.eased_toward(self.target, amount);
        let due = self
            .sent
            .is_none_or(|sent| self.current.differs_from(sent, self.threshold));
        if due {
            self.sent = Some(self.current);
            Some(self.current)
        } else {
            None
        }
    }

    /// Forgets the old place, for teleports and loads where easing from it would be wrong: the
    /// next update probes at once, jumps straight to the result and returns it to send.
    pub fn reset(&mut self) {
        self.timer = 0.0;
        self.snap = true;
        self.sent = None;
    }

    /// The eased room as of the last update.
    #[must_use]
    pub fn current(&self) -> RoomAcoustics {
        self.current
    }

    /// The latest probe's room, which [`Self::current`] is easing toward.
    #[must_use]
    pub fn target(&self) -> RoomAcoustics {
        self.target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first hit of a ray leaving `origin` against the inside of an axis-aligned box.
    fn inside_box(
        min: Vec3,
        max: Vec3,
        hardness: f32,
    ) -> impl FnMut(Vec3, Vec3, f32) -> Option<RayHit> {
        move |origin, direction, limit| {
            let mut nearest = f32::INFINITY;
            for axis in 0..3 {
                if direction[axis].abs() > 1.0e-6 {
                    let wall = if direction[axis] > 0.0 {
                        max[axis]
                    } else {
                        min[axis]
                    };
                    nearest = nearest.min((wall - origin[axis]) / direction[axis]);
                }
            }
            (nearest <= limit).then_some(RayHit {
                distance: nearest,
                hardness,
            })
        }
    }

    /// Flat ground `height` below the origin and nothing else.
    fn open_field(height: f32) -> impl FnMut(Vec3, Vec3, f32) -> Option<RayHit> {
        move |_, direction, limit| {
            if direction[1] >= -1.0e-6 {
                return None;
            }
            let distance = height / -direction[1];
            (distance <= limit).then_some(RayHit {
                distance,
                hardness: 0.1,
            })
        }
    }

    #[test]
    fn a_closed_room_is_wet_and_open_ground_is_dry() {
        let settings = ProbeSettings::default();
        let field = probe_room([0.0, 1.7, 0.0], settings, open_field(1.7));
        assert!(field.wet < 1.0e-6, "open ground: {field:?}");

        let cell = probe_room(
            [0.0, 1.7, 0.0],
            settings,
            inside_box([-2.0, 0.0, -2.5], [2.0, 3.0, 2.5], 1.0),
        );
        assert!(cell.wet > 0.3, "cell: {cell:?}");

        let hall = probe_room(
            [0.0, 1.7, 0.0],
            settings,
            inside_box([-15.0, 0.0, -30.0], [15.0, 12.0, 30.0], 1.0),
        );
        assert!(hall.wet > cell.wet, "hall {hall:?} vs cell {cell:?}");
        assert!(
            hall.room > cell.room + 0.2,
            "hall {hall:?} vs cell {cell:?}"
        );
    }

    #[test]
    fn soft_walls_damp_the_tail() {
        let settings = ProbeSettings::default();
        let stone = probe_room([0.0; 3], settings, inside_box([-4.0; 3], [4.0; 3], 1.0));
        let earth = probe_room([0.0; 3], settings, inside_box([-4.0; 3], [4.0; 3], 0.0));
        assert!((stone.damp - 0.25).abs() < 1.0e-6);
        assert!((earth.damp - 0.75).abs() < 1.0e-6);
        assert!((stone.wet - earth.wet).abs() < 1.0e-6);
    }

    #[test]
    fn the_probe_covers_the_whole_sphere() {
        let mut directions = Vec::new();
        let _ = probe_room([0.0; 3], ProbeSettings::default(), |_, direction, _| {
            directions.push(direction);
            None
        });
        assert_eq!(directions.len(), 32);
        let mut mean = [0.0_f32; 3];
        for direction in &directions {
            assert!((crate::math::length(*direction) - 1.0).abs() < 1.0e-5);
            for axis in 0..3 {
                mean[axis] += direction[axis] / 32.0;
            }
        }
        assert!(crate::math::length(mean) < 0.05, "balanced: {mean:?}");
    }

    #[test]
    fn the_tracker_eases_toward_the_probe_and_sends_only_changes() {
        let hall = RoomAcoustics {
            wet: 0.8,
            room: 0.7,
            damp: 0.3,
        };
        let mut tracker = RoomTracker::new();
        let mut probes = 0;
        let mut sent = 0;
        for _ in 0..60 {
            if tracker
                .update(1.0 / 60.0, || {
                    probes += 1;
                    Some(hall)
                })
                .is_some()
            {
                sent += 1;
            }
        }
        // One second at a 0.3 s interval: probed at 0, 0.3, 0.6 and 0.9 s.
        assert_eq!(probes, 4);
        let after_one_second = tracker.current();
        assert!(after_one_second.wet > 0.5 && after_one_second.wet < 0.65);
        assert!(sent > 10 && sent <= 60);

        for _ in 0..600 {
            tracker.update(1.0 / 60.0, || Some(hall));
        }
        assert!((tracker.current().wet - 0.8).abs() < 1.0e-3);
        assert_eq!(tracker.update(1.0 / 60.0, || Some(hall)), None, "settled");

        // Leaving the room fades back to dry.
        for _ in 0..600 {
            tracker.update(1.0 / 60.0, || None);
        }
        assert!(tracker.current().wet < 1.0e-3);

        // A teleport into the hall lands in it at once.
        tracker.reset();
        assert_eq!(tracker.update(1.0 / 60.0, || Some(hall)), Some(hall));
        assert_eq!(tracker.current(), hall);
        assert_eq!(tracker.update(1.0 / 60.0, || Some(hall)), None);
    }
}
