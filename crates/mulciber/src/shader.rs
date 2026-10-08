use std::string::String;
use std::vec::Vec;

use crate::{GraphicsError, VertexFormat};

/// The current container: each entry point records the module bindings it uses, and each uniform
/// and storage binding records its memory layout.
const MAGIC: &[u8; 8] = b"MULSHDR4";
/// The previous container, still accepted: per-entry-point binding use without buffer layouts.
const PER_ENTRY_MAGIC: &[u8; 8] = b"MULSHDR3";
/// The container before that, still accepted: its interface records bindings for the whole
/// module, so every entry point is read as using all of them.
const MODULE_WIDE_MAGIC: &[u8; 8] = b"MULSHDR2";

/// What an accepted container's interface section records.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Container {
    /// `MULSHDR2`: bindings for the whole module.
    ModuleWide,
    /// `MULSHDR3`: each entry point's bindings.
    PerEntry,
    /// `MULSHDR4`: each entry point's bindings and every buffer binding's layout.
    Layouts,
}

impl Container {
    const fn per_entry_bindings(self) -> bool {
        !matches!(self, Self::ModuleWide)
    }

    const fn layouts(self) -> bool {
        matches!(self, Self::Layouts)
    }
}
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
    container: Container,
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
        let container = match bytes.get(..8) {
            Some(magic) if bytes.len() >= HEADER_LENGTH && magic == MAGIC => Container::Layouts,
            Some(magic) if bytes.len() >= HEADER_LENGTH && magic == PER_ENTRY_MAGIC => {
                Container::PerEntry
            }
            Some(magic) if bytes.len() >= HEADER_LENGTH && magic == MODULE_WIDE_MAGIC => {
                Container::ModuleWide
            }
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

        validate_interface(interface, container)?;
        Ok(Self {
            payload,
            interface,
            container,
        })
    }

    /// The module's compiler-recorded interface: entry points with their stages, vertex inputs
    /// and the bindings each uses, and every resource binding with its kind, byte size and, for
    /// uniform and storage buffers, memory layout.
    ///
    /// This is what pipeline creation checks declarations against, readable without a device,
    /// so a test can hold an application's uniform packing and vertex layouts to the compiled
    /// shader. See also [`crate::MaterialPipelineDescriptor::validate`].
    ///
    /// # Panics
    ///
    /// Never: [`Self::new`] has already validated the interface section this decodes.
    #[must_use]
    pub fn reflect(self) -> ShaderReflection {
        let (interface, layouts) = self.decode();
        let entry_points = interface
            .entry_points
            .into_iter()
            .map(|entry| ShaderEntryPoint {
                stage: match entry.stage {
                    INTERFACE_STAGE_VERTEX => ShaderStage::Vertex,
                    INTERFACE_STAGE_FRAGMENT => ShaderStage::Fragment,
                    _ => ShaderStage::Compute,
                },
                inputs: entry
                    .inputs
                    .iter()
                    .map(|input| ShaderVertexInput {
                        location: input.location,
                        format: VertexFormat::from_interface_code(input.format)
                            .expect("interface was validated at construction"),
                    })
                    .collect(),
                used: entry
                    .used
                    .iter()
                    .map(|&index| {
                        let binding = interface.bindings[index];
                        (binding.group, binding.binding)
                    })
                    .collect(),
                name: entry.name,
            })
            .collect();
        let bindings = interface
            .bindings
            .iter()
            .zip(layouts)
            .map(|(binding, layout)| ShaderBinding {
                group: binding.group,
                binding: binding.binding,
                kind: ShaderBindingKind::from_code(binding.kind),
                size: binding.size,
                layout,
            })
            .collect();
        ShaderReflection {
            entry_points,
            bindings,
            layouts: self.container.layouts(),
        }
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
        self.decode().0
    }

    /// Decodes the interface section and, per binding, its buffer layout when the container
    /// records one.
    fn decode(self) -> (ShaderInterface, Vec<Option<BufferLayout>>) {
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
            let used = if self.container.per_entry_bindings() {
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
        let mut recorded_layouts = Vec::new();
        if self.container.layouts() {
            for _ in 0..cursor.take_u32().expect(validated) {
                let index = usize::try_from(cursor.take_u32().expect(validated)).expect(validated);
                let type_name = cursor.take_text().expect(validated);
                let mut members = Vec::new();
                for _ in 0..cursor.take_u32().expect(validated) {
                    let name = cursor.take_text().expect(validated);
                    let offset = cursor.take_u32().expect(validated);
                    let size = cursor.take_u32().expect(validated);
                    let type_name = cursor.take_text().expect(validated);
                    members.push(BufferMember {
                        name,
                        offset,
                        size,
                        type_name,
                    });
                }
                recorded_layouts.push((index, BufferLayout { type_name, members }));
            }
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
        if !self.container.per_entry_bindings() {
            for entry in &mut entry_points {
                entry.used = (0..bindings.len()).collect();
            }
        }
        let mut layouts: Vec<Option<BufferLayout>> = bindings.iter().map(|_| None).collect();
        for (index, layout) in recorded_layouts {
            layouts[index] = Some(layout);
        }
        (
            ShaderInterface {
                entry_points,
                bindings,
            },
            layouts,
        )
    }
}

/// A shader module's compiler-recorded interface, read from its artifact by
/// [`ShaderArtifact::reflect`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderReflection {
    entry_points: Vec<ShaderEntryPoint>,
    bindings: Vec<ShaderBinding>,
    layouts: bool,
}

