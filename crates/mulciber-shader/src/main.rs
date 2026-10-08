//! Command-line entry point for offline Mulciber shader compilation.

use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use mulciber_shader::{
    ShaderDef, ShaderModules, ShaderTarget, WgslShader, compile_host_field, compile_wgsl,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let command = arguments.next().ok_or(USAGE)?;
    match command.to_str() {
        Some("build") => build(arguments),
        Some("host-field") => host_field(arguments),
        _ => Err(USAGE.into()),
    }
}

/// Module directories or files and shader defs; a source using either, or containing any `#`
/// directive, compiles through the module composer.
#[derive(Default)]
struct Composition {
    modules: Vec<PathBuf>,
    defs: Vec<(String, ShaderDef)>,
}

impl Composition {
    /// Consumes `--modules` and `--define`, returning false for any other argument.
    fn accept(
        &mut self,
        argument: &str,
        arguments: &mut impl Iterator<Item = OsString>,
    ) -> Result<bool, Box<dyn Error>> {
        match argument {
            "--modules" => {
                self.modules.push(PathBuf::from(
                    arguments
                        .next()
                        .ok_or("--modules requires a directory or file")?,
                ));
            }
            "--define" => {
                let value = arguments.next().ok_or("--define requires NAME[=VALUE]")?;
                let value = value.to_str().ok_or("shader def is not UTF-8")?;
                self.defs.push(parse_def(value)?);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The composer's module set, or `None` when the source is plain WGSL.
    fn modules(&self, source: &Path) -> Result<Option<ShaderModules>, Box<dyn Error>> {
        let text = std::fs::read_to_string(source)
            .map_err(|error| format!("read {}: {error}", source.display()))?;
        let directives = text.lines().any(|line| line.trim_start().starts_with('#'));
        if self.modules.is_empty() && self.defs.is_empty() && !directives {
            return Ok(None);
        }
        let mut modules = ShaderModules::new();
        for path in &self.modules {
            if path.is_dir() {
                modules.add_dir(path)?;
            } else {
                modules.add_file(path)?;
            }
        }
        Ok(Some(modules))
    }

    fn shader<'m>(
        &self,
        modules: &'m ShaderModules,
        source: &Path,
    ) -> Result<WgslShader<'m>, Box<dyn Error>> {
        let mut shader = modules.shader(source)?;
        for (name, value) in &self.defs {
            shader = shader.define(name.clone(), *value);
        }
        Ok(shader)
    }
}

/// `NAME` is `true`; `NAME=true|false`, `NAME=-3` (signed) and `NAME=3u` (unsigned) give values.
fn parse_def(spec: &str) -> Result<(String, ShaderDef), Box<dyn Error>> {
    let (name, value) = spec.split_once('=').unwrap_or((spec, "true"));
    if name.is_empty() {
        return Err("--define requires NAME[=VALUE]".into());
    }
    let value = match value {
        "true" => ShaderDef::Bool(true),
        "false" => ShaderDef::Bool(false),
        _ => {
            if let Some(unsigned) = value.strip_suffix('u') {
                ShaderDef::UInt(unsigned.parse().map_err(|_| bad_def(spec))?)
            } else {
                ShaderDef::Int(value.parse().map_err(|_| bad_def(spec))?)
            }
        }
    };
    Ok((name.to_string(), value))
}

fn bad_def(spec: &str) -> String {
    format!("--define {spec}: a value is true, false, an integer, or an unsigned integer like 4u")
}

fn build(mut arguments: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let source = PathBuf::from(arguments.next().ok_or(USAGE)?);
    let mut target = None;
    let mut output = None;
    let mut composition = Composition::default();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--target") => {
                let value = arguments
                    .next()
                    .ok_or("--target requires vulkan or metal")?;
                target = Some(
                    ShaderTarget::parse(value.to_str().ok_or("shader target is not UTF-8")?)
                        .ok_or("--target requires vulkan or metal")?,
                );
            }
            Some("--output") => {
                output = Some(PathBuf::from(
                    arguments.next().ok_or("--output requires a path")?,
                ));
            }
            Some(other) if composition.accept(other, &mut arguments)? => {}
            _ => return Err(USAGE.into()),
        }
    }
    let target = target.ok_or("missing --target")?;
    let output = output.ok_or("missing --output")?;
    match composition.modules(&source)? {
        Some(modules) => composition
            .shader(&modules, &source)?
            .compile_wgsl(output, target)?,
        None => compile_wgsl(source, output, target)?,
    }
    Ok(())
}

fn host_field(mut arguments: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let source = PathBuf::from(arguments.next().ok_or(USAGE)?);
    let mut functions = Vec::new();
    let mut output = None;
    let mut composition = Composition::default();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--function") => {
                let value = arguments.next().ok_or("--function requires a name")?;
                functions.push(
                    value
                        .to_str()
                        .ok_or("function name is not UTF-8")?
                        .to_string(),
                );
            }
            Some("--output") => {
                output = Some(PathBuf::from(
                    arguments.next().ok_or("--output requires a path")?,
                ));
            }
            Some(other) if composition.accept(other, &mut arguments)? => {}
            _ => return Err(USAGE.into()),
        }
    }
    if functions.is_empty() {
        return Err("missing --function".into());
    }
    let output = output.ok_or("missing --output")?;
    let names: Vec<&str> = functions.iter().map(String::as_str).collect();
    let qualified = names.iter().any(|name| name.contains("::"));
    match composition.modules(&source)? {
        Some(modules) => composition
            .shader(&modules, &source)?
            .compile_host_field(output, &names)?,
        None if qualified => composition
            .shader(&ShaderModules::new(), &source)?
            .compile_host_field(output, &names)?,
        None => compile_host_field(source, output, &names)?,
    }
    Ok(())
}

const USAGE: &str = "usage: mulciber-shader build <source.wgsl> --target <vulkan|metal> --output \
                     <artifact> [--modules <dir|file>]... [--define NAME[=VALUE]]...\n       \
                     mulciber-shader host-field <source.wgsl> --function <name> [--function \
                     <name>] --output <generated.rs> [--modules <dir|file>]... [--define \
                     NAME[=VALUE]]...";
