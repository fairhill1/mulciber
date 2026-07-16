//! Offline shader toolchain evaluation harness.
//!
//! Compiles a representative WGSL corpus through Naga (as a library) and a matching Slang
//! corpus through `slangc`, validates every emitted SPIR-V module with `spirv-val` against
//! the pinned `vulkan1.4` target environment, and records a machine-readable findings
//! report under `validation-artifacts/shader-toolchain/`.
//!
//! Per-scenario compilation or validation failures are findings, not harness errors: the
//! run still succeeds and the failure text is preserved in the report.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use naga::valid::Capabilities;

/// SPIR-V consumption environment shared with `vulkan-toolchain.lock.toml`.
const SPIRV_TARGET_ENV: &str = "vulkan1.4";
/// SPIR-V version requested from the Naga backend.
///
/// Naga shares layout-decorated struct and array types between buffer and Function or
/// Workgroup storage classes. SPIR-V 1.5 removed the allowance for `Offset` and
/// `ArrayStride` decorations reaching those storage classes, so Naga output validates
/// only at SPIR-V <= 1.4. Version 1.4 is still high enough for `SPV_EXT_mesh_shader`
/// and `SPV_KHR_ray_tracing`, and Vulkan 1.4 consumes any SPIR-V version up to 1.6.
const NAGA_SPIRV_LANG_VERSION: (u8, u8) = (1, 4);

/// One corpus scenario, present as both `shaders/wgsl/<name>.wgsl` and
/// `shaders/slang/<name>.slang`.
struct Scenario {
    name: &'static str,
    milestone: &'static str,
    summary: &'static str,
    naga_capabilities: Capabilities,
    /// `(group, binding, binding_array_size)` entries for Naga's SPIR-V binding map.
    ///
    /// Naga's SPIR-V backend cannot emit `RuntimeDescriptorArray`; an unsized
    /// `binding_array` compiles only when the backend binding map rewrites it to a fixed
    /// size. When this is non-empty, every resource binding in the module must be listed,
    /// because a partial binding map is an error.
    naga_binding_map: &'static [(u32, u32, Option<u32>)],
}

/// The representative corpus: milestone 2 workload shapes plus milestone 4 capabilities.
fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "scene",
            milestone: "2",
            summary: "uniform-driven textured vertex/fragment pair",
            naga_capabilities: Capabilities::empty(),
            naga_binding_map: &[],
        },
        Scenario {
            name: "compute_storage",
            milestone: "2",
            summary: "compute with storage buffer, storage image, workgroup barrier",
            naga_capabilities: Capabilities::empty(),
            naga_binding_map: &[],
        },
        Scenario {
            name: "indirect_args",
            milestone: "2",
            summary: "compute-written indexed-indirect draw arguments",
            naga_capabilities: Capabilities::empty(),
            naga_binding_map: &[],
        },
        Scenario {
            name: "bindless",
            milestone: "4",
            summary: "binding arrays with uniform and non-uniform indexing",
            naga_capabilities: Capabilities::TEXTURE_AND_SAMPLER_BINDING_ARRAY
                .union(Capabilities::TEXTURE_AND_SAMPLER_BINDING_ARRAY_NON_UNIFORM_INDEXING),
            naga_binding_map: &[(0, 0, Some(64)), (0, 1, None), (0, 2, None)],
        },
        Scenario {
            name: "ray_query",
            milestone: "4",
            summary: "inline ray query from compute",
            naga_capabilities: Capabilities::RAY_QUERY,
            naga_binding_map: &[],
        },
        Scenario {
            name: "ray_pipeline",
            milestone: "4",
            summary: "ray generation, miss, any-hit, closest-hit stages",
            naga_capabilities: Capabilities::RAY_TRACING_PIPELINE,
            naga_binding_map: &[],
        },
        Scenario {
            name: "mesh",
            milestone: "4",
            summary: "task + mesh shading pipeline",
            naga_capabilities: Capabilities::MESH_SHADER,
            naga_binding_map: &[],
        },
    ]
}

/// Outcome of compiling and validating one scenario through one toolchain.
struct CaseResult {
    toolchain: &'static str,
    scenario: &'static str,
    milestone: &'static str,
    summary: &'static str,
    source: String,
    compiled: bool,
    diagnostics: String,
    spirv_bytes: usize,
    validated: bool,
    validation_output: String,
    spirv_version: String,
    entry_points: Vec<String>,
    capabilities: Vec<String>,
    extensions: Vec<String>,
}

