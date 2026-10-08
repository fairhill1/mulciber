//! Offline compilation from one WGSL source to Mulciber's native shader artifact.
//!
//! The compiler intentionally accepts Naga's baseline WebGPU capabilities only. Advanced shader
//! capabilities remain separate until each native output path has equivalent validation evidence.
//!
//! The same source can also answer host questions: [`compile_host_field`] generates a Rust
//! evaluator for designated WGSL functions, so a simulation can ask what the shader draws without
//! a second hand-written copy of the field.
//!
//! Shared WGSL lives in importable modules: a [`ShaderModules`] set registers them, and a
//! [`WgslShader`] composes a top-level shader with the modules it `#import`s, optionally with
//! shader defs, into the same artifact or host field. Mulciber's own modules are in every set:
//! `mulciber::colorspace`, `mulciber::photometry` (light units, falloff, exposure), `mulciber::pbr`
//! (the BRDF and split-sum environment specular) and `mulciber::tonemap`. [`bake_dfg_table`] bakes
//! the DFG lookup table `mulciber::pbr` samples.

mod dfg;
mod host_field;
#[cfg(test)]
mod library_tests;
mod modules;

use std::fmt;
use std::fs;
use std::path::Path;
use std::process::Command;

use naga::back::msl::{BindSamplerTarget, BindTarget, EntryPointResources};
use naga::valid::{Capabilities, ValidationFlags, Validator};
use naga::{AddressSpace, Binding, Handle, ResourceBinding, Scalar, ScalarKind, Type, TypeInner};

pub use dfg::{DFG_SAMPLE_COUNT, DFG_TABLE_SIZE, DfgTable, bake_dfg_table, dfg_value};
pub use modules::{ShaderDef, ShaderModules, WgslShader};

const MAGIC: &[u8; 8] = b"MULSHDR3";
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
const BINDING_DEPTH_TEXTURE_ARRAY: u8 = 6;
const BINDING_MULTISAMPLED_DEPTH: u8 = 7;
const BINDING_CUBE_TEXTURE: u8 = 8;

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
pub struct ShaderBuildError(String);

/// Shows the message as written, so the multi-line source diagnostics stay readable through
/// `expect` in a `build.rs` and the CLI's error exit.
impl fmt::Debug for ShaderBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

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
    compile_validated(&module, &info, artifact.as_ref(), target)
}

/// Writes the artifact for a module that has already parsed and validated, from one WGSL file or
/// composed from imported modules.
fn compile_validated(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
    artifact: &Path,
    target: ShaderTarget,
) -> Result<(), ShaderBuildError> {
    if module
        .global_variables
        .iter()
        .any(|(_, variable)| variable.space == AddressSpace::WorkGroup)
    {
        return Err(fail(
            "workgroup-memory shaders are disabled: Naga 30 SPIR-V output fails the pinned Vulkan validator",
        ));
    }

    let interface = shader_interface(module, info)?;
    match target {
        ShaderTarget::Vulkan => compile_vulkan(module, info, artifact, &interface),
        ShaderTarget::Metal => compile_metal(module, info, artifact, &interface),
    }
}

/// Generates a Rust source file that evaluates `functions` from the same WGSL the shader uses.
///
/// Each named function becomes a `pub fn` with the WGSL argument names, `f32`, `i32`, `u32`, and
/// `bool` scalars, `[f32; N]`-shaped vectors, and fixed-size arrays; the functions it calls are
/// generated privately beside it. The file is written to `generated`, is meant to be produced by a
/// `build.rs` and pulled in with `include!` so it cannot drift from the shader, and should be
/// included in a module of its own because the generated helper names are file-local.
///
/// The accepted subset is pure arithmetic. Bindings, textures, derivatives, atomics, barriers,
/// workgroup memory, switch statements, and matrices have no host meaning and are refused rather
/// than approximated. WGSL semantics that differ from Rust's nearest spelling — `fract`, `sign`,
/// `round`, integer wrapping, and clamped out-of-range indexing — are generated as the shader
/// computes them; transcendental functions still differ from a GPU by its documented precision,
/// and integer division by zero panics on the host where a GPU leaves it undefined.
///
/// # Errors
///
/// Returns an error for invalid WGSL, a name that is not a function in the module, a function
/// that returns nothing, or any construct outside the host-evaluable subset.
pub fn compile_host_field(
    source: impl AsRef<Path>,
    generated: impl AsRef<Path>,
    functions: &[&str],
) -> Result<(), ShaderBuildError> {
    let source_path = source.as_ref();
    let text = fs::read_to_string(source_path)
        .map_err(|error| fail(format!("read {}: {error}", source_path.display())))?;
    let module = naga::front::wgsl::parse_str(&text)
        .map_err(|error| fail(format!("WGSL parse: {}", error.emit_to_string(&text))))?;
    let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .map_err(|error| fail(format!("WGSL validation: {}", error.emit_to_string(&text))))?;
    let label = source_path.file_name().map_or_else(
        || source_path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let rust = host_field::generate(&module, &info, &label, functions)?;
    write_generated(generated.as_ref(), &rust)
}

fn write_generated(generated: &Path, rust: &str) -> Result<(), ShaderBuildError> {
    if let Some(parent) = generated.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| fail(format!("create host-field output: {error}")))?;
    }
    fs::write(generated, rust)
        .map_err(|error| fail(format!("write {}: {error}", generated.display())))
}

