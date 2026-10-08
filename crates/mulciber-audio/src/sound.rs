//! Decoded clips, decoding and resampling.

use std::sync::Arc;

/// Converts decibels to a linear amplitude factor: 0 dB is 1, -6 dB about one half.
#[must_use]
pub fn db_to_amplitude(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// Decoded, interleaved mono or stereo audio at the mixer's sample rate.
///
/// Cloning is cheap: the samples are shared, so one clip can play on many voices at once.
#[derive(Clone, Debug)]
pub struct Sound {
    pub(crate) samples: Arc<[f32]>,
    pub(crate) channels: u8,
}

impl Sound {
    /// Builds a clip from interleaved samples at `source_rate`, resampled to `target_rate`.
    ///
    /// Sources with more than two channels keep their first two. A trailing partial frame is
    /// dropped.
    ///
    /// # Panics
    ///
    /// Panics when `channels` is zero or either rate is zero.
    #[must_use]
    pub fn from_interleaved(
        samples: Vec<f32>,
        channels: u16,
        source_rate: u32,
        target_rate: u32,
    ) -> Self {
        assert!(channels > 0, "a sound needs at least one channel");
        assert!(
            source_rate > 0 && target_rate > 0,
            "sample rates must be positive"
        );
        let kept: u8 = if channels >= 2 { 2 } else { 1 };
        let mut normalized = if channels == u16::from(kept) {
            samples
        } else {
            samples
                .chunks_exact(usize::from(channels))
                .flat_map(|frame| [frame[0], frame[1]])
                .collect()
        };
        normalized.truncate(normalized.len() / usize::from(kept) * usize::from(kept));
        let samples = if source_rate == target_rate {
            normalized
        } else {
            resample(&normalized, usize::from(kept), source_rate, target_rate)
        };
        Self {
            samples: samples.into(),
            channels: kept,
        }
    }

    /// Decodes a WAV or Ogg Vorbis file and resamples it to `target_rate`, normally
    /// [`Controls::sample_rate`](crate::Controls::sample_rate).
    ///
    /// A `.ogg` extension (any case) selects Vorbis; anything else is read as WAV. WAV may be
    /// integer PCM up to 32 bits or 32-bit float.
    ///
    /// # Errors
    ///
    /// Returns a [`LoadError`] naming the path when the file cannot be read, is not valid audio,
    /// uses an unsupported WAV sample format, or needs a decoder whose feature is disabled.
    #[cfg(any(feature = "wav", feature = "ogg"))]
    pub fn load(path: impl AsRef<std::path::Path>, target_rate: u32) -> Result<Self, LoadError> {
        let path = path.as_ref();
        let error = |kind| LoadError {
            path: path.to_path_buf(),
            kind,
        };
        let ogg = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("ogg"));
        let (samples, rate, channels) = if ogg {
            decode_ogg(path).map_err(error)?
        } else {
            decode_wav(path).map_err(error)?
        };
        if channels == 0 || rate == 0 {
            return Err(error(LoadErrorKind::Empty));
        }
        Ok(Self::from_interleaved(samples, channels, rate, target_rate))
    }

    /// The interleaved samples, `channels()` per frame.
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// 1 for mono, 2 for stereo.
    #[must_use]
    pub fn channels(&self) -> u8 {
        self.channels
    }

    /// The number of frames, which is the length in samples of one channel.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels)
    }
}

/// Linearly resamples interleaved audio from `source_rate` to `target_rate`.
///
/// The output has `floor(frames * target_rate / source_rate)` frames and the same channel count.
/// This is the load-time converter; it is meant for clips decoded once, not for streaming.
///
/// # Panics
///
/// Panics when `channels` is zero.
#[must_use]
pub fn resample(samples: &[f32], channels: usize, source_rate: u32, target_rate: u32) -> Vec<f32> {
    assert!(channels > 0, "resampling needs at least one channel");
    let ratio = f64::from(source_rate) / f64::from(target_rate);
    let source_frames = samples.len() / channels;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let output_frames = (source_frames as f64 / ratio) as usize;
    let mut output = Vec::with_capacity(output_frames * channels);
    for output_frame in 0..output_frames {
        #[allow(clippy::cast_precision_loss)]
        let source = output_frame as f64 * ratio;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let index = source as usize;
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let fraction = (source - index as f64) as f32;
        for channel in 0..channels {
            let a = samples[index * channels + channel];
            let b = samples
                .get((index + 1) * channels + channel)
                .copied()
                .unwrap_or(a);
            output.push(a + (b - a) * fraction);
        }
    }
    output
}

