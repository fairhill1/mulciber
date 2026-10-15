//! The atlas, layout and batch against a real font, and the batch drawn in software.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel and byte arithmetic in the checks"
)]

use mulciber::{GeometrySource, VertexFormat};
use mulciber_text::{Atlas, AtlasBuilder, Batch, FaceId, Font, Style, VERTEX_LAYOUT};

/// Fira Sans Medium, subset to Latin-1 and a few typographer's marks (SIL OFL, see
/// `fonts/FiraSans-OFL.txt`).
const FONT: &[u8] = include_bytes!("fonts/FiraSans-Medium-Latin1.ttf");

fn ascii() -> impl Iterator<Item = char> {
    ' '..='~'
}

fn atlas() -> (Atlas, FaceId, FaceId) {
    let font = Font::from_bytes(FONT).expect("the test font parses");
    let mut builder = AtlasBuilder::new(512).expect("an atlas");
    let text = builder
        .face(&font, 20.0, ascii().chain(['é', '…']))
        .expect("the text face");
    let large = builder.face(&font, 40.0, ascii()).expect("the large face");
    (builder.build(), text, large)
}

fn advance(atlas: &Atlas, face: FaceId, text: &str) -> f32 {
    text.chars()
        .map(|c| atlas.measure(face, &c.to_string()))
        .sum()
}

#[test]
fn faces_are_white_coverage_with_their_metrics() {
    let (atlas, text, large) = atlas();
    assert_eq!(atlas.width(), 512);
    assert_eq!(
        atlas.texels().len(),
        atlas.width() as usize * atlas.height() as usize * 4
    );
    // Every texel is white; glyph coverage is the alpha alone.
    assert!(
        atlas
            .texels()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|t| t[..3] == [255; 3])
    );
    assert!(
        atlas
            .texels()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|t| t[3] == 255)
    );
    for face in [text, large] {
        assert!(atlas.has_glyph(face, 'A'));
        let metrics = atlas.metrics(face);
        assert!(metrics.ascent > metrics.cap_height && metrics.cap_height > 0.0);
        assert!(metrics.line_height >= metrics.ascent + metrics.descent - 0.5);
    }
    assert!((atlas.metrics(large).px - 2.0 * atlas.metrics(text).px).abs() < f32::EPSILON);
    // Only what was asked for: é in the text face, not the large one; a character the font
    // lacks in neither.
    assert!(atlas.has_glyph(text, 'é') && !atlas.has_glyph(large, 'é'));
    assert!(!atlas.has_glyph(text, 'Ж'));
}

#[test]
fn a_face_or_image_that_cannot_fit_is_refused_by_name() {
    let font = Font::from_bytes(FONT).unwrap();
    assert!(Font::from_bytes(b"not a font").is_err());
    assert!(AtlasBuilder::new(4).is_err());
    let mut narrow = AtlasBuilder::new(24).unwrap();
    let error = narrow
        .face(&font, 96.0, ['W'])
        .expect_err("a W wider than the atlas");
    assert!(
        error.message().starts_with("'W' at 96 px"),
        "{}",
        error.message()
    );
    let mut builder = AtlasBuilder::new(64).unwrap();
    assert!(builder.face(&font, 0.0, ['A']).is_err());
    assert!(builder.image(2, 2, &[0; 15]).is_err());
    assert!(builder.coverage(2, 2, &[0; 3]).is_err());
    assert!(builder.image(70, 1, &[0; 280]).is_err());
}

