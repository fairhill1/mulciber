//! The device-backed engine: the mixer running on a cpal output stream, moved to another device
//! when the player chooses one.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, StreamTrait};

use crate::devices::{self, AudioDevice, Direction};
use crate::mixer::{Controls, Mixer};
use crate::stream::StreamResampler;

/// The mixer's frames rendered at once on a device at another rate, to be resampled.
const BLOCK: usize = 256;

/// The [`Mixer`] running on an output device, the default or one chosen, and its [`Controls`].
///
/// Dereferences to [`Controls`], so `engine.play(..)` and friends work directly; clone
/// [`AudioEngine::controls`] to drive it from other threads. Dropping the engine closes the
/// stream.
///
/// [`AudioEngine::set_output`] moves the same mixer to another device, its voices playing on and
/// its sounds kept: the mixer keeps the rate it was opened at, and on a device at another rate
/// what it renders is resampled to the device's.
pub struct AudioEngine {
    controls: Controls,
    mixer: Arc<Mutex<Mixer>>,
    /// The stream rendering now; any other still running renders silence.
    current: Arc<AtomicU64>,
    /// The device asked for, None the default.
    device: Option<String>,
    lost: Arc<AtomicBool>,
    #[cfg(feature = "hrtf")]
    hrtf_error: Option<crate::spatial::HrtfLoadError>,
    /// The stream rendering now, closed when it's dropped.
    stream: cpal::Stream,
}

impl std::fmt::Debug for AudioEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("AudioEngine");
        debug.field("controls", &self.controls);
        debug.field("device", &self.device);
        #[cfg(feature = "hrtf")]
        debug.field("hrtf_error", &self.hrtf_error);
        debug.finish_non_exhaustive()
    }
}

/// The output devices the host has, to choose one for [`AudioEngine::on_device`] or
/// [`AudioEngine::set_output`] by its id; none if it can't list them.
#[must_use]
pub fn output_devices() -> Vec<AudioDevice> {
    devices::list(Direction::Output)
}

impl AudioEngine {
    /// Opens the default output device at its default rate and channel count, spatializing
    /// through the HRIR sphere at `hrir_path`.
    ///
    /// A missing or unreadable sphere does not fail the engine: the game keeps its sound and
    /// spatial voices fall back to the stereo pan, which [`Controls::hrtf_enabled`] reports.
    /// The fallback is observable: `AudioEngine::hrtf_error` returns why the sphere did not
    /// load, so the game can log or surface it. Without the `hrtf` feature the path is ignored.
    ///
    /// # Errors
    ///
    /// Returns an [`OutputError`] when there is no output device, it does not take `f32`
    /// samples, or the stream cannot be built or started.
    pub fn new(hrir_path: impl AsRef<Path>) -> Result<Self, OutputError> {
        Self::on_device(None, Some(hrir_path.as_ref()))
    }

    /// Opens the default output device with stereo-pan spatialization.
    ///
    /// # Errors
    ///
    /// As [`AudioEngine::new`].
    pub fn without_hrtf() -> Result<Self, OutputError> {
        Self::on_device(None, None)
    }

    /// Opens the output device `device` names (an [`AudioDevice::id`] of [`output_devices`]), or
    /// the default with none, at its default rate and channel count, spatializing through the
    /// HRIR sphere at `hrir_path` if one is given, as [`AudioEngine::new`] does.
    ///
    /// # Errors
    ///
    /// As [`AudioEngine::new`], and [`OutputError::NotFound`] when no output device has the id.
    pub fn on_device(device: Option<&str>, hrir_path: Option<&Path>) -> Result<Self, OutputError> {
        let (handle, supported) = output_config(device)?;
        let MixerSetup {
            mixer,
            controls,
            #[cfg(feature = "hrtf")]
            hrtf_error,
        } = mixer_for(supported.sample_rate(), hrir_path);
        let mixer = Arc::new(Mutex::new(mixer));
        let current = Arc::new(AtomicU64::new(0));
        let lost = Arc::new(AtomicBool::new(false));
        let stream = play_on(
            &handle,
            &supported,
            &mixer,
            controls.sample_rate(),
            (&current, 0),
            &lost,
        )?;
        Ok(Self {
            controls,
            mixer,
            current,
            device: device.map(str::to_owned),
            lost,
            #[cfg(feature = "hrtf")]
            hrtf_error,
            stream,
        })
    }

