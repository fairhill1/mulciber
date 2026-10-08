//! WGSL module imports, composed by `naga_oil`.
//!
//! A [`ShaderModules`] set holds importable WGSL modules, each naming itself with
//! `#define_import_path`. A top-level shader `#import`s them and compiles through a [`WgslShader`]
//! into the same artifact [`crate::compile_wgsl`] writes, or into a host field. Mulciber's own
//! modules live under the reserved `mulciber::` namespace and are in every set.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use naga::valid::{Capabilities, ValidationFlags, Validator};
use naga_oil::compose::preprocess::Preprocessor;
use naga_oil::compose::{
    ComposableModuleDescriptor, Composer, ComposerError, ComposerErrorInner, ErrSource,
    ImportDefinition, NagaModuleDescriptor, ShaderDefValue, ShaderLanguage, ShaderType,
    get_preprocessor_data,
};

use crate::{ShaderBuildError, ShaderTarget, compile_validated, fail, host_field, write_generated};

/// The namespace reserved for modules that ship with `mulciber-shader`.
const ENGINE_NAMESPACE: &str = "mulciber";

/// Mulciber's WGSL library: (label used in diagnostics, source). Each declares its own
/// `#define_import_path` under `mulciber::`.
pub(crate) const ENGINE_MODULES: &[(&str, &str)] = &[
    (
        "mulciber-shader/wgsl/mulciber/colorspace.wgsl",
        include_str!("../wgsl/mulciber/colorspace.wgsl"),
    ),
    (
        "mulciber-shader/wgsl/mulciber/photometry.wgsl",
        include_str!("../wgsl/mulciber/photometry.wgsl"),
    ),
    (
        "mulciber-shader/wgsl/mulciber/pbr.wgsl",
        include_str!("../wgsl/mulciber/pbr.wgsl"),
    ),
    (
        "mulciber-shader/wgsl/mulciber/tonemap.wgsl",
        include_str!("../wgsl/mulciber/tonemap.wgsl"),
    ),
];

/// A value for a shader def, tested by `#ifdef`, `#if` and `#else` and substituted for
/// `#NAME` or `#{NAME}` in the source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShaderDef {
    /// A boolean def; `#if NAME == true` compares it.
    Bool(bool),
    /// A signed integer def.
    Int(i32),
    /// An unsigned integer def.
    UInt(u32),
}

impl From<bool> for ShaderDef {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i32> for ShaderDef {
    fn from(value: i32) -> Self {
        Self::Int(value)
    }
}

impl From<u32> for ShaderDef {
    fn from(value: u32) -> Self {
        Self::UInt(value)
    }
}

impl ShaderDef {
    fn value(self) -> ShaderDefValue {
        match self {
            Self::Bool(value) => ShaderDefValue::Bool(value),
            Self::Int(value) => ShaderDefValue::Int(value),
            Self::UInt(value) => ShaderDefValue::UInt(value),
        }
    }
}

#[derive(Clone, Debug)]
struct RegisteredModule {
    label: String,
    source: String,
    imports: Vec<String>,
}

/// A set of importable WGSL modules.
///
/// Every module names itself with `#define_import_path some::name` and is imported by that name,
/// whatever its file is called. A shader or module imports with `#import some::name` (items then
/// spelled `some::name::item`), `#import some::name as alias`, or `#import some::name::{item}`
/// (spelled `item`). Registration order does not matter: imports are resolved when a shader
/// compiles, and only the modules that shader reaches are composed.
///
/// Mulciber's own modules, under `mulciber::`, are in every set without being added; that
/// namespace is reserved, so a game cannot shadow them.
#[derive(Clone, Debug)]
pub struct ShaderModules {
    modules: BTreeMap<String, RegisteredModule>,
    tracked: Vec<PathBuf>,
}

impl Default for ShaderModules {
    fn default() -> Self {
        Self::new()
    }
}

impl ShaderModules {
    /// Creates a set holding only Mulciber's own `mulciber::` modules.
    ///
    /// # Panics
    ///
    /// Only if a bundled `mulciber::` module lacks its `#define_import_path`, which the crate's
    /// tests rule out.
    #[must_use]
    pub fn new() -> Self {
        let mut modules = Self {
            modules: BTreeMap::new(),
            tracked: Vec::new(),
        };
        for (label, source) in ENGINE_MODULES {
            modules
                .register(label, source, true)
                .expect("bundled mulciber WGSL modules are well-formed");
        }
        modules
    }

    /// Registers a module from WGSL text, returning the name it declares with
    /// `#define_import_path`. `label` names it in diagnostics, typically its path.
    ///
    /// # Errors
    ///
    /// Returns an error if the source has no `#define_import_path` or a malformed directive, if
    /// the name is in the reserved `mulciber::` namespace, or if another module already has it.
    pub fn add_source(
        &mut self,
        label: impl Into<String>,
        source: impl Into<String>,
    ) -> Result<String, ShaderBuildError> {
        self.register(&label.into(), &source.into(), false)
    }