impl ShaderReflection {
    /// Every entry point, in module order.
    #[must_use]
    pub fn entry_points(&self) -> &[ShaderEntryPoint] {
        &self.entry_points
    }

    /// The entry point called `name`, if the module has one.
    #[must_use]
    pub fn entry_point(&self, name: &str) -> Option<&ShaderEntryPoint> {
        self.entry_points.iter().find(|entry| entry.name == name)
    }

    /// Every bound resource in the module, sorted by group and binding.
    #[must_use]
    pub fn bindings(&self) -> &[ShaderBinding] {
        &self.bindings
    }

    /// The resource at `group` and `binding`, if the module binds one there.
    #[must_use]
    pub fn binding(&self, group: u32, binding: u32) -> Option<&ShaderBinding> {
        self.bindings
            .iter()
            .find(|recorded| recorded.group == group && recorded.binding == binding)
    }

    /// Whether the artifact records buffer layouts. Artifacts from `mulciber-shader` 0.5.3 and
    /// older (`MULSHDR2` and `MULSHDR3` containers) do not, and their buffer bindings report no
    /// [`ShaderBinding::layout`].
    #[must_use]
    pub const fn records_layouts(&self) -> bool {
        self.layouts
    }
}

/// The pipeline stage an entry point runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShaderStage {
    /// `@vertex`.
    Vertex,
    /// `@fragment`.
    Fragment,
    /// `@compute`.
    Compute,
}

/// One entry point of a reflected module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderEntryPoint {
    stage: ShaderStage,
    name: String,
    inputs: Vec<ShaderVertexInput>,
    used: Vec<(u32, u32)>,
}

impl ShaderEntryPoint {
    /// The stage it runs in.
    #[must_use]
    pub const fn stage(&self) -> ShaderStage {
        self.stage
    }

    /// Its WGSL function name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A vertex entry point's `@location` inputs, sorted by location; empty for other stages.
    #[must_use]
    pub fn inputs(&self) -> &[ShaderVertexInput] {
        &self.inputs
    }

    /// The `(group, binding)` of every resource it reaches, directly or through called
    /// functions, in binding order. An artifact older than `MULSHDR3` records use for the whole
    /// module, so each of its entry points reports every binding.
    #[must_use]
    pub fn used_bindings(&self) -> &[(u32, u32)] {
        &self.used
    }
}

