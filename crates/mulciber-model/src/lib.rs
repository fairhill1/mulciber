//! Model loading for Mulciber games: a glTF 2.0 file (`.gltf` with its buffers, or `.glb`) read
//! into plain mesh data for a game to pack into its own vertex layout.
//!
//! [`Model::load`] walks the file's scene, bakes every node's transform into its meshes' vertices
//! and merges the triangles of each material into one [`Part`], so a model draws in one call per
//! material. Every part has positions, normals, tangents and texture coordinates for each vertex:
//! a primitive without normals gets flat ones, as glTF asks, and one without tangents gets
//! `MikkTSpace`'s (`bevy_mikktspace`), the tangent space normal maps are baked against by Blender
//! and most other tools. Triangle strips and fans are unrolled into lists; points and lines are
//! left out.
//!
//! [`Material`] carries glTF's metallic-roughness material as stated: its factors, which textures
//! it uses and from which coordinate set, its alpha mode and whether it is double sided. Images are
//! listed once in [`Model::images`], as files beside the model or bytes embedded in it, and are not
//! decoded here: the game bakes or decodes them as it does its other textures.
//!
//! The data stays in glTF's conventions: right handed, +Y up, metres, counter-clockwise front
//! faces, texture coordinates with (0, 0) at the image's top left, normal maps green up, and
//! tangents' `w` the handedness with which the bitangent is `cross(normal, tangent) * w`.
//! [`Model::transform`] moves it into a game's frame ([`Y_UP_TO_Z_UP`] for a Z-up world), turning
//! the winding back when the transform mirrors. [`Model::duplicate_double_sided`] adds the back
//! faces of double-sided materials for pipelines that cull them.
//!
//! A model whose scene has a skin or an animation also has a [`Skeleton`]: every node, parents
//! first, with its rest transform, and the joints its vertices are bound to. Every part then gives
//! each vertex four joints and their weights ([`Part::joints`], [`Part::weights`]), as a skinned
//! vertex shader blends them. A skinned mesh's vertices are bound as its skin says (up to eight
//! influences, the four heaviest kept); a mesh that isn't skinned but hangs under a node an
//! animation moves, or under a skin's joint, is carried rigidly by the nearest such node, its
//! vertices bound wholly to it, so a figure built of rigid pieces on a rig animates without skin
//! weights; and anything else is bound to a joint that never moves. [`Animation`]s key the nodes'
//! translations, rotations and scales; [`Animation::sample`] sets a [`Pose`] to where they are at a
//! time, [`Pose::blend`] mixes two, and [`Skeleton::palette`] turns a pose into the bone matrices.
//! A model with neither skin nor animation has no skeleton, and its parts no joints.
//!
//! Morph targets, cameras and lights are not read yet.

mod animation;
mod skeleton;
mod tangents;

pub use animation::{Animation, Channel, Interpolation, Keys};
pub use skeleton::{Joint, Node, Pose, Skeleton, Transform, inverse, slerp};

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use base64::Engine as _;

/// A column-major 4 x 4 matrix, as glTF stores one: `m[column][row]`.
pub type Matrix = [[f32; 4]; 4];

/// The identity.
pub const IDENTITY: Matrix = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// glTF's +Y up turned to +Z up, a quarter turn about X: (x, y, z) to (x, -z, y). What faced +Z
/// (glTF's front) faces -Y.
pub const Y_UP_TO_Z_UP: Matrix = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, -1.0, 0.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// A model: its parts, the materials they use and the images those use.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    /// One per material the scene's meshes use, in the order they are first met.
    pub parts: Vec<Part>,
    /// The file's materials, by glTF index.
    pub materials: Vec<Material>,
    /// The file's images, by glTF index.
    pub images: Vec<Image>,
    /// Its nodes and the joints its vertices are bound to, when its scene has a skin or an
    /// animation.
    pub skeleton: Option<Skeleton>,
    /// The file's animations, by glTF index; none without a skeleton. Channels of morph target
    /// weights, or of nodes outside the scene, are left out.
    pub animations: Vec<Animation>,
}

