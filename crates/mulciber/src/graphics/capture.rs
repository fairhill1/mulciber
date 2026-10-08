//! Presented-frame capture results and the byte-order normalization both backends share.
use std::vec::Vec;

/// The final color of one presented frame, read back from the presentable image after every pass
/// of its submission.
///
/// Requested with [`Surface::request_frame_capture`](super::Surface::request_frame_capture) and
/// drained with [`Surface::take_frame_capture`](super::Surface::take_frame_capture).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameCapture {
    index: u64,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl FrameCapture {
    /// Zero-based position of the captured frame among the session's presented frames, the same
    /// index [`PresentedFrame::index`](super::PresentedFrame::index) and GPU timing report.
    #[must_use]
    pub const fn index(&self) -> u64 {
        self.index
    }

    /// Width in pixels: the presentable extent the frame was rendered at.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels: the presentable extent the frame was rendered at.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Tightly packed RGBA8 pixels, `width * height * 4` bytes, rows top to bottom and pixels
    /// left to right.
    ///
    /// Color channels are the sRGB-encoded bytes the display receives, whatever the native
    /// presentable format's channel order. Alpha is 255 wherever the presentation engine
    /// composites the surface opaque, which is what the display shows; otherwise it is the
    /// stored alpha the compositor blends with.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Consumes the capture and returns its pixels without copying.
    #[must_use]
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }
}

/// Channel order of a native four-byte presentable texel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaptureByteOrder {
    /// Red, green, blue, alpha: `R8G8B8A8_*`.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    Rgba,
    /// Blue, green, red, alpha: `B8G8R8A8_*` and Metal's `BGRA8Unorm_sRGB`.
    Bgra,
}

/// Builds a capture from tightly packed native texels, reordering them in place into RGBA and
/// forcing alpha to 255 for an opaque-composited surface.
pub(crate) fn frame_capture_from_native(
    index: u64,
    width: u32,
    height: u32,
    mut pixels: Vec<u8>,
    order: CaptureByteOrder,
    opaque: bool,
) -> FrameCapture {
    debug_assert_eq!(
        Some(pixels.len()),
        usize::try_from(u64::from(width) * u64::from(height) * 4).ok()
    );
    for texel in pixels.as_chunks_mut::<4>().0 {
        if order == CaptureByteOrder::Bgra {
            texel.swap(0, 2);
        }
        if opaque {
            texel[3] = u8::MAX;
        }
    }
    FrameCapture {
        index,
        width,
        height,
        pixels,
    }
}

/// Bytes of a tightly packed four-byte-per-texel capture, or `None` past the address space.
pub(crate) fn capture_byte_len(width: u32, height: u32) -> Option<usize> {
    u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .filter(|bytes| *bytes <= isize::MAX.cast_unsigned())
}

#[cfg(test)]
mod tests {
    use super::{CaptureByteOrder, capture_byte_len, frame_capture_from_native};
    use std::vec;

    #[test]
    fn bgra_texels_are_reordered_into_rgba() {
        let capture = frame_capture_from_native(
            7,
            2,
            1,
            vec![10, 20, 30, 40, 1, 2, 3, 4],
            CaptureByteOrder::Bgra,
            false,
        );
        assert_eq!(capture.index(), 7);
        assert_eq!((capture.width(), capture.height()), (2, 1));
        assert_eq!(capture.pixels(), &[30, 20, 10, 40, 3, 2, 1, 4]);
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn rgba_texels_keep_their_order() {
        let capture = frame_capture_from_native(
            0,
            1,
            2,
            vec![10, 20, 30, 40, 1, 2, 3, 4],
            CaptureByteOrder::Rgba,
            false,
        );
        assert_eq!(capture.into_pixels(), vec![10, 20, 30, 40, 1, 2, 3, 4]);
    }

    #[test]
    fn an_opaque_surface_reports_the_alpha_it_displays() {
        let capture =
            frame_capture_from_native(0, 1, 1, vec![10, 20, 30, 0], CaptureByteOrder::Bgra, true);
        assert_eq!(capture.pixels(), &[30, 20, 10, 255]);
    }

    #[test]
    fn capture_size_is_checked_against_the_address_space() {
        assert_eq!(capture_byte_len(1920, 1080), Some(1920 * 1080 * 4));
        assert_eq!(capture_byte_len(0, 1080), Some(0));
        assert_eq!(capture_byte_len(u32::MAX, u32::MAX), None);
    }
}
