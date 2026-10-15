//! Measuring, wrapping and fitting text set in a baked face, without a device.

use std::borrow::Cow;

use crate::atlas::{Atlas, FaceId};

/// How text is set: in which face, at what scale of its baked size, and with how much extra space
/// after each character.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    /// The baked face.
    pub face: FaceId,
    /// A multiple of the face's baked size. At 1 glyphs land on whole pixels, one atlas texel
    /// each, which is sharpest; other scales are for text that grows or shrinks in motion.
    pub scale: f32,
    /// Pixels added after each character but the last (letter-spacing, tracking).
    pub tracking: f32,
}

impl Style {
    /// `face` at its baked size, without tracking.
    #[must_use]
    pub const fn new(face: FaceId) -> Self {
        Self {
            face,
            scale: 1.0,
            tracking: 0.0,
        }
    }
}

impl From<FaceId> for Style {
    fn from(face: FaceId) -> Self {
        Self::new(face)
    }
}

impl Atlas {
    /// The width of `text` set in `style`, in pixels: each character's advance, a character the
    /// face lacks as a space, plus tracking between characters.
    #[must_use]
    pub fn measure(&self, style: impl Into<Style>, text: &str) -> f32 {
        let style = style.into();
        let mut count = 0_u16;
        let advances: f32 = text
            .chars()
            .inspect(|_| count = count.saturating_add(1))
            .map(|character| self.glyph(style.face, character).map_or(0.0, |g| g.advance))
            .sum();
        advances * style.scale + style.tracking * f32::from(count.saturating_sub(1))
    }

    /// `text` broken into lines no wider than `width` pixels in `style`, at spaces, and at every
    /// line break in it.
    ///
    /// Runs of spaces collapse to one. A word wider than `width` on its own keeps a line to itself
    /// and overruns it: breaking inside a word reads worse than one long name reaching the margin.
    /// An empty line in `text` stays an empty line.
    #[must_use]
    pub fn wrap(&self, style: impl Into<Style>, text: &str, width: f32) -> Vec<String> {
        let style = style.into();
        let mut lines = Vec::new();
        for paragraph in text.split('\n') {
            let mut line = String::new();
            for word in paragraph.split_whitespace() {
                if line.is_empty() {
                    line.push_str(word);
                    continue;
                }
                let candidate = format!("{line} {word}");
                if self.measure(style, &candidate) <= width {
                    line = candidate;
                } else {
                    lines.push(std::mem::replace(&mut line, word.to_owned()));
                }
            }
            lines.push(line);
        }
        lines
    }

    /// `text` as it fits in `width` pixels in `style`: whole when it fits, else cut at a character
    /// and ended with an ellipsis (`…`, or `...` when the face lacks it), trailing spaces before
    /// it dropped. Only the ellipsis is left when not even one character fits beside it.
    #[must_use]
    pub fn fit<'a>(&self, style: impl Into<Style>, text: &'a str, width: f32) -> Cow<'a, str> {
        let style = style.into();
        if self.measure(style, text) <= width {
            return Cow::Borrowed(text);
        }
        let ellipsis = if self.has_glyph(style.face, '…') {
            "…"
        } else {
            "..."
        };
        let mut cut: Vec<(usize, char)> = text.char_indices().collect();
        while cut.pop().is_some() {
            let end = cut
                .last()
                .map_or(0, |&(at, character)| at + character.len_utf8());
            let shown = format!("{}{ellipsis}", text[..end].trim_end());
            if self.measure(style, &shown) <= width {
                return Cow::Owned(shown);
            }
        }
        Cow::Borrowed(ellipsis)
    }
}