/// One material's triangles, with every node's transform baked in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    /// Index into [`Model::materials`]; `None` for glTF's default material
    /// ([`Material::default`]).
    pub material: Option<usize>,
    /// Vertex positions.
    pub positions: Vec<[f32; 3]>,
    /// Unit vertex normals.
    pub normals: Vec<[f32; 3]>,
    /// Unit tangents along +u, with the bitangent's handedness in `w` (+1 or -1).
    pub tangents: Vec<[f32; 4]>,
    /// Texture coordinate set 0; zeros where the primitive has none.
    pub uvs: Vec<[f32; 2]>,
    /// With a [`Model::skeleton`], each vertex's four joints (indices into [`Skeleton::joints`]);
    /// empty without one.
    pub joints: Vec<[u16; 4]>,
    /// The weights of [`Part::joints`], summing to 1; empty without a skeleton.
    pub weights: Vec<[f32; 4]>,
    /// Triangle list, counter-clockwise from the front.
    pub indices: Vec<u32>,
}

impl Part {
    /// How many triangles it has.
    #[must_use]
    pub fn triangles(&self) -> usize {
        self.indices.len() / 3
    }
}

/// A glTF metallic-roughness material.
#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    /// The material's name, when it has one.
    pub name: Option<String>,
    /// Linear RGBA, multiplying the base colour texture.
    pub base_color: [f32; 4],
    /// sRGB colour, alpha in `a`.
    pub base_color_texture: Option<TextureRef>,
    /// Multiplies the metallic-roughness texture's blue channel.
    pub metallic: f32,
    /// Multiplies the metallic-roughness texture's green channel.
    pub roughness: f32,
    /// Linear data: roughness in green, metallic in blue.
    pub metallic_roughness_texture: Option<TextureRef>,
    /// Tangent-space normals, green up, with the scale their x and y are multiplied by.
    pub normal_texture: Option<(TextureRef, f32)>,
    /// Ambient occlusion in red, with the strength it is applied at.
    pub occlusion_texture: Option<(TextureRef, f32)>,
    /// Linear emitted colour, `KHR_materials_emissive_strength` multiplied in.
    pub emissive: [f32; 3],
    /// sRGB, multiplied by `emissive`.
    pub emissive_texture: Option<TextureRef>,
    /// How the base colour's alpha is used.
    pub alpha: Alpha,
    /// Whether both faces show.
    pub double_sided: bool,
}

/// glTF's default material, for a primitive that names none: white, fully metallic, fully rough.
impl Default for Material {
    fn default() -> Self {
        Self {
            name: None,
            base_color: [1.0; 4],
            base_color_texture: None,
            metallic: 1.0,
            roughness: 1.0,
            metallic_roughness_texture: None,
            normal_texture: None,
            occlusion_texture: None,
            emissive: [0.0; 3],
            emissive_texture: None,
            alpha: Alpha::Opaque,
            double_sided: false,
        }
    }
}

/// How a material's alpha is used.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Alpha {
    /// Ignored.
    Opaque,
    /// Shown where alpha is at least the cutoff, left out elsewhere.
    Mask(f32),
    /// Blended over what's behind.
    Blend,
}

/// A material's use of an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureRef {
    /// Index into [`Model::images`].
    pub image: usize,
    /// The texture coordinate set it is read with. [`Part::uvs`] is set 0.
    pub uv_set: u32,
}

/// Where an image's encoded bytes are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Image {
    /// A file, resolved against the model's folder.
    File(PathBuf),
    /// Bytes in the model (a GLB's buffer, or a data URI), with their media type when stated.
    Embedded {
        /// For instance `image/png`.
        mime_type: Option<String>,
        /// The encoded image.
        bytes: Vec<u8>,
    },
}

/// An axis-aligned box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    /// The least x, y and z.
    pub min: [f32; 3],
    /// The greatest x, y and z.
    pub max: [f32; 3],
}

/// A model that could not be read, with what and where.
#[derive(Clone, PartialEq, Eq)]
pub struct ModelError {
    message: String,
}

impl ModelError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

// Printed as written, so `expect` and `?` in `main` stay readable.
impl fmt::Debug for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModelError {}

impl Model {
    /// Reads the `.gltf` or `.glb` at `path`: its default scene (else its first), every node's
    /// meshes with their transforms baked in, one part per material.
    ///
    /// # Errors
    ///
    /// When the file or a buffer it names can't be read or isn't valid glTF, or it has no scene.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ModelError> {
        let path = path.as_ref();
        let at = |e: &dyn fmt::Display| ModelError::new(format!("{}: {e}", path.display()));
        let gltf::Gltf { document, blob } = gltf::Gltf::open(path).map_err(|e| at(&e))?;
        let base = path.parent().unwrap_or(Path::new(""));
        let buffers = gltf::import_buffers(&document, Some(base), blob).map_err(|e| at(&e))?;
        let scene = document
            .default_scene()
            .or_else(|| document.scenes().next())
            .ok_or_else(|| at(&"no scene"))?;

