//! The callback-side mixer and the game-side command handle.

#[cfg(feature = "hrtf")]
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use crate::acoustics::RoomAcoustics;
use crate::math::Vec3;
use crate::reverb::Reverb;
use crate::sound::{Sound, db_to_amplitude};
use crate::spatial::Listener;
use crate::stream::{StreamConfig, StreamReader, VoiceStream};

/// A caller-chosen identity for a moving sound source, such as a packed entity id.
///
/// Spatial voices started with an emitter follow [`Controls::move_emitter`] and stop together
/// through [`Controls::stop_emitter`], so a creature's footsteps and cries stay on the creature
/// and a death can cut off whatever it was still saying.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EmitterId(pub u64);

/// A handle to one playing voice, for fading it out before its clip ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VoiceId(pub(crate) u64);

/// Volume per bus, each 0..1. Every voice is scaled by `master` and by its own bus.
///
/// A voice follows the volume of the [`Bus`] it was played on; live streams follow `master`
/// alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Volumes {
    /// Scales everything.
    pub master: f32,
    /// [`Bus::Effects`].
    pub effects: f32,
    /// [`Bus::Music`].
    pub music: f32,
    /// [`Bus::Ambience`].
    pub ambience: f32,
}

impl Default for Volumes {
    fn default() -> Self {
        Self {
            master: 1.0,
            effects: 1.0,
            music: 1.0,
            ambience: 1.0,
        }
    }
}

impl Volumes {
    fn clamped(self) -> Self {
        Self {
            master: self.master.clamp(0.0, 1.0),
            effects: self.effects.clamp(0.0, 1.0),
            music: self.music.clamp(0.0, 1.0),
            ambience: self.ambience.clamp(0.0, 1.0),
        }
    }
}

/// How spatial voices fade with distance, in world units.
///
/// Full volume inside `reference_distance`, then inverse-square (-12 dB per doubling) so distant
/// sources read as faint background, and silent from `max_distance` on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rolloff {
    /// Distance inside which spatial voices play at full volume.
    pub reference_distance: f32,
    /// Distance from which spatial voices are silent.
    pub max_distance: f32,
}

impl Default for Rolloff {
    fn default() -> Self {
        Self {
            reference_distance: 6.0,
            max_distance: 64.0,
        }
    }
}

impl Rolloff {
    /// The distance gain at `distance`.
    #[must_use]
    pub fn gain(self, distance: f32) -> f32 {
        if distance >= self.max_distance {
            0.0
        } else if distance <= self.reference_distance {
            1.0
        } else {
            let ratio = self.reference_distance / distance;
            ratio * ratio
        }
    }
}

/// How much what stands between a voice and the listener (a wall, a shut door, a deck) muffles it:
/// turned down, and its highs cut by a low-pass of two poles at `cutoff_hz`, 12 dB an octave above
/// it. The game works out what stands between; the mixer only muffles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Muffle {
    /// Gain in decibels, on top of the voice's own and its distance's.
    pub gain_db: f32,
    /// Where the low-pass cuts, in hertz; [`f32::INFINITY`] lets every frequency through.
    pub cutoff_hz: f32,
}

impl Muffle {
    /// Nothing in the way: every frequency at the voice's own gain.
    pub const CLEAR: Self = Self {
        gain_db: 0.0,
        cutoff_hz: f32::INFINITY,
    };
}

impl Default for Muffle {
    fn default() -> Self {
        Self::CLEAR
    }
}

/// Seconds a muffle set on a playing voice takes to come most of the way (1 - 1/e) to the new
/// one, so a door shutting or a listener stepping round a corner glides rather than clicks.
const MUFFLE_GLIDE: f32 = 0.05;

/// A voice's muffle as it's heard: its gain and the low-pass's pole gliding toward the last set,
/// and the poles' state for each channel.
struct Muffler {
    gain: f32,
    target_gain: f32,
    /// The one-pole coefficient, 1 passing every frequency.
    pole: f32,
    target_pole: f32,
    /// How far toward the targets each frame goes.
    glide: f32,
    /// Each channel's two poles.
    state: [[f32; 2]; 2],
}

impl Muffler {
    fn new(muffle: Muffle, sample_rate: u32) -> Self {
        let (gain, pole) = Self::targets(muffle, sample_rate);
        #[allow(clippy::cast_precision_loss)]
        let glide = 1.0 - (-1.0 / (MUFFLE_GLIDE * sample_rate as f32)).exp();
        Self {
            gain,
            target_gain: gain,
            pole,
            target_pole: pole,
            glide,
            state: [[0.0; 2]; 2],
        }
    }

    fn targets(muffle: Muffle, sample_rate: u32) -> (f32, f32) {
        #[allow(clippy::cast_precision_loss)]
        let pole =
            1.0 - (-std::f32::consts::TAU * muffle.cutoff_hz.max(0.0) / sample_rate as f32).exp();
        (db_to_amplitude(muffle.gain_db), pole)
    }

    /// Glides to `muffle` from where it is.
    fn set(&mut self, muffle: Muffle, sample_rate: u32) {
        (self.target_gain, self.target_pole) = Self::targets(muffle, sample_rate);
    }

    /// One frame through the muffle.
    // Exact on purpose: whether a glide has arrived, or a step changed nothing.
    #[allow(clippy::float_cmp)]
    fn process(&mut self, (left, right): (f32, f32)) -> (f32, f32) {
        if self.pole >= 1.0 && self.target_pole >= 1.0 && self.gain == self.target_gain {
            // Clear: the poles follow the signal, so a muffle set later starts from it.
            self.state = [[left; 2], [right; 2]];
            return (left * self.gain, right * self.gain);
        }
        // Arrived once a step no longer moves it: near 1, f32 loses a step that small.
        let step = |value: &mut f32, target: f32, glide: f32| {
            let next = *value + (target - *value) * glide;
            *value = if next == *value { target } else { next };
        };
        step(&mut self.gain, self.target_gain, self.glide);
        step(&mut self.pole, self.target_pole, self.glide);
        let pole = self.pole;
        let filter = |state: &mut [f32; 2], sample: f32| {
            state[0] += (sample - state[0]) * pole;
            state[1] += (state[0] - state[1]) * pole;
            state[1]
        };
        let [left_state, right_state] = &mut self.state;
        (
            filter(left_state, left) * self.gain,
            filter(right_state, right) * self.gain,
        )
    }
}

/// The volume a played clip follows, one of [`Volumes`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Bus {
    /// Sound effects, which sound in the room's reverb.
    #[default]
    Effects,
    /// Music, kept out of the reverb unless it plays from a place in the world.
    Music,
    /// Ambience beds such as a recorded forest, on the effects side of the world but on their
    /// own volume, and kept out of the reverb unless placed in the world: a recording of a forest
    /// already has the forest's reverb in it.
    Ambience,
}

/// Where a played clip sounds from.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Placement {
    /// Unpositioned, as recorded: stereo stays stereo.
    #[default]
    Flat,
    /// At a world position: downmixed to mono, attenuated by distance ([`Rolloff`]), spatialized
    /// through the HRTF or the stereo pan, sounding in the room's reverb whatever its bus, and
    /// anchored in the world as the listener moves.
    World {
        /// Position in world space.
        position: [f32; 3],
        /// The source it belongs to, which [`Controls::move_emitter`] moves and
        /// [`Controls::stop_emitter`] stops.
        emitter: Option<EmitterId>,
    },
}

