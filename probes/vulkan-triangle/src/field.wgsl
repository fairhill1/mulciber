// One definition of a planetary surface height field, evaluated on both sides of the probe.
//
// The compute entry point evaluates it on the GPU for every sample direction, and the probe's
// host code evaluates the same two functions through the Rust evaluator that `mulciber-shader`
// generates from this file. Nothing in this file is duplicated in Rust by hand.

const SAMPLE_COUNT: u32 = 256u;
const OCTAVE_COUNT: u32 = 5u;
const BASE_AMPLITUDE: f32 = 120.0;
const LATTICE_SCALE: f32 = 3.0;
const RIDGE_AMPLITUDE: f32 = 90.0;

// Hashes one integer lattice corner into the unit interval.
fn lattice_value(cell: vec3<i32>) -> f32 {
    var state = u32(cell.x) * 0x9e3779b9u;
    state = state ^ (u32(cell.y) * 0x85ebca6bu);
    state = state ^ (u32(cell.z) * 0xc2b2ae35u);
    state = state ^ (state >> 15u);
    state = state * 0x2c1b3c6du;
    state = state ^ (state >> 12u);
    state = state * 0x297a2d39u;
    state = state ^ (state >> 15u);
    return f32(state & 0x00ffffffu) / 16777215.0;
}

// Smooth value noise over the integer lattice, in the range [-1, 1].
fn value_noise(point: vec3<f32>) -> f32 {
    let base = floor(point);
    let cell = vec3<i32>(base);
    let offset = point - base;
    let weight = offset * offset * (3.0 - 2.0 * offset);
    var accumulated = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let step = vec3<i32>(
            i32(corner & 1u),
            i32((corner >> 1u) & 1u),
            i32((corner >> 2u) & 1u),
        );
        let blend = mix(1.0 - weight, weight, vec3<f32>(step));
        accumulated = accumulated + lattice_value(cell + step) * blend.x * blend.y * blend.z;
    }
    return accumulated * 2.0 - 1.0;
}

// Surface height above the reference sphere, in metres, for a direction from the planet centre.
fn surface_height(direction: vec3<f32>) -> f32 {
    let unit = normalize(direction);
    var point = unit * LATTICE_SCALE;
    var amplitude = BASE_AMPLITUDE;
    var height = 0.0;
    for (var octave = 0u; octave < OCTAVE_COUNT; octave = octave + 1u) {
        height = height + value_noise(point) * amplitude;
        point = point * 2.0;
        amplitude = amplitude * 0.5;
    }
    return height + sin(unit.y * 3.0) * cos(unit.x * 3.0) * RIDGE_AMPLITUDE;
}

// Spreads sample indices over the sphere so both sides ask about the same directions.
fn sample_direction(index: u32) -> vec3<f32> {
    let position = f32(index) + 0.5;
    let height = 1.0 - 2.0 * position / f32(SAMPLE_COUNT);
    let radius = sqrt(max(0.0, 1.0 - height * height));
    let angle = fract(position * 0.381966) * 6.2831855;
    return vec3<f32>(cos(angle) * radius, height, sin(angle) * radius);
}

@group(0) @binding(0) var<storage, read_write> heights: array<f32, 256>;

@compute @workgroup_size(64)
fn field_probe(@builtin(global_invocation_id) id: vec3<u32>) {
    let index = id.x;
    if index < SAMPLE_COUNT {
        heights[index] = surface_height(sample_direction(index));
    }
}