        let mut model = Self {
            parts: Vec::new(),
            materials: document.materials().map(|m| material(&m)).collect(),
            images: document
                .images()
                .map(|image| self::image(&image, base, &buffers))
                .collect::<Result<_, String>>()
                .map_err(|e| at(&e))?,
            skeleton: None,
            animations: Vec::new(),
        };
        let mut parts = HashMap::new();
        match Rig::read(&document, &scene, &buffers).map_err(|e| at(&e))? {
            Some(mut rig) => {
                for node in scene.nodes() {
                    model.collect_rigged(&node, &buffers, &mut rig, &mut parts);
                }
                model.skeleton = Some(Skeleton {
                    nodes: rig.nodes,
                    joints: rig.joints,
                    root: IDENTITY,
                });
                model.animations = rig.animations;
            }
            None => {
                for node in scene.nodes() {
                    model.collect(&node, &buffers, &IDENTITY, &mut parts);
                }
            }
        }
        Ok(model)
    }

    /// Adds `node`'s meshes and its children's, under the `parent` transform.
    fn collect(
        &mut self,
        node: &gltf::Node<'_>,
        buffers: &[gltf::buffer::Data],
        parent: &Matrix,
        parts: &mut HashMap<Option<usize>, usize>,
    ) {
        let world = multiply(parent, &node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            for primitive in mesh.primitives() {
                let Some(mut piece) = Piece::read(&primitive, buffers, false) else {
                    continue;
                };
                piece.transform(&world);
                let key = primitive.material().index();
                let part = *parts.entry(key).or_insert_with(|| {
                    self.parts.push(Part {
                        material: key,
                        ..Part::default()
                    });
                    self.parts.len() - 1
                });
                piece.append_to(&mut self.parts[part]);
            }
        }
        for child in node.children() {
            self.collect(&child, buffers, &world, parts);
        }
    }

    /// Adds `node`'s meshes and its children's to a model with a skeleton, each vertex bound to
    /// its joints: a skinned primitive's as its skin says, in the skin's bind space (glTF ignores
    /// the skinned node's own transform); anything else placed at rest and carried by the node
    /// `rig` anchors it to.
    fn collect_rigged(
        &mut self,
        node: &gltf::Node<'_>,
        buffers: &[gltf::buffer::Data],
        rig: &mut Rig,
        parts: &mut HashMap<Option<usize>, usize>,
    ) {
        let k = rig.index_of[&node.index()];
        if let Some(mesh) = node.mesh() {
            for primitive in mesh.primitives() {
                let skin = node.skin();
                let Some(mut piece) = Piece::read(&primitive, buffers, skin.is_some()) else {
                    continue;
                };
                if let Some(skin) = skin.filter(|_| !piece.joints.is_empty()) {
                    let start = rig.skin_start(&skin, buffers);
                    for joints in &mut piece.joints {
                        *joints = joints.map(|j| j + start);
                    }
                } else {
                    piece.transform(&rig.world[k]);
                    let joint = rig.carrier(rig.anchor[k]);
                    piece.joints = vec![[joint, 0, 0, 0]; piece.positions.len()];
                    piece.weights = vec![[1.0, 0.0, 0.0, 0.0]; piece.positions.len()];
                }
                let key = primitive.material().index();
                let part = *parts.entry(key).or_insert_with(|| {
                    self.parts.push(Part {
                        material: key,
                        ..Part::default()
                    });
                    self.parts.len() - 1
                });
                piece.append_to(&mut self.parts[part]);
            }
        }
        for child in node.children() {
            self.collect_rigged(&child, buffers, rig, parts);
        }
    }

    /// Moves every vertex by `matrix`: positions as points, tangents as directions, normals by the
    /// inverse transpose. A transform that mirrors (negative determinant) turns each triangle's
    /// winding and each tangent's handedness, so the fronts still face out. A skeleton moves with
    /// the vertices ([`Skeleton::root`]), so its poses still fit them.
    pub fn transform(&mut self, matrix: &Matrix) {
        for part in &mut self.parts {
            let mut piece = Piece::take(part);
            piece.transform(matrix);
            piece.append_to(part);
        }
        if let Some(skeleton) = &mut self.skeleton {
            skeleton.root = multiply(matrix, &skeleton.root);
        }
    }

    /// Adds, for every part whose material is double sided, a copy of its triangles wound the
    /// other way with their normals reversed, for pipelines that cull back faces. The copies'
    /// tangents keep their direction and turn their handedness, so a normal map reads the same
    /// from either side.
    ///
    /// # Panics
    ///
    /// When a part has more vertices than `u32` indices reach.
    pub fn duplicate_double_sided(&mut self) {
        for part in &mut self.parts {
            let double = part
                .material
                .and_then(|m| self.materials.get(m))
                .is_some_and(|m| m.double_sided);
            if !double {
                continue;
            }
            let offset = u32::try_from(part.positions.len()).expect("a part fits u32 indices");
            let back: Vec<u32> = part
                .indices
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|t| [t[0] + offset, t[2] + offset, t[1] + offset])
                .collect();
            part.positions.extend_from_within(..);
            part.uvs.extend_from_within(..);
            part.joints.extend_from_within(..);
            part.weights.extend_from_within(..);
            let normals: Vec<[f32; 3]> = part.normals.iter().map(|n| n.map(|c| -c)).collect();
            part.normals.extend(normals);
            let tangents: Vec<[f32; 4]> = part
                .tangents
                .iter()
                .map(|t| [t[0], t[1], t[2], -t[3]])
                .collect();
            part.tangents.extend(tangents);
            part.indices.extend(back);
        }
    }

    /// The box round every vertex as loaded (a skinned mesh's as bound, which its pose may stand
    /// elsewhere), or `None` for a model with none.
    #[must_use]
    pub fn bounds(&self) -> Option<Bounds> {
        let mut points = self.parts.iter().flat_map(|p| &p.positions);
        let first = *points.next()?;
        let mut bounds = Bounds {
            min: first,
            max: first,
        };
        for p in points {
            bounds.min = std::array::from_fn(|axis| bounds.min[axis].min(p[axis]));
            bounds.max = std::array::from_fn(|axis| bounds.max[axis].max(p[axis]));
        }
        Some(bounds)
    }

    /// How many triangles all its parts have.
    #[must_use]
    pub fn triangles(&self) -> usize {
        self.parts.iter().map(Part::triangles).sum()
    }

    /// The material `part` is drawn with: its own, or glTF's default.
    #[must_use]
    pub fn material_of(&self, part: &Part) -> Material {
        part.material
            .and_then(|m| self.materials.get(m))
            .cloned()
            .unwrap_or_default()
    }
}