    /// Registers the module in a WGSL file and tracks the file for
    /// [`rerun_if_changed`](Self::rerun_if_changed).
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, or for the reasons
    /// [`add_source`](Self::add_source) gives.
    pub fn add_file(&mut self, path: impl AsRef<Path>) -> Result<String, ShaderBuildError> {
        let path = path.as_ref();
        let source = read(path)?;
        let name = self.register(&path.display().to_string(), &source, false)?;
        self.tracked.push(path.to_path_buf());
        Ok(name)
    }

    /// Registers every `.wgsl` file under `directory`, recursively, that declares
    /// `#define_import_path`, and tracks the directory for
    /// [`rerun_if_changed`](Self::rerun_if_changed). Files without the directive are top-level
    /// shaders and are skipped, so modules and the shaders that import them can share a
    /// directory. Returns the registered names in path order.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be walked or a file cannot be read, or for the
    /// reasons [`add_source`](Self::add_source) gives.
    pub fn add_dir(
        &mut self,
        directory: impl AsRef<Path>,
    ) -> Result<Vec<String>, ShaderBuildError> {
        let directory = directory.as_ref();
        let mut files = Vec::new();
        collect_wgsl(directory, &mut files)?;
        files.sort();
        let mut names = Vec::new();
        for path in files {
            let source = read(&path)?;
            if !declares_import_path(&source) {
                continue;
            }
            names.push(self.register(&path.display().to_string(), &source, false)?);
        }
        self.tracked.push(directory.to_path_buf());
        Ok(names)
    }

    /// Whether a module with this import path is registered, including Mulciber's own.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.modules.contains_key(name)
    }

    /// The import paths of every registered module, Mulciber's own included, in sorted order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.modules.keys().map(String::as_str)
    }

    /// The files and directories given to [`add_file`](Self::add_file) and
    /// [`add_dir`](Self::add_dir).
    #[must_use]
    pub fn tracked_paths(&self) -> &[PathBuf] {
        &self.tracked
    }

    /// Prints `cargo::rerun-if-changed` for every tracked file and directory, so a `build.rs`
    /// reruns when a module changes or a directory gains one. Cargo scans a directory's whole
    /// tree. Track the top-level shader files separately; they are not part of the set.
    pub fn rerun_if_changed(&self) {
        for path in &self.tracked {
            println!("cargo::rerun-if-changed={}", path.display());
        }
    }

    /// Reads a top-level shader from a file, ready to compile against this set.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read.
    pub fn shader(&self, path: impl AsRef<Path>) -> Result<WgslShader<'_>, ShaderBuildError> {
        let path = path.as_ref();
        Ok(self.shader_source(path.display().to_string(), read(path)?))
    }

    /// A top-level shader from WGSL text, ready to compile against this set. `label` names it
    /// in diagnostics.
    #[must_use]
    pub fn shader_source(
        &self,
        label: impl Into<String>,
        source: impl Into<String>,
    ) -> WgslShader<'_> {
        WgslShader {
            modules: self,
            label: label.into(),
            source: source.into(),
            defs: BTreeMap::new(),
        }
    }

    /// Generates host evaluators for functions of registered modules, named by qualified path
    /// (`game::lighting::falloff`), without a top-level shader. Each requested function becomes a
    /// `pub fn` with its plain item name, as [`WgslShader::compile_host_field`] describes; a CPU
    /// baker can call the same lighting maths its shaders import.
    ///
    /// # Errors
    ///
    /// Returns an error for an unqualified name, or the reasons
    /// [`WgslShader::compile_host_field`] gives.
    pub fn compile_host_field(
        &self,
        generated: impl AsRef<Path>,
        functions: &[&str],
    ) -> Result<(), ShaderBuildError> {
        if let Some(plain) = functions.iter().find(|name| !name.contains("::")) {
            return Err(fail(format!(
                "{plain}: a module host field names its function by module path, such as \
                 mulciber::colorspace::luminance"
            )));
        }
        let mut paths: Vec<&str> = functions
            .iter()
            .filter_map(|name| name.rsplit_once("::").map(|(path, _)| path))
            .collect();
        paths.sort_unstable();
        paths.dedup();
        self.shader_source(paths.join(", "), "")
            .compile_host_field(generated, functions)
    }

    /// Text that changes whenever any registered module changes: the `mulciber-shader` version
    /// and every module's import path, label and source. Hash it to key a build cache for
    /// [`compile_host_field`](Self::compile_host_field); a shader's own
    /// [`WgslShader::cache_key`] covers only what it imports.
    #[must_use]
    pub fn cache_key(&self) -> String {
        let mut key = format!("mulciber-shader {}\n", env!("CARGO_PKG_VERSION"));
        for (name, module) in &self.modules {
            let _ = writeln!(key, "module {name} {}\n{}", module.label, module.source);
        }
        key
    }

    fn register(
        &mut self,
        label: &str,
        source: &str,
        engine: bool,
    ) -> Result<String, ShaderBuildError> {
        let metadata = Preprocessor::default()
            .get_preprocessor_metadata(source, false)
            .map_err(|inner| {
                fail(format!(
                    "WGSL module: {}",
                    ComposerError {
                        inner,
                        source: ErrSource::Constructing {
                            path: label.to_owned(),
                            source: source.to_owned(),
                            offset: 0,
                        },
                    }
                    .emit_to_string(&Composer::default())
                ))
            })?;
        let name = metadata.name.ok_or_else(|| {
            fail(format!(
                "{label}: an importable WGSL module must name itself with #define_import_path"
            ))
        })?;
        let reserved = name == ENGINE_NAMESPACE
            || name
                .strip_prefix(ENGINE_NAMESPACE)
                .is_some_and(|rest| rest.starts_with("::"));
        if reserved && !engine {
            return Err(fail(format!(
                "{label}: the module path {name} is in the `mulciber::` namespace, which is \
                 reserved for modules that ship with mulciber-shader"
            )));
        }
        if let Some(existing) = self.modules.get(&name) {
            return Err(fail(format!(
                "{label}: the module path {name} is already registered by {}",
                existing.label
            )));
        }
        let imports = get_preprocessor_data(source)
            .1
            .into_iter()
            .map(|import| import.import)
            .collect();
        self.modules.insert(
            name.clone(),
            RegisteredModule {
                label: label.to_owned(),
                source: source.to_owned(),
                imports,
            },
        );
        Ok(name)
    }

    /// Orders the registered modules `imports` reach so each comes after the modules it imports.
    /// Unregistered names are skipped; the composer reports them at the `#import` that names
    /// them.
    fn import_order<'a>(
        &'a self,
        imports: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<&'a str>, ShaderBuildError> {
        fn visit<'a>(
            set: &'a ShaderModules,
            name: &'a str,
            path: &mut Vec<&'a str>,
            done: &mut BTreeSet<&'a str>,
            order: &mut Vec<&'a str>,
        ) -> Result<(), ShaderBuildError> {
            if done.contains(name) {
                return Ok(());
            }
            let Some(module) = set.modules.get(name) else {
                return Ok(());
            };
            if let Some(start) = path.iter().position(|entry| *entry == name) {
                let mut cycle = path[start..].to_vec();
                cycle.push(name);
                return Err(fail(format!(
                    "{}: WGSL modules import each other in a cycle: {}",
                    module.label,
                    cycle.join(" -> ")
                )));
            }
            path.push(name);
            for import in &module.imports {
                visit(set, import, path, done, order)?;
            }
            path.pop();
            done.insert(name);
            order.push(name);
            Ok(())
        }

        let mut done = BTreeSet::new();
        let mut order = Vec::new();
        for name in imports {
            visit(self, name, &mut Vec::new(), &mut done, &mut order)?;
        }
        Ok(order)
    }
}

