use crate::GraphicsError;

const MAGIC: &[u8; 8] = b"MULSHDR1";
#[cfg(any(test, target_os = "linux", target_os = "windows"))]
const VULKAN_KIND: u32 = 1;
#[cfg(any(test, target_os = "macos"))]
const METAL_KIND: u32 = 2;
const HEADER_LENGTH: usize = 16;

/// Target-selected native shader code produced from one WGSL module by
/// `mulciber-shader`.
///
/// The native bytes and their container format are deliberately opaque. Keeping this value borrowed
/// lets applications embed build output with `include_bytes!` without a startup allocation.
#[derive(Clone, Copy)]
pub struct ShaderArtifact<'bytes> {
    payload: &'bytes [u8],
}

impl<'bytes> ShaderArtifact<'bytes> {
    /// Validates target-selected output from `mulciber-shader`.
    ///
    /// # Errors
    ///
    /// Returns an error for a corrupt container, an artifact produced for the other native backend,
    /// an empty payload, or malformed SPIR-V byte alignment and magic.
    pub fn new(bytes: &'bytes [u8]) -> Result<Self, GraphicsError> {
        if bytes.len() < HEADER_LENGTH || &bytes[..8] != MAGIC {
            return Err(GraphicsError::new(
                "invalid Mulciber shader artifact header",
            ));
        }
        let kind = u32::from_le_bytes(
            bytes
                .get(8..12)
                .ok_or_else(|| GraphicsError::new("invalid Mulciber shader artifact header"))?
                .try_into()
                .map_err(|_| GraphicsError::new("invalid Mulciber shader artifact header"))?,
        );
        let payload_length = usize::try_from(u32::from_le_bytes(
            bytes
                .get(12..16)
                .ok_or_else(|| GraphicsError::new("invalid Mulciber shader artifact header"))?
                .try_into()
                .map_err(|_| GraphicsError::new("invalid Mulciber shader artifact header"))?,
        ))
        .map_err(|_| GraphicsError::new("shader artifact length exceeds this target"))?;
        let payload = bytes
            .get(HEADER_LENGTH..)
            .filter(|payload| payload.len() == payload_length && !payload.is_empty())
            .ok_or_else(|| GraphicsError::new("invalid Mulciber shader artifact length"))?;

        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if kind != VULKAN_KIND {
            return Err(GraphicsError::new(
                "shader artifact does not contain Vulkan code",
            ));
        }
        #[cfg(target_os = "macos")]
        if kind != METAL_KIND {
            return Err(GraphicsError::new(
                "shader artifact does not contain Metal code",
            ));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if payload.len() % 4 != 0 || payload.get(..4) != Some(&0x0723_0203_u32.to_le_bytes()) {
            return Err(GraphicsError::new(
                "shader artifact contains malformed SPIR-V",
            ));
        }

        Ok(Self { payload })
    }

    /// Returns the native payload size without exposing its backend-specific representation.
    #[must_use]
    pub const fn byte_len(self) -> usize {
        self.payload.len()
    }

    pub(crate) const fn payload(self) -> &'bytes [u8] {
        self.payload
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    #[allow(unused_imports)]
    use super::{HEADER_LENGTH, MAGIC, METAL_KIND, ShaderArtifact, VULKAN_KIND};

    fn artifact(kind: u32, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_LENGTH + payload.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(
            &u32::try_from(payload.len())
                .expect("test payload fits u32")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn rejects_truncated_and_wrong_target_artifacts() {
        assert!(ShaderArtifact::new(b"MULSHDR1").is_err());
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        assert!(ShaderArtifact::new(&artifact(METAL_KIND, b"metallib")).is_err());
        #[cfg(target_os = "macos")]
        assert!(ShaderArtifact::new(&artifact(VULKAN_KIND, &[3, 2, 35, 7])).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn accepts_targeted_spirv() {
        let bytes = artifact(VULKAN_KIND, &0x0723_0203_u32.to_le_bytes());
        assert_eq!(
            ShaderArtifact::new(&bytes)
                .expect("valid artifact")
                .byte_len(),
            4
        );
    }
}