#[test]
fn images_and_masks_pack_beside_the_glyphs() {
    let font = Font::from_bytes(FONT).unwrap();
    let mut builder = AtlasBuilder::new(64).unwrap();
    let face = builder.face(&font, 16.0, ascii()).unwrap();
    let red = [200, 10, 20, 255].repeat(9);
    let picture = builder.image(3, 3, &red).unwrap();
    let mask = builder.coverage(2, 1, &[128, 64]).unwrap();
    let atlas = builder.build();
    assert!(atlas.has_glyph(face, 'g'));
    let texel = |x: u32, y: u32| {
        let at = (y * atlas.width() + x) as usize * 4;
        &atlas.texels()[at..at + 4]
    };
    assert_eq!(texel(picture.x + 2, picture.y + 2), [200, 10, 20, 255]);
    assert_eq!(texel(mask.x, mask.y), [255, 255, 255, 128]);
    assert_eq!(texel(mask.x + 1, mask.y), [255, 255, 255, 64]);
    let [u0, v0, u1, v1] = atlas.uv(picture);
    assert!(u0 < u1 && v0 < v1 && u1 <= 1.0 && v1 <= 1.0);
}

#[test]
fn text_measures_by_advance_tracking_and_scale() {
    let (atlas, text, _) = atlas();
    let word = "Wave";
    let plain = atlas.measure(text, word);
    assert!((plain - advance(&atlas, text, word)).abs() < 1.0e-3);
    let tracked = Style {
        tracking: 2.0,
        ..Style::new(text)
    };
    assert!((atlas.measure(tracked, word) - (plain + 6.0)).abs() < 1.0e-3);
    let doubled = Style {
        scale: 2.0,
        ..Style::new(text)
    };
    assert!((atlas.measure(doubled, word) - 2.0 * plain).abs() < 1.0e-3);
    // A character the face lacks takes a space's room.
    assert!((atlas.measure(text, "aЖb") - atlas.measure(text, "a b")).abs() < 1.0e-3);
    assert!(atlas.measure(text, "").abs() < f32::EPSILON);
}

#[test]
fn wrapping_breaks_at_spaces_and_line_breaks() {
    let (atlas, text, _) = atlas();
    let width = atlas.measure(text, "the quick brown");
    let lines = atlas.wrap(text, "the quick brown fox  jumps over\nthe lazy dog", width);
    assert_eq!(lines, ["the quick brown", "fox jumps over", "the lazy dog"]);
    for line in &lines {
        assert!(atlas.measure(text, line) <= width);
    }
    // A word too long for the width keeps a line of its own; an empty line stays.
    let narrow = atlas.measure(text, "ab");
    assert_eq!(
        atlas.wrap(text, "a extraordinary b\n\nc", narrow),
        ["a", "extraordinary", "b", "", "c"]
    );
}

#[test]
fn fitting_cuts_with_an_ellipsis_only_when_needed() {
    let (atlas, text, large) = atlas();
    let name = "Verandah Café";
    let whole = atlas.measure(text, name);
    assert_eq!(atlas.fit(text, name, whole), name);
    let room = atlas.measure(text, "Verandah C");
    let fitted = atlas.fit(text, name, room);
    assert!(
        fitted.ends_with('…') && fitted.len() < name.len(),
        "{fitted}"
    );
    assert!(atlas.measure(text, &fitted) <= room);
    // The space before the cut goes with it.
    let fitted = atlas.fit(text, name, atlas.measure(text, "Verandah …"));
    assert_eq!(fitted, "Verandah…");
    // A face without the ellipsis uses three full stops; no room leaves the ellipsis alone.
    assert!(
        atlas
            .fit(large, name, atlas.measure(large, "Vera..."))
            .ends_with("...")
    );
    assert_eq!(atlas.fit(text, name, 1.0), "…");
}

#[test]
fn the_vertex_layout_is_the_overlay_shaders() {
    // mulciber-shader's overlay test holds `OverlayVertex` to the same locations and sizes.
    let inputs: Vec<(u32, u32)> = VERTEX_LAYOUT
        .attributes
        .iter()
        .map(|attribute| {
            let size = match attribute.format {
                VertexFormat::Float32x2 => 2,
                VertexFormat::Float32x4 => 4,
                other => panic!("an f32 vector, not {other:?}"),
            };
            (attribute.location, size)
        })
        .collect();
    assert_eq!(inputs, [(0, 2), (1, 2), (2, 4)]);
    assert_eq!(VERTEX_LAYOUT.stride, 32);
}

