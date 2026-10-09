//! Loads glTF files written here, small enough to check by hand.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use base64::Engine as _;
use mulciber_model::{Alpha, Image, Matrix, Model, Pose, TextureRef, Y_UP_TO_Z_UP};

/// A folder of its own for one test's files.
fn folder(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mulciber-model-{}-{name}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn shorts(values: &[u16]) -> Vec<u8> {
    let mut bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    bytes
}

/// A buffer of `chunks` laid end to end, and a view and accessor for each:
/// (bytes, componentType, count, type).
struct Layout {
    bytes: Vec<u8>,
    views: Vec<String>,
    accessors: Vec<String>,
}

fn layout(chunks: &[(Vec<u8>, u32, usize, &str)]) -> Layout {
    let mut out = Layout {
        bytes: Vec::new(),
        views: Vec::new(),
        accessors: Vec::new(),
    };
    for (k, (bytes, component, count, kind)) in chunks.iter().enumerate() {
        out.views.push(format!(
            r#"{{"buffer":0,"byteOffset":{},"byteLength":{}}}"#,
            out.bytes.len(),
            bytes.len()
        ));
        let bounds = if *kind == "VEC3" && *component == 5126 && k == 0 {
            // glTF requires bounds on positions; these are wide enough for every test's.
            r#","min":[-100,-100,-100],"max":[100,100,100]"#
        } else {
            ""
        };
        out.accessors.push(format!(
            r#"{{"bufferView":{k},"componentType":{component},"count":{count},"type":"{kind}"{bounds}}}"#
        ));
        out.bytes.extend_from_slice(bytes);
    }
    out
}

/// A unit quad in the XY plane facing +Z, u along +x and v down -y.
fn quad(normals: bool) -> Vec<(Vec<u8>, u32, usize, &'static str)> {
    let mut chunks = vec![(
        floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0]),
        5126,
        4,
        "VEC3",
    )];
    if normals {
        chunks.push((floats(&[0.0, 0.0, 1.0].repeat(4)), 5126, 4, "VEC3"));
    }
    chunks.push((
        floats(&[0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0]),
        5126,
        4,
        "VEC2",
    ));
    chunks.push((shorts(&[0, 1, 2, 0, 2, 3]), 5123, 6, "SCALAR"));
    chunks
}

fn data_uri(bytes: &[u8]) -> String {
    format!(
        "data:application/octet-stream;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Writes `name` in `dir`: the layout in one data-URI buffer, then `rest` (meshes, nodes and so
/// on, as JSON members).
fn write_gltf(dir: &Path, name: &str, layout: &Layout, rest: &str) -> PathBuf {
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"byteLength":{},"uri":"{}"}}],"bufferViews":[{}],"accessors":[{}],{rest}}}"#,
        layout.bytes.len(),
        data_uri(&layout.bytes),
        layout.views.join(","),
        layout.accessors.join(",")
    );
    let path = dir.join(name);
    std::fs::write(&path, json).unwrap();
    path
}

fn close(a: &[f32], b: &[f32]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
}

/// Whether each triangle's corners turn counter-clockwise about its vertices' normals.
fn fronts_face_out(model: &Model) -> bool {
    model.parts.iter().all(|part| {
        part.indices.as_chunks::<3>().0.iter().all(|t| {
            let [a, b, c] = t.map(|i| part.positions[i as usize]);
            let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let face = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let n = part.normals[t[0] as usize];
            face[0] * n[0] + face[1] * n[1] + face[2] * n[2] > 0.0
        })
    })
}