/// One primitive's vertices, or a part's while it's transformed.
struct Piece {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    tangents: Vec<[f32; 4]>,
    uvs: Vec<[f32; 2]>,
    /// Empty, or one per vertex.
    joints: Vec<[u16; 4]>,
    weights: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl Piece {
    /// A primitive's triangles, with normals and tangents made for it where it has none, and when
    /// `skinned` its joints and weights if it has them (indices into its skin's joints); `None`
    /// for one without positions or triangles.
    fn read(
        primitive: &gltf::Primitive<'_>,
        buffers: &[gltf::buffer::Data],
        skinned: bool,
    ) -> Option<Self> {
        let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(|d| &d.0[..]));
        let positions: Vec<[f32; 3]> = reader.read_positions()?.collect();
        let count = u32::try_from(positions.len()).ok()?;
        let listed: Vec<u32> = match reader.read_indices() {
            Some(indices) => indices.into_u32().collect(),
            None => (0..count).collect(),
        };
        let indices = triangle_list(primitive.mode(), &listed)?;
        if indices.iter().any(|&i| i >= count) {
            return None;
        }
        let uvs = reader
            .read_tex_coords(0)
            .map(|uvs| uvs.into_f32().collect())
            .filter(|uvs: &Vec<[f32; 2]>| uvs.len() == positions.len())
            .unwrap_or_else(|| vec![[0.0; 2]; positions.len()]);
        let normals: Option<Vec<[f32; 3]>> = reader
            .read_normals()
            .map(Iterator::collect)
            .filter(|n: &Vec<[f32; 3]>| n.len() == positions.len());
        let (joints, weights) = if skinned {
            influences(&reader, positions.len()).unwrap_or_default()
        } else {
            (Vec::new(), Vec::new())
        };
        let mut piece = match normals {
            Some(normals) => Self {
                positions,
                normals,
                tangents: Vec::new(),
                uvs,
                joints,
                weights,
                indices,
            },
            None => Self::flat(&positions, &uvs, (&joints, &weights), &indices),
        };
        let tangents: Option<Vec<[f32; 4]>> = reader
            .read_tangents()
            .map(Iterator::collect)
            .filter(|t: &Vec<[f32; 4]>| t.len() == piece.positions.len());
        match tangents {
            Some(tangents) => piece.tangents = tangents,
            None => tangents::generate(&mut piece),
        }
        Some(piece)
    }

