//! Listener frames, distance rolloff and the HRTF processor wrapper.

use crate::math::{self, Vec3};
use crate::mixer::Rolloff;

/// Samples per HRTF interpolation block, and blocks per `process_samples` call: 1024 samples is
/// about 21 ms at 48 kHz, within one output buffer period on most devices.
#[cfg(feature = "hrtf")]
pub(crate) const HRTF_INTERPOLATION_STEPS: usize = 8;
#[cfg(feature = "hrtf")]
pub(crate) const HRTF_BLOCK_LENGTH: usize = 128;
#[cfg(feature = "hrtf")]
pub(crate) const HRTF_BLOCK_TOTAL: usize = HRTF_INTERPOLATION_STEPS * HRTF_BLOCK_LENGTH;

/// The listener's position and orthonormal basis, refreshed by `Controls::set_listener`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Listener {
    pub position: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub forward: Vec3,
}

impl Default for Listener {
    /// At the origin looking down -Z with +Y up, so +X is on the right.
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            right: [1.0, 0.0, 0.0],
            up: [0.0, 1.0, 0.0],
            forward: [0.0, 0.0, -1.0],
        }
    }
}

impl Listener {
    /// A listener at `position` looking along `forward`, with `up` the world's up. A forward
    /// parallel to up falls back to the default basis instead of collapsing.
    pub(crate) fn oriented(position: Vec3, forward: Vec3, up: Vec3) -> Self {
        let forward = math::normalize_or(forward, [0.0, 0.0, 1.0]);
        let right = math::normalize_or(math::cross(forward, up), [1.0, 0.0, 0.0]);
        let up = math::normalize_or(math::cross(right, forward), [0.0, 1.0, 0.0]);
        Self {
            position,
            right,
            up,
            forward,
        }
    }

    /// Distance gain and listener-space direction (x right, y up, z forward) of a source.
    pub(crate) fn locate(&self, source: Vec3, rolloff: Rolloff) -> (f32, Vec3) {
        let offset = math::sub(source, self.position);
        let distance = math::length(offset);
        let gain = rolloff.gain(distance);
        // Straight ahead when the source sits on the listener, so the HRTF never gets a zero
        // vector.
        let direction = if distance > 0.01 {
            let direction = math::scale(offset, 1.0 / distance);
            [
                math::dot(direction, self.right),
                math::dot(direction, self.up),
                math::dot(direction, self.forward),
            ]
        } else {
            [0.0, 0.0, 1.0]
        };
        (gain, direction)
    }
}

/// Binaural spatialization from an HRIR sphere.
///
/// Spatial voices are convolved with the head-related impulse responses nearest their direction
/// and interpolated as they move. Without one, the mixer pans spatial voices with a
/// constant-power stereo law instead.
///
/// The sphere is a file in the format of the `hrtf` crate, as built by
/// [hrir_sphere_builder](https://github.com/mrDIMAS/hrir_sphere_builder); the crate does not ship
/// one, so the game supplies the path.
#[cfg(feature = "hrtf")]
pub struct Hrtf {
    pub(crate) processor: hrtf::HrtfProcessor,
}

#[cfg(feature = "hrtf")]
impl Hrtf {
    /// Loads an HRIR sphere file and resamples it to the mixer's `sample_rate`.
    ///
    /// # Errors
    ///
    /// Returns an [`HrtfLoadError`] when the file cannot be read or is not an HRIR sphere.
    pub fn load(
        path: impl AsRef<std::path::Path>,
        sample_rate: u32,
    ) -> Result<Self, HrtfLoadError> {
        let sphere =
            hrtf::HrirSphere::from_file(path, sample_rate).map_err(HrtfLoadError::from_hrtf)?;
        Ok(Self::from_sphere(sphere))
    }

    /// Reads an HRIR sphere from any reader, resampled to the mixer's `sample_rate`.
    ///
    /// # Errors
    ///
    /// Returns an [`HrtfLoadError`] when reading fails or the data is not an HRIR sphere.
    pub fn from_reader(
        reader: impl std::io::Read,
        sample_rate: u32,
    ) -> Result<Self, HrtfLoadError> {
        let sphere =
            hrtf::HrirSphere::new(reader, sample_rate).map_err(HrtfLoadError::from_hrtf)?;
        Ok(Self::from_sphere(sphere))
    }

    fn from_sphere(sphere: hrtf::HrirSphere) -> Self {
        Self {
            processor: hrtf::HrtfProcessor::new(
                sphere,
                HRTF_INTERPOLATION_STEPS,
                HRTF_BLOCK_LENGTH,
            ),
        }
    }
}

#[cfg(feature = "hrtf")]
impl std::fmt::Debug for Hrtf {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Hrtf").finish_non_exhaustive()
    }
}

/// Why an HRIR sphere failed to load.
#[cfg(feature = "hrtf")]
#[derive(Debug)]
#[non_exhaustive]
pub enum HrtfLoadError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The data is not an HRIR sphere.
    InvalidFormat,
    /// The sphere declares impulse responses of an invalid length.
    InvalidLength(usize),
}

#[cfg(feature = "hrtf")]
impl HrtfLoadError {
    fn from_hrtf(error: hrtf::HrtfError) -> Self {
        match error {
            hrtf::HrtfError::IoError(error) => Self::Io(error),
            hrtf::HrtfError::InvalidFileFormat => Self::InvalidFormat,
            hrtf::HrtfError::InvalidLength(length) => Self::InvalidLength(length),
        }
    }
}

#[cfg(feature = "hrtf")]
impl std::fmt::Display for HrtfLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "HRIR sphere could not be read: {error}"),
            Self::InvalidFormat => formatter.write_str("not an HRIR sphere file"),
            Self::InvalidLength(length) => {
                write!(formatter, "HRIR sphere has invalid impulse length {length}")
            }
        }
    }
}

#[cfg(feature = "hrtf")]
impl std::error::Error for HrtfLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_are_located_in_the_listeners_frame() {
        let listener = Listener::oriented([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]);
        let rolloff = Rolloff::default();
        let (gain, direction) = listener.locate([3.0, 0.0, 0.0], rolloff);
        assert!((gain - 1.0).abs() < 1.0e-6);
        // Looking down -Z with +Y up, +X is on the right.
        assert!(direction[0] > 0.99);
        let (_, ahead) = listener.locate([0.0, 0.0, -5.0], rolloff);
        assert!(ahead[2] > 0.99);
        let (far, _) = listener.locate([0.0, 0.0, -12.0], rolloff);
        assert!((far - 0.25).abs() < 1.0e-6);
        let (silent, _) = listener.locate([0.0, 0.0, -100.0], rolloff);
        assert!(silent.abs() < f32::EPSILON);
    }

    #[cfg(feature = "hrtf")]
    #[test]
    fn a_file_that_is_not_a_sphere_is_rejected() {
        let error = Hrtf::from_reader(&b"not a sphere at all"[..], 48_000).unwrap_err();
        assert!(matches!(
            error,
            HrtfLoadError::InvalidFormat | HrtfLoadError::Io(_)
        ));
    }
}