impl Placement {
    /// At `position`, belonging to no emitter.
    #[must_use]
    pub fn at(position: impl Into<[f32; 3]>) -> Self {
        Self::World {
            position: position.into(),
            emitter: None,
        }
    }
}

/// How [`Controls::play`] plays a clip. The default plays it once on the effects bus,
/// unpositioned, at its own gain and pitch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Play {
    /// Gain in decibels.
    pub gain_db: f32,
    /// Playback-rate (pitch) multiplier, 1 being the clip's own.
    pub rate: f64,
    /// The volume it follows.
    pub bus: Bus,
    /// Unpositioned, or at a place in the world.
    pub placement: Placement,
    /// Seconds to fade in over; 0 starts at full gain.
    pub fade_in: f32,
    /// Whether it loops until stopped rather than ending with its clip.
    pub looping: bool,
    /// How muffled it is from the start; [`Controls::set_voice_muffle`] changes it as it plays.
    pub muffle: Muffle,
}

impl Default for Play {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            rate: 1.0,
            bus: Bus::Effects,
            placement: Placement::Flat,
            fade_in: 0.0,
            looping: false,
            muffle: Muffle::CLEAR,
        }
    }
}

/// The bus a voice mixes on: a played clip's [`Bus`], or the live speech of a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MixBus {
    Effects,
    Music,
    Ambience,
    /// Live speech, on the master's volume alone: the game sets each stream's own.
    Voice,
}

impl From<Bus> for MixBus {
    fn from(bus: Bus) -> Self {
        match bus {
            Bus::Effects => Self::Effects,
            Bus::Music => Self::Music,
            Bus::Ambience => Self::Ambience,
        }
    }
}

enum Command {
    Play {
        id: VoiceId,
        samples: Arc<[f32]>,
        channels: u8,
        gain: f32,
        rate: f64,
        bus: MixBus,
        /// The world position and emitter of a placed voice.
        world: Option<(Vec3, Option<EmitterId>)>,
        fade_samples: usize,
        looping: bool,
        muffle: Muffle,
    },
    StopVoice {
        id: VoiceId,
        fade_samples: usize,
    },
    PlayStream {
        id: VoiceId,
        reader: Box<StreamReader>,
        gain: f32,
        position: Vec3,
        emitter: Option<EmitterId>,
    },
    SetGain {
        id: VoiceId,
        gain: f32,
    },
    SetMuffle {
        id: VoiceId,
        muffle: Muffle,
    },
    Listener(Listener),
    MoveEmitter {
        emitter: EmitterId,
        position: Vec3,
    },
    StopEmitter {
        emitter: EmitterId,
        fade_samples: usize,
    },
    StopMusic {
        fade_samples: usize,
    },
    Volumes(Volumes),
    Room(RoomAcoustics),
    Rolloff(Rolloff),
}

struct Fade {
    remaining: usize,
    total: usize,
}

impl Fade {
    fn over(samples: usize) -> Self {
        let samples = samples.max(1);
        Self {
            remaining: samples,
            total: samples,
        }
    }
}

struct Voice {
    id: Option<VoiceId>,
    samples: Arc<[f32]>,
    channels: u8,
    /// A live stream played in place of `samples`.
    stream: Option<Box<StreamReader>>,
    /// Fractional read position in source frames; `rate` is added per output frame.
    cursor: f64,
    rate: f64,
    /// Gain after distance attenuation for spatial voices.
    gain: f32,
    /// Gain before distance attenuation.
    base_gain: f32,
    looping: bool,
    bus: MixBus,
    spatial: bool,
    position: Vec3,
    emitter: Option<EmitterId>,
    fade_out: Option<Fade>,
    fade_in: Option<Fade>,
    /// Listener-space direction (x right, y up, z forward).
    direction: Vec3,
    muffler: Muffler,
    #[cfg(feature = "hrtf")]
    hrtf: HrtfVoice,
}

/// Per-voice HRTF state: the previous block's direction, gain and overlap, and the convolved
/// frames waiting to be mixed.
#[cfg(feature = "hrtf")]
struct HrtfVoice {
    previous_direction: Vec3,
    previous_gain: f32,
    previous_left: Vec<f32>,
    previous_right: Vec<f32>,
    output: VecDeque<(f32, f32)>,
    source_exhausted: bool,
}

impl Voice {
    fn new(samples: Arc<[f32]>, channels: u8, gain: f32, rate: f64, muffler: Muffler) -> Self {
        Self {
            id: None,
            samples,
            channels: channels.max(1),
            stream: None,
            cursor: 0.0,
            rate,
            gain,
            base_gain: gain,
            looping: false,
            bus: MixBus::Effects,
            spatial: false,
            position: [0.0; 3],
            emitter: None,
            fade_out: None,
            fade_in: None,
            direction: [0.0, 0.0, 1.0],
            muffler,
            #[cfg(feature = "hrtf")]
            hrtf: HrtfVoice {
                previous_direction: [0.0, 0.0, 1.0],
                previous_gain: gain,
                previous_left: Vec::new(),
                previous_right: Vec::new(),
                output: VecDeque::new(),
                source_exhausted: false,
            },
        }
    }

    fn fade_out(&mut self, samples: usize) {
        if self.fade_out.is_none() {
            self.fade_out = Some(Fade::over(samples));
        }
    }

    fn relocate(&mut self, listener: &Listener, rolloff: Rolloff) {
        let (attenuation, direction) = listener.locate(self.position, rolloff);
        self.gain = self.base_gain * attenuation;
        self.direction = direction;
    }