    /// Every triangle its own three vertices, each with the triangle's normal.
    fn flat(
        positions: &[[f32; 3]],
        uvs: &[[f32; 2]],
        (joints, weights): (&[[u16; 4]], &[[f32; 4]]),
        indices: &[u32],
    ) -> Self {
        let mut piece = Self {
            positions: Vec::with_capacity(indices.len()),
            normals: Vec::with_capacity(indices.len()),
            tangents: Vec::new(),
            uvs: Vec::with_capacity(indices.len()),
            joints: Vec::new(),
            weights: Vec::new(),
            indices: Vec::with_capacity(indices.len()),
        };
        for triangle in indices.as_chunks::<3>().0 {
            let [a, b, c] = [0, 1, 2].map(|k| positions[triangle[k] as usize]);
            let normal = normalize(cross(sub(b, a), sub(c, a)));
            for &i in triangle {
                piece
                    .indices
                    .push(u32::try_from(piece.positions.len()).expect("fits u32"));
                piece.positions.push(positions[i as usize]);
                piece.normals.push(normal);
                piece.uvs.push(uvs[i as usize]);
                if !joints.is_empty() {
                    piece.joints.push(joints[i as usize]);
                    piece.weights.push(weights[i as usize]);
                }
            }
        }
        piece
    }

    /// A part's vertices, leaving it empty.
    fn take(part: &mut Part) -> Self {
        Self {
            positions: std::mem::take(&mut part.positions),
            normals: std::mem::take(&mut part.normals),
            tangents: std::mem::take(&mut part.tangents),
            uvs: std::mem::take(&mut part.uvs),
            joints: std::mem::take(&mut part.joints),
            weights: std::mem::take(&mut part.weights),
            indices: std::mem::take(&mut part.indices),
        }
    }

    fn transform(&mut self, m: &Matrix) {
        let [x, y, z] = [0, 1, 2].map(|c| [m[c][0], m[c][1], m[c][2]]);
        // The inverse transpose's columns are these, over the determinant; only its sign matters
        // once the normals are made unit again.
        let det = dot(x, cross(y, z));
        let sign = if det < 0.0 { -1.0 } else { 1.0 };
        let cofactor = [cross(y, z), cross(z, x), cross(x, y)];
        for p in &mut self.positions {
            *p = add(apply(&[x, y, z], *p), [m[3][0], m[3][1], m[3][2]]);
        }
        for n in &mut self.normals {
            *n = normalize(scale(apply(&cofactor, *n), sign));
        }
        for t in &mut self.tangents {
            let [tx, ty, tz] = normalize(apply(&[x, y, z], [t[0], t[1], t[2]]));
            *t = [tx, ty, tz, t[3] * sign];
        }
        if det < 0.0 {
            for triangle in self.indices.as_chunks_mut::<3>().0 {
                triangle.swap(1, 2);
            }
        }
    }

    fn append_to(self, part: &mut Part) {
        let offset = u32::try_from(part.positions.len()).expect("a part fits u32 indices");
        part.positions.extend(self.positions);
        part.normals.extend(self.normals);
        part.tangents.extend(self.tangents);
        part.uvs.extend(self.uvs);
        part.joints.extend(self.joints);
        part.weights.extend(self.weights);
        part.indices
            .extend(self.indices.into_iter().map(|i| i + offset));
    }
}

/// Each vertex's four joints, and their weights.
type Influences = (Vec<[u16; 4]>, Vec<[f32; 4]>);