impl CaseResult {
    fn new(toolchain: &'static str, scenario: &Scenario, source: String) -> Self {
        Self {
            toolchain,
            scenario: scenario.name,
            milestone: scenario.milestone,
            summary: scenario.summary,
            source,
            compiled: false,
            diagnostics: String::new(),
            spirv_bytes: 0,
            validated: false,
            validation_output: String::new(),
            spirv_version: String::new(),
            entry_points: Vec::new(),
            capabilities: Vec::new(),
            extensions: Vec::new(),
        }
    }
}

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("tools/shader-toolchain should sit two levels under the repository root")
        .to_path_buf();
    let corpus_dir = manifest_dir.join("shaders");
    let out_dir = repo_root.join("validation-artifacts/shader-toolchain");
    fs::create_dir_all(&out_dir).expect("could not create the output directory");

    let slangc = env::var_os("SLANGC").map_or_else(|| PathBuf::from("slangc"), PathBuf::from);
    let slangc_version = tool_version(&slangc, &["-v"])
        .expect("slangc is required; install it or point the SLANGC environment variable at it");
    let spirv_val_version = tool_version(Path::new("spirv-val"), &["--version"])
        .expect("spirv-val from SPIRV-Tools is required on PATH");
    let naga_version = naga_version(&manifest_dir);

    let mut results = Vec::new();
    for scenario in scenarios() {
        results.push(run_naga_case(&scenario, &corpus_dir, &out_dir));
        results.push(run_slang_case(&scenario, &slangc, &corpus_dir, &out_dir));
    }

    let report = serde_json::json!({
        "target_environment": SPIRV_TARGET_ENV,
        "toolchains": {
            "naga": {
                "crate_version": naga_version,
                "spirv_lang_version": format!(
                    "{}.{}",
                    NAGA_SPIRV_LANG_VERSION.0, NAGA_SPIRV_LANG_VERSION.1
                ),
                "notes": "library front::wgsl + back::spv, default writer options otherwise",
            },
            "slangc": {
                "version": slangc_version.trim(),
                "invocation": "slangc <source> -target spirv -fvk-use-entrypoint-name -o <output>",
            },
            "spirv_val": { "version": spirv_val_version.lines().next().unwrap_or("") },
        },
        "cases": results.iter().map(case_json).collect::<Vec<_>>(),
    });
    let report_path = out_dir.join("report.json");
    fs::write(
        &report_path,
        serde_json::to_string_pretty(&report).expect("report must serialize") + "\n",
    )
    .expect("could not write the report");

    print_summary(&results, &report_path);
}

fn case_json(case: &CaseResult) -> serde_json::Value {
    serde_json::json!({
        "toolchain": case.toolchain,
        "scenario": case.scenario,
        "milestone": case.milestone,
        "summary": case.summary,
        "source": case.source,
        "compiled": case.compiled,
        "diagnostics": case.diagnostics,
        "spirv_bytes": case.spirv_bytes,
        "spirv_valid": case.validated,
        "validation_output": case.validation_output,
        "spirv_version": case.spirv_version,
        "entry_points": case.entry_points,
        "spirv_capabilities": case.capabilities,
        "spirv_extensions": case.extensions,
    })
}

/// Compiles one WGSL scenario through the Naga library and inspects the SPIR-V.
fn run_naga_case(scenario: &Scenario, corpus_dir: &Path, out_dir: &Path) -> CaseResult {
    let relative = format!("shaders/wgsl/{}.wgsl", scenario.name);
    let mut case = CaseResult::new("naga", scenario, relative.clone());
    let source_path = corpus_dir.join(format!("wgsl/{}.wgsl", scenario.name));
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", source_path.display()));

    let module = match naga::front::wgsl::parse_str(&source) {
        Ok(module) => module,
        Err(error) => {
            case.diagnostics = format!("parse: {}", error.emit_to_string(&source));
            return case;
        }
    };

    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        scenario.naga_capabilities,
    );
    let info = match validator.validate(&module) {
        Ok(info) => info,
        Err(error) => {
            case.diagnostics = format!("validate: {}", error.emit_to_string(&source));
            return case;
        }
    };

    let mut binding_map = naga::back::spv::BindingMap::default();
    for &(group, binding, binding_array_size) in scenario.naga_binding_map {
        binding_map.insert(
            naga::ResourceBinding { group, binding },
            naga::back::spv::BindingInfo {
                descriptor_set: group,
                binding,
                binding_array_size,
            },
        );
    }
    let options = naga::back::spv::Options {
        lang_version: NAGA_SPIRV_LANG_VERSION,
        binding_map,
        ..naga::back::spv::Options::default()
    };
    let words = match naga::back::spv::write_vec(&module, &info, &options, None) {
        Ok(words) => words,
        Err(error) => {
            case.diagnostics = format!("spv backend: {error}");
            return case;
        }
    };

    let mut bytes = Vec::with_capacity(words.len() * 4);
    for word in &words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    finish_case(&mut case, &bytes, out_dir);
    case
}

