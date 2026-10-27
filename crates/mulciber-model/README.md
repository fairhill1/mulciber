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
mulciber-model = "0.4.0"
```

```rust
use mulciber_model::{Model, Y_UP_TO_Z_UP};

let mut model = Model::load("assets/models/mantel_clock.gltf")?;
model.transform(&Y_UP_TO_Z_UP);       // glTF is +Y up, in metres
model.duplicate_double_sided();       // back faces for pipelines that cull them; clears double_sided
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

`vertex_curvature(positions, normals, indices, most)` works out how curved a part is at each vertex,
per metre: how far the normals turn between neighbours over how far apart they are, evened out over
the neighbourhood, and the same for a vertex's copies at a texture seam. `mulciber-shader`'s
pre-integrated skin reads it (pass its `SKIN_MOST_CURVATURE` as `most`).

Animations sample step, linear (rotations along the shorter arc) and cubic-spline keys, holding
their ends outside them. `Pose::blend` cross-fades two poses; `Pose::blend_masked` blends only a
subtree. `Model::transform` moves the skeleton with the vertices, so poses fit a Z-up world too.

## Morph targets

Morph targets are read by name: `Model::morphs` lists them once each, as the meshes'
`extras.targetNames` name them (Blender writes these; a target without a name is named by its index
in its mesh). Meshes with targets of the same name share one, so a weight that opens a face's mouth
moves the teeth and tongue fitted to it too. Every part keeps, for each target that moves any of its
vertices, a `Morph` of only those vertices: how far each moves at full weight and how its normal
changes. The part's transforms, its double-sided copies and its tangents' vertex splits apply to
them as to the vertices. A target moves a skinned vertex before its joints carry it, as glTF says.

```rust
let jaw = model.morph("viseme_aa").unwrap();
let mut weights = vec![0.0; model.morphs.len()];
weights[jaw] = 0.6;
for part in &model.parts {
    // part.morphs into the game's own buffers for its vertex shader, or on the CPU:
    let (positions, normals) = part.morphed(&weights);
}
```

The meshes' default weights are not applied (the rest shape is the one without targets), tangents
are not moved, and animations' channels of morph weights are left out.

Cameras and lights are not read yet.

## Hanging chains

`Hanging` swings chains of nodes no animation moves: a skirt's from the waist to the hem, a coat's
tails, hair. Each chain is a root and the line of single children under it. Every frame it takes
the node matrices as the animation leaves them, with where the model is placed in the world, and
moves the chains' points there as weights hung from their roots: falling, lagging as the model sets
off or turns (damped against where the animation carries them, so they go along with it, not
through the world), swinging and settling, drawn gently back to the animation's shape and kept
within a leash of it (`Hang::leash`), lying on a ground if one's given, kept out of `Capsule`s
carried by other nodes (a thigh, a shin, each end its own radius), neighbouring chains kept within a
spread of how far apart they are at rest, and each link turned no further than a bend from the one
above. It then turns the chains' nodes to point where their points went, so the palette carries the
vertices weighted to them. The points move in fixed steps however long the frame, and a model put
down somewhere new starts again from its pose. A long chain lightly damped (a rope) needs
`Hang::follow`, the follow-the-leader correction, or it swings on by itself. `Hanging::step_among`
keeps them out of capsules in the world too, given each frame where they are (`Around`): someone
walking into a rope hung from a ceiling.

```rust
use mulciber_model::{Around, Capsule, Hang, Hanging};

let roots: Vec<usize> = (0..12).filter_map(|c| skeleton.find(&format!("Skirt_{c}_0"))).collect();
let thigh = Capsule { node: skeleton.find("LeftUpLeg").unwrap(), ends: [[0.0; 3], [0.0, -0.42, 0.0]], radii: [0.09, 0.06] };
let hang = Hang { leash: 30f32.to_radians(), ..Hang::new([0.0, 0.0, -9.81]) };
let mut skirt = Hanging::new(skeleton, &roots, true, vec![thigh], hang);
skirt.ground = Some([0.0, 0.0, 1.0, floor]);  // a floor `floor` up, its normal +z

let mut nodes = skeleton.node_matrices(&pose);
skirt.step(&mut nodes, &placed, dt);  // the chains' nodes turned where they swing

// A rope hung from a ceiling, kept out of whoever walks into it.
let body = Around { ends: [feet, head], radii: [0.25, 0.25] };
rope.step_among(&mut nodes, &placed, dt, &[body]);
```

A node's glTF `extras` are kept as written (`Node::extras`, JSON), for what a tool says of it for
the game alone, such as how wide a limb is round its bone.