fn clip(batch: &Batch<'_>, index: u32) -> [f32; 2] {
    let at = index as usize * 32;
    let float =
        |k: usize| f32::from_ne_bytes(batch.vertices()[at + k..at + k + 4].try_into().unwrap());
    [float(0), float(4)]
}

#[test]
fn every_triangle_faces_the_screen() {
    let (atlas, text, _) = atlas();
    let mut batch = Batch::new(&atlas, 320, 200);
    assert!(batch.is_empty() && batch.geometry().is_none());
    batch.text(text, "Hello, world", 10.0, 40.0, [1.0; 4]);
    batch.rect([5.0, 5.0, 60.0, 20.0], [1.0, 0.0, 0.0, 1.0]);
    // Corners clockwise and anticlockwise on screen.
    batch.polygon(
        &[[100.0, 100.0], [140.0, 100.0], [120.0, 140.0]],
        [0.0, 1.0, 0.0, 1.0],
    );
    batch.polygon(
        &[[200.0, 100.0], [220.0, 140.0], [240.0, 100.0]],
        [0.0, 1.0, 0.0, 1.0],
    );
    batch.line([10.0, 150.0], [300.0, 190.0], 3.0, [1.0; 4]);
    batch.circle([280.0, 50.0], 20.0, [1.0; 4]);
    assert!(matches!(
        batch.geometry(),
        Some(GeometrySource::Transient(_))
    ));
    for triangle in batch.indices().as_chunks::<3>().0 {
        let [a, b, c] = triangle.map(|index| clip(&batch, index));
        let turn = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        assert!(turn >= 0.0, "a triangle turns clockwise in clip space");
    }
}

#[test]
fn text_lands_on_whole_pixels_and_returns_its_pen() {
    let (atlas, text, _) = atlas();
    let mut batch = Batch::new(&atlas, 200, 100);
    let end = batch.text(text, "Ab", 10.3, 50.6, [1.0; 4]);
    assert!((end - (10.0 + advance(&atlas, text, "Ab"))).abs() < 1.0e-3);
    let surface = [200.0_f32, 100.0];
    for index in 0..batch.vertices().len() as u32 / 32 {
        let [x, y] = clip(&batch, index);
        let pixels = [(x + 1.0) * 0.5 * surface[0], (1.0 - y) * 0.5 * surface[1]];
        for value in pixels {
            assert!(
                (value - value.round()).abs() < 1.0e-3,
                "{pixels:?} is between pixels"
            );
        }
    }
    // A space draws nothing but advances.
    let mut spaced = Batch::new(&atlas, 200, 100);
    let pen = spaced.text(text, " ", 0.0, 50.0, [1.0; 4]);
    assert!(spaced.is_empty() && pen > 0.0);
}

/// One linear channel encoded to an sRGB byte, as the rasteriser and a GPU's sRGB target do.
fn srgb(linear: f32) -> u8 {
    let encoded = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[test]
fn a_glyph_rasterises_texel_for_pixel_in_its_colour() {
    let font = Font::from_bytes(FONT).unwrap();
    let mut builder = AtlasBuilder::new(128).unwrap();
    let face = builder.face(&font, 24.0, ['R']).unwrap();
    let atlas = builder.build();
    let (width, height) = (64, 48);
    let mut batch = Batch::new(&atlas, width, height);
    batch.text(face, "R", 8.0, 36.0, [1.0, 1.0, 1.0, 1.0]);
    let image = batch.rasterise([0.0, 0.0, 0.0, 1.0]);
    // The glyph's coverage is the atlas's alpha, texel for pixel: white over black at that
    // coverage, encoded to sRGB.
    let alphas: Vec<u8> = atlas
        .texels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|t| t[3])
        .collect();
    let mut covered = 0;
    let lit: Vec<u8> = image.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
    let total_alpha: u32 = alphas.iter().map(|&a| u32::from(a)).sum();
    for pixel in &lit {
        if *pixel > 0 {
            covered += 1;
        }
    }
    assert!(covered > 20, "the R covers pixels");
    // Every coverage value in the atlas shows up as its encoded grey somewhere in the image,
    // and nothing brighter than full coverage.
    for &alpha in alphas.iter().filter(|&&a| a > 0 && a < 255) {
        let expected = srgb(f32::from(alpha) / 255.0);
        assert!(
            lit.contains(&expected),
            "coverage {alpha} is drawn as {expected}"
        );
    }
    let summed: u32 = lit
        .iter()
        .map(|&p| {
            let c = f32::from(p) / 255.0;
            let linear = if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            };
            (linear * 255.0).round() as u32
        })
        .sum();
    // Coverage is conserved: the image holds the glyph's coverage once, give or take rounding.
    assert!(
        summed.abs_diff(total_alpha - 16 * 255) <= u32::try_from(covered).unwrap(),
        "{summed} vs {total_alpha}"
    );
}