    /// Moves the mixer to the output device `device` names (an [`AudioDevice::id`] of
    /// [`output_devices`]), or the default with none: what's playing plays on there, a buffer's
    /// worth late, and the sounds loaded keep playing at the mixer's rate, resampled if the device
    /// runs at another. [`AudioEngine::lost`] is cleared.
    ///
    /// # Errors
    ///
    /// As [`AudioEngine::on_device`]; then it plays on where it was.
    pub fn set_output(&mut self, device: Option<&str>) -> Result<(), OutputError> {
        let (handle, supported) = output_config(device)?;
        let next = self.current.load(Ordering::Acquire) + 1;
        let lost = Arc::new(AtomicBool::new(false));
        let rate = self.controls.sample_rate();
        let stream = play_on(
            &handle,
            &supported,
            &self.mixer,
            rate,
            (&self.current, next),
            &lost,
        )?;
        // The new stream takes over from its next buffer, and the old one goes quiet until it's
        // dropped here.
        self.current.store(next, Ordering::Release);
        (self.stream, self.lost, self.device) = (stream, lost, device.map(str::to_owned));
        Ok(())
    }

    /// The output device asked for (an [`AudioDevice::id`]), None the default.
    #[must_use]
    pub fn device(&self) -> Option<&str> {
        self.device.as_deref()
    }

    /// Whether the device has gone (unplugged, or turned off), so nothing more is heard from it
    /// until [`AudioEngine::set_output`] moves the mixer to another, or back to it once it's
    /// there again.
    #[must_use]
    pub fn lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }

    /// The handle that drives the mixer; clone it to share.
    #[must_use]
    pub fn controls(&self) -> &Controls {
        &self.controls
    }

    /// Why the HRIR sphere passed to [`AudioEngine::new`] failed to load, in which case spatial
    /// voices pan in stereo instead.
    ///
    /// `None` when the sphere loaded, or when the engine was opened with
    /// [`AudioEngine::without_hrtf`] and no sphere was requested.
    #[cfg(feature = "hrtf")]
    #[must_use]
    pub fn hrtf_error(&self) -> Option<&crate::spatial::HrtfLoadError> {
        self.hrtf_error.as_ref()
    }
}

/// The output device `device` names, or the default, and its default configuration, which must
/// take `f32`.
fn output_config(
    device: Option<&str>,
) -> Result<(cpal::Device, cpal::SupportedStreamConfig), OutputError> {
    let handle = devices::find(Direction::Output, device)
        .map_err(|()| OutputError::NoDevice)?
        .ok_or_else(|| OutputError::NotFound(device.unwrap_or_default().to_owned()))?;
    let supported = handle
        .default_output_config()
        .map_err(OutputError::Config)?;
    if supported.sample_format() != cpal::SampleFormat::F32 {
        return Err(OutputError::UnsupportedSampleFormat(
            supported.sample_format(),
        ));
    }
    Ok((handle, supported))
}

/// A stream on `device` that renders `mixer`, at `rate`, while the current stream is
/// `generation`, and silence otherwise, marking `lost` if the device goes.
fn play_on(
    device: &cpal::Device,
    supported: &cpal::SupportedStreamConfig,
    mixer: &Arc<Mutex<Mixer>>,
    rate: u32,
    (current, generation): (&Arc<AtomicU64>, u64),
    lost: &Arc<AtomicBool>,
) -> Result<cpal::Stream, OutputError> {
    let channels = usize::from(supported.channels());
    let mut output = Output::new(rate, supported.sample_rate(), channels);
    let (mixer, current, gone) = (Arc::clone(mixer), Arc::clone(current), Arc::clone(lost));
    let stream = device
        .build_output_stream(
            supported.config(),
            move |buffer: &mut [f32], _| {
                // Never waits on the lock: only the stream taking over from this one could hold it.
                match mixer.try_lock() {
                    Ok(mut mixer) if current.load(Ordering::Acquire) == generation => {
                        output.fill(&mut mixer, buffer);
                    }
                    _ => buffer.fill(0.0),
                }
            },
            move |error| {
                if error.kind() == cpal::ErrorKind::DeviceNotAvailable {
                    gone.store(true, Ordering::Relaxed);
                }
                eprintln!("audio stream error: {error}");
            },
            None,
        )
        .map_err(OutputError::Build)?;
    stream.play().map_err(OutputError::Play)?;
    Ok(stream)
}

