//! Recipes, bakes and the source fallback, on images written to a scratch directory.

use std::path::{Path, PathBuf};

use mulciber::{BlockCompression, Ktx2Texture};
use mulciber_texture::{
    BakeMode, Baked, Chain, Channel, Fallback, MaterialDefaults, MaterialMaps, Origin, Pixels,
    Recipe, SOURCE_DIGEST_KEY, bake, bake_materials, encode_bc5, encode_bc7, find_materials,
    mip_extent, run,
};

/// A fresh directory under the system temp dir, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "mulciber-texture-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn save_rgba(path: &Path, width: u32, height: u32, texel: impl Fn(u32, u32) -> [u8; 4]) {
    let image = image::RgbaImage::from_fn(width, height, |x, y| image::Rgba(texel(x, y)));
    image.save(path).unwrap();
}

fn save_grey(path: &Path, width: u32, height: u32, value: impl Fn(u32, u32) -> u8) {
    let image = image::GrayImage::from_fn(width, height, |x, y| image::Luma([value(x, y)]));
    image.save(path).unwrap();
}

/// A smooth colour field, the kind of content BC7 is meant to keep.
#[allow(clippy::cast_possible_truncation)]
fn gradient(x: u32, y: u32) -> [u8; 4] {
    [
        (x * 4 % 256) as u8,
        (y * 5 % 256) as u8,
        ((x + y) * 2 % 256) as u8,
        255,
    ]
}

/// Decodes BC7 blocks back to RGBA8 for a `width`×`height` level.
fn decode_bc7(blocks: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (bw, bh) = (width.div_ceil(4) as usize, height.div_ceil(4) as usize);
    let mut texels = vec![0_u8; width as usize * height as usize * 4];
    for by in 0..bh {
        for bx in 0..bw {
            let mut block = [0_u8; 64];
            bcdec_rs::bc7(&blocks[(by * bw + bx) * 16..][..16], &mut block, 16);
            for y in 0..4 {
                for x in 0..4 {
                    let (tx, ty) = (bx * 4 + x, by * 4 + y);
                    if tx < width as usize && ty < height as usize {
                        let at = (ty * width as usize + tx) * 4;
                        texels[at..at + 4].copy_from_slice(&block[(y * 4 + x) * 4..][..4]);
                    }
                }
            }
        }
    }
    texels
}

/// BC5 blocks decoded to tight red-green pairs.
fn decode_bc5(blocks: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (bw, bh) = (width.div_ceil(4) as usize, height.div_ceil(4) as usize);
    let mut texels = vec![0_u8; width as usize * height as usize * 2];
    for by in 0..bh {
        for bx in 0..bw {
            let mut block = [0_u8; 32];
            bcdec_rs::bc5(&blocks[(by * bw + bx) * 16..][..16], &mut block, 8, false);
            for y in 0..4 {
                for x in 0..4 {
                    let (tx, ty) = (bx * 4 + x, by * 4 + y);
                    if tx < width as usize && ty < height as usize {
                        let at = (ty * width as usize + tx) * 2;
                        texels[at..at + 2].copy_from_slice(&block[(y * 4 + x) * 2..][..2]);
                    }
                }
            }
        }
    }
    texels
}

#[allow(clippy::cast_precision_loss)]
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
        .sum::<f64>()
        / a.len() as f64;
    10.0 * (255.0_f64 * 255.0 / mse.max(1e-9)).log10()
}

#[test]
#[allow(clippy::cast_possible_truncation)]
fn channels_pack_from_images_and_constants() {
    let scratch = Scratch::new("pack");
    let normal = scratch.path("n.png");
    let rough = scratch.path("r.png");
    save_rgba(&normal, 4, 4, |x, _| [10 + x as u8, 20, 250, 255]);
    save_grey(&rough, 4, 4, |_, y| 100 + y as u8);
    let recipe = Recipe {
        channels: [
            Channel::Red(normal.clone()),
            Channel::Green(normal.clone()),
            Channel::Blue(normal.clone()),
            Channel::Grey(rough.clone()),
        ],
        chain: Chain::Normal,
        normals: None,
        output: scratch.path("n.ktx2"),
    };
    assert_eq!(recipe.sources(), vec![normal.as_path(), rough.as_path()]);
    let base = &recipe.build().unwrap().levels[0];
    assert_eq!(&base[..4], &[10, 20, 250, 100]);
    assert_eq!(&base[(3 * 4 + 2) * 4..][..4], &[12, 20, 250, 103]);

    let constant = Recipe {
        channels: [
            Channel::Grey(rough.clone()),
            Channel::Constant(255),
            Channel::Constant(0),
            Channel::Constant(255),
        ],
        chain: Chain::Linear,
        normals: None,
        output: scratch.path("m.ktx2"),
    };
    assert_eq!(
        &constant.build().unwrap().levels[0][..4],
        &[100, 255, 0, 255]
    );

    // Sources of two sizes are refused, naming both.
    save_grey(&rough, 8, 8, |_, _| 0);
    let error = recipe.build().unwrap_err().to_string();
    assert!(
        error.contains("n.png") && error.contains("r.png") && error.contains("one size"),
        "{error}"
    );
}

