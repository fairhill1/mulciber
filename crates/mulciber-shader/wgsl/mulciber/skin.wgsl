#define_import_path mulciber::skin

// Pre-integrated skin diffusion (Penner, SIGGRAPH 2011): light scatters a little way under skin,
// red furthest, so round a curve skin takes a light more softly than Lambert's n·l, warming
// toward the shade. The table is mulciber_shader::bake_skin_table's: n·l from -1 to 1 across,
// curvature from 0 to SKIN_MOST_CURVATURE per metre down, the ends on texel centres. Curvature
// per vertex is mulciber-model's vertex_curvature.

// The most curvature the table holds, per metre: mulciber_shader::SKIN_MOST_CURVATURE.
const SKIN_MOST_CURVATURE: f32 = 500.0;

// Where in a table `size` texels across and down skin at `n_dot_l` curved `curvature` reads: the
// texels' centres at either end, so a linear sampler interpolates between them.
fn skin_table_uv(n_dot_l: f32, curvature: f32, size: vec2<f32>) -> vec2<f32> {
    let at = vec2<f32>(
        clamp(n_dot_l * 0.5 + 0.5, 0.0, 1.0),
        clamp(curvature / SKIN_MOST_CURVATURE, 0.0, 1.0),
    );
    return (at * (size - vec2<f32>(1.0)) + vec2<f32>(0.5)) / size;
}

// How much of a light at `n_dot_l` skin curved `curvature` (per metre) takes, red, green and
// blue, where Lambert's takes max(n·l, 0). Bind the table with a linear, clamp-to-edge sampler.
fn skin_diffuse(
    table: texture_2d<f32>,
    table_sampler: sampler,
    n_dot_l: f32,
    curvature: f32,
) -> vec3<f32> {
    let size = vec2<f32>(textureDimensions(table));
    return textureSampleLevel(table, table_sampler, skin_table_uv(n_dot_l, curvature, size), 0.0).rgb;
}