    /// The source frame under the cursor, linearly interpolated, as left and right; mono plays
    /// on both. Advances the cursor by the voice's rate.
    fn interpolated_frame(&mut self) -> Option<(f32, f32)> {
        if let Some(stream) = &mut self.stream {
            let mono = stream.next()?;
            return Some((mono, mono));
        }
        let channels = usize::from(self.channels);
        let total_frames = self.samples.len() / channels;
        if total_frames == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let end = total_frames as f64;
        if self.cursor >= end {
            if self.looping {
                self.cursor %= end;
            } else {
                return None;
            }
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let index = self.cursor as usize;
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let fraction = (self.cursor - index as f64) as f32;
        let next = if index + 1 < total_frames {
            index + 1
        } else if self.looping {
            0
        } else {
            index
        };
        let sample = |channel: usize| {
            let a = self.samples[index * channels + channel];
            let b = self.samples[next * channels + channel];
            a + (b - a) * fraction
        };
        let frame = if channels == 2 {
            (sample(0), sample(1))
        } else {
            let mono = sample(0);
            (mono, mono)
        };
        self.cursor += self.rate;
        Some(frame)
    }

    /// The next source frame through the voice's muffle.
    fn muffled_frame(&mut self) -> Option<(f32, f32)> {
        let frame = self.interpolated_frame()?;
        Some(self.muffler.process(frame))
    }

    /// The fade envelope for this frame, or `None` once a fade-out has finished.
    fn envelope(&mut self) -> Option<f32> {
        let mut envelope = 1.0;
        if let Some(fade) = &mut self.fade_in {
            if fade.remaining == 0 {
                self.fade_in = None;
            } else {
                #[allow(clippy::cast_precision_loss)]
                let progress = fade.remaining as f32 / fade.total as f32;
                envelope *= 1.0 - progress;
                fade.remaining -= 1;
            }
        }
        if let Some(fade) = &mut self.fade_out {
            if fade.remaining == 0 {
                return None;
            }
            #[allow(clippy::cast_precision_loss)]
            let remaining = fade.remaining as f32 / fade.total as f32;
            envelope *= remaining;
            fade.remaining -= 1;
        }
        Some(envelope)
    }

    /// The next mixed frame without HRTF: spatial voices are downmixed and panned with a
    /// constant-power law, stereo plays as is, and mono sits in the centre at -3 dB per side.
    fn next_frame(&mut self) -> Option<(f32, f32)> {
        let (source_left, source_right) = self.muffled_frame()?;
        let (left, right) = if self.spatial {
            let mono = f32::midpoint(source_left, source_right);
            let angle = (self.direction[0].clamp(-1.0, 1.0) + 1.0) * 0.25 * std::f32::consts::PI;
            (
                mono * angle.cos() * self.gain,
                mono * angle.sin() * self.gain,
            )
        } else if self.channels == 2 {
            (source_left * self.gain, source_right * self.gain)
        } else {
            let mono = source_left * std::f32::consts::FRAC_1_SQRT_2 * self.gain;
            (mono, mono)
        };
        let envelope = self.envelope()?;
        Some((left * envelope, right * envelope))
    }

    /// The next already-convolved HRTF frame. Distance gain is baked in by the processor; only
    /// the fade envelopes remain.
    #[cfg(feature = "hrtf")]
    fn next_hrtf_frame(&mut self) -> Option<(f32, f32)> {
        let (left, right) = self.hrtf.output.pop_front()?;
        let envelope = self.envelope()?;
        Some((left * envelope, right * envelope))
    }
}

/// The HRTF processor and its scratch buffers, owned by the mixer.
#[cfg(feature = "hrtf")]
struct HrtfState {
    processor: hrtf::HrtfProcessor,
    input: Vec<f32>,
    output: Vec<(f32, f32)>,
}

#[cfg(feature = "hrtf")]
impl HrtfState {
    /// Convolves one block of a spatial voice into its output queue. Returns `false` once the
    /// source is used up and no more blocks will come.
    fn refill(&mut self, voice: &mut Voice) -> bool {
        use crate::spatial::HRTF_BLOCK_TOTAL;

        if voice.hrtf.source_exhausted {
            return false;
        }
        self.input.clear();
        self.input.resize(HRTF_BLOCK_TOTAL, 0.0);
        self.output.clear();
        self.output.resize(HRTF_BLOCK_TOTAL, (0.0, 0.0));
        let mut filled = 0;
        for sample in &mut self.input {
            // A rate-aware, downmixed read, so pitch applies to spatial voices too.
            let Some((left, right)) = voice.muffled_frame() else {
                break;
            };
            *sample = f32::midpoint(left, right);
            filled += 1;
        }
        if filled == 0 {
            voice.hrtf.source_exhausted = true;
            return false;
        }
        // A source that ended mid-block is already zero-padded; mark it so the next refill
        // finishes the voice.
        let partial = filled < HRTF_BLOCK_TOTAL;
        // HRIR spheres are right-handed with +X right and +Y up, so the front is -Z; the
        // mixer's listener space has the front on +Z.
        let vector = |v: Vec3| hrtf::Vec3 {
            x: v[0],
            y: v[1],
            z: -v[2],
        };
        self.processor.process_samples(hrtf::HrtfContext {
            source: &self.input,
            output: &mut self.output,
            new_sample_vector: vector(voice.direction),
            prev_sample_vector: vector(voice.hrtf.previous_direction),
            prev_left_samples: &mut voice.hrtf.previous_left,
            prev_right_samples: &mut voice.hrtf.previous_right,
            new_distance_gain: voice.gain,
            prev_distance_gain: voice.hrtf.previous_gain,
        });
        voice.hrtf.output.extend(self.output.iter().copied());
        voice.hrtf.previous_direction = voice.direction;
        voice.hrtf.previous_gain = voice.gain;
        voice.hrtf.source_exhausted = partial;
        true
    }
}

/// The audio-thread half: every voice, the listener, the buses and the reverb.
///
/// Feed it output buffers with [`Mixer::render`]. It owns no device, so it can mix offline into
/// any buffer, which is how the tests listen to it.
pub struct Mixer {
    voices: Vec<Voice>,
    listener: Listener,
    rolloff: Rolloff,
    commands: mpsc::Receiver<Command>,
    #[cfg(feature = "hrtf")]
    hrtf: Option<HrtfState>,
    volumes: Volumes,
    reverb: Reverb,
    reverb_send: Vec<f32>,
    sample_rate: u32,
}

impl std::fmt::Debug for Mixer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Mixer")
            .field("voices", &self.voices.len())
            .field("sample_rate", &self.sample_rate)
            .field("hrtf", &self.hrtf_enabled())
            .finish_non_exhaustive()
    }
}

impl Mixer {
    /// A mixer at `sample_rate` that pans spatial voices in stereo, and the [`Controls`] that
    /// drive it.
    #[must_use]
    pub fn new(sample_rate: u32) -> (Self, Controls) {
        Self::build(
            sample_rate,
            #[cfg(feature = "hrtf")]
            None,
        )
    }

    /// A mixer at `sample_rate` that spatializes through `hrtf`, which must have been loaded at
    /// the same rate, and the [`Controls`] that drive it.
    #[cfg(feature = "hrtf")]
    #[must_use]
    pub fn with_hrtf(sample_rate: u32, hrtf: crate::spatial::Hrtf) -> (Self, Controls) {
        Self::build(sample_rate, Some(hrtf))
    }

    fn build(
        sample_rate: u32,
        #[cfg(feature = "hrtf")] hrtf: Option<crate::spatial::Hrtf>,
    ) -> (Self, Controls) {
        let (sender, receiver) = mpsc::channel();
        #[cfg(feature = "hrtf")]
        let hrtf = hrtf.map(|hrtf| HrtfState {
            processor: hrtf.processor,
            input: Vec::with_capacity(crate::spatial::HRTF_BLOCK_TOTAL),
            output: Vec::with_capacity(crate::spatial::HRTF_BLOCK_TOTAL),
        });
        let mixer = Self {
            voices: Vec::with_capacity(16),
            listener: Listener::default(),
            rolloff: Rolloff::default(),
            commands: receiver,
            #[cfg(feature = "hrtf")]
            hrtf,
            volumes: Volumes::default(),
            reverb: Reverb::new(sample_rate),
            reverb_send: Vec::with_capacity(2_048),
            sample_rate,
        };
        let controls = Controls {
            commands: sender,
            sample_rate,
            hrtf_enabled: mixer.hrtf_enabled(),
            next_voice_id: Arc::new(AtomicU64::new(1)),
        };
        (mixer, controls)
    }