/// Why [`Sound::load`] failed, with the path it was loading.
#[cfg(any(feature = "wav", feature = "ogg"))]
#[derive(Debug)]
pub struct LoadError {
    path: std::path::PathBuf,
    kind: LoadErrorKind,
}

#[cfg(any(feature = "wav", feature = "ogg"))]
impl LoadError {
    /// The file that failed to load.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// What went wrong.
    #[must_use]
    pub fn kind(&self) -> &LoadErrorKind {
        &self.kind
    }
}

/// The cause of a [`LoadError`].
#[cfg(any(feature = "wav", feature = "ogg"))]
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadErrorKind {
    /// The file could not be opened or read.
    Io(std::io::Error),
    /// The WAV decoder rejected the file.
    #[cfg(feature = "wav")]
    Wav(hound::Error),
    /// The WAV file holds a sample format the loader does not convert.
    UnsupportedWav {
        /// Whether the samples are floating point.
        float: bool,
        /// Bits per sample.
        bits: u16,
    },
    /// The Vorbis decoder rejected the file.
    #[cfg(feature = "ogg")]
    Ogg(lewton::VorbisError),
    /// The file needs a decoder whose Cargo feature (`wav` or `ogg`) is disabled.
    DecoderDisabled,
    /// The file declares no channels or a zero sample rate.
    Empty,
}

#[cfg(any(feature = "wav", feature = "ogg"))]
impl std::fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: ", self.path.display())?;
        match &self.kind {
            LoadErrorKind::Io(error) => write!(formatter, "{error}"),
            #[cfg(feature = "wav")]
            LoadErrorKind::Wav(error) => write!(formatter, "{error}"),
            LoadErrorKind::UnsupportedWav { float, bits } => write!(
                formatter,
                "unsupported WAV format {} {bits}-bit",
                if *float { "float" } else { "integer" }
            ),
            #[cfg(feature = "ogg")]
            LoadErrorKind::Ogg(error) => write!(formatter, "{error}"),
            LoadErrorKind::DecoderDisabled => {
                formatter.write_str("the decoder for this format is not enabled")
            }
            LoadErrorKind::Empty => formatter.write_str("no channels or a zero sample rate"),
        }
    }
}

#[cfg(any(feature = "wav", feature = "ogg"))]
impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            LoadErrorKind::Io(error) => Some(error),
            #[cfg(feature = "wav")]
            LoadErrorKind::Wav(error) => Some(error),
            #[cfg(feature = "ogg")]
            LoadErrorKind::Ogg(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(any(feature = "wav", feature = "ogg"))]
type Decoded = (Vec<f32>, u32, u16);

#[cfg(feature = "wav")]
fn decode_wav(path: &std::path::Path) -> Result<Decoded, LoadErrorKind> {
    let mut reader = hound::WavReader::open(path).map_err(LoadErrorKind::Wav)?;
    let specification = reader.spec();
    let samples = match (specification.sample_format, specification.bits_per_sample) {
        (hound::SampleFormat::Int, bits @ 8..=32) => {
            #[allow(clippy::cast_precision_loss)]
            let full_scale = ((1_i64 << (bits - 1)) - 1) as f32;
            reader
                .samples::<i32>()
                .map(|sample| {
                    #[allow(clippy::cast_precision_loss)]
                    sample.map(|value| value as f32 / full_scale)
                })
                .collect::<Result<_, _>>()
                .map_err(LoadErrorKind::Wav)?
        }
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(LoadErrorKind::Wav)?,
        (format, bits) => {
            return Err(LoadErrorKind::UnsupportedWav {
                float: format == hound::SampleFormat::Float,
                bits,
            });
        }
    };
    Ok((samples, specification.sample_rate, specification.channels))
}