#[test]
fn shapes_on_whole_pixels_are_crisp_and_cover_each_pixel_once() {
    let (atlas, _, _) = atlas();
    let mut batch = Batch::new(&atlas, 40, 30);
    // Half-transparent red over black: a pixel covered twice would be darker or brighter.
    batch.rect([10.0, 5.0, 30.0, 25.0], [1.0, 0.0, 0.0, 0.5]);
    let image = batch.rasterise([0.0, 0.0, 0.0, 1.0]);
    let pixel = |x: usize, y: usize| image.as_chunks::<4>().0[y * 40 + x];
    let half = srgb(0.5);
    for y in 0..30 {
        for x in 0..40 {
            let inside = (10..30).contains(&x) && (5..25).contains(&y);
            assert_eq!(
                pixel(x, y),
                if inside {
                    [half, 0, 0, 255]
                } else {
                    [0, 0, 0, 255]
                },
                "pixel {x}, {y}"
            );
        }
    }
    // The same triangle either way round fills the same pixels and leaves the same ones clear;
    // the softened rim splits into triangles along other diagonals, so its alpha may differ.
    let draw = |points: &[[f32; 2]]| {
        let mut batch = Batch::new(&atlas, 40, 30);
        batch.polygon(points, [0.2, 0.4, 1.0, 1.0]);
        let image = batch.rasterise([0.0, 0.0, 0.0, 0.0]);
        image
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[3])
            .collect::<Vec<u8>>()
    };
    let clockwise = draw(&[[5.0, 5.0], [35.0, 8.0], [12.0, 27.0]]);
    let anticlockwise = draw(&[[5.0, 5.0], [12.0, 27.0], [35.0, 8.0]]);
    for (a, b) in clockwise.iter().zip(&anticlockwise) {
        assert_eq!(*a == 255, *b == 255);
        assert_eq!(*a == 0, *b == 0);
    }
    assert!(clockwise.contains(&255));
    // The slanted edges are softened: some pixels part covered.
    assert!(clockwise.iter().any(|&a| a > 0 && a < 255));
}

#[test]
fn images_draw_in_their_own_colour_tinted() {
    let mut builder = AtlasBuilder::new(32).unwrap();
    let picture = builder.image(2, 2, &[51, 102, 204, 255].repeat(4)).unwrap();
    let atlas = builder.build();
    let mut batch = Batch::new(&atlas, 8, 8);
    // Stretched from the texel centres, so every pixel reads the image alone.
    batch.image(picture, [0.0, 0.0, 8.0, 8.0], [1.0, 1.0, 1.0, 1.0]);
    let image = batch.rasterise([0.0, 0.0, 0.0, 1.0]);
    let middle = image.as_chunks::<4>().0[4 * 8 + 4];
    assert_eq!(middle, [51, 102, 204, 255]);
    let mut tinted = Batch::new(&atlas, 8, 8);
    tinted.image(picture, [0.0, 0.0, 8.0, 8.0], [0.0, 1.0, 1.0, 1.0]);
    assert_eq!(
        tinted.rasterise([0.0, 0.0, 0.0, 1.0]).as_chunks::<4>().0[36][0],
        0
    );
}