/// Compiles one Slang scenario through `slangc` and inspects the SPIR-V.
fn run_slang_case(
    scenario: &Scenario,
    slangc: &Path,
    corpus_dir: &Path,
    out_dir: &Path,
) -> CaseResult {
    let relative = format!("shaders/slang/{}.slang", scenario.name);
    let mut case = CaseResult::new("slangc", scenario, relative);
    let source_path = corpus_dir.join(format!("slang/{}.slang", scenario.name));
    let spv_path = out_dir.join(format!("slangc-{}.spv", scenario.name));
    let _ = fs::remove_file(&spv_path);

    let output = Command::new(slangc)
        .arg(&source_path)
        .args(["-target", "spirv", "-fvk-use-entrypoint-name", "-o"])
        .arg(&spv_path)
        .output()
        .expect("could not run slangc");
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        case.diagnostics = stderr.trim().to_string();
    }
    if !output.status.success() {
        return case;
    }
    let Ok(bytes) = fs::read(&spv_path) else {
        case.diagnostics += "\nslangc reported success but wrote no output";
        return case;
    };
    finish_case(&mut case, &bytes, out_dir);
    case
}

/// Persists the SPIR-V blob, then records validation and disassembly facts on the case.
fn finish_case(case: &mut CaseResult, bytes: &[u8], out_dir: &Path) {
    case.compiled = true;
    case.spirv_bytes = bytes.len();
    let spv_path = out_dir.join(format!("{}-{}.spv", case.toolchain, case.scenario));
    fs::write(&spv_path, bytes)
        .unwrap_or_else(|error| panic!("could not write {}: {error}", spv_path.display()));

    let validation = Command::new("spirv-val")
        .args(["--target-env", SPIRV_TARGET_ENV])
        .arg(&spv_path)
        .output()
        .expect("could not run spirv-val");
    case.validated = validation.status.success();
    let mut validation_text = String::from_utf8_lossy(&validation.stdout)
        .trim()
        .to_string();
    let validation_stderr = String::from_utf8_lossy(&validation.stderr)
        .trim()
        .to_string();
    if !validation_stderr.is_empty() {
        if !validation_text.is_empty() {
            validation_text.push('\n');
        }
        validation_text.push_str(&validation_stderr);
    }
    case.validation_output = validation_text;

    let disassembly = Command::new("spirv-dis")
        .arg(&spv_path)
        .output()
        .expect("could not run spirv-dis");
    for line in String::from_utf8_lossy(&disassembly.stdout).lines() {
        let line = line.trim();
        if let Some(version) = line.strip_prefix("; Version: ") {
            case.spirv_version = version.to_string();
        } else if let Some(rest) = line.strip_prefix("OpEntryPoint ") {
            let mut parts = rest.split_whitespace();
            let stage = parts.next().unwrap_or("?");
            let name = rest.split('"').nth(1).unwrap_or("?");
            case.entry_points.push(format!("{stage}:{name}"));
        } else if let Some(capability) = line.strip_prefix("OpCapability ") {
            case.capabilities.push(capability.to_string());
        } else if let Some(extension) = line.strip_prefix("OpExtension ") {
            case.extensions
                .push(extension.trim_matches('"').to_string());
        }
    }
}

/// Returns a tool's version banner, or `None` when the tool cannot be launched.
fn tool_version(tool: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new(tool).args(arguments).output().ok()?;
    let text = if output.stdout.is_empty() {
        output.stderr
    } else {
        output.stdout
    };
    Some(String::from_utf8_lossy(&text).into_owned())
}

/// Reads the locked `naga` version out of this workspace's `Cargo.lock`.
fn naga_version(manifest_dir: &Path) -> String {
    let lock = fs::read_to_string(manifest_dir.join("Cargo.lock"))
        .expect("Cargo.lock should exist next to Cargo.toml");
    let mut in_naga = false;
    for line in lock.lines() {
        if line == "name = \"naga\"" {
            in_naga = true;
        } else if in_naga && let Some(version) = line.strip_prefix("version = ") {
            return version.trim_matches('"').to_string();
        }
    }
    panic!("could not find the locked naga version");
}

fn print_summary(results: &[CaseResult], report_path: &Path) {
    println!(
        "shader toolchain evaluation against {SPIRV_TARGET_ENV} ({} cases)",
        results.len()
    );
    for case in results {
        let status = if case.compiled && case.validated {
            "ok"
        } else if case.compiled {
            "INVALID"
        } else {
            "FAILED"
        };
        println!(
            "  {status:<7} {:<7} {:<16} milestone {} [{}] {}",
            case.toolchain,
            case.scenario,
            case.milestone,
            if case.spirv_version.is_empty() {
                "-"
            } else {
                &case.spirv_version
            },
            case.entry_points.join(", "),
        );
        if !case.compiled {
            for line in case.diagnostics.lines().take(4) {
                println!("          {line}");
            }
        } else if !case.validated {
            for line in case.validation_output.lines().take(4) {
                println!("          {line}");
            }
        }
    }
    let failed = results
        .iter()
        .filter(|case| !(case.compiled && case.validated))
        .count();
    println!(
        "{} of {} cases passed; report: {}",
        results.len() - failed,
        results.len(),
        report_path.display()
    );
}
