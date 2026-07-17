//! Command-line entry point for offline Mulciber shader compilation.

use std::env;
use std::error::Error;
use std::path::PathBuf;

use mulciber_shader::{ShaderTarget, compile_wgsl};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let command = arguments.next().ok_or(USAGE)?;
    if command != "build" {
        return Err(USAGE.into());
    }
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

const USAGE: &str =
    "usage: mulciber-shader build <source.wgsl> --target <vulkan|metal> --output <artifact>";