/// Each vertex's four heaviest joints of up to eight (`JOINTS_0` and `WEIGHTS_0`, then `_1`), with
/// their weights made to sum to 1; `None` when the primitive has none, or not one per vertex.
fn influences<'a, 's, F>(reader: &gltf::mesh::Reader<'a, 's, F>, count: usize) -> Option<Influences>
where
    F: Clone + Fn(gltf::Buffer<'a>) -> Option<&'s [u8]>,
{
    let mut sets = Vec::new();
    for set in 0..2 {
        let (Some(joints), Some(weights)) = (reader.read_joints(set), reader.read_weights(set))
        else {
            break;
        };
        let joints: Vec<[u16; 4]> = joints.into_u16().collect();
        let weights: Vec<[f32; 4]> = weights.into_f32().collect();
        if joints.len() != count || weights.len() != count {
            return None;
        }
        sets.push((joints, weights));
    }
    if sets.is_empty() {
        return None;
    }
    let mut joints = Vec::with_capacity(count);
    let mut weights = Vec::with_capacity(count);
    for v in 0..count {
        let mut pairs: Vec<(u16, f32)> = sets
            .iter()
            .flat_map(|(j, w)| j[v].into_iter().zip(w[v]))
            .collect();
        pairs.sort_by(|a, b| b.1.total_cmp(&a.1));
        pairs.resize(4, (0, 0.0));
        let sum: f32 = pairs.iter().map(|p| p.1.max(0.0)).sum();
        joints.push(std::array::from_fn(|k| pairs[k].0));
        weights.push(if sum > 0.0 {
            std::array::from_fn(|k| pairs[k].1.max(0.0) / sum)
        } else {
            // Weighted nothing, which glTF forbids: wholly its first joint.
            [1.0, 0.0, 0.0, 0.0]
        });
    }
    Some((joints, weights))
}

/// What a model with a skeleton is read with: its scene's nodes, where they rest in the model, the
/// joints bound so far, and its animations.
struct Rig {
    nodes: Vec<Node>,
    /// glTF node index to [`Rig::nodes`] index.
    index_of: HashMap<usize, usize>,
    /// Each node's rest transform in the model.
    world: Vec<Matrix>,
    /// The nearest node at or above each that moves (an animation's or a skin's), which carries
    /// the meshes under it.
    anchor: Vec<Option<usize>>,
    joints: Vec<Joint>,
    /// Where each skin's joints begin in `joints`, by glTF skin index.
    skins: HashMap<usize, u16>,
    /// The joint carrying everything a node (or nothing, for `None`) carries rigidly.
    carriers: HashMap<Option<usize>, u16>,
    animations: Vec<Animation>,
}

impl Rig {
    /// The scene's rig, or `None` when it has neither a skin nor an animation of its nodes.
    fn read(
        document: &gltf::Document,
        scene: &gltf::Scene<'_>,
        buffers: &[gltf::buffer::Data],
    ) -> Result<Option<Self>, String> {
        let mut rig = Self {
            nodes: Vec::new(),
            index_of: HashMap::new(),
            world: Vec::new(),
            anchor: Vec::new(),
            joints: Vec::new(),
            skins: HashMap::new(),
            carriers: HashMap::new(),
            animations: Vec::new(),
        };
        let mut skinned = false;
        for node in scene.nodes() {
            skinned |= rig.walk(&node, None);
        }
        let mut moving = vec![false; rig.nodes.len()];
        for animation in document.animations() {
            let read = rig.animation(&animation, buffers)?;
            for channel in &read.channels {
                moving[channel.node] = true;
            }
            rig.animations.push(read);
        }
        if !skinned && !moving.contains(&true) {
            return Ok(None);
        }
        for skin in document.skins() {
            for joint in skin.joints() {
                if let Some(&k) = rig.index_of.get(&joint.index()) {
                    moving[k] = true;
                }
            }
        }
        for (k, &moves) in moving.iter().enumerate() {
            let parent = rig.nodes[k].parent;
            rig.anchor.push(if moves {
                Some(k)
            } else {
                parent.and_then(|p| rig.anchor[p])
            });
        }
        Ok(Some(rig))
    }

    /// Adds `node` and its children, parents first; whether any of them has a skinned mesh.
    fn walk(&mut self, node: &gltf::Node<'_>, parent: Option<usize>) -> bool {
        let k = self.nodes.len();
        self.index_of.insert(node.index(), k);
        let (translation, rotation, scale) = node.transform().decomposed();
        let rest = Transform {
            translation,
            rotation,
            scale,
        };
        let above = parent.map_or(IDENTITY, |p| self.world[p]);
        self.world.push(multiply(&above, &rest.matrix()));
        self.nodes.push(Node {
            name: node.name().map(str::to_owned),
            parent,
            rest,
        });
        let mut skinned = node.skin().is_some() && node.mesh().is_some();
        for child in node.children() {
            skinned |= self.walk(&child, Some(k));
        }
        skinned
    }

    /// The channels of `animation` that move the scene's nodes.
    fn animation(
        &self,
        animation: &gltf::Animation<'_>,
        buffers: &[gltf::buffer::Data],
    ) -> Result<Animation, String> {
        use gltf::animation::util::ReadOutputs;
        let mut channels = Vec::new();
        for (c, channel) in animation.channels().enumerate() {
            let Some(&node) = self.index_of.get(&channel.target().node().index()) else {
                continue;
            };
            let reader = channel.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
            let (Some(times), Some(outputs)) = (reader.read_inputs(), reader.read_outputs()) else {
                continue;
            };
            let keys = match outputs {
                ReadOutputs::Translations(v) => Keys::Translation(v.collect()),
                ReadOutputs::Rotations(v) => Keys::Rotation(v.into_f32().collect()),
                ReadOutputs::Scales(v) => Keys::Scale(v.collect()),
                ReadOutputs::MorphTargetWeights(_) => continue,
            };
            let interpolation = match channel.sampler().interpolation() {
                gltf::animation::Interpolation::Step => Interpolation::Step,
                gltf::animation::Interpolation::Linear => Interpolation::Linear,
                gltf::animation::Interpolation::CubicSpline => Interpolation::CubicSpline,
            };
            let times: Vec<f32> = times.collect();
            let per_key = if interpolation == Interpolation::CubicSpline {
                3
            } else {
                1
            };
            if times.is_empty() || keys.len() != times.len() * per_key {
                return Err(format!(
                    "animation {} channel {c}: {} keys for {} times",
                    animation.index(),
                    keys.len(),
                    times.len()
                ));
            }
            channels.push(Channel {
                node,
                interpolation,
                times,
                keys,
            });
        }
        let duration = channels
            .iter()
            .filter_map(|c| c.times.last().copied())
            .fold(0.0, f32::max);
        Ok(Animation {
            name: animation.name().map(str::to_owned),
            duration,
            channels,
        })
    }

    /// Where `skin`'s joints begin among the rig's, adding them the first time, each with its
    /// inverse bind matrix (the identity where the skin has none, as glTF says). A joint outside
    /// the scene never moves.
    fn skin_start(&mut self, skin: &gltf::Skin<'_>, buffers: &[gltf::buffer::Data]) -> u16 {
        if let Some(&start) = self.skins.get(&skin.index()) {
            return start;
        }
        let start = u16::try_from(self.joints.len()).expect("joints fit u16");
        let reader = skin.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
        let mut binds = reader.read_inverse_bind_matrices().into_iter().flatten();
        for joint in skin.joints() {
            self.joints.push(Joint {
                node: self.index_of.get(&joint.index()).copied(),
                inverse_bind: binds.next().unwrap_or(IDENTITY),
            });
        }
        self.skins.insert(skin.index(), start);
        start
    }

    /// The joint carrying what `node` carries rigidly (the joint that never moves for `None`),
    /// added the first time.
    fn carrier(&mut self, node: Option<usize>) -> u16 {
        if let Some(&joint) = self.carriers.get(&node) {
            return joint;
        }
        let joint = u16::try_from(self.joints.len()).expect("joints fit u16");
        self.joints.push(Joint {
            node,
            inverse_bind: node.map_or(IDENTITY, |n| skeleton::inverse(&self.world[n])),
        });
        self.carriers.insert(node, joint);
        joint
    }
}

/// `indices` as a triangle list for a primitive drawn as `mode`; `None` for points and lines.
fn triangle_list(mode: gltf::mesh::Mode, indices: &[u32]) -> Option<Vec<u32>> {
    use gltf::mesh::Mode;
    let n = indices.len();
    Some(match mode {
        Mode::Triangles => indices[..n - n % 3].to_vec(),
        // Every other triangle of a strip is wound backwards; turning it keeps them all facing
        // the way the first does.
        Mode::TriangleStrip => (2..n)
            .flat_map(|k| {
                let [a, b, c] = [indices[k - 2], indices[k - 1], indices[k]];
                if k % 2 == 0 { [a, b, c] } else { [b, a, c] }
            })
            .collect(),
        Mode::TriangleFan => (2..n)
            .flat_map(|k| [indices[k - 1], indices[k], indices[0]])
            .collect(),
        Mode::Points | Mode::Lines | Mode::LineLoop | Mode::LineStrip => return None,
    })
}

fn material(m: &gltf::Material<'_>) -> Material {
    let pbr = m.pbr_metallic_roughness();
    let reference = |info: gltf::texture::Info<'_>| TextureRef {
        image: info.texture().source().index(),
        uv_set: info.tex_coord(),
    };
    let strength = m.emissive_strength().unwrap_or(1.0);
    Material {
        name: m.name().map(str::to_owned),
        base_color: pbr.base_color_factor(),
        base_color_texture: pbr.base_color_texture().map(reference),
        metallic: pbr.metallic_factor(),
        roughness: pbr.roughness_factor(),
        metallic_roughness_texture: pbr.metallic_roughness_texture().map(reference),
        normal_texture: m.normal_texture().map(|t| {
            let at = TextureRef {
                image: t.texture().source().index(),
                uv_set: t.tex_coord(),
            };
            (at, t.scale())
        }),
        occlusion_texture: m.occlusion_texture().map(|t| {
            let at = TextureRef {
                image: t.texture().source().index(),
                uv_set: t.tex_coord(),
            };
            (at, t.strength())
        }),
        emissive: m.emissive_factor().map(|c| c * strength),
        emissive_texture: m.emissive_texture().map(reference),
        alpha: match m.alpha_mode() {
            gltf::material::AlphaMode::Opaque => Alpha::Opaque,
            gltf::material::AlphaMode::Mask => Alpha::Mask(m.alpha_cutoff().unwrap_or(0.5)),
            gltf::material::AlphaMode::Blend => Alpha::Blend,
        },
        double_sided: m.double_sided(),
    }
}