#[test]
fn nodes_bake_their_transforms_and_one_material_makes_one_part() {
    let dir = folder("nodes");
    let mesh = r#""meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2},"indices":3,"material":0}]}]"#;
    // One quad moved 10 along x; another under a parent scaled 2, itself moved 1 along z.
    let rest = format!(
        r#"{mesh},"materials":[{{"name":"Walnut"}}],"nodes":[{{"mesh":0,"translation":[10,0,0]}},{{"scale":[2,2,2],"children":[2]}},{{"mesh":0,"translation":[0,0,1]}}],"scenes":[{{"nodes":[0,1]}}],"scene":0"#
    );
    let path = write_gltf(&dir, "two.gltf", &layout(&quad(true)), &rest);
    let model = Model::load(&path).unwrap();

    assert_eq!(model.parts.len(), 1);
    let part = &model.parts[0];
    assert_eq!(part.material, Some(0));
    assert_eq!(model.materials[0].name.as_deref(), Some("Walnut"));
    assert_eq!(part.positions.len(), 8);
    assert_eq!(part.triangles(), 4);
    assert!(close(&part.positions[1], &[11.0, 0.0, 0.0]));
    // The parent's scale applies to the child's move too.
    assert!(close(&part.positions[6], &[2.0, 2.0, 2.0]));
    let bounds = model.bounds().unwrap();
    assert!(close(&bounds.min, &[0.0, 0.0, 0.0]));
    assert!(close(&bounds.max, &[11.0, 2.0, 2.0]));
    assert!(part.normals.iter().all(|n| close(n, &[0.0, 0.0, 1.0])));
    // u runs along +x; v runs down the image while y runs up, and the handedness says so.
    assert!(
        part.tangents
            .iter()
            .all(|t| close(t, &[1.0, 0.0, 0.0, 1.0]))
    );
    assert!(fronts_face_out(&model));
}