/// A top-level WGSL shader with the shader defs to compile it with, borrowed from the
/// [`ShaderModules`] it imports from.
///
/// The shader holds the entry points: entry points inside imported modules are not entry points
/// of the composed shader. Its own names, entry points included, are kept; items from imported
/// modules are renamed by the composer, which is invisible to pipelines but shows in generated
/// MSL and SPIR-V debug names.
#[derive(Clone, Debug)]
pub struct WgslShader<'m> {
    modules: &'m ShaderModules,
    label: String,
    source: String,
    defs: BTreeMap<String, ShaderDef>,
}

impl WgslShader<'_> {
    /// Sets a shader def for this compile. Defs reach the shader and every module it imports, so
    /// one source compiles into several variants.
    #[must_use]
    pub fn define(mut self, name: impl Into<String>, value: impl Into<ShaderDef>) -> Self {
        self.defs.insert(name.into(), value.into());
        self
    }

    /// Composes the shader with its imports and writes the same opaque Mulciber artifact
    /// [`crate::compile_wgsl`] writes for one WGSL file, with the same validation and native
    /// tools.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing or cyclic import, a malformed directive, invalid WGSL in
    /// the shader or any module it imports (reported against that module's own file and line),
    /// or any reason [`crate::compile_wgsl`] gives.
    pub fn compile_wgsl(
        &self,
        artifact: impl AsRef<Path>,
        target: ShaderTarget,
    ) -> Result<(), ShaderBuildError> {
        let (module, info) = self.compose(&[])?;
        compile_validated(&module, &info, artifact.as_ref(), target)
    }

    /// Composes the shader with its imports and generates host evaluators for `functions`, like
    /// [`crate::compile_host_field`].
    ///
    /// A plain name requests a function of the top-level shader. A qualified name such as
    /// `mulciber::colorspace::luminance` requests a function of a registered module directly, whether
    /// or not the shader imports it; its generated `pub fn` takes the plain item name
    /// (`luminance`). Private helpers from imported modules are named after their module
    /// (`mulciber_colorspace_srgb_to_linear_channel`).
    ///
    /// # Errors
    ///
    /// Returns an error for the composition failures [`compile_wgsl`](Self::compile_wgsl)
    /// reports, a qualified name whose module is not registered, two generated functions that
    /// would share a Rust name, or the reasons [`crate::compile_host_field`] gives.
    pub fn compile_host_field(
        &self,
        generated: impl AsRef<Path>,
        functions: &[&str],
    ) -> Result<(), ShaderBuildError> {
        let mut extra = Vec::new();
        for function in functions {
            let Some((path, item)) = function.rsplit_once("::") else {
                continue;
            };
            if !self.modules.contains(path) {
                let known: Vec<&str> = self.modules.names().collect();
                return Err(fail(format!(
                    "{function}: {path} is not a registered WGSL module; registered: {}",
                    known.join(", ")
                )));
            }
            extra.push((path, item));
        }
        let (mut module, info) = self.compose(&extra)?;
        let order = self.import_order(&extra)?;
        let requested = host_names(&mut module, &order, functions)?;
        let requested: Vec<&str> = requested.iter().map(String::as_str).collect();
        let label = Path::new(&self.label).file_name().map_or_else(
            || self.label.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        let rust = host_field::generate(&module, &info, &label, &requested)?;
        write_generated(generated.as_ref(), &rust)
    }

    /// Text that changes whenever anything this shader compiles from changes: the
    /// `mulciber-shader` version, the defs, the shader, and the label and source of every
    /// module it can reach through `#import`, whichever defs are set. Hash it to key a build
    /// cache.
    ///
    /// # Errors
    ///
    /// Returns an error if the registered modules import each other in a cycle.
    pub fn cache_key(&self) -> Result<String, ShaderBuildError> {
        let mut key = format!("mulciber-shader {}\n", env!("CARGO_PKG_VERSION"));
        for (name, value) in &self.defs {
            let _ = writeln!(key, "def {name}={value:?}");
        }
        let _ = writeln!(key, "shader {}\n{}", self.label, self.source);
        for name in self.import_order(&[])? {
            let module = &self.modules.modules[name];
            let _ = writeln!(key, "module {name} {}\n{}", module.label, module.source);
        }
        Ok(key)
    }

    /// The registered modules the shader and the `extra` (module, item) pairs reach, each after
    /// its own imports.
    fn import_order(&self, extra: &[(&str, &str)]) -> Result<Vec<&str>, ShaderBuildError> {
        let imports = get_preprocessor_data(&self.source).1;
        let names: Vec<&str> = imports
            .iter()
            .map(|import| import.import.as_str())
            .chain(extra.iter().map(|(path, _)| *path))
            .filter_map(|import| {
                self.modules
                    .modules
                    .get_key_value(import)
                    .map(|(name, _)| name.as_str())
            })
            .collect();
        self.modules.import_order(names)
    }

    /// Composes the shader with the modules it imports, plus the `extra` (module, item) pairs.
    /// The composer copies only the items a shader uses, so a host field's requested module
    /// functions are imported by name.
    pub(crate) fn compose(
        &self,
        extra: &[(&str, &str)],
    ) -> Result<(naga::Module, naga::valid::ModuleInfo), ShaderBuildError> {
        // Validate exactly as compile_wgsl does; the composer's default adds capabilities.
        let mut composer = Composer::default().with_capabilities(Capabilities::empty());
        for name in self.import_order(extra)? {
            let module = &self.modules.modules[name];
            let added = composer
                .add_composable_module(ComposableModuleDescriptor {
                    source: &module.source,
                    file_path: &module.label,
                    language: ShaderLanguage::Wgsl,
                    ..Default::default()
                })
                .map(|_| ());
            added.map_err(|error| self.composition_error(&error, &composer))?;
        }
        let shader_defs: HashMap<String, ShaderDefValue> = self
            .defs
            .iter()
            .map(|(name, value)| (name.clone(), value.value()))
            .collect();
        let mut grouped: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for (path, item) in extra {
            grouped.entry(path).or_default().push((*item).to_owned());
        }
        let additional_imports: Vec<ImportDefinition> = grouped
            .into_iter()
            .map(|(path, items)| ImportDefinition {
                import: path.to_owned(),
                items,
            })
            .collect();
        let module = composer
            .make_naga_module(NagaModuleDescriptor {
                source: &self.source,
                file_path: &self.label,
                shader_type: ShaderType::Wgsl,
                shader_defs,
                additional_imports: &additional_imports,
            })
            .map_err(|error| self.composition_error(&error, &composer))?;
        let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
            .validate(&module)
            .map_err(|error| {
                fail(format!(
                    "WGSL validation of {} composed with its imports: {error}",
                    self.label
                ))
            })?;
        Ok((module, info))
    }

    fn composition_error(&self, error: &ComposerError, composer: &Composer) -> ShaderBuildError {
        let mut message = format!(
            "WGSL composition: {}",
            error.emit_to_string(composer).trim_end()
        );
        if let ComposerErrorInner::ImportNotFound(name, _) = &error.inner {
            let prefix = format!("{name}::");
            let shadowed: Vec<&str> = self
                .modules
                .names()
                .filter(|module| module.starts_with(&prefix))
                .collect();
            if shadowed.is_empty() {
                let known: Vec<&str> = self.modules.names().collect();
                let _ = write!(
                    message,
                    "\n{name} is not a registered WGSL module; registered: {}",
                    known.join(", ")
                );
            }
            for module in shadowed {
                let alias = module.rsplit("::").next().unwrap_or(module);
                let _ = write!(
                    message,
                    "\n`#import {module}` also makes `{alias}` name that module in the importing \
                     file, so the identifier `{alias}` marked above is read as `{module}`; \
                     rename it, or import with `#import {module} as another_name` or \
                     `#import {module}::{{items}}`"
                );
            }
        }
        ShaderBuildError(message.trim_end().to_owned())
    }
}

