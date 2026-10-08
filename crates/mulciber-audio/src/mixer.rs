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

/// A caller-chosen identity for a moving sound source, such as a packed entity id.
///
/// Spatial voices started with an emitter follow [`Controls::move_emitter`] and stop together
/// through [`Controls::stop_emitter`], so a creature's footsteps and cries stay on the creature
/// and a death can cut off whatever it was still saying.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EmitterId(pub u64);

/// A handle to one playing voice, for fading it out before its clip ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VoiceId(u64);

/// Volume per bus, each 0..1. Every voice is scaled by `master` and by its own bus.
///
/// Effects are one-shots and spatial voices, music is [`Controls::start_music`], and ambience is
/// the looping beds of [`Controls::play_bed`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Volumes {
    /// Scales everything.
    pub master: f32,
    /// One-shots and spatial voices.
    pub effects: f32,
    /// Music tracks.
    pub music: f32,
    /// Ambience beds.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bus {
    Effects,
    Music,
    /// On the effects side of the world but on its own volume, and kept out of the reverb send:
    /// a recording of a forest already has the forest's reverb in it.
    Ambience,
}

enum Command {
    Play {
        id: Option<VoiceId>,
        samples: Arc<[f32]>,
        channels: u8,
        gain: f32,
        rate: f64,
    },
    PlayBed {
        id: VoiceId,
        samples: Arc<[f32]>,
        channels: u8,
        gain: f32,
        fade_samples: usize,
    },
    StopVoice {
        id: VoiceId,
        fade_samples: usize,
    },
    PlaySpatial {
        id: Option<VoiceId>,
        samples: Arc<[f32]>,
        channels: u8,
        gain: f32,
        rate: f64,
        position: Vec3,
        emitter: Option<EmitterId>,
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
    StartMusic {
        samples: Arc<[f32]>,
        channels: u8,
        gain: f32,
        fade_samples: usize,
        looping: bool,
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
    /// Fractional read position in source frames; `rate` is added per output frame.
    cursor: f64,
    rate: f64,
    /// Gain after distance attenuation for spatial voices.
    gain: f32,
    /// Gain before distance attenuation.
    base_gain: f32,
    looping: bool,
    bus: Bus,
    spatial: bool,
    position: Vec3,
    emitter: Option<EmitterId>,
    fade_out: Option<Fade>,
    fade_in: Option<Fade>,
    /// Listener-space direction (x right, y up, z forward).
    direction: Vec3,
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
    fn new(samples: Arc<[f32]>, channels: u8, gain: f32, rate: f64) -> Self {
        Self {
            id: None,
            samples,
            channels: channels.max(1),
            cursor: 0.0,
            rate,
            gain,
            base_gain: gain,
            looping: false,
            bus: Bus::Effects,
            spatial: false,
            position: [0.0; 3],
            emitter: None,
            fade_out: None,
            fade_in: None,
            direction: [0.0, 0.0, 1.0],
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
        let (source_left, source_right) = self.interpolated_frame()?;
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
            let Some((left, right)) = voice.interpolated_frame() else {
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
                    Bus::Effects => self.volumes.effects,
                    Bus::Music => self.volumes.music,
                    Bus::Ambience => self.volumes.ambience,
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
                // ambience stay out of the room.
                if voice.bus == Bus::Effects {
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
            } => {
                let mut voice = Voice::new(samples, channels, gain, rate);
                voice.id = id;
                self.voices.push(voice);
            }
            Command::PlayBed {
                id,
                samples,
                channels,
                gain,
                fade_samples,
            } => {
                let mut voice = Voice::new(samples, channels, gain, 1.0);
                voice.id = Some(id);
                voice.looping = true;
                voice.bus = Bus::Ambience;
                if fade_samples > 0 {
                    voice.fade_in = Some(Fade::over(fade_samples));
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
            Command::PlaySpatial {
                id,
                samples,
                channels,
                gain,
                rate,
                position,
                emitter,
            } => {
                let mut voice = Voice::new(samples, channels, gain, rate);
                voice.id = id;
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
            Command::StartMusic {
                samples,
                channels,
                gain,
                fade_samples,
                looping,
            } => {
                let mut voice = Voice::new(samples, channels, gain, 1.0);
                voice.bus = Bus::Music;
                voice.looping = looping;
                if fade_samples > 0 {
                    voice.fade_in = Some(Fade::over(fade_samples));
                }
                self.voices.push(voice);
            }
            Command::StopMusic { fade_samples } => {
                for voice in &mut self.voices {
                    if voice.bus == Bus::Music {
                        voice.fade_out(fade_samples);
                    }
                }
            }
            Command::Volumes(volumes) => self.volumes = volumes,
            Command::Room(room) => self.reverb.set_params(room.wet, room.room, room.damp),
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

    /// Plays a clip once on the effects bus, unpositioned.
    pub fn play(&self, sound: &Sound, gain_db: f32, rate: f64) {
        self.send(Command::Play {
            id: None,
            samples: Arc::clone(&sound.samples),
            channels: sound.channels,
            gain: db_to_amplitude(gain_db),
            rate,
        });
    }

    /// Plays a clip once on the effects bus, unpositioned, returning a handle that
    /// [`Self::stop_voice`] can fade.
    #[must_use = "the handle is the only way to stop the voice; use `play` otherwise"]
    pub fn play_tracked(&self, sound: &Sound, gain_db: f32, rate: f64) -> VoiceId {
        let id = self.next_id();
        self.send(Command::Play {
            id: Some(id),
            samples: Arc::clone(&sound.samples),
            channels: sound.channels,
            gain: db_to_amplitude(gain_db),
            rate,
        });
        id
    }

    /// Starts a looping, unpositioned bed on the ambience bus that fades in over `fade_seconds`.
    /// Beds skip the reverb send. Fading one out with [`Self::stop_voice`] while starting the
    /// next is a crossfade.
    #[must_use = "a looping bed plays until stopped through its handle"]
    pub fn play_bed(&self, sound: &Sound, gain_db: f32, fade_seconds: f32) -> VoiceId {
        let id = self.next_id();
        self.send(Command::PlayBed {
            id,
            samples: Arc::clone(&sound.samples),
            channels: sound.channels,
            gain: db_to_amplitude(gain_db),
            fade_samples: self.fade_samples(fade_seconds),
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

    /// Plays a clip once at a world position on the effects bus. The voice is downmixed to mono,
    /// attenuated by distance and spatialized, and stays anchored in the world as the listener
    /// moves. With an emitter it follows [`Self::move_emitter`].
    pub fn play_spatial(
        &self,
        sound: &Sound,
        gain_db: f32,
        rate: f64,
        position: impl Into<[f32; 3]>,
        emitter: Option<EmitterId>,
    ) {
        self.send(Command::PlaySpatial {
            id: None,
            samples: Arc::clone(&sound.samples),
            channels: sound.channels,
            gain: db_to_amplitude(gain_db),
            rate,
            position: position.into(),
            emitter,
        });
    }

    /// Like [`Self::play_spatial`] without an emitter, returning a handle that
    /// [`Self::stop_voice`] can fade.
    #[must_use = "the handle is the only way to stop the voice; use `play_spatial` otherwise"]
    pub fn play_spatial_tracked(
        &self,
        sound: &Sound,
        gain_db: f32,
        rate: f64,
        position: impl Into<[f32; 3]>,
    ) -> VoiceId {
        let id = self.next_id();
        self.send(Command::PlaySpatial {
            id: Some(id),
            samples: Arc::clone(&sound.samples),
            channels: sound.channels,
            gain: db_to_amplitude(gain_db),
            rate,
            position: position.into(),
            emitter: None,
        });
        id
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

    /// Starts a track on the music bus, fading in over `fade_seconds`. A looping track plays
    /// until [`Self::stop_music`]; otherwise it ends with its clip. Music skips the reverb.
    /// Stopping the old track while starting a new one is a crossfade.
    pub fn start_music(&self, sound: &Sound, gain_db: f32, fade_seconds: f32, looping: bool) {
        self.send(Command::StartMusic {
            samples: Arc::clone(&sound.samples),
            channels: sound.channels,
            gain: db_to_amplitude(gain_db),
            fade_samples: self.fade_samples(fade_seconds),
            looping,
        });
    }

    /// Fades out every music track over `fade_seconds`.
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
        controls.play(&constant(0.5, 100, 1), 0.0, 1.0);
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
        controls.play(&constant(0.5, 100, 1), 0.0, 2.0);
        let buffer = render(&mut mixer, 256);
        assert!(buffer[2 * 49] > 0.0);
        assert!(buffer[2 * 50..].iter().all(|&sample| sample == 0.0));
    }

    #[test]
    fn gain_and_buses_scale_their_own_voices() {
        let (mut mixer, controls) = Mixer::new(RATE);
        let sound = constant(0.5, 4_800, 2);
        controls.play(&sound, -6.0, 1.0);
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
        controls.start_music(&sound, 0.0, 0.0, true);
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

    #[test]
    fn music_fades_in_and_out() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.start_music(&constant(0.5, RATE as usize, 2), 0.0, 0.01, true);
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
        let bed = controls.play_bed(&constant(0.25, 100, 1), 0.0, 0.0);
        let buffer = render(&mut mixer, 1_000);
        assert!(buffer.iter().all(|&sample| sample > 0.0));
        controls.stop_voice(bed, 0.0);
        render(&mut mixer, 8);
        assert_eq!(mixer.active_voices(), 0);
    }

    #[test]
    fn stereo_fallback_pans_toward_the_source() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        let sound = constant(0.5, 4_800, 1);
        controls.play_spatial(&sound, 0.0, 1.0, [3.0, 0.0, 0.0], None);
        let right = render(&mut mixer, 64);
        assert!(right[1] > 0.49, "hard right: {right:?}");
        assert!(right[0].abs() < 1.0e-6);

        let (mut mixer, controls) = Mixer::new(RATE);
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        controls.play_spatial(&sound, 0.0, 1.0, [0.0, 0.0, -3.0], None);
        let ahead = render(&mut mixer, 64);
        assert!((ahead[0] - ahead[1]).abs() < 1.0e-6);
        assert!((ahead[0] - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1.0e-5);

        // Turning to face the source re-pans the live voice to the centre.
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        controls.play_spatial(&sound, 0.0, 1.0, [3.0, 0.0, 0.0], None);
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
        controls.play_spatial(&constant(0.5, 3_000, 1), 0.0, 1.0, [3.0, 0.0, 0.0], None);
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
            controls.play_spatial(&constant(0.5, 2_048, 1), 0.0, 1.0, position, None);
            render(&mut mixer, 2_048)
                .iter()
                .map(|sample| sample * sample)
                .sum::<f32>()
        };
        let ahead = energy_from([0.0, 0.0, -3.0]);
        let behind = energy_from([0.0, 0.0, 3.0]);
        assert!(ahead > 4.0 * behind, "ahead {ahead} behind {behind}");
    }

    #[test]
    fn emitters_carry_their_voices_and_fade_by_distance() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.set_listener([0.0, 0.0, 0.0], [0.0, 0.0, -1.0]);
        let walker = EmitterId(7);
        controls.play_spatial(
            &constant(0.5, RATE as usize, 1),
            0.0,
            1.0,
            [0.0, 0.0, -3.0],
            Some(walker),
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
        let tracked = controls.play_spatial_tracked(&sound, 0.0, 1.0, [1.0, 0.0, 0.0]);
        controls.play_spatial(&sound, 0.0, 1.0, [1.0, 0.0, 0.0], None);
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
                controls.start_music(&click, 0.0, 0.0, false);
            } else {
                controls.play(&click, 0.0, 1.0);
            }
            render(&mut mixer, 4_800);
            peak(&render(&mut mixer, 4_800))
        };
        let hall = RoomAcoustics {
            wet: 0.8,
            room: 0.8,
            damp: 0.3,
        };
        assert!(tail(false, hall) > 1.0e-4);
        assert!(tail(false, RoomAcoustics::DRY) < 1.0e-9);
        assert!(tail(true, hall) < 1.0e-9);
    }

    #[test]
    fn surround_and_mono_devices_get_a_sensible_mix() {
        let (mut mixer, controls) = Mixer::new(RATE);
        controls.play(&constant(0.5, 64, 2), 0.0, 1.0);
        let mut surround = vec![0.0; 6 * 32];
        mixer.render(&mut surround, 6);
        assert!(
            surround[..6]
                .iter()
                .all(|&sample| (sample - 0.5).abs() < 1.0e-6)
        );

        let (mut mixer, controls) = Mixer::new(RATE);
        controls.play(&constant(0.5, 64, 2), 0.0, 1.0);
        let mut mono = vec![0.0; 32];
        mixer.render(&mut mono, 1);
        assert!(mono.iter().all(|&sample| (sample - 0.5).abs() < 1.0e-6));
    }

    #[test]
    fn controls_outlive_the_mixer_quietly() {
        let (mixer, controls) = Mixer::new(RATE);
        drop(mixer);
        controls.play(&constant(0.5, 64, 1), 0.0, 1.0);
        controls.set_room(RoomAcoustics::DRY);
    }
}
