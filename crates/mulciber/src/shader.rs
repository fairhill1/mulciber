use std::string::String;
use std::vec::Vec;

use crate::GraphicsError;

/// The current container: each entry point records the module bindings it uses.
const MAGIC: &[u8; 8] = b"MULSHDR3";
/// The previous container, still accepted: its interface records bindings for the whole module,
/// so every entry point is read as using all of them.
const MODULE_WIDE_MAGIC: &[u8; 8] = b"MULSHDR2";
#[cfg(any(test, target_os = "linux", target_os = "windows"))]
const VULKAN_KIND: u32 = 1;
#[cfg(any(test, target_os = "macos"))]
const METAL_KIND: u32 = 2;
const HEADER_LENGTH: usize = 20;

const STAGE_LIMIT: u8 = 2;
const VERTEX_FORMAT_LIMIT: u8 = 11;
const BINDING_KIND_LIMIT: u8 = 8;

/// Target-selected native shader code produced from one WGSL module by
/// `mulciber-shader`.
///
/// The native bytes and their container format are deliberately opaque. Keeping this value borrowed
/// lets applications embed build output with `include_bytes!` without a startup allocation. The
/// container also carries the module's compiler-recorded interface — entry points, vertex inputs,
/// resource bindings, and which bindings each entry point uses — which pipeline creation validates
/// application declarations against.
#[derive(Clone, Copy)]
pub struct ShaderArtifact<'bytes> {
    payload: &'bytes [u8],
    interface: &'bytes [u8],
    per_entry_bindings: bool,
}

impl<'bytes> ShaderArtifact<'bytes> {
    /// Validates target-selected output from `mulciber-shader`.
    ///
    /// # Errors
    ///
    /// Returns an error for a corrupt container, an artifact produced for the other native backend
    /// or by a `mulciber-shader` container format older than `MULSHDR2`, an empty payload,
    /// malformed SPIR-V byte alignment and magic, or a malformed interface section.
    pub fn new(bytes: &'bytes [u8]) -> Result<Self, GraphicsError> {
        let per_entry_bindings = match bytes.get(..8) {
            Some(magic) if bytes.len() >= HEADER_LENGTH && magic == MAGIC => true,
            Some(magic) if bytes.len() >= HEADER_LENGTH && magic == MODULE_WIDE_MAGIC => false,
            _ => {
                return Err(GraphicsError::invalid_request(
                    "invalid Mulciber shader artifact header",
                ));
            }
        };
        let kind = header_field(bytes, 8)?;
        let payload_length = usize::try_from(header_field(bytes, 12)?).map_err(|_| {
            GraphicsError::invalid_request("shader artifact length exceeds this target")
        })?;
        let interface_length = usize::try_from(header_field(bytes, 16)?).map_err(|_| {
            GraphicsError::invalid_request("shader artifact length exceeds this target")
        })?;
        let sections = &bytes[HEADER_LENGTH..];
        if sections.len()
            != payload_length
                .checked_add(interface_length)
                .ok_or_else(|| {
                    GraphicsError::invalid_request("shader artifact length exceeds this target")
                })?
        {
            return Err(GraphicsError::invalid_request(
                "invalid Mulciber shader artifact length",
            ));
        }
        let (payload, interface) = sections.split_at(payload_length);
        if payload.is_empty() {
            return Err(GraphicsError::invalid_request(
                "invalid Mulciber shader artifact length",
            ));
        }

        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if kind != VULKAN_KIND {
            return Err(GraphicsError::invalid_request(
                "shader artifact does not contain Vulkan code",
            ));
        }
        #[cfg(target_os = "macos")]
        if kind != METAL_KIND {
            return Err(GraphicsError::invalid_request(
                "shader artifact does not contain Metal code",
            ));
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if payload.len() % 4 != 0 || payload.get(..4) != Some(&0x0723_0203_u32.to_le_bytes()) {
            return Err(GraphicsError::invalid_request(
                "shader artifact contains malformed SPIR-V",
            ));
        }

        validate_interface(interface, per_entry_bindings)?;
        Ok(Self {
            payload,
            interface,
            per_entry_bindings,
        })
    }

    /// Returns the native payload size without exposing its backend-specific representation.
    #[must_use]
    pub const fn byte_len(self) -> usize {
        self.payload.len()
    }

    pub(crate) const fn payload(self) -> &'bytes [u8] {
        self.payload
    }

