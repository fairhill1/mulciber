//! Overlay geometry: text, images and flat shapes as vertices for one material record.

use mulciber::{
    GeometrySource, MeshIndices, TransientGeometry, VertexAttribute, VertexFormat, VertexLayout,
};

use crate::atlas::{Atlas, Region};
use crate::layout::Style;

/// Bytes per vertex: position, texture coordinate and colour.
pub const VERTEX_STRIDE: u32 = 32;

/// The vertex layout of a [`Batch`]'s vertices, for the overlay pipeline's
/// `MaterialPipelineDescriptor::vertex_layout`. It is what `mulciber::overlay`'s `OverlayVertex`
/// declares in WGSL:
///
/// - location 0, `vec2<f32>`: clip-space position;
/// - location 1, `vec2<f32>`: atlas texture coordinate;
/// - location 2, `vec4<f32>`: linear colour with straight (not premultiplied) alpha.
pub const VERTEX_LAYOUT: VertexLayout<'static> = VertexLayout {
    stride: VERTEX_STRIDE,
    attributes: &[
        VertexAttribute {
            location: 0,
            format: VertexFormat::Float32x2,
            offset: 0,
        },
        VertexAttribute {
            location: 1,
            format: VertexFormat::Float32x2,
            offset: 8,
        },
        VertexAttribute {
            location: 2,
            format: VertexFormat::Float32x4,
            offset: 16,
        },
    ],
};

/// A frame's overlay geometry for a surface of known size, built in pixels from its top-left
/// corner and stored in clip space.
///
/// Everything samples the one atlas, so one material record draws it all: glyphs and coverage
/// masks white in the colour given, images in their own colour tinted by it, flat shapes from the
/// atlas's solid block. Colours are linear with straight alpha; the overlay shader premultiplies.
/// Triangles turn counter-clockwise on screen, as Mulciber's pipelines cull back faces, and are
/// drawn in the order added: later over earlier.
#[derive(Debug)]
pub struct Batch<'a> {
    atlas: &'a Atlas,
    width: f32,
    height: f32,
    vertices: Vec<u8>,
    indices: Vec<u32>,
}

impl<'a> Batch<'a> {
    /// An empty batch for a surface `width` × `height` pixels.
    #[must_use]
    pub fn new(atlas: &'a Atlas, width: u32, height: u32) -> Self {
        #[allow(clippy::cast_precision_loss)]
        Self {
            atlas,
            width: width.max(1) as f32,
            height: height.max(1) as f32,
            vertices: Vec::new(),
            indices: Vec::new(),
        }
    }

    /// Whether nothing has been added; a record needs at least one triangle.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// The vertex bytes, laid out as [`VERTEX_LAYOUT`].
    #[must_use]
    pub fn vertices(&self) -> &[u8] {
        &self.vertices
    }

