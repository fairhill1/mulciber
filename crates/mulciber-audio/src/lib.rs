//! Game audio for Mulciber: a callback-thread mixer with pitchable voices, volume buses, music
//! fades, moving emitters, binaural HRTF spatialization with a stereo-pan fallback, and a
//! Freeverb-style room reverb sized by a probe of the world around the listener.
//!
//! The mixer is split in two. [`Mixer`] holds every voice and DSP state and renders interleaved
//! `f32` frames with [`Mixer::render`]; it belongs on the audio callback thread. [`Controls`] is
//! the cloneable, thread-safe handle the game keeps: every call on it becomes a command the mixer
//! drains at the start of its next buffer, so the game thread never blocks on audio.
//!
//! With the default `output` feature, `AudioEngine` opens the default output device through
//! cpal, moves the mixer onto its callback, and dereferences to its [`Controls`]. Without a
//! device, or in tests, build the pair with [`Mixer::new`] (or `Mixer::with_hrtf`) and call
//! [`Mixer::render`] on a buffer to mix offline.
//!
//! ```
//! use mulciber_audio::{Mixer, Sound};
//!
//! let (mut mixer, controls) = Mixer::new(48_000);
//! let click = Sound::from_interleaved(vec![0.5; 480], 1, 48_000, 48_000);
//! controls.play(&click, -6.0, 1.0);
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
//! Positions and directions are `[f32; 3]`, accepted through `impl Into<[f32; 3]>` so math types
//! such as `glam::Vec3` pass straight in.

mod acoustics;
mod math;
mod mixer;
#[cfg(feature = "output")]
mod output;
mod reverb;
mod sound;
mod spatial;

pub use acoustics::{ProbeSettings, RayHit, RoomAcoustics, RoomTracker, probe_room};
pub use mixer::{Controls, EmitterId, Mixer, Rolloff, VoiceId, Volumes};
#[cfg(feature = "output")]
pub use output::{AudioEngine, OutputError};
#[cfg(any(feature = "wav", feature = "ogg"))]
pub use sound::{LoadError, LoadErrorKind};
pub use sound::{Sound, db_to_amplitude, resample};
#[cfg(feature = "hrtf")]
pub use spatial::{Hrtf, HrtfLoadError};
