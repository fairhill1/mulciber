//! A software rasteriser for a batch, so a screen can be looked at, and tested, without a GPU.

use crate::atlas::Atlas;
use crate::batch::{Batch, VERTEX_STRIDE};

impl Batch<'_> {
    /// Draws the batch over `background` (linear, straight alpha) as the overlay shader would at
    /// one sample a pixel, and returns the surface as RGBA8 sRGB, row by row: for tests and
    /// previews on a machine with no GPU.
    ///
    /// It samples the atlas bilinearly with clamped edges, as a linear, clamped sampler does: the
    /// same as the README's nearest sampler for flat shapes and for text at its baked size on
    /// whole pixels, but smoother than it for scaled text and stretched images. It premultiplies
    /// and blends source-over in linear light, and fills a pixel whose centre lies inside a
    /// triangle or on its top or left edge, as GPUs do, so neighbouring triangles never cover a
    /// pixel twice. GPUs round differently in the last bit or so: compare its output with a
    /// tolerance.
    #[must_use]
    pub fn rasterise(&self, background: [f32; 4]) -> Vec<u8> {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (width, height) = (self.surface()[0] as usize, self.surface()[1] as usize);
        let [r, g, b, a] = background;
        let mut surface = vec![[r * a, g * a, b * a, a]; width * height];
        let vertex = |index: u32| self.raster_vertex(index);
        for triangle in self.indices().as_chunks::<3>().0 {
            let [v0, mut v1, mut v2] = triangle.map(vertex);
            if cross(sub(v1.position, v0.position), sub(v2.position, v0.position)) < 0.0 {
                std::mem::swap(&mut v1, &mut v2);
            }
            let area = cross(sub(v1.position, v0.position), sub(v2.position, v0.position));
            if area <= 0.0 {
                continue;
            }
            let xs = [v0.position[0], v1.position[0], v2.position[0]];
            let ys = [v0.position[1], v1.position[1], v2.position[1]];
            let span = |values: [f32; 3], limit: usize| {
                let low = values.iter().copied().fold(f32::MAX, f32::min);
                let high = values.iter().copied().fold(f32::MIN, f32::max);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let range = (low - 0.5).floor().max(0.0) as usize
                    ..((high - 0.5).ceil().max(0.0) as usize + 1).min(limit);
                range
            };
            for y in span(ys, height) {
                for x in span(xs, width) {
                    #[allow(clippy::cast_precision_loss)]
                    let centre = [x as f32 + 0.5, y as f32 + 0.5];
                    let edges = [(v1, v2), (v2, v0), (v0, v1)].map(|(from, to)| {
                        (
                            cross(sub(to.position, from.position), sub(centre, from.position)),
                            from,
                            to,
                        )
                    });
                    if !edges.iter().all(|&(w, from, to)| {
                        w > 0.0 || (w == 0.0 && top_left(from.position, to.position))
                    }) {
                        continue;
                    }
                    let weights = edges.map(|(w, _, _)| w / area);
                    let mix = |pick: fn(&RasterVertex) -> f32| {
                        weights[0] * pick(&v0) + weights[1] * pick(&v1) + weights[2] * pick(&v2)
                    };
                    let uv = [mix(|v| v.uv[0]), mix(|v| v.uv[1])];
                    let color = [
                        mix(|v| v.color[0]),
                        mix(|v| v.color[1]),
                        mix(|v| v.color[2]),
                        mix(|v| v.color[3]),
                    ];
                    let texel = sample(self.atlas(), uv);
                    let alpha = texel[3] * color[3];
                    let pixel = &mut surface[y * width + x];
                    for channel in 0..3 {
                        pixel[channel] = color[channel] * texel[channel] * alpha
                            + pixel[channel] * (1.0 - alpha);
                    }
                    pixel[3] = alpha + pixel[3] * (1.0 - alpha);
                }
            }
        }
        surface
            .into_iter()
            .flat_map(|[r, g, b, a]| {
                let straight = |c: f32| if a > 0.0 { c / a } else { 0.0 };
                [
                    encode_srgb(straight(r)),
                    encode_srgb(straight(g)),
                    encode_srgb(straight(b)),
                    unit_to_byte(a),
                ]
            })
            .collect()
    }

    fn raster_vertex(&self, index: u32) -> RasterVertex {
        let at = index as usize * VERTEX_STRIDE as usize;
        let float = |k: usize| {
            let offset = at + k * 4;
            f32::from_ne_bytes(
                self.vertices()[offset..offset + 4]
                    .try_into()
                    .expect("four bytes"),
            )
        };
        let [width, height] = self.surface();
        RasterVertex {
            position: [
                f32::midpoint(float(0), 1.0) * width,
                (1.0 - float(1)) * 0.5 * height,
            ],
            uv: [float(2), float(3)],
            color: [float(4), float(5), float(6), float(7)],
        }
    }
}

#[derive(Clone, Copy)]
struct RasterVertex {
    /// Pixels from the surface's top-left corner.
    position: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

/// Whether the edge `from` → `to` of a triangle wound clockwise on screen (y down) is a top edge
/// (level, along the top) or a left edge, which own the pixel centres lying on them.
fn top_left(from: [f32; 2], to: [f32; 2]) -> bool {
    let [dx, dy] = sub(to, from);
    (dy == 0.0 && dx > 0.0) || dy < 0.0
}

/// The atlas at `uv`, filtered bilinearly with clamped edges: linear colour and alpha.
fn sample(atlas: &Atlas, [u, v]: [f32; 2]) -> [f32; 4] {
    #[allow(clippy::cast_precision_loss)]
    let (width, height) = (atlas.width() as f32, atlas.height() as f32);
    let (x, y) = (u * width - 0.5, v * height - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let texel = |tx: f32, ty: f32| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (tx, ty) = (
            tx.clamp(0.0, width - 1.0) as usize,
            ty.clamp(0.0, height - 1.0) as usize,
        );
        let at = (ty * atlas.width() as usize + tx) * 4;
        let bytes = &atlas.texels()[at..at + 4];
        [
            decode_srgb(bytes[0]),
            decode_srgb(bytes[1]),
            decode_srgb(bytes[2]),
            f32::from(bytes[3]) / 255.0,
        ]
    };
    let corners = [
        (texel(x0, y0), (1.0 - fx) * (1.0 - fy)),
        (texel(x0 + 1.0, y0), fx * (1.0 - fy)),
        (texel(x0, y0 + 1.0), (1.0 - fx) * fy),
        (texel(x0 + 1.0, y0 + 1.0), fx * fy),
    ];
    let mut out = [0.0; 4];
    for (value, weight) in corners {
        for channel in 0..4 {
            out[channel] += value[channel] * weight;
        }
    }
    out
}

fn decode_srgb(byte: u8) -> f32 {
    let c = f32::from(byte) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn encode_srgb(linear: f32) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let encoded = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    unit_to_byte(encoded)
}

fn unit_to_byte(value: f32) -> u8 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    byte
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn cross(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[1] - a[1] * b[0]
}
