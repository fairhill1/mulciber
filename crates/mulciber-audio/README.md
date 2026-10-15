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
- **Music in the world**: spatial voices on the music bus, a radio's or a gramophone's, that
  follow the music volume live and sound in the room.
- **Room reverb**: a Freeverb-style bus fed by the effect voices, set by wet, decay (RT60 in
  seconds) and damp. `probe_room` sizes the room by casting a sphere of rays through a trace the
  game supplies (voxels, heightfield, BSP, colliders) and works out its decay by Sabine's formula,
  open sides absorbing as open windows do, and `RoomTracker` re-probes on an interval and eases
  the result.
- **Live voices**: `play_spatial_stream` plays audio pushed as it arrives, a player's speech
  from the network, through the spatial path and the room's reverb. A jitter buffer holds back
  `StreamConfig::latency` (60 ms by default) and leans the playback rate by at most 1% to keep it
  through bunched packets and a sender's clock running fast or slow; a stream run dry fades out
  and waits, and one fallen far behind drops what's late. `set_voice_gain` sets its volume live.
- **Microphone** (`input` feature): the default input device, read as mono at the rate asked
  for, through `StreamResampler`, which resamples chunked audio without seams.
- **Decoding**: WAV (`wav` feature) and Ogg Vorbis (`ogg` feature), resampled to the mixer rate
  at load.

`AudioEngine` (feature `output`, on by default) opens the default output device through cpal.
`Mixer::render` mixes into any buffer, so tests and offline tools can listen without a device:

```rust
use mulciber_audio::{Mixer, Play, Sound};

let (mut mixer, controls) = Mixer::new(48_000);
let click = Sound::from_interleaved(vec![0.5; 480], 1, 48_000, 48_000);
controls.play(&click, Play { gain_db: -6.0, ..Play::default() });

let mut buffer = vec![0.0; 2 * 512];
mixer.render(&mut buffer, 2);
```

No HRIR data ships with the crate; pass the game's sphere file to `AudioEngine::new`. Spheres
built from the IRCAM Listen database by
[hrir_sphere_builder](https://github.com/mrDIMAS/hrir_sphere_builder) work; IRCAM asks that
products using Listen acknowledge it. A sphere that fails to load does not fail the engine:
spatial voices fall back to the stereo pan, and `AudioEngine::hrtf_error` says why.