#[test]
fn a_primitive_without_normals_gets_flat_ones() {
    let dir = folder("flat");
    // A triangle, not indexed, without normals.
    let positions = floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    let layout = layout(&[(positions, 5126, 3, "VEC3")]);
    let rest = r#""meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],"nodes":[{"mesh":0}],"scenes":[{"nodes":[0]}]"#;
    let model = Model::load(write_gltf(&dir, "flat.gltf", &layout, rest)).unwrap();

    let part = &model.parts[0];
    assert_eq!(part.material, None);
    assert_eq!(model.material_of(part).metallic, 1.0);
    assert_eq!(part.indices, [0, 1, 2]);
    assert!(part.normals.iter().all(|n| close(n, &[0.0, 0.0, 1.0])));
    // No texture coordinates: still a unit tangent across the normal.
    for t in &part.tangents {
        let n = [0.0, 0.0, 1.0];
        assert!((t[0] * n[0] + t[1] * n[1] + t[2] * n[2]).abs() < 1e-5);
        assert!(((t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt() - 1.0).abs() < 1e-5);
    }
}

#[test]
fn transforms_turn_normals_and_a_mirror_turns_the_winding() {
    let dir = folder("mirror");
    let rest = r#""meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2},"indices":3}]}],"nodes":[{"mesh":0}],"scenes":[{"nodes":[0]}]"#;
    let path = write_gltf(&dir, "quad.gltf", &layout(&quad(true)), rest);

    let mut upright = Model::load(&path).unwrap();
    upright.transform(&Y_UP_TO_Z_UP);
    // The quad faced +Z, glTF's front; turned up, it faces -Y.
    assert!(
        upright.parts[0]
            .normals
            .iter()
            .all(|n| close(n, &[0.0, -1.0, 0.0]))
    );
    assert!(close(&upright.parts[0].positions[2], &[1.0, 0.0, 1.0]));
    assert!(fronts_face_out(&upright));

    let mut mirrored = Model::load(&path).unwrap();
    let flip_x: Matrix = [
        [-1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    mirrored.transform(&flip_x);
    let part = &mirrored.parts[0];
    assert!(part.normals.iter().all(|n| close(n, &[0.0, 0.0, 1.0])));
    assert!(fronts_face_out(&mirrored));
    // The tangent follows u, now along -x, and the bitangent keeps following v.
    assert!(
        part.tangents
            .iter()
            .all(|t| close(t, &[-1.0, 0.0, 0.0, -1.0]))
    );
}

#[test]
fn double_sided_materials_get_back_faces() {
    let dir = folder("double");
    let rest = r#""meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2},"indices":3,"material":0}]}],"materials":[{"doubleSided":true}],"nodes":[{"mesh":0}],"scenes":[{"nodes":[0]}]"#;
    let mut model = Model::load(write_gltf(&dir, "leaf.gltf", &layout(&quad(true)), rest)).unwrap();
    assert!(model.materials[0].double_sided);
    model.duplicate_double_sided();

    let part = &model.parts[0];
    assert_eq!(part.triangles(), 4);
    assert_eq!(part.positions.len(), 8);
    assert!(
        part.normals[4..]
            .iter()
            .all(|n| close(n, &[0.0, 0.0, -1.0]))
    );
    assert!(
        part.tangents[4..]
            .iter()
            .all(|t| close(t, &[1.0, 0.0, 0.0, -1.0]))
    );
    assert!(fronts_face_out(&model));
}

#[test]
fn materials_and_images_read_as_stated() {
    let dir = folder("images");
    let png = b"\x89PNG not really".to_vec();
    let mut chunks = quad(true);
    chunks.push((png.clone(), 5121, png.len(), "SCALAR"));
    let mut layout = layout(&chunks);
    // The image's view isn't an accessor's.
    layout.accessors.pop();
    let embedded = data_uri(b"jpeg bytes").replace("application/octet-stream", "image/jpeg");
    let rest = format!(
        r#""meshes":[{{"primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}},"indices":3,"material":0}}]}}],
        "images":[{{"uri":"maps/oak%20leaf.png"}},{{"uri":"{embedded}"}},{{"bufferView":4,"mimeType":"image/png"}}],
        "textures":[{{"source":0}},{{"source":1}},{{"source":2}}],
        "materials":[{{"pbrMetallicRoughness":{{"baseColorFactor":[0.5,0.25,1,0.75],"baseColorTexture":{{"index":0}},"metallicFactor":0,"roughnessFactor":0.4,"metallicRoughnessTexture":{{"index":2}}}},
          "normalTexture":{{"index":1,"texCoord":1,"scale":0.5}},"occlusionTexture":{{"index":2,"strength":0.8}},
          "emissiveFactor":[1,0.5,0],"extensions":{{"KHR_materials_emissive_strength":{{"emissiveStrength":4}}}},
          "alphaMode":"MASK","alphaCutoff":0.3}}],
        "extensionsUsed":["KHR_materials_emissive_strength"],
        "nodes":[{{"mesh":0}}],"scenes":[{{"nodes":[0]}}]"#
    );
    let model = Model::load(write_gltf(&dir, "lamp.gltf", &layout, &rest)).unwrap();

    assert_eq!(
        model.images,
        [
            Image::File(dir.join("maps/oak leaf.png")),
            Image::Embedded {
                mime_type: Some("image/jpeg".into()),
                bytes: b"jpeg bytes".to_vec()
            },
            Image::Embedded {
                mime_type: Some("image/png".into()),
                bytes: png
            },
        ]
    );
    let m = &model.materials[0];
    assert_eq!(m.base_color, [0.5, 0.25, 1.0, 0.75]);
    assert_eq!(
        m.base_color_texture,
        Some(TextureRef {
            image: 0,
            uv_set: 0
        })
    );
    assert_eq!((m.metallic, m.roughness), (0.0, 0.4));
    assert_eq!(
        m.metallic_roughness_texture,
        Some(TextureRef {
            image: 2,
            uv_set: 0
        })
    );
    assert_eq!(
        m.normal_texture,
        Some((
            TextureRef {
                image: 1,
                uv_set: 1
            },
            0.5
        ))
    );
    assert_eq!(
        m.occlusion_texture,
        Some((
            TextureRef {
                image: 2,
                uv_set: 0
            },
            0.8
        ))
    );
    assert_eq!(m.emissive, [4.0, 2.0, 0.0]);
    assert_eq!(m.alpha, Alpha::Mask(0.3));
    assert!(!m.double_sided);
}

#[test]
fn a_glb_loads_from_its_binary_chunk() {
    let dir = folder("glb");
    let layout = layout(&quad(true));
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"byteLength":{}}}],"bufferViews":[{}],"accessors":[{}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2}},"indices":3}}]}}],"nodes":[{{"mesh":0}}],"scenes":[{{"nodes":[0]}}]}}"#,
        layout.bytes.len(),
        layout.views.join(","),
        layout.accessors.join(",")
    );
    let mut json = json.into_bytes();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bin = layout.bytes.clone();
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut glb = Vec::new();
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&u32::try_from(total).unwrap().to_le_bytes());
    glb.extend_from_slice(&u32::try_from(json.len()).unwrap().to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json);
    glb.extend_from_slice(&u32::try_from(bin.len()).unwrap().to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&bin);
    let path = dir.join("quad.glb");
    std::fs::write(&path, glb).unwrap();

    let model = Model::load(&path).unwrap();
    assert_eq!(model.triangles(), 2);
    assert!(close(&model.parts[0].positions[2], &[1.0, 1.0, 0.0]));
}

