//! Offline compilation from one WGSL source to Mulciber's native shader artifact.
//!
//! The compiler intentionally accepts Naga's baseline WebGPU capabilities only. Advanced shader
//! capabilities remain separate until each native output path has equivalent validation evidence.

use std::fmt;
use std::fs;
use std::path::Path;
use std::process::Command;

use naga::back::msl::{BindSamplerTarget, BindTarget, EntryPointResources};
use naga::valid::{Capabilities, ValidationFlags, Validator};
use naga::{AddressSpace, Binding, Handle, ResourceBinding, Scalar, ScalarKind, Type, TypeInner};

const MAGIC: &[u8; 8] = b"MULSHDR2";
const VULKAN_KIND: u32 = 1;
const METAL_KIND: u32 = 2;

const STAGE_VERTEX: u8 = 0;
const STAGE_FRAGMENT: u8 = 1;
const STAGE_COMPUTE: u8 = 2;

const BINDING_UNIFORM: u8 = 0;
const BINDING_SAMPLED_TEXTURE: u8 = 1;
const BINDING_SAMPLER: u8 = 2;
const BINDING_STORAGE: u8 = 3;
const BINDING_DEPTH_TEXTURE: u8 = 4;
const BINDING_COMPARISON_SAMPLER: u8 = 5;

/// Native shader output selected for an application target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShaderTarget {
    /// Vulkan 1.3+ with SPIR-V 1.4 modules.
    Vulkan,
    /// Metal 3.1 with an Apple metallib.
    Metal,
}

impl ShaderTarget {
    /// Parses the CLI spelling of a shader target.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "vulkan" => Some(Self::Vulkan),
            "metal" => Some(Self::Metal),
            _ => None,
        }
    }
}

/// A WGSL parse, validation, native-code generation, or host-tool failure.
#[derive(Debug)]
pub struct ShaderBuildError(String);

impl fmt::Display for ShaderBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ShaderBuildError {}

/// Compiles one WGSL module and writes an opaque Mulciber artifact.
///
/// Vulkan output is checked with `spirv-val --target-env vulkan1.3` before packaging. Metal output
/// is compiled and linked with Xcode's `metal` and `metallib` tools. Resource bindings preserve
/// their WGSL binding number independently in Metal's buffer, texture, and sampler namespaces.
///
/// # Errors
///
/// Returns an error for invalid WGSL, unsupported shader features, unrepresentable Metal binding
/// slots, Naga output failures, missing native validation/compiler tools, or file-system failures.
pub fn compile_wgsl(
    source: impl AsRef<Path>,
    artifact: impl AsRef<Path>,
    target: ShaderTarget,
) -> Result<(), ShaderBuildError> {
    let source_path = source.as_ref();
    let source = fs::read_to_string(source_path)
        .map_err(|error| fail(format!("read {}: {error}", source_path.display())))?;
    let module = naga::front::wgsl::parse_str(&source)
        .map_err(|error| fail(format!("WGSL parse: {}", error.emit_to_string(&source))))?;
    let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .map_err(|error| {
            fail(format!(
                "WGSL validation: {}",
                error.emit_to_string(&source)
            ))
        })?;
    if module
        .global_variables
        .iter()
        .any(|(_, variable)| variable.space == AddressSpace::WorkGroup)
    {
        return Err(fail(
            "workgroup-memory shaders are disabled: Naga 30 SPIR-V output fails the pinned Vulkan validator",
        ));
    }

    let interface = shader_interface(&module)?;
    match target {
        ShaderTarget::Vulkan => compile_vulkan(&module, &info, artifact.as_ref(), &interface),
        ShaderTarget::Metal => compile_metal(&module, &info, artifact.as_ref(), &interface),
    }
}

