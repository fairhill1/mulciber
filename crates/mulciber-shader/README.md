# mulciber-shader

`mulciber-shader` is Mulciber's offline single-source shader compiler. It converts one WGSL
module, optionally composed from [importable modules](#wgsl-modules-and-imports), into a cached
native Vulkan or Metal artifact; it is a development tool, not an application runtime dependency.

```console
mulciber-shader build src/scene.wgsl --target vulkan --output artifacts/scene.vulkan.shaderbin
mulciber-shader build src/scene.wgsl --target metal --output artifacts/scene.metal.shaderbin
```

Vulkan generation requires `spirv-val` and validates against `vulkan1.3`. Metal generation runs
on macOS and requires Xcode's `metal` and `metallib` tools. The compiler deliberately accepts
only Naga's validation-backed cross-backend feature intersection; unsupported advanced shaders
fail instead of requesting a second user-authored source.

Each artifact (`MULSHDR3` container) records the module's interface — per entry point its
stage, name, vertex-input locations with formats, and the bindings it uses (directly or through
called functions), plus every binding's kind: uniform,
texture, cube texture, sampler, depth texture, depth-texture array, comparison sampler, and
read-only storage with its creation-fixed byte size. The paired `mulciber` crate validates pipeline
declarations against that record. Writable and runtime-sized storage are rejected at compile
time instead of being recorded without a proven mapping. The tool and the `mulciber` crate
ship together; no artifact stability is promised across versions. Pipelines validate against
their own entry points' bindings, so one module can hold, say, a plain and a skinned vertex stage
where only the skinned one reads a storage palette. `mulciber` still reads `MULSHDR2` artifacts,
attributing every binding to every entry point; older `mulciber` releases reject `MULSHDR3`.

## WGSL modules and imports

Shared WGSL lives in importable modules instead of being copied or concatenated into each shader.
A module names itself with `#define_import_path`, whatever its file is called, and a shader or
another module imports it; composition is [naga_oil](https://crates.io/crates/naga_oil)'s, Bevy's
shader composer, on the same Naga version as the compiler.

```wgsl
// shaders/lib/lighting.wgsl
#define_import_path game::lighting

fn falloff(distance: f32) -> f32 {
    return 1.0 / (1.0 + distance * distance);
}
```

```wgsl
// shaders/terrain.wgsl
#import game::lighting
#import mulciber::color::{luminance}

@fragment fn terrain_fragment(surface: Surface) -> @location(0) vec4<f32> {
    let lit = surface.albedo * game::lighting::falloff(surface.distance);
#ifdef DEBUG_LUMINANCE
    return vec4<f32>(vec3<f32>(luminance(lit)), 1.0);
#else
    return vec4<f32>(lit, 1.0);
#endif
}
```

`#import a::b` makes items spelled `a::b::item`; `#import a::b as alias` spells them
`alias::item`; `#import a::b::{item, other}` makes `item` usable unqualified. Shader defs drive
`#ifdef`, `#ifndef`, `#if NAME == value`, `#else` and `#endif`, in the shader and every module it
imports, and `#NAME` substitutes a def's value.

From a `build.rs`:

```rust
use mulciber_shader::{ShaderModules, ShaderTarget};

fn main() {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let target = match std::env::var("CARGO_CFG_TARGET_OS").unwrap().as_str() {
        "macos" => ShaderTarget::Metal,
        _ => ShaderTarget::Vulkan,
    };

    let mut modules = ShaderModules::new(); // already holds mulciber::color
    modules.add_dir("shaders/lib").expect("register WGSL modules");
    modules.rerun_if_changed(); // cargo::rerun-if-changed=shaders/lib
    println!("cargo::rerun-if-changed=shaders/terrain.wgsl");

    let terrain = modules.shader("shaders/terrain.wgsl").expect("read terrain.wgsl");
    terrain
        .compile_wgsl(out.join("terrain.shaderbin"), target)
        .expect("compile terrain");
    terrain
        .clone()
        .define("DEBUG_LUMINANCE", true)
        .compile_wgsl(out.join("terrain-luminance.shaderbin"), target)
        .expect("compile the luminance view");

    // The CPU lightmap baker calls the same lighting maths the shaders import.
    modules
        .compile_host_field(out.join("lighting_host.rs"), &["game::lighting::falloff"])
        .expect("generate the lighting host field");
}
```

- `ShaderModules::new()` holds Mulciber's own modules. `add_source(label, text)` registers one
  module from a string, `add_file(path)` from a file, and `add_dir(dir)` every `.wgsl` file under a
  directory that declares `#define_import_path`; files without it are top-level shaders and are
  skipped, so modules and shaders can share a directory. Registration order does not matter, and
  only the modules a shader reaches are composed. `rerun_if_changed()` prints
  `cargo::rerun-if-changed` for every file and directory given to `add_file` and `add_dir` (Cargo
  rescans a directory's whole tree, so a new module also triggers a rebuild).
- `modules.shader(path)` or `modules.shader_source(label, text)` gives a `WgslShader`.
  `.define(name, value)` sets a def (`bool`, `i32` or `u32`). `.compile_wgsl(artifact, target)`
  writes the same `MULSHDR3` artifact and runs the same validation and native tools as
  `compile_wgsl`. `.compile_host_field(generated, functions)` generates host evaluators like
  `compile_host_field`.
- `WgslShader::cache_key()` returns text covering the crate version, the defs, the shader and every
  module it can reach through `#import`; hash it to key a build cache. `ShaderModules::cache_key()`
  covers every registered module.

The plain `compile_wgsl` and `compile_host_field` functions are unchanged. They do not run the
composer, so a source with `#import` must go through `ShaderModules`. The CLI composes when it is
given `--modules <dir|file>` or `--define NAME[=VALUE]`, or when the source has any `#` directive
line:

```console
mulciber-shader build shaders/terrain.wgsl --modules shaders/lib --define DEBUG_LUMINANCE \
    --target vulkan --output artifacts/terrain-luminance.vulkan.shaderbin
mulciber-shader host-field shaders/terrain.wgsl --modules shaders/lib \
    --function game::lighting::falloff --output src/lighting_host.rs
```

`--define NAME` sets `true`; `NAME=false`, `NAME=-3` and `NAME=3u` give a boolean, signed and
unsigned value.

### Engine modules

Modules under `mulciber::` ship inside `mulciber-shader`: their WGSL is compiled into the crate
(`wgsl/mulciber/`) and registered by `ShaderModules::new()`, so every game can import them without
registering anything. The namespace is reserved: registering a module whose path is `mulciber` or
starts with `mulciber::` is an error, so a game cannot shadow one.

| Module | Items |
| --- | --- |
| `mulciber::color` | `srgb_to_linear`, `linear_to_srgb` (vec3, piecewise IEC 61966-2-1 curve) and their `_channel` scalar forms; `luminance` (BT.709 weights over linear Rec. 709 colour) |

### Composition rules and limits

- Errors point at the file and line where they occur, in the shader or in an imported module: a
  parse error or a type error inside `game::lighting` is reported against `shaders/lib/lighting.wgsl`
  with its own line. A missing module is reported at the `#import`ed name's first use and lists the
  registered modules. Import cycles are refused by name.
- The composer copies only the module items a shader uses. An `#import` whose module is never used
  is ignored, even when that module is not registered.
- `#import a::b` also makes `b` alone name the module in that file, so a variable, parameter or
  function called `b` is read as the module. With `#import mulciber::color`, a local `color` fails
  with "required import 'mulciber' not found" and a hint naming the clash. Import the items
  (`#import mulciber::color::{luminance}`) or use `as` instead.
- Entry points come from the top-level shader only; entry points inside imported modules become
  ordinary functions. The shader's own names, entry points included, are kept, so pipelines name
  entry points as before. Items from imported modules are renamed with an encoded module path
  (`luminanceX_naga_oil_mod_X…X`), which shows in generated MSL and SPIR-V debug names only.
- A binding declared in an imported module belongs to the composed shader when the shader (or a
  module it imports) uses an item that reaches it. The interface records it like any other binding,
  and an entry point that does not reach it does not list it, so pipelines declare it only where it
  is used. As in one file, an entry point that reaches two bindings with the same group and binding
fails validation.
- The composer rebuilds the module, so type order and SPIR-V ids may differ from a direct parse of
  the same text; the recorded interface does not.

### Host fields from modules

`WgslShader::compile_host_field` accepts plain names for functions of the top-level shader and
qualified names (`game::lighting::falloff`) for functions of any registered module, imported by the
shader or not. A requested module function's `pub fn` takes its plain item name (`falloff`). The
functions it calls are generated privately, named after their module
(`mulciber_color_srgb_to_linear_channel`). Two functions that would get the same Rust name are
refused. `ShaderModules::compile_host_field` does the same without a top-level shader and takes
qualified names only. The accepted subset is the one described below.

## Host-evaluable fields

A game whose simulation needs an answer the shader already computes — the height of a displaced
terrain surface in some direction, say — can generate a host evaluator from the same WGSL instead of
maintaining a second copy in Rust:

```console
mulciber-shader host-field src/field.wgsl --function surface_height --output src/field_host.rs
```

or, keeping the two permanently in step, from a `build.rs`:

```rust
mulciber_shader::compile_host_field(
    "src/field.wgsl",
    std::path::Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("field_host.rs"),
    &["surface_height"],
)
.expect("generate the host field");
```

Each named WGSL function becomes a `pub fn` with its WGSL argument names, `f32`/`i32`/`u32`/`bool`
scalars, `[f32; N]`-shaped vectors, and fixed-size arrays; the functions it calls are generated
privately beside it. Include the file in a module of its own, because the generated helper names are
file-local. The host answer is an ordinary synchronous call with no device involvement.

The accepted subset is pure arithmetic. Bindings, textures, derivatives, atomics, barriers,
workgroup memory, switch statements, and matrices have no host meaning and are refused rather than
approximated. WGSL semantics that differ from Rust's nearest spelling are generated as the shader
computes them: `fract` is `x - floor(x)`, `sign(0.0)` is `0.0`, `round` breaks ties to even,
integer arithmetic wraps, `mix` is `x * (1 - a) + y * a`, and an out-of-range index is clamped the
way Naga's bounds-check policy clamps it. Two differences remain by nature: transcendental
functions differ from a device by its documented precision, and integer division by zero panics on
the host where a GPU leaves it undefined. `mulciber-vulkan-triangle` measures the first of those
against a real device — see the [Linux runbook](../../docs/linux-validation.md).

The reflected interface also distinguishes single-sample and multisampled 2D depth textures.
`texture_depth_multisampled_2d` artifacts require Mulciber 0.13.6 or newer; older runtimes
reject this binding kind rather than treating it as a single-sample texture.

`texture_cube<f32>` records its own binding kind, which `MaterialBinding::CubeTexture` declares.
Mulciber releases before cube textures reject artifacts that contain it rather than treating it as a
2D texture. Arrayed cubes, depth cubes and integer cubes have no proven mapping and fail to compile.