#[test]
fn a_file_that_cannot_be_read_says_which() {
    let error = Model::load("no/such/chair.glb").unwrap_err();
    assert!(
        error.message().starts_with("no/such/chair.glb: "),
        "{error}"
    );
}

/// Where each vertex of `model`'s parts goes in `pose`, its joints' bone matrices blended by its
/// weights.
fn posed(model: &Model, pose: &Pose) -> Vec<[f32; 3]> {
    let palette = model.skeleton.as_ref().unwrap().palette(pose);
    let apply = |m: &Matrix, p: [f32; 3]| -> [f32; 3] {
        std::array::from_fn(|r| m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r])
    };
    model
        .parts
        .iter()
        .flat_map(|part| {
            part.positions.iter().enumerate().map(|(v, &p)| {
                let mut out = [0.0; 3];
                for (j, w) in part.joints[v].iter().zip(part.weights[v]) {
                    let moved = apply(&palette[usize::from(*j)], p);
                    for k in 0..3 {
                        out[k] += moved[k] * w;
                    }
                }
                out
            })
        })
        .collect()
}

/// The accessor `k` of `layout` given the bounds glTF asks of an animation's times.
fn timed(mut layout: Layout, k: usize, last: f32) -> Layout {
    let accessor = layout.accessors[k].trim_end_matches('}').to_owned();
    layout.accessors[k] = format!(r#"{accessor},"min":[0],"max":[{last}]}}"#);
    layout
}

/// A rotation about +z turning to a quarter turn over a second.
fn quarter_turn() -> [(Vec<u8>, u32, usize, &'static str); 2] {
    let half = std::f32::consts::FRAC_1_SQRT_2;
    [
        (floats(&[0.0, 1.0]), 5126, 2, "SCALAR"),
        (
            floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, half, half]),
            5126,
            2,
            "VEC4",
        ),
    ]
}

#[test]
fn a_skinned_mesh_bends_with_its_joints() {
    let dir = folder("skin");
    // The quad's bottom edge bound to the hips at (0, 1, 0), its top edge to an arm at (1, 1, 0),
    // their inverse binds undoing those.
    let mut chunks = quad(true);
    chunks.push((
        shorts(&[0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]),
        5123,
        4,
        "VEC4",
    ));
    chunks.push((floats(&[1.0, 0.0, 0.0, 0.0].repeat(4)), 5126, 4, "VEC4"));
    let unbind = |x: f32, y: f32| {
        [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -x, -y, 0.0, 1.0,
        ]
    };
    chunks.push((
        floats(&[unbind(0.0, 1.0), unbind(1.0, 1.0)].concat()),
        5126,
        2,
        "MAT4",
    ));
    chunks.extend(quarter_turn());
    let layout = timed(layout(&chunks), 7, 1.0);
    let rest = r#""meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2,"JOINTS_0":4,"WEIGHTS_0":5},"indices":3}]}],
        "skins":[{"joints":[0,1],"inverseBindMatrices":6}],
        "nodes":[{"name":"Hips","translation":[0,1,0],"children":[1]},{"name":"Arm","translation":[1,0,0]},{"mesh":0,"skin":0,"translation":[50,0,0]}],
        "animations":[{"name":"Wave","channels":[{"sampler":0,"target":{"node":1,"path":"rotation"}}],"samplers":[{"input":7,"output":8}]}],
        "scenes":[{"nodes":[0,2]}]"#;
    let path = write_gltf(&dir, "skin.gltf", &layout, rest);
    let model = Model::load(&path).unwrap();

    let skeleton = model.skeleton.as_ref().unwrap();
    assert_eq!(skeleton.nodes.len(), 3);
    assert_eq!(skeleton.find("Arm"), Some(1));
    assert_eq!(skeleton.nodes[1].parent, Some(0));
    assert_eq!(model.animations.len(), 1);
    let wave = &model.animations[0];
    assert_eq!((wave.name.as_deref(), wave.duration), (Some("Wave"), 1.0));
    let part = &model.parts[0];
    assert_eq!(part.joints.len(), 4);
    assert_eq!(part.weights[0], [1.0, 0.0, 0.0, 0.0]);

    // At rest the quad is where it was bound, the skinned node's own move ignored, as glTF says.
    let mut pose = skeleton.rest_pose();
    let at_rest = posed(&model, &pose);
    assert!(close(
        &at_rest.concat(),
        &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0]
    ));
    // A second in, the arm has turned a quarter about +z: the top edge swings round its corner at
    // (1, 1, 0).
    wave.sample(1.0, &mut pose);
    let waved = posed(&model, &pose);
    assert!(close(&waved[2], &[1.0, 1.0, 0.0]));
    assert!(close(&waved[3], &[1.0, 0.0, 0.0]));
    assert!(close(&waved[0], &[0.0, 0.0, 0.0]), "the hips' edge stays");

    // Turned upright, the model and its poses turn together.
    let mut upright = Model::load(&path).unwrap();
    upright.transform(&Y_UP_TO_Z_UP);
    let turned = posed(&upright, &pose);
    for (a, b) in waved.iter().zip(&turned) {
        assert!(close(b, &[a[0], -a[2], a[1]]), "{a:?} turned is {b:?}");
    }
    // The arm's node in the model's frame, for holding things: at its joint, turned up.
    let arm = upright.skeleton.as_ref().unwrap().node_matrices(&pose)[1];
    assert!(close(&arm[3], &[1.0, 0.0, 1.0, 1.0]));
}