/// How a device's buffers are filled from the mixer: rendered straight into them at the same
/// rate, or on a device at another, rendered a block at a time and resampled channel by channel.
struct Output {
    channels: usize,
    resampling: Option<Resampling>,
}

struct Resampling {
    resamplers: Vec<StreamResampler>,
    /// A block as the mixer renders it, interleaved, and one channel of it.
    block: Vec<f32>,
    channel: Vec<f32>,
    /// Each channel resampled and not yet played, as many frames in each.
    ready: Vec<Vec<f32>>,
}

impl Output {
    /// For a mixer at `mixer_rate` playing on a device at `device_rate` with `channels`.
    fn new(mixer_rate: u32, device_rate: u32, channels: usize) -> Self {
        let resampling = (mixer_rate != device_rate).then(|| Resampling {
            resamplers: vec![StreamResampler::new(mixer_rate, device_rate); channels],
            block: vec![0.0; BLOCK * channels],
            channel: Vec::with_capacity(BLOCK),
            ready: vec![Vec::with_capacity(4 * BLOCK); channels],
        });
        Self {
            channels,
            resampling,
        }
    }

    /// Fills `buffer`, interleaved frames of the device's channels, from `mixer`.
    fn fill(&mut self, mixer: &mut Mixer, buffer: &mut [f32]) {
        let Some(r) = &mut self.resampling else {
            mixer.render(buffer, self.channels);
            return;
        };
        if self.channels == 0 {
            return;
        }
        let frames = buffer.len() / self.channels;
        while r.ready[0].len() < frames {
            mixer.render(&mut r.block, self.channels);
            for (c, (resampler, ready)) in r.resamplers.iter_mut().zip(&mut r.ready).enumerate() {
                r.channel.clear();
                r.channel
                    .extend(r.block.iter().skip(c).step_by(self.channels));
                resampler.process(&r.channel, ready);
            }
        }
        for (c, ready) in r.ready.iter_mut().enumerate() {
            for (frame, &sample) in ready[..frames].iter().enumerate() {
                buffer[frame * self.channels + c] = sample.clamp(-1.0, 1.0);
            }
            ready.drain(..frames);
        }
    }
}

/// The mixer pair for the stream, and why a requested HRIR sphere did not load.
struct MixerSetup {
    mixer: Mixer,
    controls: Controls,
    #[cfg(feature = "hrtf")]
    hrtf_error: Option<crate::spatial::HrtfLoadError>,
}

#[cfg(feature = "hrtf")]
fn mixer_for(sample_rate: u32, hrir_path: Option<&Path>) -> MixerSetup {
    let (hrtf, hrtf_error) =
        match hrir_path.map(|path| crate::spatial::Hrtf::load(path, sample_rate)) {
            Some(Ok(hrtf)) => (Some(hrtf), None),
            Some(Err(error)) => (None, Some(error)),
            None => (None, None),
        };
    let (mixer, controls) = match hrtf {
        Some(hrtf) => Mixer::with_hrtf(sample_rate, hrtf),
        None => Mixer::new(sample_rate),
    };
    MixerSetup {
        mixer,
        controls,
        hrtf_error,
    }
}

#[cfg(not(feature = "hrtf"))]
fn mixer_for(sample_rate: u32, _: Option<&Path>) -> MixerSetup {
    let (mixer, controls) = Mixer::new(sample_rate);
    MixerSetup { mixer, controls }
}