    /// The rate the mixer renders at, in frames per second.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Whether spatial voices go through the HRTF rather than the stereo pan.
    #[must_use]
    pub fn hrtf_enabled(&self) -> bool {
        #[cfg(feature = "hrtf")]
        {
            self.hrtf.is_some()
        }
        #[cfg(not(feature = "hrtf"))]
        {
            false
        }
    }

    /// Voices still playing after the last render, including ones fading out.
    #[must_use]
    pub fn active_voices(&self) -> usize {
        self.voices.len()
    }

    /// Applies every pending command, then mixes one buffer of interleaved frames into `output`,
    /// overwriting it. `output.len()` should be a multiple of `channels`; a mono device gets the
    /// downmix, and channels past the second get the centre feed. The result is clamped to
    /// -1..1.
    pub fn render(&mut self, output: &mut [f32], channels: usize) {
        self.drain_commands();
        output.fill(0.0);
        if channels == 0 {
            return;
        }
        let frames = output.len() / channels;
        self.reverb_send.clear();
        self.reverb_send.resize(frames, 0.0);

        let mut index = 0;
        while index < self.voices.len() {
            let use_hrtf = self.prepare_hrtf(index, frames);
            let voice = &mut self.voices[index];
            let bus = self.volumes.master
                * match voice.bus {
                    MixBus::Effects => self.volumes.effects,
                    MixBus::Music => self.volumes.music,
                    MixBus::Ambience => self.volumes.ambience,
                    MixBus::Voice => 1.0,
                };
            let mut ended = false;
            for frame in 0..frames {
                let sample = if use_hrtf {
                    #[cfg(feature = "hrtf")]
                    {
                        voice.next_hrtf_frame()
                    }
                    #[cfg(not(feature = "hrtf"))]
                    {
                        None
                    }
                } else {
                    voice.next_frame()
                };
                let Some((left, right)) = sample else {
                    ended = true;
                    break;
                };
                let (left, right) = (left * bus, right * bus);
                // Post-gain send: distance and the user's volume carry into the tail. Music and
                // ambience stay out of the room, unless they sound in it, from a place in the world.
                if voice.bus == MixBus::Effects || voice.spatial {
                    self.reverb_send[frame] += f32::midpoint(left, right);
                }
                if channels >= 2 {
                    let base = frame * channels;
                    output[base] += left;
                    output[base + 1] += right;
                    let center = f32::midpoint(left, right);
                    for sample in &mut output[base + 2..base + channels] {
                        *sample += center;
                    }
                } else {
                    output[frame] += f32::midpoint(left, right);
                }
            }
            if ended {
                self.voices.swap_remove(index);
            } else {
                index += 1;
            }
        }
        self.reverb.process(
            &self.reverb_send,
            &mut output[..frames * channels],
            channels,
        );
        for sample in output {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }

    /// Convolves enough HRTF blocks for a spatial voice to cover `frames`. Returns whether the
    /// voice plays through the HRTF at all.
    #[cfg(feature = "hrtf")]
    fn prepare_hrtf(&mut self, index: usize, frames: usize) -> bool {
        let Some(hrtf) = self.hrtf.as_mut() else {
            return false;
        };
        let voice = &mut self.voices[index];
        if !voice.spatial {
            return false;
        }
        while voice.hrtf.output.len() < frames {
            if !hrtf.refill(voice) {
                break;
            }
        }
        true
    }

    #[cfg(not(feature = "hrtf"))]
    #[allow(clippy::unused_self)]
    fn prepare_hrtf(&mut self, _: usize, _: usize) -> bool {
        false
    }

    fn drain_commands(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            self.apply(command);
        }
    }

    // One arm per command; splitting the match would only scatter it.
    #[allow(clippy::too_many_lines)]
    fn apply(&mut self, command: Command) {
        match command {
            Command::Play {
                id,
                samples,
                channels,
                gain,
                rate,
                bus,
                world,
                fade_samples,
                looping,
                muffle,
            } => {
                let muffler = Muffler::new(muffle, self.sample_rate);
                let mut voice = Voice::new(samples, channels, gain, rate, muffler);
                voice.id = Some(id);
                voice.bus = bus;
                voice.looping = looping;
                if fade_samples > 0 {
                    voice.fade_in = Some(Fade::over(fade_samples));
                }
                if let Some((position, emitter)) = world {
                    voice.spatial = true;
                    voice.position = position;
                    voice.emitter = emitter;
                    voice.relocate(&self.listener, self.rolloff);
                    #[cfg(feature = "hrtf")]
                    {
                        voice.hrtf.previous_direction = voice.direction;
                        voice.hrtf.previous_gain = voice.gain;
                    }
                }
                self.voices.push(voice);
            }
            Command::StopVoice { id, fade_samples } => {
                for voice in &mut self.voices {
                    if voice.id == Some(id) {
                        voice.fade_out(fade_samples);
                    }
                }
            }
            Command::PlayStream {
                id,
                reader,
                gain,
                position,
                emitter,
            } => {
                let muffler = Muffler::new(Muffle::CLEAR, self.sample_rate);
                let mut voice = Voice::new(Arc::from([]), 1, gain, 1.0, muffler);
                voice.id = Some(id);
                voice.stream = Some(reader);
                voice.bus = MixBus::Voice;
                voice.spatial = true;
                voice.position = position;
                voice.emitter = emitter;
                voice.relocate(&self.listener, self.rolloff);
                #[cfg(feature = "hrtf")]
                {
                    voice.hrtf.previous_direction = voice.direction;
                    voice.hrtf.previous_gain = voice.gain;
                }
                self.voices.push(voice);
            }
            Command::SetGain { id, gain } => {
                for voice in &mut self.voices {
                    if voice.id == Some(id) {
                        voice.base_gain = gain;
                        if voice.spatial {
                            voice.relocate(&self.listener, self.rolloff);
                        } else {
                            voice.gain = gain;
                        }
                    }
                }
            }
            Command::SetMuffle { id, muffle } => {
                for voice in &mut self.voices {
                    if voice.id == Some(id) {
                        voice.muffler.set(muffle, self.sample_rate);
                    }
                }
            }
            Command::Listener(listener) => {
                self.listener = listener;
                self.relocate_spatial(|_| true);
            }
            Command::MoveEmitter { emitter, position } => {
                for voice in &mut self.voices {
                    if voice.spatial && voice.emitter == Some(emitter) {
                        voice.position = position;
                    }
                }
                self.relocate_spatial(|voice| voice.emitter == Some(emitter));
            }
            Command::StopEmitter {
                emitter,
                fade_samples,
            } => {
                for voice in &mut self.voices {
                    if voice.emitter == Some(emitter) {
                        voice.fade_out(fade_samples);
                    }
                }
            }
            Command::StopMusic { fade_samples } => {
                for voice in &mut self.voices {
                    if voice.bus == MixBus::Music && !voice.spatial {
                        voice.fade_out(fade_samples);
                    }
                }
            }
            Command::Volumes(volumes) => self.volumes = volumes,
            Command::Room(room) => self.reverb.set_params(room.wet, room.decay, room.damp),
            Command::Rolloff(rolloff) => {
                self.rolloff = rolloff;
                self.relocate_spatial(|_| true);
            }
        }
    }

