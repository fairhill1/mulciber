//! The microphone: the default input device on a cpal stream, read as mono at the rate asked for.

use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};

use crate::stream::{Ring, StreamResampler};

/// How much the microphone holds for the game before dropping what's newest, in seconds.
const HELD: f64 = 0.5;

/// The default input device, captured from when it opens. [`Self::read`] what it heard since the
/// last read, mono at the rate it was opened for, each game frame or on a thread of its own.
///
/// Dropping it closes the device. Like [`crate::AudioEngine`]'s, its stream belongs on the thread
/// that opened it.
pub struct Microphone {
    ring: Arc<Ring>,
    resampler: StreamResampler,
    device_rate: u32,
    scratch: Vec<f32>,
    _stream: cpal::Stream,
}

impl std::fmt::Debug for Microphone {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Microphone")
            .field("device_rate", &self.device_rate)
            .finish_non_exhaustive()
    }
}

impl Microphone {
    /// Opens the default input device at its own rate and format, to be read at `sample_rate`.
    ///
    /// # Errors
    ///
    /// Returns an [`InputError`] when there is no input device, its sample format is not one of
    /// `f32`, `i16`, `u16` or `i32`, or the stream cannot be built or started (on macOS, also when
    /// the player hasn't allowed the game the microphone).
    pub fn open(sample_rate: u32) -> Result<Self, InputError> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or(InputError::NoDevice)?;
        let supported = device.default_input_config().map_err(InputError::Config)?;
        let device_rate = supported.sample_rate();
        let channels = usize::from(supported.channels()).max(1);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ring = Arc::new(Ring::new((f64::from(device_rate) * HELD) as usize));
        let config = supported.config();
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => capture::<f32>(&device, config, channels, &ring),
            cpal::SampleFormat::I16 => capture::<i16>(&device, config, channels, &ring),
            cpal::SampleFormat::U16 => capture::<u16>(&device, config, channels, &ring),
            cpal::SampleFormat::I32 => capture::<i32>(&device, config, channels, &ring),
            format => return Err(InputError::UnsupportedSampleFormat(format)),
        }
        .map_err(InputError::Build)?;
        stream.play().map_err(InputError::Play)?;
        Ok(Self {
            ring,
            resampler: StreamResampler::new(device_rate, sample_rate),
            device_rate,
            scratch: Vec::new(),
            _stream: stream,
        })
    }

    /// Appends what the microphone heard since the last read to `output`, mono at the rate it
    /// was opened for. Half a second unread is all it keeps.
    pub fn read(&mut self, output: &mut Vec<f32>) {
        self.scratch.clear();
        self.scratch.extend(std::iter::from_fn(|| self.ring.pop()));
        self.resampler.process(&self.scratch, output);
    }

    /// Drops what the microphone heard and hasn't been read, as when push-to-talk is pressed
    /// after a while unread.
    pub fn clear(&mut self) {
        self.ring.discard(self.ring.len());
    }

    /// The device's own rate, which [`Self::read`] resamples from.
    #[must_use]
    pub fn device_sample_rate(&self) -> u32 {
        self.device_rate
    }
}

fn capture<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    ring: &Arc<Ring>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ring = Arc::clone(ring);
    let mut mono = Vec::with_capacity(4_096);
    device.build_input_stream(
        config,
        move |input: &[T], _| {
            mono.clear();
            mono.extend(downmix(input, channels));
            ring.push(&mono);
        },
        |error| eprintln!("audio input stream error: {error}"),
        None,
    )
}

/// Interleaved frames of any sample type as mono `f32`, each frame the mean of its channels.
fn downmix<T>(input: &[T], channels: usize) -> impl Iterator<Item = f32> + '_
where
    T: Sample,
    f32: FromSample<T>,
{
    #[allow(clippy::cast_precision_loss)]
    let scale = 1.0 / channels as f32;
    input.chunks_exact(channels).map(move |frame| {
        frame
            .iter()
            .map(|&sample| sample.to_sample::<f32>())
            .sum::<f32>()
            * scale
    })
}

/// Why [`Microphone`] could not open the input.
#[derive(Debug)]
#[non_exhaustive]
pub enum InputError {
    /// The host has no default input device.
    NoDevice,
    /// The device's default configuration could not be read.
    Config(cpal::Error),
    /// The device's default sample format is not one the microphone reads.
    UnsupportedSampleFormat(cpal::SampleFormat),
    /// The input stream could not be built.
    Build(cpal::Error),
    /// The input stream could not be started.
    Play(cpal::Error),
}

impl std::fmt::Display for InputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDevice => formatter.write_str("no audio input device"),
            Self::Config(error) => write!(formatter, "audio input configuration: {error}"),
            Self::UnsupportedSampleFormat(format) => {
                write!(formatter, "unsupported input sample format {format:?}")
            }
            Self::Build(error) => write!(formatter, "audio input stream: {error}"),
            Self::Play(error) => write!(formatter, "starting audio input: {error}"),
        }
    }
}

impl std::error::Error for InputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(error) | Self::Build(error) | Self::Play(error) => Some(error),
            Self::NoDevice | Self::UnsupportedSampleFormat(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_of_any_format_come_down_to_mono() {
        let stereo: Vec<f32> = downmix(&[0.5_f32, -0.5, 1.0, 0.0], 2).collect();
        assert_eq!(stereo, [0.0, 0.5]);
        let shorts: Vec<f32> = downmix(&[i16::MAX, i16::MIN, 0], 1).collect();
        assert!((shorts[0] - 1.0).abs() < 1.0e-4 && (shorts[1] + 1.0).abs() < 1.0e-4);
        assert!(shorts[2].abs() < f32::EPSILON);
        let unsigned: Vec<f32> = downmix(&[u16::MAX / 2 + 1], 1).collect();
        assert!(unsigned[0].abs() < 1.0e-4);
        assert_eq!(
            downmix(&[0.1_f32; 5], 2).count(),
            2,
            "a torn frame is left out"
        );
    }
}