/// Encodes the module's pipeline-facing interface: per entry point its stage, name, and
/// vertex-stage input locations with formats, then the module's resource bindings with their
/// kinds and, for uniform data, the WGSL byte size. `mulciber` validates application pipeline
/// declarations against this section, so an interface construct without a proven mapping is a
/// compile error rather than a silently unnamed slot.
fn shader_interface(module: &naga::Module) -> Result<Vec<u8>, ShaderBuildError> {
    let mut bytes = Vec::new();
    push_count(&mut bytes, module.entry_points.len(), "entry points")?;
    for entry in &module.entry_points {
        let stage = match entry.stage {
            naga::ShaderStage::Vertex => STAGE_VERTEX,
            naga::ShaderStage::Fragment => STAGE_FRAGMENT,
            naga::ShaderStage::Compute => STAGE_COMPUTE,
            _ => {
                return Err(fail(format!(
                    "entry point {} has no proven interface stage",
                    entry.name
                )));
            }
        };
        bytes.push(stage);
        push_count(&mut bytes, entry.name.len(), "entry-point name")?;
        bytes.extend_from_slice(entry.name.as_bytes());
        let mut inputs = Vec::new();
        if stage == STAGE_VERTEX {
            for argument in &entry.function.arguments {
                push_vertex_inputs(
                    module,
                    argument.ty,
                    argument.binding.as_ref(),
                    &entry.name,
                    &mut inputs,
                )?;
            }
            inputs.sort_unstable();
        }
        push_count(&mut bytes, inputs.len(), "vertex inputs")?;
        for (location, format) in inputs {
            bytes.extend_from_slice(&location.to_le_bytes());
            bytes.push(format);
        }
    }

    let mut bindings = Vec::new();
    for (_, variable) in module.global_variables.iter() {
        let Some(binding) = &variable.binding else {
            continue;
        };
        let inner = &module.types[variable.ty].inner;
        let (kind, size) = match (&variable.space, inner) {
            (AddressSpace::Uniform, _) => (BINDING_UNIFORM, inner.size(module.to_ctx())),
            (AddressSpace::Storage { .. }, _) => (BINDING_STORAGE, 0),
            (
                AddressSpace::Handle,
                TypeInner::Image {
                    dim: naga::ImageDimension::D2,
                    arrayed: false,
                    class:
                        naga::ImageClass::Sampled {
                            kind: ScalarKind::Float,
                            multi: false,
                        },
                },
            ) => (BINDING_SAMPLED_TEXTURE, 0),
            (
                AddressSpace::Handle,
                TypeInner::Image {
                    dim: naga::ImageDimension::D2,
                    arrayed: false,
                    class: naga::ImageClass::Depth { multi: false },
                },
            ) => (BINDING_DEPTH_TEXTURE, 0),
            (AddressSpace::Handle, TypeInner::Sampler { comparison }) => (
                if *comparison {
                    BINDING_COMPARISON_SAMPLER
                } else {
                    BINDING_SAMPLER
                },
                0,
            ),
            _ => {
                return Err(fail(format!(
                    "WGSL binding {}:{} has no proven interface mapping",
                    binding.group, binding.binding
                )));
            }
        };
        bindings.push((binding.group, binding.binding, kind, size));
    }
    bindings.sort_unstable();
    push_count(&mut bytes, bindings.len(), "resource bindings")?;
    for (group, binding, kind, size) in bindings {
        bytes.extend_from_slice(&group.to_le_bytes());
        bytes.extend_from_slice(&binding.to_le_bytes());
        bytes.push(kind);
        bytes.extend_from_slice(&size.to_le_bytes());
    }
    Ok(bytes)
}

fn push_vertex_inputs(
    module: &naga::Module,
    ty: Handle<Type>,
    binding: Option<&Binding>,
    entry_name: &str,
    inputs: &mut Vec<(u32, u8)>,
) -> Result<(), ShaderBuildError> {
    let inner = &module.types[ty].inner;
    match binding {
        Some(Binding::BuiltIn(_)) => Ok(()),
        Some(Binding::Location { location, .. }) => {
            let format = vertex_input_format(inner).ok_or_else(|| {
                fail(format!(
                    "vertex input location {location} of {entry_name} has no proven vertex format"
                ))
            })?;
            inputs.push((*location, format));
            Ok(())
        }
        None => {
            let TypeInner::Struct { members, .. } = inner else {
                return Err(fail(format!(
                    "unbound non-struct vertex input in {entry_name}"
                )));
            };
            for member in members {
                push_vertex_inputs(
                    module,
                    member.ty,
                    member.binding.as_ref(),
                    entry_name,
                    inputs,
                )?;
            }
            Ok(())
        }
    }
}

/// Maps 32-bit scalar and vector inputs to interface format codes 0 through 11: float, unsigned,
/// and signed families, each as scalar through four components.
fn vertex_input_format(inner: &TypeInner) -> Option<u8> {
    fn family(scalar: Scalar) -> Option<u8> {
        match (scalar.kind, scalar.width) {
            (ScalarKind::Float, 4) => Some(0),
            (ScalarKind::Uint, 4) => Some(4),
            (ScalarKind::Sint, 4) => Some(8),
            _ => None,
        }
    }
    match inner {
        TypeInner::Scalar(scalar) => family(*scalar),
        TypeInner::Vector { size, scalar } => {
            let columns = match size {
                naga::VectorSize::Bi => 1,
                naga::VectorSize::Tri => 2,
                naga::VectorSize::Quad => 3,
            };
            family(*scalar).map(|base| base + columns)
        }
        _ => None,
    }
}