#[test]
fn the_digest_follows_sources_and_packing_but_not_paths() {
    let scratch = Scratch::new("digest");
    let a = scratch.path("a.png");
    save_rgba(&a, 4, 4, gradient);
    let recipe = Recipe::image(&a, Chain::Color, scratch.path("a.ktx2"));
    let digest = recipe.digest().unwrap();
    assert_eq!(recipe.digest().unwrap(), digest);
    let moved = scratch.path("b.png");
    std::fs::copy(&a, &moved).unwrap();
    assert_eq!(
        Recipe::image(&moved, Chain::Color, scratch.path("b.ktx2"))
            .digest()
            .unwrap(),
        digest
    );
    assert_ne!(
        Recipe::image(&a, Chain::Cutout, scratch.path("a.ktx2"))
            .digest()
            .unwrap(),
        digest
    );
    let mut packed = recipe.clone();
    packed.channels[3] = Channel::Constant(255);
    assert_ne!(packed.digest().unwrap(), digest);
    save_rgba(&a, 4, 4, |x, y| {
        let mut t = gradient(x, y);
        t[0] ^= 1;
        t
    });
    assert_ne!(recipe.digest().unwrap(), digest);
}

#[test]
fn bc7_keeps_a_smooth_image_and_pads_partial_blocks() {
    let scratch = Scratch::new("bc7");
    // 30x18: no level is a multiple of four.
    let source = scratch.path("g.png");
    save_rgba(&source, 30, 18, gradient);
    let levels = Recipe::image(&source, Chain::Linear, scratch.path("g.ktx2"))
        .build()
        .unwrap();
    let blocks = encode_bc7(&levels);
    assert_eq!(blocks.len(), levels.levels.len());
    for (level, (encoded, texels)) in (0_u32..).zip(blocks.iter().zip(&levels.levels)) {
        let (w, h) = (mip_extent(30, level), mip_extent(18, level));
        assert_eq!(
            encoded.len(),
            mulciber_texture::level_bytes(BlockCompression::Bc7Unorm, w, h)
        );
        let decoded = decode_bc7(encoded, w, h);
        let quality = psnr(&decoded, texels);
        assert!(quality > 35.0, "level {level} ({w}x{h}): {quality:.1} dB");
    }
}

#[test]
fn bc5_keeps_red_and_green_and_pads_partial_blocks() {
    // 6x5 is not a whole number of blocks, at the base or below it.
    let base: Vec<u8> = (0..5_u8)
        .flat_map(|y| (0..6_u8).map(move |x| (x, y)))
        .flat_map(|(x, y)| [40 + x * 30, 60 + y * 25, 255, 255])
        .collect();
    let levels = Chain::TangentNormal.build(6, 5, base);
    let blocks = encode_bc5(&levels);
    assert_eq!(blocks.len(), levels.levels.len());
    for (level, (encoded, texels)) in (0_u32..).zip(blocks.iter().zip(&levels.levels)) {
        let (width, height) = (mip_extent(6, level), mip_extent(5, level));
        assert_eq!(
            encoded.len(),
            width.div_ceil(4) as usize * height.div_ceil(4) as usize * 16
        );
        let decoded = decode_bc5(encoded, width, height);
        let wanted: Vec<u8> = texels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|texel| [texel[0], texel[1]])
            .collect();
        assert!(
            psnr(&decoded, &wanted) > 40.0,
            "level {level}: {}",
            psnr(&decoded, &wanted)
        );
    }
}