/// Gives the composed module's functions Rust-spellable names and returns the names to request.
///
/// The composer decorates every item of an imported module with an encoding of the module's
/// path. A requested `module::item` takes the plain `item`; every other imported function takes
/// `module_path_item`.
fn host_names(
    module: &mut naga::Module,
    imported: &[&str],
    functions: &[&str],
) -> Result<Vec<String>, ShaderBuildError> {
    let suffixes: Vec<(&str, String)> = imported
        .iter()
        .map(|name| (*name, Composer::decorated_name(Some(name), "")))
        .collect();
    let mut requested = Vec::with_capacity(functions.len());
    let mut renames: HashMap<String, (&str, &str)> = HashMap::new();
    for function in functions {
        let Some((path, item)) = function.rsplit_once("::") else {
            requested.push((*function).to_owned());
            continue;
        };
        if !imported.contains(&path) {
            return Err(fail(format!(
                "{function} names a function of {path}, which is not composed into the shader"
            )));
        }
        renames.insert(Composer::decorated_name(Some(path), item), (path, item));
        requested.push(item.to_owned());
    }

    let mut taken: BTreeMap<String, String> = BTreeMap::new();
    for (_, function) in module.functions.iter_mut() {
        let Some(name) = function.name.clone() else {
            continue;
        };
        let (rust, wgsl) = if let Some((path, item)) = renames.get(&name) {
            ((*item).to_owned(), format!("{path}::{item}"))
        } else if let Some((path, item)) = suffixes
            .iter()
            .find_map(|(path, suffix)| name.strip_suffix(suffix.as_str()).map(|item| (path, item)))
        {
            (
                format!("{}_{item}", path.replace("::", "_")),
                format!("{path}::{item}"),
            )
        } else {
            (name.clone(), name.clone())
        };
        if let Some(other) = taken.insert(rust.clone(), wgsl.clone()) {
            return Err(fail(format!(
                "{wgsl} and {other} would both generate the Rust function {rust}; generate one of \
                 them from a wrapper with another name"
            )));
        }
        function.name = Some(rust);
    }
    Ok(requested)
}