/// Encodes the module's pipeline-facing interface: per entry point its stage, name,
/// vertex-stage input locations with formats, and the ascending indices of the module bindings it
/// uses, then the module's resource bindings sorted by group and binding with their kinds and, for
/// uniform and read-only storage data, the WGSL byte size. `mulciber` validates application
/// pipeline declarations against this section, so an interface construct without a proven
/// mapping is a compile error rather than a silently unnamed slot.
///
/// An entry point uses a binding when Naga's analysis finds the global reachable from it,
/// directly or through a called function. The SPIR-V and MSL writers emit an entry point's
/// resources from the same analysis, so the recorded set is exactly what its native code binds.
#[allow(clippy::too_many_lines)]
fn shader_interface(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
) -> Result<Vec<u8>, ShaderBuildError> {
    let bindings = module_bindings(module)?;
    let mut bytes = Vec::new();
    push_count(&mut bytes, module.entry_points.len(), "entry points")?;
    for (index, entry) in module.entry_points.iter().enumerate() {
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
        let usage = info.get_entry_point(index);
        let used: Vec<u32> = (0_u32..)
            .zip(&bindings)
            .filter(|(_, (_, global))| !usage[*global].is_empty())
            .map(|(position, _)| position)
            .collect();
        push_count(&mut bytes, used.len(), "entry-point bindings")?;
        for position in used {
            bytes.extend_from_slice(&position.to_le_bytes());
        }
    }

    push_count(&mut bytes, bindings.len(), "resource bindings")?;
    for ((group, binding, kind, size), _) in bindings {
        bytes.extend_from_slice(&group.to_le_bytes());
        bytes.extend_from_slice(&binding.to_le_bytes());
        bytes.push(kind);
        bytes.extend_from_slice(&size.to_le_bytes());
    }
    Ok(bytes)
}

/// A recorded binding: group, binding, kind and byte size.
type RecordedBinding = (u32, u32, u8, u32);

/// Classifies every bound global, sorted by group and binding, keeping its handle so entry-point
/// usage can be looked up.
fn module_bindings(
    module: &naga::Module,
) -> Result<Vec<(RecordedBinding, Handle<naga::GlobalVariable>)>, ShaderBuildError> {
    let mut bindings = Vec::new();
    for (handle, variable) in module.global_variables.iter() {
        let Some(binding) = &variable.binding else {
            continue;
        };
        let inner = &module.types[variable.ty].inner;
        let (kind, size) = match (&variable.space, inner) {
            (AddressSpace::Uniform, _) => (BINDING_UNIFORM, inner.size(module.to_ctx())),
            (AddressSpace::Storage { access }, _) => {
                if *access != naga::StorageAccess::LOAD {
                    return Err(fail(format!(
                        "WGSL binding {}:{} is writable storage, which has no proven interface \
                         mapping",
                        binding.group, binding.binding
                    )));
                }
                if inner.is_dynamically_sized(&module.types) {
                    return Err(fail(format!(
                        "WGSL binding {}:{} is runtime-sized storage, which has no proven \
                         interface mapping; declare a creation-fixed array size",
                        binding.group, binding.binding
                    )));
                }
                (BINDING_STORAGE, inner.size(module.to_ctx()))
            }
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
                    dim: naga::ImageDimension::Cube,
                    arrayed: false,
                    class:
                        naga::ImageClass::Sampled {
                            kind: ScalarKind::Float,
                            multi: false,
                        },
                },
            ) => (BINDING_CUBE_TEXTURE, 0),
            (
                AddressSpace::Handle,
                TypeInner::Image {
                    dim: naga::ImageDimension::D2,
                    arrayed,
                    class: naga::ImageClass::Depth { multi: false },
                },
            ) => (
                if *arrayed {
                    BINDING_DEPTH_TEXTURE_ARRAY
                } else {
                    BINDING_DEPTH_TEXTURE
                },
                0,
            ),
            (
                AddressSpace::Handle,
                TypeInner::Image {
                    dim: naga::ImageDimension::D2,
                    arrayed: false,
                    class: naga::ImageClass::Depth { multi: true },
                },
            ) => (BINDING_MULTISAMPLED_DEPTH, 0),
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
        bindings.push(((binding.group, binding.binding, kind, size), handle));
    }
    bindings.sort_unstable_by_key(|&(recorded, _)| recorded);
    Ok(bindings)
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
    let msl = metal_source(module, info)?;
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