fn push_count(bytes: &mut Vec<u8>, count: usize, what: &str) -> Result<(), ShaderBuildError> {
    bytes.extend_from_slice(
        &u32::try_from(count)
            .map_err(|_| fail(format!("{what} exceed u32")))?
            .to_le_bytes(),
    );
    Ok(())
}

fn compile_vulkan(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
    artifact: &Path,
    interface: &[u8],
) -> Result<(), ShaderBuildError> {
    if let Some(parent) = artifact.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| fail(format!("create shader output: {error}")))?;
    }
    let words = naga::back::spv::write_vec(
        module,
        info,
        &naga::back::spv::Options {
            lang_version: (1, 4),
            ..Default::default()
        },
        None,
    )
    .map_err(|error| fail(format!("SPIR-V generation: {error}")))?;
    let mut payload = Vec::with_capacity(words.len() * 4);
    for word in words {
        payload.extend_from_slice(&word.to_le_bytes());
    }
    let validation_path = artifact.with_extension("validation.spv");
    fs::write(&validation_path, &payload)
        .map_err(|error| fail(format!("write validation SPIR-V: {error}")))?;
    let validation = run(
        Command::new("spirv-val")
            .args(["--target-env", "vulkan1.3"])
            .arg(&validation_path),
        "validate generated SPIR-V",
    );
    let cleanup = fs::remove_file(&validation_path);
    validation?;
    cleanup.map_err(|error| fail(format!("remove validation SPIR-V: {error}")))?;
    write_artifact(artifact, VULKAN_KIND, &payload, interface)
}

fn compile_metal(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
    artifact: &Path,
    interface: &[u8],
) -> Result<(), ShaderBuildError> {
    let resources = metal_resources(module)?;
    let entry_resources = EntryPointResources {
        resources,
        ..Default::default()
    };
    let options = naga::back::msl::Options {
        lang_version: (3, 1),
        per_entry_point_map: module
            .entry_points
            .iter()
            .map(|entry| (entry.name.clone(), entry_resources.clone()))
            .collect(),
        fake_missing_bindings: false,
        ..Default::default()
    };
    let (msl, _) = naga::back::msl::write_string(
        module,
        info,
        &options,
        &naga::back::msl::PipelineOptions::default(),
    )
    .map_err(|error| fail(format!("MSL generation: {error}")))?;

    let directory = artifact
        .parent()
        .ok_or_else(|| fail("shader artifact has no parent directory"))?;
    fs::create_dir_all(directory)
        .map_err(|error| fail(format!("create shader output: {error}")))?;
    let msl_path = directory.join("mulciber-shader.metal");
    let air_path = directory.join("mulciber-shader.air");
    let library_path = directory.join("mulciber-shader.metallib");
    let cache_path = directory.join("metal-module-cache");
    fs::create_dir_all(&cache_path)
        .map_err(|error| fail(format!("create Metal cache: {error}")))?;
    fs::write(&msl_path, msl).map_err(|error| fail(format!("write generated MSL: {error}")))?;
    run(
        Command::new("xcrun")
            .args(["-sdk", "macosx", "metal"])
            .arg(format!("-fmodules-cache-path={}", cache_path.display()))
            .args(["-std=metal3.1", "-mmacosx-version-min=13.0", "-c"])
            .arg(&msl_path)
            .arg("-o")
            .arg(&air_path),
        "compile generated MSL",
    )?;
    run(
        Command::new("xcrun")
            .args(["-sdk", "macosx", "metallib"])
            .arg(&air_path)
            .arg("-o")
            .arg(&library_path),
        "link generated metallib",
    )?;
    let library =
        fs::read(&library_path).map_err(|error| fail(format!("read metallib: {error}")))?;
    write_artifact(artifact, METAL_KIND, &library, interface)
}