#[test]
fn rigid_pieces_ride_the_nodes_above_them() {
    let dir = folder("rigid");
    // An arm at (1, 1, 0) turning, a quad hung under it, and a quad standing apart that nothing
    // moves.
    let mut chunks = quad(true);
    chunks.extend(quarter_turn());
    let layout = timed(layout(&chunks), 4, 1.0);
    let rest = r#""meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2},"indices":3,"material":0}]}],
        "materials":[{"doubleSided":true}],
        "nodes":[{"name":"Arm","translation":[1,1,0],"children":[1]},{"mesh":0},{"mesh":0,"translation":[5,0,0]}],
        "animations":[{"channels":[{"sampler":0,"target":{"node":0,"path":"rotation"}}],"samplers":[{"input":4,"output":5}]}],
        "scenes":[{"nodes":[0,2]}]"#;
    let mut model = Model::load(write_gltf(&dir, "rigid.gltf", &layout, rest)).unwrap();

    let part = &model.parts[0];
    assert!(part.weights.iter().all(|w| close(w, &[1.0, 0.0, 0.0, 0.0])));
    // Placed at rest, as a static model is.
    assert!(close(&part.positions[1], &[2.0, 1.0, 0.0]));
    assert!(close(&part.positions[5], &[6.0, 0.0, 0.0]));
    let skeleton = model.skeleton.as_ref().unwrap();
    let mut pose = skeleton.rest_pose();
    model.animations[0].sample(1.0, &mut pose);
    let moved = posed(&model, &pose);
    // The hung quad's (1, 0, 0) corner turns round the arm to (1, 2, 0); the other stays.
    assert!(close(&moved[1], &[1.0, 2.0, 0.0]));
    assert!(close(&moved[5], &[6.0, 0.0, 0.0]));
    // Back faces keep their joints.
    model.duplicate_double_sided();
    let part = &model.parts[0];
    assert_eq!((part.joints.len(), part.weights.len()), (16, 16));
    assert_eq!(part.joints[9], part.joints[1]);
}

#[test]
fn a_model_that_never_moves_has_no_skeleton() {
    let dir = folder("still");
    let rest = r#""meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2},"indices":3}]}],"nodes":[{"mesh":0}],"scenes":[{"nodes":[0]}]"#;
    let model = Model::load(write_gltf(&dir, "still.gltf", &layout(&quad(true)), rest)).unwrap();
    assert_eq!(model.skeleton, None);
    assert_eq!(model.animations.len(), 0);
    assert_eq!(
        (model.parts[0].joints.len(), model.parts[0].weights.len()),
        (0, 0)
    );
}