    /// The triangle list's indices.
    #[must_use]
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// The batch as a material record's geometry, or `None` when it is empty.
    #[must_use]
    pub fn geometry(&self) -> Option<GeometrySource<'_>> {
        (!self.is_empty()).then(|| {
            GeometrySource::Transient(TransientGeometry {
                vertices: &self.vertices,
                indices: MeshIndices::U32(&self.indices),
            })
        })
    }

    /// `text` set in `style` with its first character's pen at `left` and its glyphs on
    /// `baseline`, in `color`. Returns where the pen ends, for setting more after it.
    ///
    /// At scale 1 the pen and baseline round to whole pixels, so each glyph texel lands on one
    /// pixel. A character the face lacks advances as a space.
    pub fn text(
        &mut self,
        style: impl Into<Style>,
        text: &str,
        left: f32,
        baseline: f32,
        color: [f32; 4],
    ) -> f32 {
        let style = style.into();
        // Exactly the baked size: anything else lands between pixels anyway.
        #[allow(clippy::float_cmp)]
        let snap = style.scale == 1.0;
        let (mut pen, baseline) = if snap {
            (left.round(), baseline.round())
        } else {
            (left, baseline)
        };
        let mut first = true;
        for character in text.chars() {
            let Some(&glyph) = self.atlas.glyph(style.face, character) else {
                continue;
            };
            if !first {
                pen += style.tracking;
            }
            first = false;
            if glyph.region.width > 0 && glyph.region.height > 0 {
                #[allow(clippy::cast_precision_loss)]
                let (width, height) = (
                    glyph.region.width as f32 * style.scale,
                    glyph.region.height as f32 * style.scale,
                );
                let mut x = pen + glyph.xmin * style.scale;
                let mut bottom = baseline - glyph.ymin * style.scale;
                if snap {
                    (x, bottom) = (x.round(), bottom.round());
                }
                self.image(glyph.region, [x, bottom - height, x + width, bottom], color);
            }
            pen += glyph.advance * style.scale;
        }
        pen
    }

    /// `text` set in `style` and turned `angle` radians clockwise on the screen about `centre`:
    /// the middle of the line across, and of its capitals from the baseline up. A stamp, or a
    /// label along a slope.
    ///
    /// Turned glyphs land between pixels, so their edges are rougher than level text's; a linear
    /// sampler smooths them.
    pub fn text_turned(
        &mut self,
        style: impl Into<Style>,
        text: &str,
        centre: [f32; 2],
        angle: f32,
        color: [f32; 4],
    ) {
        let style = style.into();
        let (sin, cos) = angle.sin_cos();
        let turn =
            |[x, y]: [f32; 2]| [centre[0] + x * cos - y * sin, centre[1] + x * sin + y * cos];
        let middle = self.atlas.metrics(style.face).cap_height * style.scale * 0.5;
        let mut pen = -self.atlas.measure(style, text) * 0.5;
        let mut first = true;
        for character in text.chars() {
            let Some(&glyph) = self.atlas.glyph(style.face, character) else {
                continue;
            };
            if !first {
                pen += style.tracking;
            }
            first = false;
            if glyph.region.width > 0 && glyph.region.height > 0 {
                #[allow(clippy::cast_precision_loss)]
                let (width, height) = (
                    glyph.region.width as f32 * style.scale,
                    glyph.region.height as f32 * style.scale,
                );
                let left = pen + glyph.xmin * style.scale;
                let bottom = middle - glyph.ymin * style.scale;
                let top = bottom - height;
                self.quad(
                    glyph.region,
                    [
                        turn([left, top]),
                        turn([left + width, top]),
                        turn([left + width, bottom]),
                        turn([left, bottom]),
                    ],
                    color,
                );
            }
            pen += glyph.advance * style.scale;
        }
    }

    /// `region` of the atlas stretched over `rect` (left, top, right, bottom), tinted by `color`:
    /// an icon, a picture, a mask.
    pub fn image(&mut self, region: Region, [left, top, right, bottom]: [f32; 4], color: [f32; 4]) {
        self.quad(
            region,
            [[left, top], [right, top], [right, bottom], [left, bottom]],
            color,
        );
    }

    /// Triangles of the atlas's solid block, each corner in its own colour (linear, straight
    /// alpha) and blended between them: for shapes the others don't make, such as rings, arcs and
    /// gradients. `triangles` index `corners`, wound either way.
    ///
    /// Their edges are not softened; fade a rim of corners to clear for that, as
    /// [`polygon`](Self::polygon) does.
    ///
    /// # Panics
    ///
    /// When a triangle indexes past `corners`, or with more vertices in the batch than `u32`
    /// indices reach.
    pub fn mesh(&mut self, corners: &[([f32; 2], [f32; 4])], triangles: &[[u32; 3]]) {
        let uv = self.atlas.solid_uv();
        let first = self.next_index();
        let count = u32::try_from(corners.len()).expect("a mesh's corners fit u32");
        for &(position, color) in corners {
            self.vertex(position, uv, color);
        }
        for &[a, b, c] in triangles {
            assert!(
                a < count && b < count && c < count,
                "a triangle's corners {a}, {b}, {c} index past the {count} given"
            );
            self.triangle(first + a, first + b, first + c);
        }
    }

    /// The vertex bytes and indices, taken out of the batch: for geometry kept past the atlas's
    /// borrow, such as a frame built in one place and recorded in another.
    #[must_use]
    pub fn into_parts(self) -> (Vec<u8>, Vec<u32>) {
        (self.vertices, self.indices)
    }

    /// A filled convex polygon, its corners in either winding.
    ///
    /// Its edges are softened across one pixel, half inside and half outside: the overlay pass
    /// renders one sample a pixel, so nothing else smooths a slanted edge, and an edge along whole
    /// pixels stays crisp.
    ///
    /// # Panics
    ///
    /// With more corners, or more vertices in the batch, than `u32` indices reach.
    pub fn polygon(&mut self, points: &[[f32; 2]], color: [f32; 4]) {
        let n = points.len();
        if n < 3 {
            return;
        }
        let uv = self.atlas.solid_uv();
        let area: f32 = (0..n).map(|k| cross(points[k], points[(k + 1) % n])).sum();
        let side = if area > 0.0 { 1.0 } else { -1.0 };
        let outward = |k: usize| {
            let [x, y] = normalised(sub(points[(k + 1) % n], points[k]));
            [y * side, -x * side]
        };
        let clear = [color[0], color[1], color[2], 0.0];
        let first = self.next_index();
        for (k, &point) in points.iter().enumerate() {
            // Out along both edges' normals at once, as far as half a pixel from each.
            let (before, after) = (outward((k + n - 1) % n), outward(k));
            let mut out = [
                f32::midpoint(before[0], after[0]),
                f32::midpoint(before[1], after[1]),
            ];
            let square = out[0] * out[0] + out[1] * out[1];
            if square > 1.0e-6 {
                let scale = 1.0 / square.max(0.25);
                out = [out[0] * scale, out[1] * scale];
            }
            self.vertex(
                [point[0] - out[0] * 0.5, point[1] - out[1] * 0.5],
                uv,
                color,
            );
            self.vertex(
                [point[0] + out[0] * 0.5, point[1] + out[1] * 0.5],
                uv,
                clear,
            );
        }
        let count = u32::try_from(n).expect("a polygon's corners fit u32");
        let inner = |k: u32| first + 2 * (k % count);
        let outer = |k: u32| first + 2 * (k % count) + 1;
        for k in 1..count - 1 {
            self.triangle(inner(0), inner(k), inner(k + 1));
        }
        for k in 0..count {
            self.triangle(inner(k), outer(k), outer(k + 1));
            self.triangle(inner(k), outer(k + 1), inner(k + 1));
        }
    }

    /// A filled rectangle: left, top, right, bottom.
    pub fn rect(&mut self, [left, top, right, bottom]: [f32; 4], color: [f32; 4]) {
        self.polygon(
            &[[left, top], [right, top], [right, bottom], [left, bottom]],
            color,
        );
    }

    /// A straight line `width` pixels wide from `from` to `to`, square at its ends.
    pub fn line(&mut self, from: [f32; 2], to: [f32; 2], width: f32, color: [f32; 4]) {
        let [x, y] = normalised(sub(to, from));
        let side = [-y * width * 0.5, x * width * 0.5];
        self.polygon(
            &[
                [from[0] - side[0], from[1] - side[1]],
                [to[0] - side[0], to[1] - side[1]],
                [to[0] + side[0], to[1] + side[1]],
                [from[0] + side[0], from[1] + side[1]],
            ],
            color,
        );
    }

    /// A filled circle, with enough sides that none shows.
    pub fn circle(&mut self, centre: [f32; 2], radius: f32, color: [f32; 4]) {
        if radius <= 0.0 {
            return;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let sides = (radius * 1.5).clamp(12.0, 128.0) as usize;
        #[allow(clippy::cast_precision_loss)]
        let points: Vec<[f32; 2]> = (0..sides)
            .map(|k| {
                let angle = std::f32::consts::TAU * k as f32 / sides as f32;
                [
                    centre[0] + radius * angle.cos(),
                    centre[1] + radius * angle.sin(),
                ]
            })
            .collect();
        self.polygon(&points, color);
    }

    /// The surface's width and height in pixels.
    pub(crate) const fn surface(&self) -> [f32; 2] {
        [self.width, self.height]
    }

    pub(crate) const fn atlas(&self) -> &Atlas {
        self.atlas
    }

    /// `region` stretched over the quadrilateral `corners`: its top left, top right, bottom
    /// right and bottom left.
    fn quad(&mut self, region: Region, corners: [[f32; 2]; 4], color: [f32; 4]) {
        let [u0, v0, u1, v1] = self.atlas.uv(region);
        let first = self.vertex(corners[0], [u0, v0], color);
        self.vertex(corners[1], [u1, v0], color);
        self.vertex(corners[2], [u1, v1], color);
        self.vertex(corners[3], [u0, v1], color);
        self.triangle(first, first + 1, first + 2);
        self.triangle(first, first + 2, first + 3);
    }

    fn next_index(&self) -> u32 {
        u32::try_from(self.vertices.len() / VERTEX_STRIDE as usize)
            .expect("a batch's vertices fit u32 indices")
    }

    /// Adds a vertex at `position` in pixels and returns its index.
    fn vertex(&mut self, [x, y]: [f32; 2], uv: [f32; 2], color: [f32; 4]) -> u32 {
        let index = self.next_index();
        let clip = [x / self.width * 2.0 - 1.0, 1.0 - y / self.height * 2.0];
        for value in clip.into_iter().chain(uv).chain(color) {
            self.vertices.extend_from_slice(&value.to_ne_bytes());
        }
        index
    }

    fn clip_position(&self, index: u32) -> [f32; 2] {
        let at = index as usize * VERTEX_STRIDE as usize;
        let float = |offset: usize| {
            f32::from_ne_bytes(
                self.vertices[at + offset..at + offset + 4]
                    .try_into()
                    .expect("four bytes"),
            )
        };
        [float(0), float(4)]
    }

    /// A triangle of added vertices, wound counter-clockwise in clip space whichever way they
    /// came.
    fn triangle(&mut self, a: u32, b: u32, c: u32) {
        let (pa, pb, pc) = (
            self.clip_position(a),
            self.clip_position(b),
            self.clip_position(c),
        );
        if cross(sub(pb, pa), sub(pc, pa)) >= 0.0 {
            self.indices.extend([a, b, c]);
        } else {
            self.indices.extend([a, c, b]);
        }
    }
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn cross(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[1] - a[1] * b[0]
}

fn normalised([x, y]: [f32; 2]) -> [f32; 2] {
    let length = x.hypot(y);
    if length > 0.0 {
        [x / length, y / length]
    } else {
        [0.0, 0.0]
    }
}