    /// Decodes the compiler-recorded interface section.
    ///
    /// Construction already validated the section's structure, so decoding cannot fail.
    pub(crate) fn parse_interface(self) -> ShaderInterface {
        let mut cursor = InterfaceCursor {
            bytes: self.interface,
        };
        let validated = "interface was validated at construction";
        let mut entry_points = Vec::new();
        for _ in 0..cursor.take_u32().expect(validated) {
            let stage = cursor.take_u8().expect(validated);
            let name_length =
                usize::try_from(cursor.take_u32().expect(validated)).expect(validated);
            let name = String::from_utf8(cursor.take_bytes(name_length).expect(validated).to_vec())
                .expect(validated);
            let mut inputs = Vec::new();
            for _ in 0..cursor.take_u32().expect(validated) {
                let location = cursor.take_u32().expect(validated);
                let format = cursor.take_u8().expect(validated);
                inputs.push(InterfaceVertexInput { location, format });
            }
            let used = if self.per_entry_bindings {
                let count = cursor.take_u32().expect(validated);
                (0..count)
                    .map(|_| usize::try_from(cursor.take_u32().expect(validated)).expect(validated))
                    .collect()
            } else {
                Vec::new()
            };
            entry_points.push(InterfaceEntryPoint {
                stage,
                name,
                inputs,
                used,
            });
        }
        let mut bindings = Vec::new();
        for _ in 0..cursor.take_u32().expect(validated) {
            let group = cursor.take_u32().expect(validated);
            let binding = cursor.take_u32().expect(validated);
            let kind = cursor.take_u8().expect(validated);
            let size = cursor.take_u32().expect(validated);
            bindings.push(InterfaceBinding {
                group,
                binding,
                kind,
                size,
            });
        }
        if !self.per_entry_bindings {
            for entry in &mut entry_points {
                entry.used = (0..bindings.len()).collect();
            }
        }
        ShaderInterface {
            entry_points,
            bindings,
        }
    }
}

pub(crate) const INTERFACE_STAGE_VERTEX: u8 = 0;
pub(crate) const INTERFACE_STAGE_FRAGMENT: u8 = 1;

pub(crate) const INTERFACE_BINDING_UNIFORM: u8 = 0;
pub(crate) const INTERFACE_BINDING_SAMPLED_TEXTURE: u8 = 1;
pub(crate) const INTERFACE_BINDING_SAMPLER: u8 = 2;
pub(crate) const INTERFACE_BINDING_STORAGE: u8 = 3;
pub(crate) const INTERFACE_BINDING_DEPTH_TEXTURE: u8 = 4;
pub(crate) const INTERFACE_BINDING_COMPARISON_SAMPLER: u8 = 5;
pub(crate) const INTERFACE_BINDING_DEPTH_TEXTURE_ARRAY: u8 = 6;
pub(crate) const INTERFACE_BINDING_MULTISAMPLED_DEPTH: u8 = 7;
pub(crate) const INTERFACE_BINDING_CUBE_TEXTURE: u8 = 8;

/// The compiler-recorded interface of one shader module.
pub(crate) struct ShaderInterface {
    pub(crate) entry_points: Vec<InterfaceEntryPoint>,
    /// Every bound global in the module, sorted by group and binding.
    pub(crate) bindings: Vec<InterfaceBinding>,
}

