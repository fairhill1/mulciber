# mulciber-model

`mulciber-model` reads glTF 2.0 models (`.gltf` with its buffers, or `.glb`) into plain mesh data
for a Mulciber game to pack into its own vertex layout. It walks the file's scene, bakes every node's
transform into the vertices, and merges each material's triangles into one part, so a model draws in
one call per material.

Every vertex has a position, a unit normal, a tangent with its handedness, and texture coordinates.
A primitive without normals gets flat ones, as glTF asks, and one without tangents gets MikkTSpace's
(`bevy_mikktspace`), with the handedness a Blender export would state. Triangle strips and fans are
unrolled; points and lines are left out. Materials carry glTF's metallic-roughness factors and
textures as stated, and images are listed as files beside the model or bytes embedded in it,
undecoded, for the game to bake as it does its other textures (`mulciber-texture`).

```toml
[dependencies]
mulciber-model = "0.1.0"
```

```rust
use mulciber_model::{Model, Y_UP_TO_Z_UP};

let mut model = Model::load("assets/models/mantel_clock.gltf")?;
model.transform(&Y_UP_TO_Z_UP);       // glTF is +Y up, in metres
model.duplicate_double_sided();       // back faces for pipelines that cull them
for part in &model.parts {
    let material = model.material_of(part);
    // part.positions, normals, tangents, uvs and indices into the game's vertex layout
}
```

The data stays in glTF's conventions until transformed: right handed, +Y up, metres,
counter-clockwise front faces, (0, 0) at an image's top left, normal maps green up. A transform that
mirrors turns the winding and the tangents' handedness with it.

Skins, morph targets, animations, cameras and lights are not read yet.