#[test]
fn a_material_bakes_its_normal_as_bc5_and_its_widened_roughness_as_bc7() {
    let scratch = Scratch::new("surface");
    let albedo = scratch.path("tile.png");
    save_rgba(&albedo, 8, 8, gradient);
    let bumps = |x: u32, _: u32| {
        if x.is_multiple_of(2) {
            [200, 128, 200, 255]
        } else {
            [55, 128, 200, 255]
        }
    };
    let normal_map = scratch.path("tile_normal.png");
    save_rgba(&normal_map, 8, 8, bumps);
    save_grey(&scratch.path("tile_rough.png"), 8, 8, |_, _| 60);
    let maps = MaterialMaps::beside(&albedo, &MaterialDefaults::default());
    let (normal, surface) = (maps.normal.unwrap(), maps.surface.unwrap());

    assert_eq!(surface.normals.as_deref(), Some(normal_map.as_path()));
    let levels = surface.build().unwrap();
    assert_eq!(
        levels.levels[0][2], 60,
        "the base's roughness is as authored"
    );
    assert!(levels.levels[1][2] > 100, "{}", levels.levels[1][2]);

    assert_eq!(bake(&normal, BakeMode::IfStale).unwrap(), Baked::Built);
    assert_eq!(bake(&surface, BakeMode::IfStale).unwrap(), Baked::Built);
    let compression = |recipe: &Recipe| {
        let prepared = recipe.prepare().unwrap();
        assert_eq!(prepared.origin, Origin::Bake);
        let Pixels::Ktx2(bytes) = &prepared.pixels else {
            panic!("{:?}", prepared.origin)
        };
        Ktx2Texture::parse(bytes).unwrap().compression()
    };
    assert_eq!(compression(&normal), BlockCompression::Bc5Unorm);
    assert_eq!(compression(&surface), BlockCompression::Bc7Unorm);

    // The normal map is a source of the surface though no channel reads it: changing it stales
    // the bake, and the fallback is a full chain of the same layout.
    assert!(surface.is_current().unwrap());
    save_rgba(&normal_map, 8, 8, |_, _| [128, 128, 255, 255]);
    assert!(!surface.is_current().unwrap());
    let fallback = surface.prepare().unwrap();
    assert_eq!(fallback.origin, Origin::Sources(Fallback::Stale));
    let Pixels::Rgba8 { levels, .. } = &fallback.pixels else {
        panic!("{:?}", fallback.origin)
    };
    assert_eq!(levels[1][2], 60);
}

#[test]
fn a_bake_beside_a_material_with_sources_is_not_read_as_shipped() {
    let scratch = Scratch::new("orphan");
    let albedo = scratch.path("wall.png");
    save_rgba(&albedo, 4, 4, gradient);
    save_grey(&scratch.path("wall_rough.png"), 4, 4, |_, _| 90);
    // A bake left by an older layout, whose map is gone: the material has no normal any more.
    std::fs::write(scratch.path("wall_normal.ktx2"), b"stale").unwrap();
    let maps = MaterialMaps::beside(&albedo, &MaterialDefaults::default());
    assert!(maps.normal.is_none());
    assert_eq!(
        &maps.surface.unwrap().build().unwrap().levels[0][..4],
        &[0, 255, 90, 255]
    );
}

