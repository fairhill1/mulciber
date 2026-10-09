# mulciber-model

`mulciber-model` reads glTF 2.0 models (`.gltf` with its buffers, or `.glb`) into plain mesh data
for a Mulciber game to pack into its own vertex layout, and animates the ones that move. It walks the file's scene, bakes every node's
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
mulciber-model = "0.2.0"
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

## Skins and animations

A model whose scene has a skin or an animation has a `Skeleton`: its nodes, parents first, with
their rest transforms, and the joints its vertices are bound to. Every vertex then has four joints
and weights summing to 1, for a skinned vertex shader (Mulciber's material pipelines take the bone
palette as a record's storage). Skinned meshes are bound as their skins say, the four heaviest of up
to eight influences kept. A mesh that isn't skinned but hangs under an animated node or a skin's
joint rides that node rigidly, so a figure built of rigid pieces parented to a rig animates without
any skin weights; anything else is bound to a joint that never moves.

```rust
let skeleton = model.skeleton.as_ref().unwrap();
let walk = model.animations.iter().find(|a| a.name.as_deref() == Some("Walk")).unwrap();
let swing = model.animations.iter().find(|a| a.name.as_deref() == Some("Swing")).unwrap();

let mut pose = skeleton.rest_pose();
walk.sample(seconds % walk.duration, &mut pose);
// The swing's upper body over the walk's legs.
let mut swung = skeleton.rest_pose();
swing.sample(swing_seconds, &mut swung);
let spine = skeleton.find("Spine").unwrap();
pose.blend_masked(&swung, 1.0, &skeleton.subtree(spine));

let palette = skeleton.palette(&pose);          // a matrix per joint, for the vertex shader
let hand = skeleton.node_matrices(&pose)[skeleton.find("Hand_R").unwrap()]; // to hold a weapon
```

Animations sample step, linear (rotations along the shorter arc) and cubic-spline keys, holding
their ends outside them. `Pose::blend` cross-fades two poses; `Pose::blend_masked` blends only a
subtree. `Model::transform` moves the skeleton with the vertices, so poses fit a Z-up world too.

Morph targets, cameras and lights are not read yet.