    fn relocate_spatial(&mut self, mut which: impl FnMut(&Voice) -> bool) {
        for voice in &mut self.voices {
            if voice.spatial && which(voice) {
                voice.relocate(&self.listener, self.rolloff);
            }
        }
    }
}

/// The game-side handle to a [`Mixer`]: cheap to clone, safe to share between threads.
///
/// Each call queues a command that the mixer applies at the start of its next buffer. Gains are
/// in decibels, `rate` is a playback-rate (pitch) multiplier where 1 is the clip's own, and fades
/// are in seconds. Calls after the mixer is gone are ignored.
#[derive(Clone, Debug)]
pub struct Controls {
    commands: mpsc::Sender<Command>,
    sample_rate: u32,
    hrtf_enabled: bool,
    next_voice_id: Arc<AtomicU64>,
}

impl Controls {
    /// The mixer's sample rate: load sounds at this rate.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Whether spatial voices go through the HRTF rather than the stereo pan.
    #[must_use]
    pub fn hrtf_enabled(&self) -> bool {
        self.hrtf_enabled
    }

    fn send(&self, command: Command) {
        // A closed channel means the mixer (and its device) is gone; there is nobody to hear.
        let _ = self.commands.send(command);
    }

    fn next_id(&self) -> VoiceId {
        VoiceId(self.next_voice_id.fetch_add(1, Ordering::Relaxed))
    }

    fn fade_samples(&self, seconds: f32) -> usize {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let samples = (seconds.max(0.0) * self.sample_rate as f32) as usize;
        samples
    }

    /// Plays a clip as `play` says: once or looping, on a bus, unpositioned or at a place in
    /// the world, fading in or not. Returns a handle that [`Self::stop_voice`] fades and
    /// [`Self::set_voice_gain`] turns; it can be ignored for a one-shot.
    ///
    /// A looping voice plays until [`Self::stop_voice`], [`Self::stop_emitter`] for its
    /// emitter, or for unpositioned music [`Self::stop_music`]. Fading one out while playing the
    /// next is a crossfade.
    // Not `must_use`: a one-shot's handle is routinely ignored.
    #[allow(clippy::must_use_candidate)]
    pub fn play(&self, sound: &Sound, play: Play) -> VoiceId {
        let id = self.next_id();
        self.send(Command::Play {
            id,
            samples: Arc::clone(&sound.samples),
            channels: sound.channels,
            gain: db_to_amplitude(play.gain_db),
            rate: play.rate,
            bus: play.bus.into(),
            world: match play.placement {
                Placement::Flat => None,
                Placement::World { position, emitter } => Some((position, emitter)),
            },
            fade_samples: self.fade_samples(play.fade_in),
            looping: play.looping,
            muffle: play.muffle,
        });
        id
    }

    /// Fades a voice out over `fade_seconds` and ends it. Unknown or finished voices are
    /// ignored.
    pub fn stop_voice(&self, id: VoiceId, fade_seconds: f32) {
        self.send(Command::StopVoice {
            id,
            fade_samples: self.fade_samples(fade_seconds),
        });
    }

    /// Starts a live voice at a world position, a player's speech, and returns the
    /// [`VoiceStream`] to push its samples into as they arrive. It's spatial as a clip played at
    /// [`Placement::World`] is, follows `emitter` with [`Self::move_emitter`], and
    /// sounds in the room's reverb; it's on the master volume alone, so set its own with
    /// [`Self::set_voice_gain`]. It plays what's pushed [`StreamConfig::latency`] late, and keeps
    /// to that through bunched packets and a sender whose clock runs fast or slow.
    ///
    /// It ends once the stream is dropped and what was in it has played, or through
    /// [`Self::stop_voice`] or [`Self::stop_emitter`].
    #[must_use = "the stream is the only way to feed the voice, which ends when it is dropped"]
    pub fn play_spatial_stream(
        &self,
        config: StreamConfig,
        gain_db: f32,
        position: impl Into<[f32; 3]>,
        emitter: Option<EmitterId>,
    ) -> VoiceStream {
        let id = self.next_id();
        let (reader, stream) = StreamReader::pair(config, self.sample_rate, id);
        self.send(Command::PlayStream {
            id,
            reader: Box::new(reader),
            gain: db_to_amplitude(gain_db),
            position: position.into(),
            emitter,
        });
        stream
    }

    /// Sets a live voice's gain, in place of the one it started with: a player's voice volume,
    /// or a speaker turned down or muted. Unknown or finished voices are ignored.
    pub fn set_voice_gain(&self, id: VoiceId, gain_db: f32) {
        self.send(Command::SetGain {
            id,
            gain: db_to_amplitude(gain_db),
        });
    }

    /// Muffles a playing voice, a clip's or a live one's, gliding to `muffle` from how muffled it
    /// was over about 50 ms: what stands between it and the listener changed.
    /// [`Muffle::CLEAR`] unmuffles it. Unknown or finished voices are ignored.
    pub fn set_voice_muffle(&self, id: VoiceId, muffle: Muffle) {
        self.send(Command::SetMuffle { id, muffle });
    }

    /// Moves the listener, in a world whose up is +Y. Call once per rendered frame with the
    /// camera.
    pub fn set_listener(&self, position: impl Into<[f32; 3]>, forward: impl Into<[f32; 3]>) {
        self.set_listener_oriented(position, forward, [0.0, 1.0, 0.0]);
    }

    /// Moves the listener in a world with its own up axis, such as +Z.
    pub fn set_listener_oriented(
        &self,
        position: impl Into<[f32; 3]>,
        forward: impl Into<[f32; 3]>,
        up: impl Into<[f32; 3]>,
    ) {
        self.send(Command::Listener(Listener::oriented(
            position.into(),
            forward.into(),
            up.into(),
        )));
    }

    /// Re-anchors every live voice started with `emitter` at `position`.
    pub fn move_emitter(&self, emitter: EmitterId, position: impl Into<[f32; 3]>) {
        self.send(Command::MoveEmitter {
            emitter,
            position: position.into(),
        });
    }

    /// Fades out every live voice started with `emitter`.
    pub fn stop_emitter(&self, emitter: EmitterId, fade_seconds: f32) {
        self.send(Command::StopEmitter {
            emitter,
            fade_samples: self.fade_samples(fade_seconds),
        });
    }

    /// Fades out every unpositioned voice on [`Bus::Music`] over `fade_seconds`, leaving music
    /// playing from a place in the world (a radio, a gramophone) be.
    pub fn stop_music(&self, fade_seconds: f32) {
        self.send(Command::StopMusic {
            fade_samples: self.fade_samples(fade_seconds),
        });
    }

    /// Sets the bus volumes, clamped to 0..1. Live voices follow immediately.
    pub fn set_volumes(&self, volumes: Volumes) {
        self.send(Command::Volumes(volumes.clamped()));
    }

    /// Sets the reverb's room. The mixer smooths the change over about 50 ms, so this can be
    /// sent whenever the room changes. [`RoomAcoustics::DRY`] turns the reverb off.
    pub fn set_room(&self, room: RoomAcoustics) {
        self.send(Command::Room(room));
    }