fn declares_import_path(source: &str) -> bool {
    source
        .lines()
        .any(|line| line.trim_start().starts_with("#define_import_path"))
}

fn collect_wgsl(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), ShaderBuildError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| fail(format!("read {}: {error}", directory.display())))?;
    for entry in entries {
        let path = entry
            .map_err(|error| fail(format!("read {}: {error}", directory.display())))?
            .path();
        if path.is_dir() {
            collect_wgsl(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "wgsl")
        {
            files.push(path);
        }
    }
    Ok(())
}

fn read(path: &Path) -> Result<String, ShaderBuildError> {
    fs::read_to_string(path).map_err(|error| fail(format!("read {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ShaderModules, WgslShader};
    use crate::{metal_source, shader_interface};

    const SHADOW: &str = "#define_import_path game::shadow

@group(0) @binding(4) var shadow_map: texture_depth_2d;
@group(0) @binding(5) var shadow_sampler: sampler_comparison;

fn shadow_factor(uv: vec2<f32>, depth: f32) -> f32 {
    return textureSampleCompareLevel(shadow_map, shadow_sampler, uv, depth);
}
";

    const LIGHT: &str = "#define_import_path game::light
#import game::shadow

const AMBIENT: f32 = 0.1;

fn falloff(distance: f32) -> f32 {
    return 1.0 / (1.0 + distance * distance);
}

fn lit(uv: vec2<f32>, depth: f32, distance: f32) -> f32 {
    return AMBIENT + game::shadow::shadow_factor(uv, depth) * falloff(distance);
}
";

    const SCENE: &str = "#import game::light

struct Surface {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex fn scene_vertex(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>) -> Surface {
    return Surface(vec4<f32>(position, 1.0), uv);
}

@fragment fn scene_fragment(surface: Surface) -> @location(0) vec4<f32> {
    return vec4<f32>(game::light::lit(surface.uv, 0.5, 2.0));
}
";

    fn game_modules() -> ShaderModules {
        let mut modules = ShaderModules::new();
        // Registered before the module it imports: order does not matter.
        assert_eq!(
            modules.add_source("shaders/light.wgsl", LIGHT).unwrap(),
            "game::light"
        );
        assert_eq!(
            modules.add_source("shaders/shadow.wgsl", SHADOW).unwrap(),
            "game::shadow"
        );
        modules
    }

    fn compose(shader: &WgslShader<'_>) -> (naga::Module, naga::valid::ModuleInfo) {
        shader.compose(&[]).expect("shader composes")
    }

    fn compose_error(shader: &WgslShader<'_>) -> String {
        shader
            .compose(&[])
            .expect_err("composition fails")
            .to_string()
    }

    fn spirv(module: &naga::Module, info: &naga::valid::ModuleInfo) -> Vec<u32> {
        naga::back::spv::write_vec(
            module,
            info,
            &naga::back::spv::Options {
                lang_version: (1, 4),
                ..Default::default()
            },
            None,
        )
        .expect("SPIR-V generation")
    }

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "mulciber-shader-modules-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn imports_compose_across_two_modules() {
        let modules = game_modules();
        let (module, info) = compose(&modules.shader_source("shaders/scene.wgsl", SCENE));
        let names: Vec<&str> = module
            .entry_points
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, ["scene_vertex", "scene_fragment"]);

        // The bindings declared in game::shadow, reached through game::light, are the shader's.
        let interface = shader_interface(&module, &info).expect("interface");
        let records = &interface[interface.len() - 26..];
        assert_eq!(records[4..8], 4_u32.to_le_bytes());
        assert_eq!(records[8], crate::BINDING_DEPTH_TEXTURE);
        assert_eq!(records[17..21], 5_u32.to_le_bytes());
        assert_eq!(records[21], crate::BINDING_COMPARISON_SAMPLER);

        assert_eq!(spirv(&module, &info).first().copied(), Some(0x0723_0203));
        let msl = metal_source(&module, &info).expect("MSL generation");
        assert!(msl.contains("[[texture(4)]]") && msl.contains("[[sampler(5)]]"));
    }

    /// The composer rebuilds the module, so type order and therefore SPIR-V ids can differ from
    /// a direct parse; the interface `mulciber` validates against is identical.
    #[test]
    fn a_shader_without_imports_composes_to_the_compile_wgsl_module() {
        let source = include_str!("../../../examples/cube/src/cube.wgsl");
        let plain = naga::front::wgsl::parse_str(source).expect("cube WGSL parses");
        let plain_info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&plain)
        .expect("cube WGSL validates");
        let modules = ShaderModules::new();
        let (composed, info) = compose(&modules.shader_source("cube.wgsl", source));
        assert_eq!(
            shader_interface(&composed, &info).unwrap(),
            shader_interface(&plain, &plain_info).unwrap()
        );
        assert_eq!(
            spirv(&composed, &info).len(),
            spirv(&plain, &plain_info).len()
        );
        assert_eq!(
            metal_source(&composed, &info).unwrap().len(),
            metal_source(&plain, &plain_info).unwrap().len()
        );
    }

    #[test]
    fn a_missing_import_names_the_module_file_and_line() {
        let modules = game_modules();
        let error = compose_error(&modules.shader_source(
            "shaders/scene.wgsl",
            "#import game::lights\n\n@fragment fn main() -> @location(0) vec4<f32> {\n    \
             return vec4<f32>(game::lights::falloff(1.0));\n}\n",
        ));
        assert!(error.contains("'game::lights' not found"), "{error}");
        assert!(error.contains("shaders/scene.wgsl:4:22"), "{error}");
        assert!(
            error.contains("registered: game::light, game::shadow, mulciber::colorspace"),
            "{error}"
        );

        // A module importing an unregistered module is reported against its own file.
        let mut modules = ShaderModules::new();
        modules
            .add_source(
                "shaders/fog.wgsl",
                "#define_import_path game::fog\n#import game::noise\n\nfn fog(x: f32) -> f32 \
                 {\n    return game::noise::value(x);\n}\n",
            )
            .unwrap();
        let error = compose_error(&modules.shader_source(
            "shaders/scene.wgsl",
            "#import game::fog\n@fragment fn main() -> @location(0) vec4<f32> { return \
             vec4<f32>(game::fog::fog(1.0)); }\n",
        ));
        assert!(error.contains("'game::noise' not found"), "{error}");
        assert!(error.contains("shaders/fog.wgsl:5:"), "{error}");
    }

    #[test]
    fn a_local_named_like_an_imported_module_is_explained() {
        let modules = game_modules();
        let error = compose_error(&modules.shader_source(
            "lit.wgsl",
            "#import game::shadow
@fragment fn lit(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let shadow = game::shadow::shadow_factor(uv, 0.5);
    return vec4<f32>(shadow);
}
",
        ));
        assert!(error.contains("lit.wgsl:3:9"), "{error}");
        assert!(
            error.contains("also makes `shadow` name that module"),
            "{error}"
        );
        // Importing the items, or the module under another name, leaves `shadow` free.
        for import in [
            "#import game::shadow::{shadow_factor}",
            "#import game::shadow as shadowing",
        ] {
            let call = if import.contains("as shadowing") {
                "shadowing::shadow_factor"
            } else {
                "shadow_factor"
            };
            compose(&modules.shader_source(
                "lit.wgsl",
                format!(
                    "{import}
@fragment fn lit(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {{
    let shadow = {call}(uv, 0.5);
    return vec4<f32>(shadow);
}}
"
                ),
            ));
        }
    }

    #[test]
    fn module_bindings_a_shader_does_not_reach_stay_out_of_its_entry_points() {
        let modules = game_modules();
        let (module, info) = compose(&modules.shader_source(
            "shaders/scene.wgsl",
            "#import game::light
@fragment fn main(@location(0) distance: f32) -> @location(0) vec4<f32> {
    return vec4<f32>(game::light::falloff(distance));
}
",
        ));
        // game::light's own import of game::shadow brings the shadow bindings into the module,
        // but `main` reaches neither, so a pipeline built from it declares none.
        let interface = shader_interface(&module, &info).unwrap();
        let word = |at: usize| u32::from_le_bytes(interface[at..at + 4].try_into().unwrap());
        assert_eq!(word(0), 1);
        assert_eq!(&interface[9..13], b"main");
        assert_eq!(word(13), 0, "fragment inputs are not recorded");
        assert_eq!(word(17), 0, "main uses no binding");
        assert_eq!(
            word(21),
            0,
            "the shadow bindings are no buffers, so there are no layouts"
        );
        assert_eq!(word(25), 2, "the module table records both shadow bindings");
    }

    #[test]
    fn errors_inside_an_imported_module_point_at_its_file_and_line() {
        let mut modules = game_modules();
        modules
            .add_source(
                "shaders/broken.wgsl",
                "#define_import_path game::broken\n\nfn scaled(x: f32) -> f32 {\n    return x * \
                 true;\n}\n\nfn unparsed(x: f32) -> f32 {\n    return x +;\n}\n",
            )
            .unwrap();
        let error = compose_error(&modules.shader_source(
            "shaders/scene.wgsl",
            "#import game::broken\n@fragment fn main() -> @location(0) vec4<f32> { return \
             vec4<f32>(game::broken::scaled(1.0)); }\n",
        ));
        assert!(error.contains("shaders/broken.wgsl:8:15"), "{error}");
        assert!(error.contains("expected expression"), "{error}");

        let mut modules = game_modules();
        modules
            .add_source(
                "shaders/invalid.wgsl",
                "#define_import_path game::invalid\n\nfn scaled(x: f32) -> f32 {\n    return x * \
                 true;\n}\n",
            )
            .unwrap();
        let error = compose_error(&modules.shader_source(
            "shaders/scene.wgsl",
            "#import game::invalid\n@fragment fn main() -> @location(0) vec4<f32> { return \
             vec4<f32>(game::invalid::scaled(1.0)); }\n",
        ));
        assert!(error.contains("shaders/invalid.wgsl:3:1"), "{error}");
        assert!(error.contains("return x * true"), "{error}");
        assert!(error.contains("game::invalid::scaled"), "{error}");
    }

    #[test]
    fn shader_defs_select_variants_in_the_shader_and_its_modules() {
        let mut modules = ShaderModules::new();
        modules
            .add_source(
                "shaders/depth.wgsl",
                "#define_import_path game::depth
#ifdef MSAA
@group(0) @binding(1) var scene_depth: texture_depth_multisampled_2d;
fn depth_at(p: vec2<i32>) -> f32 { return textureLoad(scene_depth, p, 0); }
#else
@group(0) @binding(1) var scene_depth: texture_depth_2d;
fn depth_at(p: vec2<i32>) -> f32 { return textureLoad(scene_depth, p, 0); }
#endif
",
            )
            .unwrap();
        let source = "#import game::depth
@fragment fn main(@builtin(position) at: vec4<f32>) -> @location(0) vec4<f32> {
#if STEPS == 2
    return vec4<f32>(game::depth::depth_at(vec2<i32>(at.xy)) * 2.0);
#else
    return vec4<f32>(game::depth::depth_at(vec2<i32>(at.xy)));
#endif
}
";
        let kind = |shader: WgslShader<'_>| {
            let (module, info) = compose(&shader);
            let interface = shader_interface(&module, &info).unwrap();
            interface[interface.len() - 5]
        };
        let shader = modules.shader_source("shaders/scene.wgsl", source);
        assert_eq!(
            kind(shader.clone().define("STEPS", 1)),
            crate::BINDING_DEPTH_TEXTURE
        );
        assert_eq!(
            kind(shader.clone().define("STEPS", 2).define("MSAA", true)),
            crate::BINDING_MULTISAMPLED_DEPTH
        );
        // `#if` compares against a def that must exist.
        let error = compose_error(&shader);
        assert!(error.contains("Unknown shader def: 'STEPS'"), "{error}");
        assert!(error.contains("shaders/scene.wgsl:3:"), "{error}");
    }

    #[test]
    fn engine_modules_are_importable_without_registering() {
        let modules = ShaderModules::new();
        assert!(modules.contains("mulciber::colorspace"));
        let (module, info) = compose(&modules.shader_source(
            "grade.wgsl",
            "#import mulciber::colorspace
#import mulciber::colorspace::{luminance}
@fragment fn grade(@location(0) encoded: vec3<f32>) -> @location(0) vec4<f32> {
    let linear = mulciber::colorspace::srgb_to_linear(encoded);
    return vec4<f32>(mulciber::colorspace::linear_to_srgb(linear * 0.5), luminance(linear));
}
",
        ));
        assert_eq!(spirv(&module, &info).first().copied(), Some(0x0723_0203));
        metal_source(&module, &info).expect("MSL generation");

        let mut modules = ShaderModules::new();
        let error = modules
            .add_source(
                "shaders/color.wgsl",
                "#define_import_path mulciber::colorspace\nfn luminance(c: vec3<f32>) -> f32 { return \
                 c.y; }\n",
            )
            .expect_err("the mulciber namespace is reserved");
        assert!(error.to_string().contains("reserved"), "{error}");
        let error = modules
            .add_source("shaders/mine.wgsl", "#define_import_path mulciber::mine\n")
            .expect_err("the mulciber namespace is reserved");
        assert!(error.to_string().contains("reserved"), "{error}");
        modules
            .add_source("shaders/mine.wgsl", "#define_import_path mulcibers::mine\n")
            .expect("only the mulciber namespace itself is reserved");
    }

    #[test]
    fn registration_rejects_duplicates_unnamed_modules_and_cycles() {
        let mut modules = game_modules();
        let error = modules
            .add_source("shaders/light2.wgsl", LIGHT)
            .expect_err("duplicate import path");
        assert!(
            error
                .to_string()
                .contains("game::light is already registered by shaders/light.wgsl"),
            "{error}"
        );
        let error = modules
            .add_source("shaders/anonymous.wgsl", "fn f() {}\n")
            .expect_err("no import path");
        assert!(error.to_string().contains("#define_import_path"), "{error}");

        modules
            .add_source(
                "shaders/a.wgsl",
                "#define_import_path game::a\n#import game::b\nfn a() -> f32 { return \
                 game::b::b(); }\n",
            )
            .unwrap();
        modules
            .add_source(
                "shaders/b.wgsl",
                "#define_import_path game::b\n#import game::a\nfn b() -> f32 { return \
                 game::a::a(); }\n",
            )
            .unwrap();
        let error = compose_error(&modules.shader_source(
            "scene.wgsl",
            "#import game::a\n@fragment fn main() -> @location(0) vec4<f32> { return \
             vec4<f32>(game::a::a()); }\n",
        ));
        assert!(
            error.contains("in a cycle: game::a -> game::b -> game::a"),
            "{error}"
        );
    }

    #[test]
    fn directories_register_modules_and_skip_top_level_shaders() {
        let directory = scratch("dir");
        std::fs::create_dir_all(directory.join("lighting")).unwrap();
        std::fs::write(directory.join("lighting/shadow.wgsl"), SHADOW).unwrap();
        std::fs::write(directory.join("lighting/light.wgsl"), LIGHT).unwrap();
        std::fs::write(directory.join("scene.wgsl"), SCENE).unwrap();
        std::fs::write(directory.join("notes.txt"), "not WGSL").unwrap();

        let mut modules = ShaderModules::new();
        let names = modules.add_dir(&directory).unwrap();
        assert_eq!(names, ["game::light", "game::shadow"]);
        assert_eq!(modules.tracked_paths(), std::slice::from_ref(&directory));
        let shader = modules.shader(directory.join("scene.wgsl")).unwrap();
        compose(&shader);

        // A cache key moves with every module the shader reaches.
        let before = shader.cache_key().unwrap();
        let mut changed = ShaderModules::new();
        changed
            .add_file(directory.join("lighting/light.wgsl"))
            .unwrap();
        changed
            .add_source(
                directory.join("lighting/shadow.wgsl").display().to_string(),
                SHADOW.replace("binding(5)", "binding(6)"),
            )
            .unwrap();
        let after = changed
            .shader(directory.join("scene.wgsl"))
            .unwrap()
            .cache_key()
            .unwrap();
        assert_ne!(before, after);
        assert_eq!(
            changed.tracked_paths(),
            [directory.join("lighting/light.wgsl")]
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn host_fields_reach_imported_functions() {
        let directory = scratch("host");
        let modules = game_modules();
        let shader = modules.shader_source(
            "shaders/bake.wgsl",
            "#import game::light
#import mulciber::colorspace

fn baked(distance: f32, albedo: vec3<f32>) -> f32 {
    return mulciber::colorspace::luminance(albedo) * game::light::falloff(distance);
}
",
        );
        let output = directory.join("bake_host.rs");
        shader
            .compile_host_field(&output, &["baked", "game::light::falloff"])
            .expect("host field with imports");
        let rust = std::fs::read_to_string(&output).unwrap();
        assert!(rust.starts_with("// Generated by mulciber-shader from bake.wgsl."));
        assert!(rust.contains("pub fn baked(distance: f32, albedo: [f32; 3]) -> f32"));
        assert!(rust.contains("pub fn falloff(distance: f32) -> f32"));
        assert!(rust.contains("fn mulciber_colorspace_luminance(linear: [f32; 3]) -> f32"));
        assert!(!rust.contains("naga_oil"), "{rust}");

        // A function that touches a binding is still refused, through the import.
        let error = shader
            .compile_host_field(&output, &["game::light::lit"])
            .expect_err("bindings have no host meaning");
        assert!(error.to_string().contains("texture read"), "{error}");

        let error = modules
            .compile_host_field(&output, &["game::lights::falloff"])
            .expect_err("unregistered module");
        assert!(
            error
                .to_string()
                .contains("game::lights is not a registered WGSL module"),
            "{error}"
        );
        let error = modules
            .compile_host_field(&output, &["falloff"])
            .expect_err("module host fields are qualified");
        assert!(error.to_string().contains("module path"), "{error}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn host_names_refuse_colliding_rust_names() {
        let modules = game_modules();
        let error = modules
            .shader_source(
                "bake.wgsl",
                "#import game::light
fn falloff(distance: f32) -> f32 { return game::light::falloff(distance) * 2.0; }
",
            )
            .compile_host_field(scratch("collide").join("out.rs"), &["game::light::falloff"])
            .expect_err("two functions named falloff");
        assert!(
            error
                .to_string()
                .contains("would both generate the Rust function falloff"),
            "{error}"
        );
    }
}