fn image(
    image: &gltf::Image<'_>,
    base: &Path,
    buffers: &[gltf::buffer::Data],
) -> Result<Image, String> {
    match image.source() {
        gltf::image::Source::View { view, mime_type } => {
            let buffer = buffers
                .get(view.buffer().index())
                .ok_or_else(|| format!("image {}: no buffer", image.index()))?;
            let bytes = buffer
                .get(view.offset()..view.offset() + view.length())
                .ok_or_else(|| format!("image {}: view outside its buffer", image.index()))?;
            Ok(Image::Embedded {
                mime_type: Some(mime_type.to_owned()),
                bytes: bytes.to_vec(),
            })
        }
        gltf::image::Source::Uri { uri, mime_type } => match uri.strip_prefix("data:") {
            Some(data) => {
                let (header, encoded) = data
                    .split_once(',')
                    .ok_or_else(|| format!("image {}: a data URI without data", image.index()))?;
                let header = header
                    .strip_suffix(";base64")
                    .ok_or_else(|| format!("image {}: a data URI not in base64", image.index()))?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|e| format!("image {}: {e}", image.index()))?;
                let stated = mime_type.or((!header.is_empty()).then_some(header));
                Ok(Image::Embedded {
                    mime_type: stated.map(str::to_owned),
                    bytes,
                })
            }
            None => Ok(Image::File(base.join(percent_decoded(uri)))),
        },
    }
}

