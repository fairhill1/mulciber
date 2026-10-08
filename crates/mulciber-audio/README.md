# mulciber-audio

Game audio for Mulciber: a mixer that runs on the audio callback thread, driven from the game
through a cloneable command handle.

- **Voices**: one-shots with gain in dB and a playback-rate multiplier for pitch, tracked voices
  that can be faded out early, and looping ambience beds that crossfade.
- **Buses**: master, effects, music and ambience volumes, applied live.
- **Music**: tracks that fade in and out, looping or one-shot.
- **Spatial voices**: world-anchored one-shots with inverse-square rolloff that stay put as the
  listener moves, and emitters that carry their voices with a moving source. Binaural through an
  HRIR sphere (the `hrtf` crate's format) when one is supplied, constant-power stereo pan
  otherwise.
- **Room reverb**: a Freeverb-style bus fed by the effect voices, set by wet, room and damp.
  `probe_room` sizes the room by casting a sphere of rays through a trace the game supplies
  (voxels, heightfield, BSP, colliders), and `RoomTracker` re-probes on an interval and eases the
  result.
- **Decoding**: WAV (`wav` feature) and Ogg Vorbis (`ogg` feature), resampled to the mixer rate
  at load.

`AudioEngine` (feature `output`, on by default) opens the default output device through cpal.
`Mixer::render` mixes into any buffer, so tests and offline tools can listen without a device:

```rust
use mulciber_audio::{Mixer, Sound};

let (mut mixer, controls) = Mixer::new(48_000);
let click = Sound::from_interleaved(vec![0.5; 480], 1, 48_000, 48_000);
controls.play(&click, -6.0, 1.0);

let mut buffer = vec![0.0; 2 * 512];
mixer.render(&mut buffer, 2);
```

No HRIR data ships with the crate; pass the game's sphere file to `AudioEngine::new`. Spheres
built from the IRCAM Listen database by
[hrir_sphere_builder](https://github.com/mrDIMAS/hrir_sphere_builder) work; IRCAM asks that
products using Listen acknowledge it.