fn metal_resources(
    module: &naga::Module,
) -> Result<std::collections::BTreeMap<ResourceBinding, BindTarget>, ShaderBuildError> {
    let mut resources = std::collections::BTreeMap::new();
    for (_, variable) in module.global_variables.iter() {
        let Some(binding) = &variable.binding else {
            continue;
        };
        let slot = u8::try_from(binding.binding).map_err(|_| {
            fail(format!(
                "Metal binding {} exceeds slot 255",
                binding.binding
            ))
        })?;
        let target = match (&variable.space, &module.types[variable.ty].inner) {
            (AddressSpace::Uniform | AddressSpace::Storage { .. }, _) => BindTarget {
                buffer: Some(slot),
                ..Default::default()
            },
            (AddressSpace::Handle, TypeInner::Image { .. }) => BindTarget {
                texture: Some(slot),
                ..Default::default()
            },
            (AddressSpace::Handle, TypeInner::Sampler { .. }) => BindTarget {
                sampler: Some(BindSamplerTarget::Resource(slot)),
                ..Default::default()
            },
            _ => {
                return Err(fail(format!(
                    "WGSL binding {}:{} has no proven Metal mapping",
                    binding.group, binding.binding
                )));
            }
        };
        resources.insert(*binding, target);
    }
    Ok(resources)
}

fn write_artifact(
    path: &Path,
    kind: u32,
    payload: &[u8],
    interface: &[u8],
) -> Result<(), ShaderBuildError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| fail(format!("create shader output: {error}")))?;
    }
    let mut bytes = Vec::with_capacity(20 + payload.len() + interface.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&kind.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(payload.len())
            .map_err(|_| fail("shader payload exceeds u32"))?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(interface.len())
            .map_err(|_| fail("shader interface exceeds u32"))?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(interface);
    fs::write(path, bytes).map_err(|error| fail(format!("write {}: {error}", path.display())))
}

fn run(command: &mut Command, action: &str) -> Result<(), ShaderBuildError> {
    let output = command
        .output()
        .map_err(|error| fail(format!("could not {action}: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(fail(format!(
            "failed to {action}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn fail(message: impl Into<String>) -> ShaderBuildError {
    ShaderBuildError(message.into())
}

#[cfg(test)]
mod tests {
    use naga::valid::{Capabilities, ValidationFlags, Validator};

    use super::{ShaderTarget, metal_resources, shader_interface};

    #[test]
    fn parses_target_names() {
        assert_eq!(ShaderTarget::parse("vulkan"), Some(ShaderTarget::Vulkan));
        assert_eq!(ShaderTarget::parse("metal"), Some(ShaderTarget::Metal));
        assert_eq!(ShaderTarget::parse("dx12"), None);
    }

    #[test]
    fn cube_shader_has_native_outputs_and_mapped_resources() {
        let source = include_str!("../../../examples/cube/src/cube.wgsl");
        let module = naga::front::wgsl::parse_str(source).expect("cube WGSL parses");
        let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
            .validate(&module)
            .expect("cube WGSL validates");
        let words = naga::back::spv::write_vec(
            &module,
            &info,
            &naga::back::spv::Options {
                lang_version: (1, 4),
                ..Default::default()
            },
            None,
        )
        .expect("cube shader emits SPIR-V");
        assert_eq!(words.first().copied(), Some(0x0723_0203));
        assert_eq!(metal_resources(&module).expect("Metal mapping").len(), 3);
    }

    #[test]
    fn cube_shader_interface_records_entries_and_bindings() {
        let source = include_str!("../../../examples/cube/src/cube.wgsl");
        let module = naga::front::wgsl::parse_str(source).expect("cube WGSL parses");
        let interface = shader_interface(&module).expect("cube interface");

        let mut expected = Vec::new();
        expected.extend_from_slice(&2_u32.to_le_bytes());
        // cube_vertex: position vec3<f32> at 0, color vec3<f32> at 1, uv vec2<f32> at 2.
        expected.push(0);
        expected.extend_from_slice(&11_u32.to_le_bytes());
        expected.extend_from_slice(b"cube_vertex");
        expected.extend_from_slice(&3_u32.to_le_bytes());
        for (location, format) in [(0_u32, 2_u8), (1, 2), (2, 1)] {
            expected.extend_from_slice(&location.to_le_bytes());
            expected.push(format);
        }
        // cube_fragment records no vertex-stage inputs.
        expected.push(1);
        expected.extend_from_slice(&13_u32.to_le_bytes());
        expected.extend_from_slice(b"cube_fragment");
        expected.extend_from_slice(&0_u32.to_le_bytes());
        // One 64-byte uniform, one sampled texture, one sampler in group 0.
        expected.extend_from_slice(&3_u32.to_le_bytes());
        for (binding, kind, size) in [(0_u32, 0_u8, 64_u32), (1, 1, 0), (2, 2, 0)] {
            expected.extend_from_slice(&0_u32.to_le_bytes());
            expected.extend_from_slice(&binding.to_le_bytes());
            expected.push(kind);
            expected.extend_from_slice(&size.to_le_bytes());
        }

        assert_eq!(interface, expected);
    }
}