#[test]
fn a_bake_is_used_while_current_and_the_sources_otherwise() {
    let scratch = Scratch::new("cycle");
    let source = scratch.path("wall.png");
    save_rgba(&source, 16, 8, |x, y| {
        let mut t = gradient(x, y);
        t[3] = if x == 0 { 100 } else { 255 };
        t
    });
    let recipe = Recipe::image(&source, Chain::Color, scratch.path("wall.ktx2"));

    let fallback = recipe.prepare().unwrap();
    assert_eq!(fallback.origin, Origin::Sources(Fallback::NoBake));
    match &fallback.pixels {
        Pixels::Rgba8GeneratedMips { srgb: true, base } => assert_eq!(base.len(), 16 * 8 * 4),
        other => panic!("a colour fallback leaves its mips to the GPU: {other:?}"),
    }
    assert_eq!((fallback.width, fallback.height), (16, 8));
    assert_eq!(fallback.stats.min_alpha, 100);

    assert_eq!(bake(&recipe, BakeMode::IfStale).unwrap(), Baked::Built);
    assert_eq!(bake(&recipe, BakeMode::IfStale).unwrap(), Baked::Current);
    assert_eq!(bake(&recipe, BakeMode::Always).unwrap(), Baked::Built);
    assert!(recipe.is_current().unwrap());
    let baked = recipe.prepare().unwrap();
    assert_eq!(baked.origin, Origin::Bake);
    let Pixels::Ktx2(bytes) = &baked.pixels else {
        panic!("{:?}", baked.origin)
    };
    let file = Ktx2Texture::parse(bytes).unwrap();
    assert_eq!(file.compression(), BlockCompression::Bc7Srgb);
    assert_eq!(file.level_count(), 5);
    assert_eq!(baked.stats, fallback.stats, "the bake carries the stats");
    assert_eq!(
        file.value(SOURCE_DIGEST_KEY),
        Some(format!("{:016x}", recipe.digest().unwrap()).as_bytes())
    );

    // An edited source stales the bake: the edit shows, built from the source.
    save_rgba(&source, 16, 8, gradient);
    assert!(!recipe.is_current().unwrap());
    assert_eq!(
        recipe.prepare().unwrap().origin,
        Origin::Sources(Fallback::Stale)
    );

    // A release ships the bake without its source, and the bake is trusted.
    std::fs::remove_file(&source).unwrap();
    assert_eq!(recipe.prepare().unwrap().origin, Origin::Bake);

    // A corrupt bake falls back to the sources while they exist, and is an error without them.
    std::fs::write(&recipe.output, b"not a texture").unwrap();
    let error = recipe.prepare().unwrap_err().to_string();
    assert!(error.contains("wall.ktx2"), "{error}");
    save_rgba(&source, 16, 8, gradient);
    assert!(matches!(
        recipe.prepare().unwrap().origin,
        Origin::Sources(Fallback::Unreadable(_))
    ));
}

#[test]
fn a_material_finds_its_maps_by_suffix_and_bakes_only_what_exists() {
    let scratch = Scratch::new("material");
    let dir = scratch.path("deco");
    std::fs::create_dir_all(&dir).unwrap();
    let carpet = dir.join("carpet.png");
    save_rgba(&carpet, 8, 8, gradient);
    save_rgba(&dir.join("carpet_normal.png"), 8, 8, |_, _| {
        [128, 128, 255, 255]
    });
    save_grey(&dir.join("carpet_rough.png"), 8, 8, |_, _| 230);
    save_grey(&dir.join("carpet_ao.png"), 8, 8, |_, _| 200);
    let plain = dir.join("plain.png");
    save_rgba(&plain, 4, 4, gradient);
    std::fs::write(dir.join("carpet.material"), "footstep carpet\n").unwrap();

    assert_eq!(
        find_materials(&scratch.0).unwrap(),
        vec![carpet.clone(), plain.clone()]
    );
    let defaults = MaterialDefaults::default();
    let maps = MaterialMaps::beside(&carpet, &defaults);
    let normal = maps.normal.as_ref().unwrap();
    assert_eq!(normal.output, dir.join("carpet_normal.ktx2"));
    let metal = maps.surface.as_ref().unwrap();
    assert_eq!(metal.output, dir.join("carpet_metal.ktx2"));
    assert_eq!(metal.channels[0], Channel::Constant(0), "no metal map");
    assert_eq!(&metal.build().unwrap().levels[0][..4], &[0, 200, 230, 255]);
    assert_eq!(
        &normal.build().unwrap().levels[0][..4],
        &[128, 128, 255, 255]
    );
    let bare = MaterialMaps::beside(&plain, &defaults);
    assert!(bare.normal.is_none() && bare.surface.is_none());

    let baked = bake_materials(
        std::slice::from_ref(&scratch.0),
        &defaults,
        BakeMode::IfStale,
    )
    .unwrap();
    let outputs: Vec<&Path> = baked.iter().map(|(path, _)| path.as_path()).collect();
    assert_eq!(
        outputs,
        [
            dir.join("carpet.ktx2"),
            dir.join("carpet_normal.ktx2"),
            dir.join("carpet_metal.ktx2"),
            dir.join("plain.ktx2"),
        ]
    );
    assert!(baked.iter().all(|(_, b)| *b == Ok(Baked::Built)));
    let again = bake_materials(
        std::slice::from_ref(&scratch.0),
        &defaults,
        BakeMode::IfStale,
    )
    .unwrap();
    assert!(again.iter().all(|(_, b)| *b == Ok(Baked::Current)));
    let forced = bake_materials(
        std::slice::from_ref(&scratch.0),
        &defaults,
        BakeMode::Always,
    )
    .unwrap();
    assert!(forced.iter().all(|(_, b)| *b == Ok(Baked::Built)));
    // The bakes themselves are not mistaken for materials.
    assert_eq!(find_materials(&scratch.0).unwrap().len(), 2);

    // A game ships the bakes without their sources: the material still has its maps, each read
    // from its bake as it is, and a material that had none still has none.
    for source in [
        "carpet.png",
        "carpet_normal.png",
        "carpet_rough.png",
        "carpet_ao.png",
        "plain.png",
    ] {
        std::fs::remove_file(dir.join(source)).unwrap();
    }
    let shipped = MaterialMaps::beside(&carpet, &defaults);
    for recipe in shipped.recipes() {
        assert!(!recipe.has_sources(), "{}", recipe.output.display());
        assert_eq!(
            recipe.prepare().unwrap().origin,
            Origin::Bake,
            "{}",
            recipe.output.display()
        );
    }
    assert_eq!(shipped.recipes().count(), 3);
    let bare = MaterialMaps::beside(&plain, &defaults);
    assert!(bare.normal.is_none() && bare.surface.is_none());
    assert_eq!(bare.albedo.prepare().unwrap().origin, Origin::Bake);
}