/// Generates the module's MSL. Every entry point gets the whole module's resource map, keyed by
/// WGSL binding number; Naga emits arguments only for the globals each entry point uses, so an
/// entry point's Metal slots are the WGSL binding numbers of exactly the bindings the interface
/// records for it.
fn metal_source(
    module: &naga::Module,
    info: &naga::valid::ModuleInfo,
) -> Result<String, ShaderBuildError> {
    let entry_resources = EntryPointResources {
        resources: metal_resources(module)?,
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
    Ok(msl)
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

    fn validate(module: &naga::Module) -> naga::valid::ModuleInfo {
        Validator::new(ValidationFlags::all(), Capabilities::empty())
            .validate(module)
            .expect("test WGSL validates")
    }

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
        let interface = shader_interface(&module, &validate(&module)).expect("cube interface");

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
        // The vertex stage reads only the uniform: binding table index 0.
        for word in [1_u32, 0] {
            expected.extend_from_slice(&word.to_le_bytes());
        }
        // cube_fragment records no vertex-stage inputs and samples the texture and sampler,
        // binding table indices 1 and 2.
        expected.push(1);
        expected.extend_from_slice(&13_u32.to_le_bytes());
        expected.extend_from_slice(b"cube_fragment");
        expected.extend_from_slice(&0_u32.to_le_bytes());
        for word in [2_u32, 1, 2] {
            expected.extend_from_slice(&word.to_le_bytes());
        }
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

    #[test]
    fn multisampled_depth_records_a_distinct_kind_and_metal_binding() {
        let source = "@group(0) @binding(1) var depth: texture_depth_multisampled_2d;
            @fragment fn sample_depth() -> @location(0) vec4<f32> {
                return vec4<f32>(textureLoad(depth, vec2<i32>(0), 0));
            }";
        let module = naga::front::wgsl::parse_str(source).unwrap();
        let interface = shader_interface(&module, &validate(&module)).unwrap();
        assert_eq!(
            interface[interface.len() - 5],
            super::BINDING_MULTISAMPLED_DEPTH
        );
        assert_eq!(metal_resources(&module).unwrap().len(), 1);
        let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
            .validate(&module)
            .unwrap();
        let options = naga::back::msl::Options {
            lang_version: (3, 1),
            per_entry_point_map: module
                .entry_points
                .iter()
                .map(|entry| {
                    (
                        entry.name.clone(),
                        naga::back::msl::EntryPointResources {
                            resources: metal_resources(&module).unwrap(),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ..Default::default()
        };
        let (msl, _) = naga::back::msl::write_string(
            &module,
            &info,
            &options,
            &naga::back::msl::PipelineOptions::default(),
        )
        .unwrap();
        assert!(msl.contains("depth2d_ms"));
    }

    #[test]
    fn depth_texture_array_records_its_own_kind() {
        let source = "
            @group(0) @binding(0) var cascades: texture_depth_2d_array;
            @group(0) @binding(1) var comparison: sampler_comparison;
            @fragment fn main_fragment() -> @location(0) vec4<f32> {
                let lit = textureSampleCompareLevel(
                    cascades,
                    comparison,
                    vec2<f32>(0.5, 0.5),
                    0,
                    0.5,
                );
                return vec4<f32>(lit);
            }
        ";
        let module = naga::front::wgsl::parse_str(source).expect("array WGSL parses");
        let interface = shader_interface(&module, &validate(&module)).expect("array interface");
        // Two binding records trail the interface: the depth-texture array records kind 6 and
        // the comparison sampler records kind 5, each with a zero byte size.
        let records = &interface[interface.len() - 26..];
        assert_eq!(records[8], 6);
        assert_eq!(records[21], 5);
        assert_eq!(metal_resources(&module).expect("Metal mapping").len(), 2);
    }

    #[test]
    fn cube_texture_records_its_own_kind_and_metal_texture_slot() {
        let source = "
            @group(0) @binding(3) var environment: texture_cube<f32>;
            @group(0) @binding(4) var environment_sampler: sampler;
            @fragment fn reflect_fragment() -> @location(0) vec4<f32> {
                return textureSampleLevel(
                    environment,
                    environment_sampler,
                    vec3<f32>(1.0, 0.0, 0.0),
                    0.0,
                );
            }
        ";
        let module = naga::front::wgsl::parse_str(source).expect("cube WGSL parses");
        let interface = shader_interface(&module, &validate(&module)).expect("cube interface");
        // Two binding records trail the interface: the cube texture records kind 8 and the
        // sampler kind 2, each with a zero byte size.
        let records = &interface[interface.len() - 26..];
        assert_eq!(records[4..8], 3_u32.to_le_bytes());
        assert_eq!(records[8], super::BINDING_CUBE_TEXTURE);
        assert_eq!(records[21], super::BINDING_SAMPLER);
        let resources = metal_resources(&module).expect("Metal mapping");
        assert_eq!(resources.len(), 2);
        assert!(
            resources
                .values()
                .any(|target| target.texture == Some(3) && target.sampler.is_none())
        );
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
        .expect("cube sampling emits SPIR-V");
        assert_eq!(words.first().copied(), Some(0x0723_0203));
    }

    #[test]
    fn arrayed_and_depth_cube_textures_are_rejected() {
        for declaration in [
            "@group(0) @binding(0) var map: texture_cube_array<f32>;",
            "@group(0) @binding(0) var map: texture_depth_cube;",
            "@group(0) @binding(0) var map: texture_cube<u32>;",
        ] {
            let source = std::format!(
                "{declaration}
                @fragment fn main_fragment() -> @location(0) vec4<f32> {{
                    return vec4<f32>(f32(textureNumLevels(map)));
                }}"
            );
            let module = naga::front::wgsl::parse_str(&source).expect("WGSL parses");
            // Cube arrays also need a capability the compiler never enables; validate with every
            // capability so the interface refusal itself is what is tested.
            let info = Validator::new(ValidationFlags::all(), Capabilities::all())
                .validate(&module)
                .expect("WGSL validates");
            let failure = shader_interface(&module, &info).expect_err("no proven mapping");
            assert!(failure.to_string().contains("no proven interface mapping"));
        }
    }

    /// Interface words after an entry point's name and vertex inputs: its used binding indices.
    fn entry_usage(interface: &[u8], name: &str) -> Vec<u32> {
        let word = |at: usize| u32::from_le_bytes(interface[at..at + 4].try_into().unwrap());
        let mut at = 4;
        for _ in 0..word(0) {
            let length = word(at + 1) as usize;
            let entry = &interface[at + 5..at + 5 + length];
            at += 5 + length;
            at += 4 + 5 * word(at) as usize;
            let count = word(at) as usize;
            let used = (0..count).map(|i| word(at + 4 + 4 * i)).collect();
            at += 4 + 4 * count;
            if entry == name.as_bytes() {
                return used;
            }
        }
        panic!("no entry point {name}");
    }

    const SHARED_MODULE: &str = "
        struct Draw { clip_from_object: mat4x4<f32>, tint: vec4<f32> }
        @group(0) @binding(0) var<uniform> draw: Draw;
        @group(0) @binding(1) var albedo: texture_2d<f32>;
        @group(0) @binding(2) var albedo_sampler: sampler;
        @group(0) @binding(3) var<storage, read> bones: array<mat4x4<f32>, 4>;
        @group(0) @binding(4) var environment: texture_cube<f32>;

        struct Surface { @builtin(position) clip: vec4<f32>, @location(0) uv: vec2<f32> }

        fn skin(position: vec3<f32>, index: u32) -> vec4<f32> {
            return bones[index] * vec4<f32>(position, 1.0);
        }

        @vertex fn prop_vertex(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>) -> Surface {
            return Surface(draw.clip_from_object * vec4<f32>(position, 1.0), uv);
        }
        @vertex fn skinned_vertex(
            @location(0) position: vec3<f32>,
            @location(1) uv: vec2<f32>,
            @location(2) bone: vec4<u32>,
        ) -> Surface {
            return Surface(draw.clip_from_object * skin(position, bone.x), uv);
        }
        @fragment fn prop_fragment(surface: Surface) -> @location(0) vec4<f32> {
            return textureSample(albedo, albedo_sampler, surface.uv) * draw.tint;
        }
        @fragment fn chrome_fragment(surface: Surface) -> @location(0) vec4<f32> {
            return textureSample(environment, albedo_sampler, vec3<f32>(surface.uv, 1.0));
        }
    ";

    #[test]
    fn each_entry_point_records_the_bindings_it_reaches() {
        let module = naga::front::wgsl::parse_str(SHARED_MODULE).expect("WGSL parses");
        let info = validate(&module);
        let interface = shader_interface(&module, &info).expect("interface");
        // The binding table is sorted by slot, so indices equal binding numbers here.
        assert_eq!(entry_usage(&interface, "prop_vertex"), [0]);
        // The bone palette is reached through `skin`, a called function.
        assert_eq!(entry_usage(&interface, "skinned_vertex"), [0, 3]);
        assert_eq!(entry_usage(&interface, "prop_fragment"), [0, 1, 2]);
        assert_eq!(entry_usage(&interface, "chrome_fragment"), [2, 4]);
        // The module table still records all five bindings.
        let tail = &interface[interface.len() - 5 * 13 - 4..];
        assert_eq!(tail[..4], 5_u32.to_le_bytes());
    }

    #[test]
    fn metal_entry_points_take_only_the_resources_they_use() {
        let module = naga::front::wgsl::parse_str(SHARED_MODULE).expect("WGSL parses");
        let info = validate(&module);
        let msl = super::metal_source(&module, &info).expect("MSL generation");
        // Each entry point's signature runs from its name to the opening brace of its body.
        let signature = |name: &str| {
            let start = msl
                .find(&std::format!(" {name}("))
                .expect("entry point in MSL");
            msl[start..start + msl[start..].find('{').expect("body")].to_owned()
        };
        let prop = signature("prop_vertex");
        assert!(prop.contains("[[buffer(0)]]"));
        assert!(!prop.contains("[[buffer(3)]]"));
        let skinned = signature("skinned_vertex");
        assert!(skinned.contains("[[buffer(0)]]") && skinned.contains("[[buffer(3)]]"));
        let fragment = signature("prop_fragment");
        assert!(fragment.contains("[[texture(1)]]") && fragment.contains("[[sampler(2)]]"));
        assert!(!fragment.contains("[[texture(4)]]") && !fragment.contains("[[buffer(3)]]"));
        let chrome = signature("chrome_fragment");
        assert!(chrome.contains("[[texture(4)]]") && !chrome.contains("[[texture(1)]]"));
    }

    #[test]
    fn read_only_fixed_storage_records_kind_and_size() {
        let source = "
            @group(0) @binding(0) var<storage, read> bones: array<mat4x4<f32>, 8>;
            @vertex fn skin(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
                return bones[0] * vec4<f32>(position, 1.0);
            }
        ";
        let module = naga::front::wgsl::parse_str(source).expect("storage WGSL parses");
        let interface = shader_interface(&module, &validate(&module)).expect("storage interface");
        // The binding record trails the interface: group, binding, kind 3, 8 * 64 bytes.
        let record = &interface[interface.len() - 13..];
        assert_eq!(record[8], 3);
        assert_eq!(record[9..], 512_u32.to_le_bytes());
    }

    #[test]
    fn writable_storage_is_rejected() {
        let source = "
            @group(0) @binding(0) var<storage, read_write> data: array<vec4<f32>, 4>;
            @vertex fn main_vertex() -> @builtin(position) vec4<f32> {
                return data[0];
            }
        ";
        let module = naga::front::wgsl::parse_str(source).expect("storage WGSL parses");
        let failure = shader_interface(&module, &validate(&module))
            .expect_err("writable storage is rejected");
        assert!(failure.to_string().contains("writable storage"));
    }

    #[test]
    fn runtime_sized_storage_is_rejected() {
        let source = "
            @group(0) @binding(0) var<storage, read> data: array<vec4<f32>>;
            @vertex fn main_vertex() -> @builtin(position) vec4<f32> {
                return data[0];
            }
        ";
        let module = naga::front::wgsl::parse_str(source).expect("storage WGSL parses");
        let failure = shader_interface(&module, &validate(&module))
            .expect_err("runtime-sized storage is rejected");
        assert!(failure.to_string().contains("runtime-sized storage"));
    }
}
