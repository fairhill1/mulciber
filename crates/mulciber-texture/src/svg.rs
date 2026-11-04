//! A sheet of vector pictures, each a group by id drawn about its own origin, rasterised with resvg
//! at the size a game shows it: crisp at any window size, with no bake to keep in step with the
//! source. The game names the ids it needs when it reads the sheet, so a picture renamed or lost
//! from the sheet is an error on loading rather than a blank on screen.
//!
//! A picture is drawn about the origin of the group it is, wherever the sheet places that group:
//! a badge drawn round (0, 0) and moved to the sheet's (240, 120) to be seen while editing is drawn
//! about its own middle. Text in the sheet is not drawn (resvg's `text` feature is off); a sheet
//! should have its type converted to paths.

use crate::TextureError;
use resvg::{tiny_skia, usvg};
use std::path::Path;

/// A parsed SVG sheet whose pictures are drawn by id ([`SvgSheet::draw`]).
pub struct SvgSheet {
    tree: usvg::Tree,
}

impl SvgSheet {
    /// The sheet at `path`, which must have every picture in `required`.
    ///
    /// # Errors
    ///
    /// The file can't be read, isn't SVG, or lacks one of `required`; the message names the file.
    pub fn read(path: &Path, required: &[&str]) -> Result<Self, TextureError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| TextureError::new(format!("{}: {e}", path.display())))?;
        Self::parse(&text, required)
            .map_err(|e| TextureError::new(format!("{}: {}", path.display(), e.message())))
    }

    /// The sheet `text`, which must have every picture in `required`.
    ///
    /// # Errors
    ///
    /// `text` isn't SVG, or lacks one of `required`.
    pub fn parse(text: &str, required: &[&str]) -> Result<Self, TextureError> {
        let tree = usvg::Tree::from_str(text, &usvg::Options::default())
            .map_err(|e| TextureError::new(e.to_string()))?;
        let sheet = Self { tree };
        if let Some(missing) = required.iter().find(|id| !sheet.has(id)) {
            return Err(TextureError::new(format!("no picture {missing:?}")));
        }
        Ok(sheet)
    }

    /// Whether the sheet has a picture `id`.
    #[must_use]
    pub fn has(&self, id: &str) -> bool {
        self.tree.node_by_id(id).is_some()
    }

    /// The pictures `ids` drawn in order, their origin at the middle of a `width` x `height`
    /// canvas and `scale` pixels to a unit of the sheet's, over `under`: premultiplied sRGB RGBA
    /// of the canvas's size, or empty for a clear canvas. Returns RGBA, sRGB, straight alpha,
    /// rows from the top. A picture with nothing in it to draw is skipped.
    ///
    /// # Panics
    ///
    /// The sheet has no picture with one of `ids`, `width` or `height` is 0, or `under` is neither
    /// empty nor `width * height * 4` bytes.
    #[must_use]
    pub fn draw(&self, ids: &[&str], width: u32, height: u32, scale: f32, under: &[u8]) -> Vec<u8> {
        let mut pixmap = tiny_skia::Pixmap::new(width, height).expect("a canvas of some size");
        if !under.is_empty() {
            pixmap.data_mut().copy_from_slice(under);
        }
        #[allow(clippy::cast_precision_loss)]
        let place = tiny_skia::Transform::from_row(
            scale,
            0.0,
            0.0,
            scale,
            width as f32 * 0.5,
            height as f32 * 0.5,
        );
        for id in ids {
            let node = self
                .tree
                .node_by_id(id)
                .unwrap_or_else(|| panic!("no picture {id:?} on the sheet"));
            // resvg draws a node in its group's own frame, shifted to its bounds' corner: undo the
            // shift and the group's own placement on the sheet.
            let Some(bounds) = node.abs_layer_bounding_box() else {
                continue;
            };
            let own = match node {
                usvg::Node::Group(group) => group.transform(),
                _ => tiny_skia::Transform::identity(),
            };
            let Some(unplace) = own.invert() else {
                continue;
            };
            let transform = place
                .pre_concat(unplace)
                .pre_translate(bounds.x(), bounds.y());
            resvg::render_node(node, transform, &mut pixmap.as_mut());
        }
        let mut pixels = pixmap.take();
        for texel in pixels.as_chunks_mut::<4>().0 {
            let a = texel[3];
            if a > 0 && a < 255 {
                for c in &mut texel[..3] {
                    // Non-negative and at most 255, so the cast is exact.
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let straight = (f32::from(*c) * 255.0 / f32::from(a)).round().min(255.0) as u8;
                    *c = straight;
                }
            }
        }
        pixels
    }

    /// The stops of the linear gradient `id`: where each is along it (0 to 1) and its colour,
    /// sRGB, for a game drawing in code to match the sheet. `None` if the sheet has no linear
    /// gradient by that id. resvg drops a gradient nothing on the sheet is painted with.
    #[must_use]
    pub fn gradient(&self, id: &str) -> Option<Vec<(f32, [u8; 3])>> {
        self.tree
            .linear_gradients()
            .iter()
            .find(|g| g.id() == id)
            .map(|g| {
                g.stops()
                    .iter()
                    .map(|s| {
                        let c = s.color();
                        (s.offset().get(), [c.red, c.green, c.blue])
                    })
                    .collect()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ring round (0, 0) placed at (300, 200), a dot round (0, 0) placed at (50, 400) and
    /// shifted down 10 inside its group, and a gradient the ring is painted with.
    const SHEET: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="500" height="500">
  <defs>
    <linearGradient id="gold" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#ffd040"/>
      <stop offset="1" stop-color="#804000"/>
    </linearGradient>
  </defs>
  <g id="ring" transform="translate(300 200)">
    <circle r="90" fill="none" stroke="url(#gold)" stroke-width="20"/>
  </g>
  <g id="dot" transform="translate(50 400)">
    <circle cy="10" r="20" fill="#0000ff"/>
  </g>
</svg>"##;

    fn at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * width + x) * 4) as usize;
        pixels[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn draws_each_picture_about_its_own_origin() {
        let sheet = SvgSheet::parse(SHEET, &["ring", "dot"]).unwrap();
        // 100 units to a 64 pixel half-width: the ring's middle (r 90) 57.6 pixels out.
        let ring = sheet.draw(&["ring"], 128, 128, 0.64, &[]);
        for (x, y) in [(64, 6), (64, 121), (6, 64), (121, 64)] {
            assert_eq!(at(&ring, 128, x, y)[3], 255, "ring at {x}, {y}");
        }
        assert_eq!(at(&ring, 128, 64, 64)[3], 0, "clear in the middle");
        let dot = sheet.draw(&["dot"], 64, 64, 1.0, &[]);
        assert_eq!(
            at(&dot, 64, 32, 42),
            [0, 0, 255, 255],
            "the dot 10 below the middle"
        );
        assert_eq!(at(&dot, 64, 32, 15)[3], 0, "clear above it");
    }

    #[test]
    fn draws_a_wide_canvas_about_its_middle() {
        let sheet = SvgSheet::parse(SHEET, &[]).unwrap();
        let dot = sheet.draw(&["dot"], 200, 50, 1.0, &[]);
        assert_eq!(at(&dot, 200, 100, 35)[3], 255);
        assert_eq!(at(&dot, 200, 60, 35)[3], 0);
    }

    #[test]
    fn draws_over_what_is_under_and_returns_straight_alpha() {
        let sheet = SvgSheet::parse(SHEET, &[]).unwrap();
        // Half-covered red, premultiplied.
        let under = [128, 0, 0, 128].repeat(64 * 64);
        let drawn = sheet.draw(&["dot"], 64, 64, 1.0, &under);
        assert_eq!(at(&drawn, 64, 32, 42), [0, 0, 255, 255], "the dot over it");
        assert_eq!(
            at(&drawn, 64, 2, 2),
            [255, 0, 0, 128],
            "red, unpremultiplied"
        );
    }

    #[test]
    fn reads_a_gradients_stops() {
        let sheet = SvgSheet::parse(SHEET, &[]).unwrap();
        assert_eq!(
            sheet.gradient("gold"),
            Some(vec![(0.0, [0xff, 0xd0, 0x40]), (1.0, [0x80, 0x40, 0x00])])
        );
        assert_eq!(sheet.gradient("silver"), None);
    }

    #[test]
    fn refuses_a_sheet_without_a_required_picture() {
        let error = SvgSheet::parse(SHEET, &["ring", "star"]).err().unwrap();
        assert_eq!(error.message(), "no picture \"star\"");
        assert!(SvgSheet::parse("not svg", &[]).is_err());
    }
}