impl ShaderInterface {
    /// The bindings any of `entries` uses, in module order: what a pipeline built from those
    /// entry points binds. A `MULSHDR2` artifact attributes every binding to every entry point.
    pub(crate) fn bindings_used_by(
        &self,
        entries: &[&InterfaceEntryPoint],
    ) -> Vec<InterfaceBinding> {
        (0..self.bindings.len())
            .filter(|index| entries.iter().any(|entry| entry.used.contains(index)))
            .map(|index| self.bindings[index])
            .collect()
    }
}

pub(crate) struct InterfaceEntryPoint {
    pub(crate) stage: u8,
    pub(crate) name: String,
    /// Vertex-stage input locations with format codes, sorted by location; empty for other stages.
    pub(crate) inputs: Vec<InterfaceVertexInput>,
    /// Ascending indices into the module's `bindings` of the bindings this entry point uses,
    /// directly or through called functions.
    pub(crate) used: Vec<usize>,
}

#[derive(Clone, Copy)]
pub(crate) struct InterfaceVertexInput {
    pub(crate) location: u32,
    pub(crate) format: u8,
}

#[derive(Clone, Copy)]
pub(crate) struct InterfaceBinding {
    pub(crate) group: u32,
    pub(crate) binding: u32,
    pub(crate) kind: u8,
    pub(crate) size: u32,
}

fn header_field(bytes: &[u8], offset: usize) -> Result<u32, GraphicsError> {
    bytes
        .get(offset..offset + 4)
        .and_then(|field| field.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| GraphicsError::invalid_request("invalid Mulciber shader artifact header"))
}

/// Walks the interface grammar without allocating: entry points with stage, UTF-8 name,
/// location/format vertex inputs and, in `MULSHDR3`, strictly ascending indices of the bindings
/// they use, then resource bindings with kind and uniform byte size. Every used index must name a
/// recorded binding.
fn validate_interface(bytes: &[u8], per_entry_bindings: bool) -> Result<(), GraphicsError> {
    let mut cursor = InterfaceCursor { bytes };
    // One past the largest binding index any entry point uses.
    let mut used_bound = 0_u64;
    let entry_points = cursor.take_u32()?;
    for _ in 0..entry_points {
        if cursor.take_u8()? > STAGE_LIMIT {
            return Err(interface_error());
        }
        let name_length = usize::try_from(cursor.take_u32()?).map_err(|_| interface_error())?;
        let name = cursor.take_bytes(name_length)?;
        if name.is_empty() || core::str::from_utf8(name).is_err() {
            return Err(interface_error());
        }
        let inputs = cursor.take_u32()?;
        for _ in 0..inputs {
            cursor.take_u32()?;
            if cursor.take_u8()? > VERTEX_FORMAT_LIMIT {
                return Err(interface_error());
            }
        }
        if per_entry_bindings {
            let mut next = 0_u64;
            for _ in 0..cursor.take_u32()? {
                let index = u64::from(cursor.take_u32()?);
                if index < next {
                    return Err(interface_error());
                }
                next = index + 1;
            }
            used_bound = used_bound.max(next);
        }
    }
    let bindings = cursor.take_u32()?;
    if used_bound > u64::from(bindings) {
        return Err(interface_error());
    }
    for _ in 0..bindings {
        cursor.take_u32()?;
        cursor.take_u32()?;
        if cursor.take_u8()? > BINDING_KIND_LIMIT {
            return Err(interface_error());
        }
        cursor.take_u32()?;
    }
    if cursor.bytes.is_empty() {
        Ok(())
    } else {
        Err(interface_error())
    }
}

fn interface_error() -> GraphicsError {
    GraphicsError::invalid_request("invalid Mulciber shader artifact interface")
}

struct InterfaceCursor<'bytes> {
    bytes: &'bytes [u8],
}