impl std::ops::Deref for AudioEngine {
    type Target = Controls;

    fn deref(&self) -> &Controls {
        &self.controls
    }
}

/// Why [`AudioEngine`] could not open the output.
#[derive(Debug)]
#[non_exhaustive]
pub enum OutputError {
    /// The host has no default output device.
    NoDevice,
    /// No output device has the id asked for (unplugged, or another machine's).
    NotFound(String),
    /// The device's default configuration could not be read.
    Config(cpal::Error),
    /// The device's default sample format is not `f32`, which the mixer renders.
    UnsupportedSampleFormat(cpal::SampleFormat),
    /// The output stream could not be built.
    Build(cpal::Error),
    /// The output stream could not be started.
    Play(cpal::Error),
}

impl std::fmt::Display for OutputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDevice => formatter.write_str("no audio output device"),
            Self::NotFound(id) => write!(formatter, "no audio output device {id}"),
            Self::Config(error) => write!(formatter, "audio output configuration: {error}"),
            Self::UnsupportedSampleFormat(format) => write!(
                formatter,
                "unsupported output sample format {format:?}; the audio mixer requires f32"
            ),
            Self::Build(error) => write!(formatter, "audio output stream: {error}"),
            Self::Play(error) => write!(formatter, "starting audio output: {error}"),
        }
    }
}

impl std::error::Error for OutputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(error) | Self::Build(error) | Self::Play(error) => Some(error),
            Self::NoDevice | Self::NotFound(_) | Self::UnsupportedSampleFormat(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixer::{Bus, Play};
    use crate::sound::Sound;

    /// A mixer at `rate` playing a 1 kHz tone, over and over, in the middle.
    fn playing_a_tone(rate: u32) -> Mixer {
        let (mixer, controls) = Mixer::new(rate);
        #[allow(clippy::cast_precision_loss)]
        let tone: Vec<f32> = (0..rate)
            .map(|n| (std::f32::consts::TAU * 1_000.0 * n as f32 / rate as f32).sin() * 0.5)
            .collect();
        let sound = Sound::from_interleaved(tone, 1, rate, rate);
        controls.play(
            &sound,
            Play {
                bus: Bus::Music,
                looping: true,
                ..Play::default()
            },
        );
        mixer
    }

    #[test]
    fn a_device_at_the_mixers_rate_is_rendered_into_straight() {
        let (mut straight, mut through) = (playing_a_tone(48_000), playing_a_tone(48_000));
        let mut output = Output::new(48_000, 48_000, 2);
        let (mut a, mut b) = (vec![0.0; 2 * 480], vec![0.0; 2 * 480]);
        for _ in 0..5 {
            straight.render(&mut a, 2);
            output.fill(&mut through, &mut b);
            assert_eq!(a, b);
        }
    }

    /// A mixer at 48 kHz on a device at 44.1 kHz fills every buffer the device asks for, of
    /// whatever size, and the tone comes out at its own pitch, without a seam between buffers.
    #[test]
    fn a_device_at_another_rate_hears_the_mixer_resampled() {
        let mut mixer = playing_a_tone(48_000);
        let mut output = Output::new(48_000, 44_100, 2);
        let mut heard = Vec::new();
        for size in [441, 333, 1_024, 7, 2_205].into_iter().cycle().take(70) {
            let mut buffer = vec![f32::NAN; 2 * size];
            output.fill(&mut mixer, &mut buffer);
            assert!(buffer.iter().all(|s| s.is_finite()), "every frame filled");
            heard.extend(buffer.iter().step_by(2).copied());
        }
        // A second past the first few samples: 2,000 crossings of zero at 1 kHz.
        let second = &heard[100..100 + 44_100];
        let crossings = second
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        assert!((1_995..=2_005).contains(&crossings), "{crossings}");
        // No step from one buffer to the next bigger than the tone's own between samples.
        #[allow(clippy::cast_precision_loss)]
        let most = std::f32::consts::TAU * 1_000.0 / 44_100.0 * 0.5 * 1.1;
        let jump = second
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(jump < most, "{jump} > {most}");
    }
}