#[test]
fn a_failed_texture_is_reported_beside_the_others_baked() {
    let scratch = Scratch::new("partial");
    let dir = scratch.path("walls");
    std::fs::create_dir_all(&dir).unwrap();
    let brick = dir.join("brick.png");
    save_rgba(&brick, 8, 8, gradient);
    save_rgba(&dir.join("brick_normal.png"), 8, 8, |_, _| {
        [128, 128, 255, 255]
    });
    // A roughness map that is not an image fails the surface's bake alone.
    std::fs::write(dir.join("brick_rough.png"), b"not a png").unwrap();
    save_grey(&dir.join("brick_ao.png"), 8, 8, |_, _| 200);
    save_rgba(&dir.join("plaster.png"), 4, 4, gradient);
    let defaults = MaterialDefaults::default();

    let results = bake_materials(std::slice::from_ref(&dir), &defaults, BakeMode::IfStale).unwrap();
    let outputs: Vec<&Path> = results.iter().map(|(path, _)| path.as_path()).collect();
    assert_eq!(
        outputs,
        [
            dir.join("brick.ktx2"),
            dir.join("brick_normal.ktx2"),
            dir.join("brick_metal.ktx2"),
            dir.join("plaster.ktx2"),
        ]
    );
    for (path, result) in &results {
        if path.ends_with("brick_metal.ktx2") {
            let error = result.as_ref().unwrap_err().to_string();
            assert!(error.contains("brick_rough.png"), "{error}");
            assert!(!path.exists());
        } else {
            assert_eq!(*result, Ok(Baked::Built), "{}", path.display());
            assert!(path.exists());
        }
    }

    // The command line tries every texture, reports the failure and fails; the rest are current.
    let args = ["bake".to_owned(), dir.display().to_string()];
    let error = run(args.clone()).unwrap_err().to_string();
    assert!(error.contains("1 of 4"), "{error}");
    let again = bake_materials(std::slice::from_ref(&dir), &defaults, BakeMode::IfStale).unwrap();
    assert_eq!(
        again
            .iter()
            .filter(|(_, result)| *result == Ok(Baked::Current))
            .count(),
        3
    );

    // A path naming no material is the only error for the whole call: nothing is baked.
    let stray = dir.join("brick_normal.png");
    let error = bake_materials(&[dir.clone(), stray], &defaults, BakeMode::Always)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("not a directory or a material's albedo"),
        "{error}"
    );

    // Once the map is fixed, `--force` rebakes everything.
    save_grey(&dir.join("brick_rough.png"), 8, 8, |_, _| 230);
    run([args[0].clone(), args[1].clone(), "--force".to_owned()]).unwrap();
    assert!(dir.join("brick_metal.ktx2").exists());
}
