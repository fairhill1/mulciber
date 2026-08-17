//! Command-line entry point for offline Mulciber shader compilation.

use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::path::PathBuf;

use mulciber_shader::{ShaderTarget, compile_host_field, compile_wgsl};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let command = arguments.next().ok_or(USAGE)?;
    match command.to_str() {
        Some("build") => build(arguments),
        Some("host-field") => host_field(arguments),
        _ => Err(USAGE.into()),
    }
}

fn build(mut arguments: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let source = PathBuf::from(arguments.next().ok_or(USAGE)?);
    let mut target = None;
    let mut output = None;
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
            _ => return Err(USAGE.into()),
        }
    }
    let target = target.ok_or("missing --target")?;
    let output = output.ok_or("missing --output")?;
    compile_wgsl(source, output, target)?;
    Ok(())
}

fn host_field(mut arguments: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let source = PathBuf::from(arguments.next().ok_or(USAGE)?);
    let mut functions = Vec::new();
    let mut output = None;
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
            _ => return Err(USAGE.into()),
        }
    }
    if functions.is_empty() {
        return Err("missing --function".into());
    }
    let output = output.ok_or("missing --output")?;
    let names: Vec<&str> = functions.iter().map(String::as_str).collect();
    compile_host_field(source, output, &names)?;
    Ok(())
}

const USAGE: &str = "usage: mulciber-shader build <source.wgsl> --target <vulkan|metal> --output \
                     <artifact>\n       mulciber-shader host-field <source.wgsl> --function <name> \
                     [--function <name>] --output <generated.rs>";
