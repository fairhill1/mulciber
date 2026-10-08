//! The device-backed engine: the mixer running on a cpal output stream.

use std::path::Path;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::mixer::{Controls, Mixer};

/// The [`Mixer`] running on the default output device, and its [`Controls`].
///
/// Dereferences to [`Controls`], so `engine.play(..)` and friends work directly; clone
/// [`AudioEngine::controls`] to drive it from other threads. Dropping the engine closes the
/// stream.
pub struct AudioEngine {
    controls: Controls,
    _stream: cpal::Stream,
}

impl std::fmt::Debug for AudioEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AudioEngine")
            .field("controls", &self.controls)
            .finish_non_exhaustive()
    }
}

impl AudioEngine {
    /// Opens the default output device at its default rate and channel count, spatializing
    /// through the HRIR sphere at `hrir_path`.
    ///
    /// A missing or unreadable sphere is not an error: spatial voices fall back to the stereo
    /// pan, which [`Controls::hrtf_enabled`] reports. Without the `hrtf` feature the path is
    /// ignored.
    ///
    /// # Errors
    ///
    /// Returns an [`OutputError`] when there is no output device, it does not take `f32`
    /// samples, or the stream cannot be built or started.
    pub fn new(hrir_path: impl AsRef<Path>) -> Result<Self, OutputError> {
        Self::open(Some(hrir_path.as_ref()))
    }

    /// Opens the default output device with stereo-pan spatialization.
    ///
    /// # Errors
    ///
    /// As [`AudioEngine::new`].
    pub fn without_hrtf() -> Result<Self, OutputError> {
        Self::open(None)
    }

    fn open(hrir_path: Option<&Path>) -> Result<Self, OutputError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(OutputError::NoDevice)?;
        let supported = device
            .default_output_config()
            .map_err(OutputError::Config)?;
        if supported.sample_format() != cpal::SampleFormat::F32 {
            return Err(OutputError::UnsupportedSampleFormat(
                supported.sample_format(),
            ));
        }
        let sample_rate = supported.sample_rate();
        let channels = usize::from(supported.channels());
        let (mut mixer, controls) = mixer_for(sample_rate, hrir_path);
        let stream = device
            .build_output_stream(
                supported.config(),
                move |output: &mut [f32], _| mixer.render(output, channels),
                |error| eprintln!("audio stream error: {error}"),
                None,
            )
            .map_err(OutputError::Build)?;
        stream.play().map_err(OutputError::Play)?;
        Ok(Self {
            controls,
            _stream: stream,
        })
    }

    /// The handle that drives the mixer; clone it to share.
    #[must_use]
    pub fn controls(&self) -> &Controls {
        &self.controls
    }
}

#[cfg(feature = "hrtf")]
fn mixer_for(sample_rate: u32, hrir_path: Option<&Path>) -> (Mixer, Controls) {
    match hrir_path.map(|path| crate::spatial::Hrtf::load(path, sample_rate)) {
        Some(Ok(hrtf)) => Mixer::with_hrtf(sample_rate, hrtf),
        _ => Mixer::new(sample_rate),
    }
}

#[cfg(not(feature = "hrtf"))]
fn mixer_for(sample_rate: u32, _: Option<&Path>) -> (Mixer, Controls) {
    Mixer::new(sample_rate)
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
            Self::NoDevice | Self::UnsupportedSampleFormat(_) => None,
        }
    }
}
