//! Text and flat shapes over a Mulciber scene: the mechanics a game's HUD, menus and chat share,
//! without their look.
//!
//! The game brings its fonts, sizes, character sets, colours and layout; this crate rasterises
//! them and builds the geometry.
//!
//! - [`AtlasBuilder`] rasterises each [`Font`] the game supplies at the pixel sizes it asks for,
//!   with the characters it asks for, into one RGBA8 sRGB [`Atlas`]: glyphs white with coverage in
//!   alpha, a solid block for flat shapes, and any coverage masks or colour images the game packs
//!   beside them (icons, keycaps, pictures).
//! - [`Atlas::measure`], [`Atlas::wrap`] and [`Atlas::fit`] measure text, break it into lines and
//!   cut it short with an ellipsis, all without a device.
//! - [`Batch`] builds one frame's overlay geometry in pixels from the surface's top-left corner:
//!   text, images, convex polygons, rectangles, lines and circles, edges softened across a pixel.
//!   It becomes one material record's transient geometry, laid out as [`VERTEX_LAYOUT`].
//! - [`Batch::rasterise`] draws it in software as the overlay shader does, so a screen can be
//!   tested or previewed with no GPU.
//!
//! The shader is `mulciber-shader`'s `mulciber::overlay` WGSL module: a game's overlay shader
//! declares the atlas texture and sampler and calls `overlay_raster` and `overlay_shade`.
//! See the README for the pipeline and a frame.

mod atlas;
mod batch;
mod layout;
mod raster;

pub use atlas::{Atlas, AtlasBuilder, FaceId, FaceMetrics, Font, Region, TextError};
pub use batch::{Batch, VERTEX_LAYOUT, VERTEX_STRIDE};
pub use layout::Style;