#[cfg(all(feature = "ogg", not(feature = "wav")))]
fn decode_wav(_: &std::path::Path) -> Result<Decoded, LoadErrorKind> {
    Err(LoadErrorKind::DecoderDisabled)
}

#[cfg(feature = "ogg")]
fn decode_ogg(path: &std::path::Path) -> Result<Decoded, LoadErrorKind> {
    use lewton::inside_ogg::OggStreamReader;

    let file = std::fs::File::open(path).map_err(LoadErrorKind::Io)?;
    let mut reader = OggStreamReader::new(file).map_err(LoadErrorKind::Ogg)?;
    let sample_rate = reader.ident_hdr.audio_sample_rate;
    let channels = u16::from(reader.ident_hdr.audio_channels);
    let mut samples = Vec::new();
    while let Some(packet) = reader.read_dec_packet_itl().map_err(LoadErrorKind::Ogg)? {
        samples.extend(
            packet
                .into_iter()
                .map(|sample| f32::from(sample) / f32::from(i16::MAX)),
        );
    }
    Ok((samples, sample_rate, channels))
}

#[cfg(all(feature = "wav", not(feature = "ogg")))]
fn decode_ogg(_: &std::path::Path) -> Result<Decoded, LoadErrorKind> {
    Err(LoadErrorKind::DecoderDisabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_preserves_endpoints_and_channel_count() {
        let source = [0.0, 1.0, 2.0, 3.0];
        let output = resample(&source, 1, 4, 8);
        assert_eq!(output.len(), 8);
        assert!(output[0].abs() < f32::EPSILON);
        assert!((output[7] - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn resampling_scales_the_length_by_the_rate_ratio() {
        let stereo: Vec<f32> = (0..2 * 44_100).map(|i| [0.0, 1.0, 2.0][i % 3]).collect();
        let up = resample(&stereo, 2, 44_100, 48_000);
        assert_eq!(up.len(), 2 * 48_000);
        let down = resample(&stereo, 2, 44_100, 22_050);
        assert_eq!(down.len(), 2 * 22_050);
        let same = Sound::from_interleaved(stereo.clone(), 2, 44_100, 44_100);
        assert_eq!(same.samples(), &stereo[..]);
    }

    #[test]
    fn wide_sources_keep_their_first_two_channels() {
        let quad = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let sound = Sound::from_interleaved(quad, 4, 48_000, 48_000);
        assert_eq!(sound.channels(), 2);
        assert_eq!(sound.samples(), &[1.0, 2.0, 5.0, 6.0]);
        assert_eq!(sound.frames(), 2);
    }

    #[test]
    fn decibels_convert_to_amplitude() {
        assert!((db_to_amplitude(0.0) - 1.0).abs() < 1.0e-6);
        assert!((db_to_amplitude(-20.0) - 0.1).abs() < 1.0e-6);
        assert!((db_to_amplitude(-6.0) - 0.501).abs() < 1.0e-3);
    }

    #[cfg(feature = "wav")]
    #[test]
    fn wav_files_decode_and_resample() {
        let directory =
            std::env::temp_dir().join(format!("mulciber-audio-wav-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("tone.wav");
        let specification = hound::WavSpec {
            channels: 1,
            sample_rate: 24_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, specification).unwrap();
        for index in 0..2_400 {
            writer
                .write_sample(if index % 2 == 0 { 16_000_i16 } else { -16_000 })
                .unwrap();
        }
        writer.finalize().unwrap();

        let sound = Sound::load(&path, 48_000).unwrap();
        assert_eq!(sound.channels(), 1);
        assert_eq!(sound.frames(), 4_800);
        assert!((sound.samples()[0] - 16_000.0 / 32_767.0).abs() < 1.0e-6);

        let missing = Sound::load(directory.join("missing.wav"), 48_000).unwrap_err();
        assert!(missing.to_string().contains("missing.wav"));
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