/// A relative URI's path with its `%XX` escapes decoded.
fn percent_decoded(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut k = 0;
    while k < bytes.len() {
        let hex = bytes
            .get(k + 1..k + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[k], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                k += 3;
            }
            (byte, _) => {
                out.push(byte);
                k += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    std::array::from_fn(|column| {
        std::array::from_fn(|row| (0..4).map(|k| a[k][row] * b[column][k]).sum())
    })
}

/// `columns` times `v`.
fn apply(columns: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    add(
        add(scale(columns[0], v[0]), scale(columns[1], v[1])),
        scale(columns[2], v[2]),
    )
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    a.map(|c| c * s)
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// `a` made unit length; +Z when it has none.
fn normalize(a: [f32; 3]) -> [f32; 3] {
    let length = dot(a, a).sqrt();
    if length > 1e-12 {
        scale(a, 1.0 / length)
    } else {
        [0.0, 0.0, 1.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrices_multiply_column_major() {
        let translate: Matrix = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [5.0, 0.0, 0.0, 1.0],
        ];
        let double: Matrix = std::array::from_fn(|c| {
            std::array::from_fn(|r| {
                if c == r {
                    if c < 3 { 2.0 } else { 1.0 }
                } else {
                    0.0
                }
            })
        });
        // Scale first, then move: the translation is unscaled.
        let m = multiply(&translate, &double);
        assert_eq!(m[3], [5.0, 0.0, 0.0, 1.0]);
        assert_eq!(m[0], [2.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn uris_are_percent_decoded() {
        assert_eq!(percent_decoded("tex%20one/a%2Bb.png"), "tex one/a+b.png");
        assert_eq!(percent_decoded("100%"), "100%");
    }

    #[test]
    fn strips_and_fans_unroll_facing_one_way() {
        use gltf::mesh::Mode;
        assert_eq!(
            triangle_list(Mode::TriangleStrip, &[0, 1, 2, 3]),
            Some(vec![0, 1, 2, 2, 1, 3])
        );
        assert_eq!(
            triangle_list(Mode::TriangleFan, &[0, 1, 2, 3]),
            Some(vec![1, 2, 0, 2, 3, 0])
        );
        assert_eq!(triangle_list(Mode::Lines, &[0, 1]), None);
    }
}