/// One `@location` input of a vertex entry point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShaderVertexInput {
    location: u32,
    format: VertexFormat,
}

impl ShaderVertexInput {
    /// Its `@location`.
    #[must_use]
    pub const fn location(&self) -> u32 {
        self.location
    }

    /// The 32-bit format of the WGSL type it is declared as. A [`crate::VertexAttribute`] may
    /// instead supply a packed format the shader reads as this type, such as
    /// [`VertexFormat::Unorm8x4`] for `vec4<f32>`.
    #[must_use]
    pub const fn format(&self) -> VertexFormat {
        self.format
    }
}

/// What a reflected binding holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShaderBindingKind {
    /// `var<uniform>`.
    Uniform,
    /// `var<storage, read>` of a creation-fixed size.
    Storage,
    /// `texture_2d<f32>`.
    SampledTexture,
    /// `texture_cube<f32>`.
    CubeTexture,
    /// `sampler`.
    Sampler,
    /// `sampler_comparison`.
    ComparisonSampler,
    /// `texture_depth_2d`.
    DepthTexture,
    /// `texture_depth_2d_array`.
    DepthTextureArray,
    /// `texture_depth_multisampled_2d`.
    MultisampledDepthTexture,
}

impl ShaderBindingKind {
    const fn from_code(code: u8) -> Self {
        match code {
            INTERFACE_BINDING_UNIFORM => Self::Uniform,
            INTERFACE_BINDING_SAMPLED_TEXTURE => Self::SampledTexture,
            INTERFACE_BINDING_SAMPLER => Self::Sampler,
            INTERFACE_BINDING_STORAGE => Self::Storage,
            INTERFACE_BINDING_DEPTH_TEXTURE => Self::DepthTexture,
            INTERFACE_BINDING_COMPARISON_SAMPLER => Self::ComparisonSampler,
            INTERFACE_BINDING_DEPTH_TEXTURE_ARRAY => Self::DepthTextureArray,
            INTERFACE_BINDING_MULTISAMPLED_DEPTH => Self::MultisampledDepthTexture,
            _ => Self::CubeTexture,
        }
    }
}

/// One bound resource of a reflected module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderBinding {
    group: u32,
    binding: u32,
    kind: ShaderBindingKind,
    size: u32,
    layout: Option<BufferLayout>,
}

impl ShaderBinding {
    /// Its `@group`.
    #[must_use]
    pub const fn group(&self) -> u32 {
        self.group
    }

    /// Its `@binding`.
    #[must_use]
    pub const fn binding(&self) -> u32 {
        self.binding
    }

    /// What it holds.
    #[must_use]
    pub const fn kind(&self) -> ShaderBindingKind {
        self.kind
    }

    /// A uniform or storage buffer's WGSL byte size, which its material declaration must match;
    /// zero for textures and samplers.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// A uniform or storage buffer's memory layout, when the artifact records one.
    #[must_use]
    pub const fn layout(&self) -> Option<&BufferLayout> {
        self.layout.as_ref()
    }
}

/// The memory layout of a uniform or storage buffer, as WGSL lays it out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferLayout {
    type_name: String,
    members: Vec<BufferMember>,
}

impl BufferLayout {
    /// The buffer's WGSL type: a struct's name, or a spelling such as `array<Light, 32>`.
    #[must_use]
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    /// A struct's members in declaration order; empty for any other type.
    #[must_use]
    pub fn members(&self) -> &[BufferMember] {
        &self.members
    }

    /// The member called `name`, if the buffer is a struct with one.
    #[must_use]
    pub fn member(&self, name: &str) -> Option<&BufferMember> {
        self.members.iter().find(|member| member.name == name)
    }
}

/// One member of a struct buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferMember {
    name: String,
    offset: u32,
    size: u32,
    type_name: String,
}

