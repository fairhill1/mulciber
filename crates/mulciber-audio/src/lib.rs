//! Game audio for Mulciber: a callback-thread mixer with pitchable voices, volume buses, music
//! fades, moving emitters, binaural HRTF spatialization with a stereo-pan fallback, and a
//! Freeverb-style room reverb sized by a probe of the world around the listener.
//!
//! The mixer is split in two. [`Mixer`] holds every voice and DSP state and renders interleaved
//! `f32` frames with [`Mixer::render`]; it belongs on the audio callback thread. [`Controls`] is
//! the cloneable, thread-safe handle the game keeps: every call on it becomes a command the mixer
//! drains at the start of its next buffer, so the game thread never blocks on audio.
//!
//! With the default `output` feature, `AudioEngine` opens the default output device (or one of
//! `output_devices`) through cpal, moves the mixer onto its callback, and dereferences to its
//! [`Controls`]; `AudioEngine::set_output` moves the mixer to another device as it plays. Without a
//! device, or in tests, build the pair with [`Mixer::new`] (or `Mixer::with_hrtf`) and call
//! [`Mixer::render`] on a buffer to mix offline.
//!
//! ```
//! use mulciber_audio::{Mixer, Play, Sound};
//!
//! let (mut mixer, controls) = Mixer::new(48_000);
//! let click = Sound::from_interleaved(vec![0.5; 480], 1, 48_000, 48_000);
//! controls.play(&click, Play { gain_db: -6.0, ..Play::default() });
//!
//! let mut buffer = vec![0.0; 2 * 512];
//! mixer.render(&mut buffer, 2);
//! assert!(buffer[0] > 0.0);
//! assert_eq!(mixer.active_voices(), 0);
//! ```
//!
//! Room acoustics are game-agnostic: [`probe_room`] casts a sphere of rays through a trace the game
//! supplies (voxels, a heightfield, BSP, colliders) and returns the [`RoomAcoustics`] to feed the
//! reverb, and [`RoomTracker`] re-probes on an interval and eases the result so walking through a
//! door fades the tail instead of cutting it.
//!
//! A sound recorded several times (footsteps, knocks, a weapon's swings) plays as [`Takes`]: a
//! take at random each time, never the same one twice running, at a playback rate varied within
//! a range; [`other_take`] is the same choice for a game that keeps its own random source.
//!
//! A voice with something in the way (a wall, a shut door, a deck) is muffled: turned down and
//! dulled by a low-pass ([`Muffle`]), set as it starts ([`Play::muffle`]) or as it plays
//! ([`Controls::set_voice_muffle`]). What stands between is the game's to work out.
//!
//! Live audio, a player's speech, plays through [`Controls::play_spatial_stream`]: push samples
//! into the [`VoiceStream`] it returns as they arrive, and a jitter buffer plays them a little
//! late, in the world and the room as any spatial voice. With the `input` feature, `Microphone`
//! captures the default input device, or one of `input_devices`.
//!
//! Positions and directions are `[f32; 3]`, accepted through `impl Into<[f32; 3]>` so math types
//! such as `glam::Vec3` pass straight in.

mod acoustics;
#[cfg(any(feature = "input", feature = "output"))]
mod devices;
#[cfg(feature = "input")]
mod input;
mod math;
mod mixer;
#[cfg(feature = "output")]
mod output;
mod reverb;
mod sound;
mod spatial;
mod stream;
mod takes;

pub use acoustics::{ProbeSettings, RayHit, RoomAcoustics, RoomTracker, probe_room};
#[cfg(any(feature = "input", feature = "output"))]
pub use devices::AudioDevice;
#[cfg(feature = "input")]
pub use input::{InputError, Microphone, input_devices};
pub use mixer::{
    Bus, Controls, EmitterId, Mixer, Muffle, Placement, Play, Rolloff, VoiceId, Volumes,
};
#[cfg(feature = "output")]
pub use output::{AudioEngine, OutputError, output_devices};
#[cfg(any(feature = "wav", feature = "ogg"))]
pub use sound::{LoadError, LoadErrorKind};
pub use sound::{Sound, db_to_amplitude, resample};
#[cfg(feature = "hrtf")]
pub use spatial::{Hrtf, HrtfLoadError};
pub use stream::{StreamConfig, StreamResampler, VoiceStream};
pub use takes::{Takes, other_take};