impl<'bytes> InterfaceCursor<'bytes> {
    fn take_bytes(&mut self, length: usize) -> Result<&'bytes [u8], GraphicsError> {
        if length > self.bytes.len() {
            return Err(interface_error());
        }
        let (taken, rest) = self.bytes.split_at(length);
        self.bytes = rest;
        Ok(taken)
    }

    fn take_u32(&mut self) -> Result<u32, GraphicsError> {
        let field = self.take_bytes(4)?;
        Ok(u32::from_le_bytes(
            field.try_into().map_err(|_| interface_error())?,
        ))
    }

    fn take_u8(&mut self) -> Result<u8, GraphicsError> {
        Ok(self.take_bytes(1)?[0])
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use crate::GraphicsErrorKind;

    #[allow(unused_imports)]
    use super::{HEADER_LENGTH, MAGIC, METAL_KIND, MODULE_WIDE_MAGIC, ShaderArtifact, VULKAN_KIND};

    fn artifact(kind: u32, payload: &[u8], interface: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_LENGTH + payload.len() + interface.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(
            &u32::try_from(payload.len())
                .expect("test payload fits u32")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(
            &u32::try_from(interface.len())
                .expect("test interface fits u32")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(interface);
        bytes
    }

    const EMPTY_INTERFACE: [u8; 8] = [0; 8];

    #[test]
    fn rejects_truncated_and_wrong_target_artifacts() {
        assert_eq!(
            ShaderArtifact::new(b"MULSHDR2")
                .err()
                .expect("truncated artifact must fail")
                .kind(),
            GraphicsErrorKind::InvalidRequest
        );
        assert_eq!(
            ShaderArtifact::new(b"MULSHDR1\x01\x00\x00\x00\x04\x00\x00\x00\x03\x02\x23\x07")
                .err()
                .expect("previous container format must fail")
                .kind(),
            GraphicsErrorKind::InvalidRequest
        );
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        assert_eq!(
            ShaderArtifact::new(&artifact(METAL_KIND, b"metallib", &EMPTY_INTERFACE))
                .err()
                .expect("wrong-target artifact must fail")
                .kind(),
            GraphicsErrorKind::InvalidRequest
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            ShaderArtifact::new(&artifact(VULKAN_KIND, &[3, 2, 35, 7], &EMPTY_INTERFACE))
                .err()
                .expect("wrong-target artifact must fail")
                .kind(),
            GraphicsErrorKind::InvalidRequest
        );
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn accepts_targeted_spirv() {
        let bytes = artifact(
            VULKAN_KIND,
            &0x0723_0203_u32.to_le_bytes(),
            &EMPTY_INTERFACE,
        );
        let parsed = ShaderArtifact::new(&bytes).expect("valid artifact");
        assert_eq!(parsed.byte_len(), 4);
        let interface = parsed.parse_interface();
        assert!(interface.entry_points.is_empty());
        assert!(interface.bindings.is_empty());
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn reads_cube_texture_bindings_and_rejects_unknown_kinds() {
        let payload = 0x0723_0203_u32.to_le_bytes();
        let interface = |kind: u8| {
            let mut bytes = 0_u32.to_le_bytes().to_vec();
            bytes.extend_from_slice(&1_u32.to_le_bytes());
            bytes.extend_from_slice(&0_u32.to_le_bytes());
            bytes.extend_from_slice(&5_u32.to_le_bytes());
            bytes.push(kind);
            bytes.extend_from_slice(&0_u32.to_le_bytes());
            bytes
        };
        let cube = artifact(
            VULKAN_KIND,
            &payload,
            &interface(super::INTERFACE_BINDING_CUBE_TEXTURE),
        );
        let parsed = ShaderArtifact::new(&cube).expect("cube binding kind is known");
        let bindings = parsed.parse_interface().bindings;
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].binding, 5);
        assert_eq!(bindings[0].kind, super::INTERFACE_BINDING_CUBE_TEXTURE);
        let unknown = artifact(VULKAN_KIND, &payload, &interface(9));
        assert!(ShaderArtifact::new(&unknown).is_err());
    }

    /// One vertex entry point named `v` using `used`, over `bindings` uniform slots.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn per_entry_interface(used: &[u32], bindings: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.push(super::INTERFACE_STAGE_VERTEX);
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.push(b'v');
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(used.len()).unwrap().to_le_bytes());
        for index in used {
            bytes.extend_from_slice(&index.to_le_bytes());
        }
        bytes.extend_from_slice(&bindings.to_le_bytes());
        for binding in 0..bindings {
            bytes.extend_from_slice(&0_u32.to_le_bytes());
            bytes.extend_from_slice(&binding.to_le_bytes());
            bytes.push(super::INTERFACE_BINDING_UNIFORM);
            bytes.extend_from_slice(&16_u32.to_le_bytes());
        }
        bytes
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn reads_per_entry_binding_use_and_rejects_bad_indices() {
        let payload = 0x0723_0203_u32.to_le_bytes();
        let bytes = artifact(VULKAN_KIND, &payload, &per_entry_interface(&[0, 2], 3));
        let interface = ShaderArtifact::new(&bytes)
            .expect("per-entry artifact")
            .parse_interface();
        assert_eq!(interface.entry_points[0].used, [0, 2]);
        let used = interface.bindings_used_by(&[&interface.entry_points[0]]);
        assert_eq!(
            used.iter()
                .map(|binding| binding.binding)
                .collect::<Vec<_>>(),
            [0, 2]
        );
        // An index past the table, a repeat, and a descending pair are all malformed.
        for used in [&[3_u32][..], &[1, 1], &[2, 0]] {
            let bytes = artifact(VULKAN_KIND, &payload, &per_entry_interface(used, 3));
            assert!(ShaderArtifact::new(&bytes).is_err(), "{used:?}");
        }
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn module_wide_artifacts_attribute_every_binding_to_every_entry_point() {
        let payload = 0x0723_0203_u32.to_le_bytes();
        // The same entry point without its usage list is a MULSHDR2 interface.
        let mut interface = per_entry_interface(&[], 2);
        interface.drain(10..14);
        let mut bytes = artifact(VULKAN_KIND, &payload, &interface);
        bytes[..8].copy_from_slice(MODULE_WIDE_MAGIC);
        let parsed = ShaderArtifact::new(&bytes)
            .expect("MULSHDR2 artifacts stay readable")
            .parse_interface();
        assert_eq!(parsed.entry_points[0].used, [0, 1]);
        // Read as MULSHDR3, the missing usage list makes the interface malformed.
        bytes[..8].copy_from_slice(MAGIC);
        assert!(ShaderArtifact::new(&bytes).is_err());
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn rejects_malformed_interfaces() {
        let payload = 0x0723_0203_u32.to_le_bytes();
        // One entry point declared but no entry bytes follow.
        let truncated = artifact(VULKAN_KIND, &payload, &1_u32.to_le_bytes());
        // A structurally complete entry with an unknown stage code.
        let mut bad_stage_interface = Vec::new();
        bad_stage_interface.extend_from_slice(&1_u32.to_le_bytes());
        bad_stage_interface.push(9);
        bad_stage_interface.extend_from_slice(&1_u32.to_le_bytes());
        bad_stage_interface.push(b'v');
        bad_stage_interface.extend_from_slice(&0_u32.to_le_bytes());
        bad_stage_interface.extend_from_slice(&0_u32.to_le_bytes());
        let bad_stage = artifact(VULKAN_KIND, &payload, &bad_stage_interface);
        // Valid interface followed by unconsumed trailing bytes.
        let mut trailing_interface = EMPTY_INTERFACE.to_vec();
        trailing_interface.push(0);
        let trailing = artifact(VULKAN_KIND, &payload, &trailing_interface);

        for bytes in [truncated, bad_stage, trailing] {
            assert_eq!(
                ShaderArtifact::new(&bytes)
                    .err()
                    .expect("malformed interface must fail")
                    .kind(),
                GraphicsErrorKind::InvalidRequest
            );
        }
    }
}