impl BufferMember {
    /// Its WGSL name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its byte offset from the start of the buffer, alignment padding included.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Its own byte size, without padding up to the next member.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// Its WGSL type, such as `mat4x4<f32>`, `vec3<f32>` or a struct's name.
    #[must_use]
    pub fn type_name(&self) -> &str {
        &self.type_name
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
fn validate_interface(bytes: &[u8], container: Container) -> Result<(), GraphicsError> {
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
        if container.per_entry_bindings() {
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
    // Layouts name buffer bindings by strictly ascending table index, checked against the
    // table's kinds once it has been read.
    let mut layout_indices = Vec::new();
    if container.layouts() {
        for _ in 0..cursor.take_u32()? {
            let index = cursor.take_u32()?;
            if layout_indices.last().is_some_and(|&last| index <= last) {
                return Err(interface_error());
            }
            layout_indices.push(index);
            cursor.take_text()?;
            for _ in 0..cursor.take_u32()? {
                cursor.take_text()?;
                cursor.take_u32()?;
                cursor.take_u32()?;
                cursor.take_text()?;
            }
        }
    }
    let bindings = cursor.take_u32()?;
    if used_bound > u64::from(bindings) {
        return Err(interface_error());
    }
    let mut kinds = Vec::new();
    for _ in 0..bindings {
        cursor.take_u32()?;
        cursor.take_u32()?;
        let kind = cursor.take_u8()?;
        if kind > BINDING_KIND_LIMIT {
            return Err(interface_error());
        }
        kinds.push(kind);
        cursor.take_u32()?;
    }
    for index in layout_indices {
        match usize::try_from(index)
            .ok()
            .and_then(|index| kinds.get(index))
        {
            Some(&(INTERFACE_BINDING_UNIFORM | INTERFACE_BINDING_STORAGE)) => {}
            _ => return Err(interface_error()),
        }
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

    /// A length-prefixed UTF-8 string.
    fn take_text(&mut self) -> Result<String, GraphicsError> {
        let length = usize::try_from(self.take_u32()?).map_err(|_| interface_error())?;
        core::str::from_utf8(self.take_bytes(length)?)
            .map(String::from)
            .map_err(|_| interface_error())
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use crate::GraphicsErrorKind;

    #[allow(unused_imports)]
    use super::{
        HEADER_LENGTH, MAGIC, METAL_KIND, MODULE_WIDE_MAGIC, PER_ENTRY_MAGIC, ShaderArtifact,
        VULKAN_KIND,
    };

    /// A `MULSHDR3` artifact: these fixtures carry no buffer-layout section.
    fn artifact(kind: u32, payload: &[u8], interface: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_LENGTH + payload.len() + interface.len());
        bytes.extend_from_slice(PER_ENTRY_MAGIC);
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
        bytes[..8].copy_from_slice(PER_ENTRY_MAGIC);
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

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn push_text(bytes: &mut Vec<u8>, text: &str) {
        bytes.extend_from_slice(&u32::try_from(text.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(text.as_bytes());
    }

    /// A `MULSHDR4` interface: vertex entry `v` reading a `vec3<f32>` at location 0 and using a
    /// uniform `Params` at binding 0 and a sampler at binding 1, with `layout_indices` naming the
    /// table entries the layout section describes.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn layouts_interface(layout_indices: &[u32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.push(super::INTERFACE_STAGE_VERTEX);
        push_text(&mut bytes, "v");
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.push(2);
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(layout_indices.len()).unwrap().to_le_bytes());
        for index in layout_indices {
            bytes.extend_from_slice(&index.to_le_bytes());
            push_text(&mut bytes, "Params");
            bytes.extend_from_slice(&2_u32.to_le_bytes());
            for (name, offset, size, type_name) in [
                ("clip_to_world", 0_u32, 64_u32, "mat4x4<f32>"),
                ("exposure", 64, 4, "f32"),
            ] {
                push_text(&mut bytes, name);
                bytes.extend_from_slice(&offset.to_le_bytes());
                bytes.extend_from_slice(&size.to_le_bytes());
                push_text(&mut bytes, type_name);
            }
        }
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        for (binding, kind, size) in [
            (0_u32, super::INTERFACE_BINDING_UNIFORM, 80_u32),
            (1, super::INTERFACE_BINDING_SAMPLER, 0),
        ] {
            bytes.extend_from_slice(&0_u32.to_le_bytes());
            bytes.extend_from_slice(&binding.to_le_bytes());
            bytes.push(kind);
            bytes.extend_from_slice(&size.to_le_bytes());
        }
        bytes
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn reflects_entry_points_bindings_and_buffer_layouts() {
        use crate::{ShaderBindingKind, ShaderStage, VertexFormat};

        let payload = 0x0723_0203_u32.to_le_bytes();
        let mut bytes = artifact(VULKAN_KIND, &payload, &layouts_interface(&[0]));
        bytes[..8].copy_from_slice(MAGIC);
        let reflection = ShaderArtifact::new(&bytes)
            .expect("MULSHDR4 artifact")
            .reflect();
        assert!(reflection.records_layouts());
        let entry = reflection.entry_point("v").expect("entry point v");
        assert_eq!(entry.stage(), ShaderStage::Vertex);
        assert_eq!(entry.inputs().len(), 1);
        assert_eq!(entry.inputs()[0].location(), 0);
        assert_eq!(entry.inputs()[0].format(), VertexFormat::Float32x3);
        assert_eq!(entry.used_bindings(), [(0, 0), (0, 1)]);
        let uniform = reflection.binding(0, 0).expect("uniform binding");
        assert_eq!(uniform.kind(), ShaderBindingKind::Uniform);
        assert_eq!(uniform.size(), 80);
        let layout = uniform.layout().expect("uniform layout");
        assert_eq!(layout.type_name(), "Params");
        let exposure = layout.member("exposure").expect("exposure member");
        assert_eq!(
            (exposure.offset(), exposure.size(), exposure.type_name()),
            (64, 4, "f32")
        );
        assert_eq!(layout.members()[0].type_name(), "mat4x4<f32>");
        let sampler = reflection.binding(0, 1).expect("sampler binding");
        assert_eq!(sampler.kind(), ShaderBindingKind::Sampler);
        assert!(sampler.layout().is_none());
        // The layout section does not shift what pipeline creation reads.
        let interface = ShaderArtifact::new(&bytes).unwrap().parse_interface();
        assert_eq!(interface.bindings.len(), 2);
        assert_eq!(interface.entry_points[0].used, [0, 1]);
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn older_containers_reflect_without_layouts() {
        let payload = 0x0723_0203_u32.to_le_bytes();
        let bytes = artifact(VULKAN_KIND, &payload, &per_entry_interface(&[1], 2));
        let reflection = ShaderArtifact::new(&bytes)
            .expect("MULSHDR3 artifacts stay readable")
            .reflect();
        assert!(!reflection.records_layouts());
        assert_eq!(reflection.entry_points()[0].used_bindings(), [(0, 1)]);
        assert_eq!(reflection.bindings()[0].size(), 16);
        assert!(reflection.bindings()[0].layout().is_none());
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn rejects_malformed_layout_sections() {
        let payload = 0x0723_0203_u32.to_le_bytes();
        // A layout for the sampler, one past the table, and a repeated index.
        for indices in [&[1_u32][..], &[2], &[0, 0]] {
            let mut bytes = artifact(VULKAN_KIND, &payload, &layouts_interface(indices));
            bytes[..8].copy_from_slice(MAGIC);
            assert!(ShaderArtifact::new(&bytes).is_err(), "{indices:?}");
        }
        // A MULSHDR4 interface read as MULSHDR3 does not parse.
        let bytes = artifact(VULKAN_KIND, &payload, &layouts_interface(&[0]));
        assert!(ShaderArtifact::new(&bytes).is_err());
    }
}
