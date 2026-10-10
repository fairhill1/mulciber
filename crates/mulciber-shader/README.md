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

Each artifact (`MULSHDR4` container) records the module's interface — per entry point its
stage, name, vertex-input locations with formats, and the bindings it uses (directly or through
called functions), plus every binding's kind: uniform,
texture, cube texture, cube texture array, sampler, depth texture, depth-texture array, comparison
sampler, and
read-only storage with its creation-fixed byte size. Every uniform and storage binding also
records its memory layout: the WGSL type name and, for a struct, each member's name, byte offset,
size and type. The paired `mulciber` crate validates pipeline
declarations against that record. Writable and runtime-sized storage are rejected at compile
time instead of being recorded without a proven mapping. The tool and the `mulciber` crate
ship together; no artifact stability is promised across versions. Pipelines validate against
their own entry points' bindings, so one module can hold, say, a plain and a skinned vertex stage
where only the skinned one reads a storage palette. `mulciber` still reads `MULSHDR3` artifacts
(without layouts) and `MULSHDR2` artifacts (attributing every binding to every entry point); older
`mulciber` releases reject `MULSHDR4`. `ShaderArtifact::reflect` exposes the whole record, and
`MaterialPipelineDescriptor::validate` checks a declaration against it, both without a device.

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
#import mulciber::colorspace::{luminance}

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

    let mut modules = ShaderModules::new(); // already holds mulciber::colorspace
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
| `mulciber::colorspace` | `srgb_to_linear`, `linear_to_srgb` (vec3, piecewise IEC 61966-2-1 curve) and their `_channel` scalar forms; `luminance` (BT.709 weights over linear Rec. 709 colour) |
| `mulciber::photometry` | light units, falloff and exposure; see [the lighting library](#the-lighting-library) |
| `mulciber::pbr` | the BRDF, punctual lights and split-sum environment specular |
| `mulciber::tonemap` | `hue_preserving_shoulder` |

The module names end in segments that are unlikely local variable names, because of the alias rule
under [composition rules](#composition-rules-and-limits).

### The lighting library

`mulciber::photometry`, `mulciber::pbr` and `mulciber::tonemap` are one shading model shared by
every game: Filament's units and BRDF, and Isle of Rán's tone-mapping shoulder.

**Units** are SI and photometric, and distances are in metres:

| Quantity | Unit | Authored for |
| --- | --- | --- |
| luminous flux Φ | lumen (lm) | lamps (a 60 W incandescent bulb is about 800 lm) |
| luminous intensity I | candela (cd = lm/sr) | point and spot lights as shaded; spots may be authored in it |
| illuminance E | lux (lx = lm/m²) | the sun and other directional lights |
| luminance L | nit (cd/m²) | skies, emissive surfaces, calibrated HDRIs, and every shaded value |

Colours are linear and scale these per channel. A BRDF in 1/sr times illuminance in lux is
luminance in nits.

`mulciber::photometry`:

- `point_light_intensity(lumens)` is Φ/4π. `spot_light_intensity(lumens)` is Φ/π whatever the
  cone, Filament's default spot: narrowing the cone darkens the lit area rather than concentrating
  the light, so artists can change the angle without changing brightness.
  `focused_spot_light_intensity(lumens, cos_outer)` is Φ/(2π(1 − cos θ_outer)), the physically
  correct spot whose light concentrates as the cone narrows.
- `punctual_illuminance(intensity, distance_squared, range)` is Filament's falloff,
  `E = I / max(d², 0.01²) · saturate(1 − (d/r)⁴)²`: inverse square, with a window that reaches zero
  with zero slope at the range r. `distance_attenuation` and `range_window` are its parts, and
  `spot_angle_attenuation(cos_angle, cos_inner, cos_outer)` is Filament's squared cone falloff.
  Generate them into the CPU lightmap baker with `compile_host_field` so baked and dynamic lights
  cannot drift apart.
- `exposure_from_ev100(ev100)` is `1 / (1.2 · 2^EV100)`. `ev100_from_camera(aperture,
  shutter_seconds, iso)` and `ev100_from_luminance(average_luminance)` (meter constant K = 12.5)
  give EV100. `pre_expose` and `pre_expose_intensity` multiply by the exposure: apply it to light
  intensities and environment luminance before shading, as Filament does, so values stay small in
  16-bit targets.

`mulciber::pbr` keeps the π in the BRDF and works in the metallic workflow, with perceptual
roughness as authored:

- `lambert(diffuse_color)` is albedo/π. `d_ggx(n_dot_h, alpha)` is GGX.
  `v_smith_ggx_correlated(n_dot_v, n_dot_l, alpha)` is the exact height-correlated Smith visibility
  (G / 4 n·v n·l). `f_schlick(f0, f90, v_dot_h)` is Schlick's Fresnel. `specular_brdf` is
  D · V · F with f90 = 1.
- `f0_from_metallic` is `mix(0.04, base_color, metallic)`, and `diffuse_color_from_metallic` is
  `base_color · (1 − metallic)`. `alpha_from_perceptual_roughness` squares the perceptual roughness
  after `clamp_perceptual_roughness` holds it in [0.089, 1], so α² stays above fp16's smallest
  normal. `clamped_n_dot_v` keeps n·v above 1e-4.
- `punctual_light(n, v, l, illuminance, base_color, metallic, perceptual_roughness)` returns a
  `PunctualLighting { diffuse, specular }`: the luminance BRDF · E · n·l, with the two kept apart
  so a caller can occlude, weight or compensate them separately. `illuminance` is the light's
  colour times its (pre-exposed) illuminance in lux. `punctual_diffuse` and `punctual_specular`
  are the halves.
- Environment specular uses the split sum. `sample_dfg(table, sampler, n_dot_v,
  perceptual_roughness)` reads the DFG table; `specular_dfg(f0, dfg)` is `mix(dfg.r, dfg.g, F0)`;
  `energy_compensation(f0, dfg)` is Filament's `1 + F0 (1/dfg.g − 1)`; `environment_specular(
  prefiltered, f0, dfg)` multiplies all three. `lod_from_roughness(perceptual_roughness, max_lod)`
  is `max_lod · perceptual_roughness`, for a cubemap whose level k was prefiltered for roughness
  k / max_lod.

`mulciber::tonemap::hue_preserving_shoulder(luminance)` takes exposed luminance to display-linear
values below 1. A colour whose brightest channel is at most 0.6 passes through unchanged. Above
that the brightest channel p maps to `0.6 + 0.4 (p − 0.6) / (p − 0.2)` and the whole colour scales
with it, so hue and saturation are kept: 1 becomes 0.8, 2 becomes 0.911, 10 becomes 0.984.

A typical lit fragment shader:

```wgsl
#import mulciber::photometry
#import mulciber::pbr
#import mulciber::tonemap

struct Frame {
    camera_position: vec3<f32>,
    // photometry::exposure_from_ev100(ev100), computed on the CPU.
    exposure: f32,
    light_position: vec3<f32>,
    // Candelas: photometry::point_light_intensity(lumens), computed on the CPU.
    light_intensity: f32,
    light_color: vec3<f32>,
    light_range: f32,
    environment_max_lod: f32,
}

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var albedo_map: texture_2d<f32>;
@group(0) @binding(2) var material_sampler: sampler;
@group(0) @binding(3) var dfg_table: texture_2d<f32>;
@group(0) @binding(4) var dfg_sampler: sampler;
@group(0) @binding(5) var environment: texture_cube<f32>;

struct Surface {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
}

@fragment fn lit_fragment(surface: Surface) -> @location(0) vec4<f32> {
    let base_color = textureSample(albedo_map, material_sampler, surface.uv).rgb;
    let metallic = 0.0;
    let roughness = mulciber::pbr::clamp_perceptual_roughness(0.5);

    let n = normalize(surface.normal);
    let v = normalize(frame.camera_position - surface.world_position);
    let to_light = frame.light_position - surface.world_position;
    let l = normalize(to_light);

    // Lux at the surface, pre-exposed so values stay small in a 16-bit target.
    let illuminance = frame.light_color * mulciber::photometry::punctual_illuminance(
        frame.light_intensity * frame.exposure,
        dot(to_light, to_light),
        frame.light_range,
    );

    let n_dot_v = mulciber::pbr::clamped_n_dot_v(n, v);
    let dfg = mulciber::pbr::sample_dfg(dfg_table, dfg_sampler, n_dot_v, roughness);
    let f0 = mulciber::pbr::f0_from_metallic(base_color, metallic);
    let lit = mulciber::pbr::punctual_light(n, v, l, illuminance, base_color, metallic, roughness);
    var luminance = lit.diffuse + lit.specular * mulciber::pbr::energy_compensation(f0, dfg);

    // The environment is stored in nits; expose it like the light.
    let lod = mulciber::pbr::lod_from_roughness(roughness, frame.environment_max_lod);
    let prefiltered = textureSampleLevel(environment, material_sampler, reflect(-v, n), lod).rgb;
    luminance += mulciber::pbr::environment_specular(prefiltered * frame.exposure, f0, dfg);

    return vec4<f32>(mulciber::tonemap::hue_preserving_shoulder(luminance), 1.0);
}
```

**The DFG table.** `bake_dfg_table(size, samples)` bakes Filament's multiple-scattering table on
the CPU: texel (i, j) holds, at n·v = (i + ½)/size and perceptual roughness (j + ½)/size, R = ∫ Fc ·
D · V · n·l and G = ∫ D · V · n·l (Fc = (1 − v·h)⁵), from GGX importance sampling over a Hammersley
set, so the bake is deterministic. Filament uses `DFG_TABLE_SIZE` (128) and `DFG_SAMPLE_COUNT`
(1024); the bake took 0.7 s unoptimised. Bake it in `build.rs` and upload it as RGBA16Float:

```rust
// build.rs
let table = mulciber_shader::bake_dfg_table(
    mulciber_shader::DFG_TABLE_SIZE,
    mulciber_shader::DFG_SAMPLE_COUNT,
);
std::fs::write(out.join("dfg.bin"), table.to_le_bytes()).unwrap();

// The game: R, G as little-endian f32 pairs, row-major.
let texels: Vec<[f32; 4]> = include_bytes!(concat!(env!("OUT_DIR"), "/dfg.bin"))
    .chunks_exact(8)
    .map(|pair| {
        let r = f32::from_le_bytes(pair[..4].try_into().unwrap());
        let g = f32::from_le_bytes(pair[4..].try_into().unwrap());
        [r, g, 0.0, 1.0]
    })
    .collect();
let dfg = device.create_rgba16_float_texture(128, 128, &texels)?;
```

Sample it with a linear, clamp-to-edge sampler. `DfgTable::rgba()` gives the same texels directly
where the table is baked in the same process.

The library's tests run on host evaluators generated from these modules, so they measure the WGSL
that shaders import: GGX normalisation, reciprocity, a white furnace whose quadrature matches the
baked DFG table, the table's smooth-surface limit, falloff, exposure and the tone mapper's shape.

### Composition rules and limits

- Errors point at the file and line where they occur, in the shader or in an imported module: a
  parse error or a type error inside `game::lighting` is reported against `shaders/lib/lighting.wgsl`
  with its own line. A missing module is reported at the `#import`ed name's first use and lists the
  registered modules. Import cycles are refused by name.
- The composer copies only the module items a shader uses. An `#import` whose module is never used
  is ignored, even when that module is not registered.
- `#import a::b` also makes `b` alone name the module in that file, so a variable, parameter or
  function called `b` is read as the module. With `#import game::shadow`, a local `shadow` fails
  with "required import 'game' not found" and a hint naming the clash. Import the items
  (`#import game::shadow::{shadow_factor}`) or use `as` instead, and give modules last segments
  that are unlikely local names: Mulciber's are `colorspace`, `photometry`, `pbr` and `tonemap`.
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
(`mulciber_colorspace_srgb_to_linear_channel`). Two functions that would get the same Rust name are
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
2D texture. `texture_cube_array<f32>` (since 0.5.5) records another, which
`MaterialBinding::CubeTextureArray` declares and Mulciber 0.16.0 or newer reads; older runtimes
reject the artifact. The compiler validates with Naga's `CUBE_ARRAY_TEXTURES` capability, part of
WebGPU's core profile, so a module or an imported module may declare one and sample it with
`textureSampleLevel(map, sampler, direction, layer, lod)`; its SPIR-V declares `SampledCubeArray`,
which the Vulkan runtime requires the device's `imageCubeArray` feature for. Depth cubes, depth cube
arrays and integer cubes have no proven mapping and fail to compile.