    /// Sets how spatial voices fade with distance. Live voices follow immediately.
    pub fn set_rolloff(&self, rolloff: Rolloff) {
        self.send(Command::Rolloff(rolloff));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn constant(value: f32, frames: usize, channels: u16) -> Sound {
        Sound::from_interleaved(
            vec![value; frames * usize::from(channels)],
            channels,
            RATE,
            RATE,
        )
    }

    fn render(mixer: &mut Mixer, frames: usize) -> Vec<f32> {
        let mut buffer = vec![0.0; frames * 2];
        mixer.render(&mut buffer, 2);
        buffer
    }

    fn peak(buffer: &[f32]) -> f32 {
        buffer
            .iter()
            .fold(0.0, |peak, sample| peak.max(sample.abs()))
    }

    #[test]
    fn a_voice_plays_and_ends() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.play(&constant(0.5, 100, 1), Play::default());
        let buffer = render(&mut mixer, 256);
        let expected = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        assert!((buffer[0] - expected).abs() < 1.0e-6);
        assert!((buffer[2 * 99 + 1] - expected).abs() < 1.0e-6);
        assert!(buffer[200..].iter().all(|&sample| sample == 0.0));
        assert_eq!(mixer.active_voices(), 0);
    }

    #[test]
    fn a_doubled_rate_plays_in_half_the_time() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.play(
            &constant(0.5, 100, 1),
            Play {
                rate: 2.0,
                ..Play::default()
            },
        );
        let buffer = render(&mut mixer, 256);
        assert!(buffer[2 * 49] > 0.0);
        assert!(buffer[2 * 50..].iter().all(|&sample| sample == 0.0));
    }

    #[test]
    fn gain_and_buses_scale_their_own_voices() {
        let (mut mixer, controls) = Mixer::new(RATE);
        let sound = constant(0.5, 4_800, 2);
        controls.play(
            &sound,
            Play {
                gain_db: -6.0,
                ..Play::default()
            },
        );
        let effect = render(&mut mixer, 64)[0];
        assert!((effect - 0.5 * db_to_amplitude(-6.0)).abs() < 1.0e-6);

        controls.set_volumes(Volumes {
            master: 0.5,
            effects: 0.5,
            music: 1.0,
            ambience: 1.0,
        });
        let quieter = render(&mut mixer, 64)[0];
        assert!((quieter - effect * 0.25).abs() < 1.0e-6);

        // Music ignores the effects bus but not the master.
        controls.play(
            &sound,
            Play {
                bus: Bus::Music,
                looping: true,
                ..Play::default()
            },
        );
        let with_music = render(&mut mixer, 64)[0];
        assert!((with_music - quieter - 0.25).abs() < 1.0e-6);

        controls.set_volumes(Volumes {
            master: 2.0,
            effects: -1.0,
            music: 1.0,
            ambience: 1.0,
        });
        let clamped = render(&mut mixer, 64)[0];
        assert!((clamped - 0.5).abs() < 1.0e-6, "only the music, at unity");
    }

    /// Music played in the world is on the music bus, follows its volume while it plays, sounds
    /// in the room, and outlasts the tracks stopping.
    #[test]
    fn music_in_the_world_follows_the_music_volume() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        let sound = constant(0.5, RATE as usize, 1);
        let radio = controls.play(
            &sound,
            Play {
                bus: Bus::Music,
                placement: Placement::at([0.0, 0.0, -1.0]),
                ..Play::default()
            },
        );
        let full = render(&mut mixer, 64)[0];
        assert!(full > 0.3, "{full}");
        controls.set_volumes(Volumes {
            master: 1.0,
            effects: 0.0,
            music: 0.5,
            ambience: 1.0,
        });
        let half = render(&mut mixer, 64)[0];
        assert!((half - full * 0.5).abs() < 1.0e-5, "{half} of {full}");
        controls.stop_music(0.0);
        assert!((render(&mut mixer, 64)[0] - half).abs() < 1.0e-5);
        controls.set_room(RoomAcoustics {
            wet: 1.0,
            decay: 2.0,
            damp: 0.2,
        });
        render(&mut mixer, 4_800);
        controls.stop_voice(radio, 0.0);
        let tail = render(&mut mixer, 4_800);
        assert_eq!(mixer.active_voices(), 0);
        assert!(
            tail.iter().any(|&sample| sample.abs() > 1.0e-4),
            "no reverb tail"
        );
    }

    #[test]
    fn music_fades_in_and_out() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.play(
            &constant(0.5, RATE as usize, 2),
            Play {
                bus: Bus::Music,
                fade_in: 0.01,
                looping: true,
                ..Play::default()
            },
        );
        let fade_in = render(&mut mixer, 960);
        assert!(fade_in[0].abs() < 1.0e-3);
        assert!((fade_in[2 * 240] - 0.25).abs() < 1.0e-3);
        assert!((fade_in[2 * 959] - 0.5).abs() < 1.0e-3);
        assert!((render(&mut mixer, 16)[0] - 0.5).abs() < 1.0e-6);

        controls.stop_music(0.01);
        let fade_out = render(&mut mixer, 960);
        assert!((fade_out[0] - 0.5).abs() < 1.0e-3);
        assert!((fade_out[2 * 240] - 0.25).abs() < 1.0e-3);
        assert!(fade_out[2 * 480..].iter().all(|&sample| sample == 0.0));
        assert_eq!(mixer.active_voices(), 0);
    }

    #[test]
    fn looping_beds_keep_playing_until_stopped() {
        let (mut mixer, controls) = Mixer::new(RATE);
        let bed = controls.play(
            &constant(0.25, 100, 1),
            Play {
                bus: Bus::Ambience,
                looping: true,
                ..Play::default()
            },
        );
        let buffer = render(&mut mixer, 1_000);
        assert!(buffer.iter().all(|&sample| sample > 0.0));
        controls.stop_voice(bed, 0.0);
        render(&mut mixer, 8);
        assert_eq!(mixer.active_voices(), 0);
    }

    /// Any clip can fade in and loop wherever it plays: a waterfall's ambience placed in the
    /// world rises over its fade, outlasts its clip, and stops with its emitter.
    #[test]
    fn a_placed_ambience_fades_in_loops_and_stops_with_its_emitter() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        let waterfall = EmitterId(7);
        controls.play(
            &constant(0.5, 100, 1),
            Play {
                bus: Bus::Ambience,
                placement: Placement::World {
                    position: [0.0, 0.0, -1.0],
                    emitter: Some(waterfall),
                },
                fade_in: 0.01,
                looping: true,
                ..Play::default()
            },
        );
        let rising = render(&mut mixer, 960);
        assert!(rising[0].abs() < 1.0e-3, "{}", rising[0]);
        assert!(rising[2 * 959] > rising[2 * 240] && rising[2 * 240] > 0.0);
        render(&mut mixer, 1_000);
        assert_eq!(
            mixer.active_voices(),
            1,
            "a looping voice outlasts its clip"
        );
        controls.stop_emitter(waterfall, 0.0);
        render(&mut mixer, 8);
        assert_eq!(mixer.active_voices(), 0);
    }

    #[test]
    fn stereo_fallback_pans_toward_the_source() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        let sound = constant(0.5, 4_800, 1);
        controls.play(
            &sound,
            Play {
                placement: Placement::at([3.0, 0.0, 0.0]),
                ..Play::default()
            },
        );
        let right = render(&mut mixer, 64);
        assert!(right[1] > 0.49, "hard right: {right:?}");
        assert!(right[0].abs() < 1.0e-6);

        let (mut mixer, controls) = Mixer::new(RATE);
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        controls.play(
            &sound,
            Play {
                placement: Placement::at([0.0, 0.0, -3.0]),
                ..Play::default()
            },
        );
        let ahead = render(&mut mixer, 64);
        assert!((ahead[0] - ahead[1]).abs() < 1.0e-6);
        assert!((ahead[0] - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1.0e-5);

        // Turning to face the source re-pans the live voice to the centre.
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        controls.play(
            &sound,
            Play {
                placement: Placement::at([3.0, 0.0, 0.0]),
                ..Play::default()
            },
        );
        render(&mut mixer, 64);
        controls.set_listener([0.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
        let turned = render(&mut mixer, 64);
        assert!((turned[0] - turned[1]).abs() < 1.0e-5);
    }

    /// An octahedral HRIR sphere whose impulse puts a source's x on the matching ear and makes
    /// the back (+Z, as in measured spheres) quieter than the front, so the HRTF path can be
    /// heard without a measured data set.
    #[cfg(feature = "hrtf")]
    fn ear_sphere() -> crate::spatial::Hrtf {
        let points: [[f32; 3]; 6] = [
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ];
        let faces: [[u32; 3]; 8] = [
            [0, 2, 4],
            [4, 2, 1],
            [1, 2, 5],
            [5, 2, 0],
            [0, 4, 3],
            [4, 1, 3],
            [1, 5, 3],
            [5, 0, 3],
        ];
        let length = 16_u32;
        let mut bytes = b"HRIR".to_vec();
        for value in [RATE, length, 6, 24] {
            bytes.extend(value.to_le_bytes());
        }
        for index in faces.iter().flatten() {
            bytes.extend(index.to_le_bytes());
        }
        for point in points {
            for coordinate in point {
                bytes.extend(coordinate.to_le_bytes());
            }
            let behind = if point[2] > 0.5 { 0.25 } else { 1.0 };
            for ear in [(1.0 - point[0]) * 0.5, f32::midpoint(1.0, point[0])] {
                let ear = ear * behind;
                for tap in 0..length {
                    let value: f32 = if tap == 0 { ear } else { 0.0 };
                    bytes.extend(value.to_le_bytes());
                }
            }
        }
        crate::spatial::Hrtf::from_reader(&bytes[..], RATE).unwrap()
    }

    #[cfg(feature = "hrtf")]
    #[test]
    fn hrtf_voices_land_on_the_right_ear_and_end() {
        let (mut mixer, controls) = Mixer::with_hrtf(RATE, ear_sphere());
        assert!(controls.hrtf_enabled());
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        controls.play(
            &constant(0.5, 3_000, 1),
            Play {
                placement: Placement::at([3.0, 0.0, 0.0]),
                ..Play::default()
            },
        );
        let buffer = render(&mut mixer, 2_048);
        let (left, right) = buffer
            .as_chunks::<2>()
            .0
            .iter()
            .fold((0.0, 0.0), |(l, r), frame| {
                (l + frame[0] * frame[0], r + frame[1] * frame[1])
            });
        assert!(right > 0.0);
        assert!(left < right * 0.01, "left {left} right {right}");
        assert_eq!(mixer.active_voices(), 1);
        for _ in 0..3 {
            render(&mut mixer, 1_024);
        }
        assert_eq!(mixer.active_voices(), 0);
    }

    #[cfg(feature = "hrtf")]
    #[test]
    fn hrtf_keeps_the_front_in_front() {
        let energy_from = |position: [f32; 3]| {
            let (mut mixer, controls) = Mixer::with_hrtf(RATE, ear_sphere());
            controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
            controls.play(
                &constant(0.5, 2_048, 1),
                Play {
                    placement: Placement::at(position),
                    ..Play::default()
                },
            );
            render(&mut mixer, 2_048)
                .iter()
                .map(|sample| sample * sample)
                .sum::<f32>()
        };
        let ahead = energy_from([0.0, 0.0, -3.0]);
        let behind = energy_from([0.0, 0.0, 3.0]);
        assert!(ahead > 4.0 * behind, "ahead {ahead} behind {behind}");
    }

    /// A sound at the highest frequency there is, its samples alternating.
    fn highest(value: f32, frames: usize) -> Sound {
        let samples = (0..frames)
            .map(|frame| if frame % 2 == 0 { value } else { -value })
            .collect();
        Sound::from_interleaved(samples, 1, RATE, RATE)
    }

    /// A muffled voice is turned down by its gain and its highs are cut: a steady level comes
    /// through at the gain alone, the highest frequency next to not at all.
    #[test]
    fn a_muffle_turns_down_and_dulls() {
        let muffle = Muffle {
            gain_db: -12.0,
            cutoff_hz: 400.0,
        };
        let settled = |sound: &Sound, muffle: Muffle| {
            let (mut mixer, controls) = Mixer::new(RATE);
            controls.play(
                sound,
                Play {
                    muffle,
                    ..Play::default()
                },
            );
            peak(&render(&mut mixer, 4_800)[2 * 4_000..])
        };
        let steady = constant(0.5, 4_800, 1);
        let (clear, muffled) = (settled(&steady, Muffle::CLEAR), settled(&steady, muffle));
        assert!(
            (muffled - clear * db_to_amplitude(-12.0)).abs() < 1.0e-4,
            "{muffled} of {clear}"
        );
        let high = highest(0.5, 4_800);
        let (clear, muffled) = (settled(&high, Muffle::CLEAR), settled(&high, muffle));
        assert!(clear > 0.3, "{clear}");
        assert!(muffled < clear * 1.0e-3, "{muffled} of {clear}");
    }

    /// A muffle set on a playing voice glides there rather than stepping, and back once clear.
    #[test]
    fn a_muffle_set_while_playing_glides() {
        let (mut mixer, controls) = Mixer::new(RATE);
        let radio = controls.play(
            &constant(0.5, RATE as usize, 1),
            Play {
                looping: true,
                ..Play::default()
            },
        );
        let clear = render(&mut mixer, 64)[0];
        controls.set_voice_muffle(
            radio,
            Muffle {
                gain_db: -20.0,
                cutoff_hz: f32::INFINITY,
            },
        );
        let gliding = render(&mut mixer, 2_400);
        assert!(
            gliding[0] > clear * 0.99,
            "a step: {} of {clear}",
            gliding[0]
        );
        let later = gliding[2 * 2_399];
        assert!(
            later > clear * 0.3 && later < clear * 0.6,
            "{later} of {clear}"
        );
        render(&mut mixer, 24_000);
        let muffled = render(&mut mixer, 64)[0];
        assert!(
            (muffled - clear * 0.1).abs() < 1.0e-4,
            "{muffled} of {clear}"
        );
        controls.set_voice_muffle(radio, Muffle::CLEAR);
        render(&mut mixer, 24_000);
        assert!((render(&mut mixer, 64)[0] - clear).abs() < 1.0e-6);
    }

    #[cfg(feature = "hrtf")]
    #[test]
    fn the_hrtf_hears_a_voice_muffled() {
        let energy = |muffle: Muffle| {
            let (mut mixer, controls) = Mixer::with_hrtf(RATE, ear_sphere());
            controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
            controls.play(
                &highest(0.5, 2_048),
                Play {
                    placement: Placement::at([0.0, 0.0, -3.0]),
                    muffle,
                    ..Play::default()
                },
            );
            render(&mut mixer, 2_048)
                .iter()
                .map(|sample| sample * sample)
                .sum::<f32>()
        };
        let clear = energy(Muffle::CLEAR);
        let muffled = energy(Muffle {
            gain_db: 0.0,
            cutoff_hz: 400.0,
        });
        assert!(clear > 0.0);
        assert!(muffled < clear * 1.0e-3, "{muffled} of {clear}");
    }

    #[test]
    fn emitters_carry_their_voices_and_fade_by_distance() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        let walker = EmitterId(7);
        controls.play(
            &constant(0.5, RATE as usize, 1),
            Play {
                placement: Placement::World {
                    position: [0.0, 0.0, -3.0],
                    emitter: Some(walker),
                },
                ..Play::default()
            },
        );
        let near = render(&mut mixer, 16)[0];
        controls.move_emitter(walker, [0.0, 0.0, -12.0]);
        let far = render(&mut mixer, 16)[0];
        assert!((far - near * 0.25).abs() < 1.0e-5);
        controls.move_emitter(walker, [0.0, 0.0, -80.0]);
        assert!(peak(&render(&mut mixer, 16)) < 1.0e-9);
        controls.stop_emitter(walker, 0.0);
        render(&mut mixer, 4);
        assert_eq!(mixer.active_voices(), 0);
    }

    #[test]
    fn a_tracked_spatial_voice_fades_without_stopping_its_neighbours() {
        let (mut mixer, controls) = Mixer::new(RATE);
        let sound = constant(0.1, RATE as usize, 1);
        let tracked = controls.play(
            &sound,
            Play {
                placement: Placement::at([1.0, 0.0, 0.0]),
                ..Play::default()
            },
        );
        controls.play(
            &sound,
            Play {
                placement: Placement::at([1.0, 0.0, 0.0]),
                ..Play::default()
            },
        );
        controls.stop_voice(tracked, 0.16);
        mixer.drain_commands();
        assert!(mixer.voices[0].spatial);
        assert!(mixer.voices[0].fade_out.is_some());
        assert!(mixer.voices[1].fade_out.is_none());
        render(&mut mixer, 7_681);
        assert_eq!(mixer.active_voices(), 1);
    }

    #[test]
    fn the_room_adds_a_tail_to_effects_only() {
        let click = constant(0.5, 48, 1);
        let tail = |music: bool, room: RoomAcoustics| {
            let (mut mixer, controls) = Mixer::new(RATE);
            controls.set_room(room);
            render(&mut mixer, 4_800);
            if music {
                controls.play(
                    &click,
                    Play {
                        bus: Bus::Music,
                        ..Play::default()
                    },
                );
            } else {
                controls.play(&click, Play::default());
            }
            render(&mut mixer, 4_800);
            peak(&render(&mut mixer, 4_800))
        };
        let hall = RoomAcoustics {
            wet: 0.8,
            decay: 2.0,
            damp: 0.3,
        };
        assert!(tail(false, hall) > 1.0e-4);
        assert!(tail(false, RoomAcoustics::DRY) < 1.0e-9);
        assert!(tail(true, hall) < 1.0e-9);
    }

    #[test]
    fn surround_and_mono_devices_get_a_sensible_mix() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.play(&constant(0.5, 64, 2), Play::default());
        let mut surround = vec![0.0; 6 * 32];
        mixer.render(&mut surround, 6);
        assert!(
            surround[..6]
                .iter()
                .all(|&sample| (sample - 0.5).abs() < 1.0e-6)
        );

        let (mut mixer, controls) = Mixer::new(RATE);
        controls.play(&constant(0.5, 64, 2), Play::default());
        let mut mono = vec![0.0; 32];
        mixer.render(&mut mono, 1);
        assert!(mono.iter().all(|&sample| (sample - 0.5).abs() < 1.0e-6));
    }

    /// A live voice is spatial and in the room, on the master volume and its own gain but not the
    /// effects', outlasts a gap in what it's fed, and lets its writer know once it's stopped.
    #[test]
    fn a_live_voice_is_heard_in_the_world_and_room() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        controls.set_room(RoomAcoustics {
            wet: 1.0,
            decay: 2.0,
            damp: 0.2,
        });
        let speaker = EmitterId(3);
        let mut stream = controls.play_spatial_stream(
            StreamConfig::new(RATE),
            0.0,
            [3.0, 0.0, 0.0],
            Some(speaker),
        );
        stream.push(&vec![0.5; 4_800]);
        let buffer = render(&mut mixer, 1_024);
        let (left, right) = buffer.as_chunks::<2>().0[512..]
            .iter()
            .fold((0.0_f32, 0.0_f32), |(l, r), frame| {
                (l.max(frame[0].abs()), r.max(frame[1]))
            });
        assert!(right > 0.45, "right {right}");
        assert!(left < right * 0.2, "left {left}, from the reverb alone");

        controls.set_volumes(Volumes {
            master: 1.0,
            effects: 0.0,
            music: 0.0,
            ambience: 0.0,
        });
        controls.set_voice_gain(stream.id(), -6.0);
        let quieter = render(&mut mixer, 64)[1];
        assert!(
            (quieter - right * db_to_amplitude(-6.0)).abs() < 0.03,
            "{quieter} of {right}"
        );

        // Run it dry and leave it a while: the voice waits rather than ending.
        render(&mut mixer, 8_192);
        assert_eq!(mixer.active_voices(), 1);
        assert!(stream.is_playing());
        controls.stop_emitter(speaker, 0.0);
        render(&mut mixer, 16);
        assert_eq!(mixer.active_voices(), 0);
        assert!(!stream.is_playing());
    }

    #[cfg(feature = "hrtf")]
    #[test]
    fn a_live_voice_through_the_hrtf_waits_out_a_gap_and_ends_when_dropped() {
        let (mut mixer, controls) = Mixer::with_hrtf(RATE, ear_sphere());
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        let mut stream =
            controls.play_spatial_stream(StreamConfig::new(RATE), 0.0, [3.0, 0.0, 0.0], None);
        stream.push(&vec![0.5; 2_880]);
        // The test sphere's level is its own; what counts is the voice sounding again as loud.
        let first = peak(&render(&mut mixer, 4_096));
        assert!(first > 0.0);
        render(&mut mixer, 8_192);
        assert_eq!(mixer.active_voices(), 1, "ended in a gap");
        stream.push(&vec![0.5; 2_880]);
        let again = peak(&render(&mut mixer, 4_096));
        assert!(again > first * 0.9, "{again} after the gap, {first} before");
        drop(stream);
        render(&mut mixer, 8_192);
        assert_eq!(mixer.active_voices(), 0);
    }

    #[test]
    fn controls_outlive_the_mixer_quietly() {
        let (mixer, controls) = Mixer::new(RATE);
        drop(mixer);
        controls.play(&constant(0.5, 64, 1), Play::default());
        controls.set_room(RoomAcoustics::DRY);
    }
}
