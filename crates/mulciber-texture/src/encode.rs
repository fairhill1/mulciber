//! BC7 encoding with Intel's ISPC texture compressor, and the bakes it writes.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use intel_tex_2::{RgbaSurface, bc6h, bc7};

use crate::TextureError;
use crate::chain::{Levels, mip_extent};
use crate::container::{SOURCE_DIGEST_KEY, refuse_misread_chain, write_ktx2};
use crate::hdr::unsigned_halves;
use crate::material::{MaterialDefaults, MaterialMaps, find_materials, is_albedo};
use crate::recipe::{Recipe, Stats};

/// What [`bake`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Baked {
    /// The bake was written.
    Built,
    /// The bake on disk was already current, and was left alone.
    Current,
}

/// Whether [`bake`] and [`bake_materials`] rebuild a bake that is already current.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BakeMode {
    /// Bake only when the bake on disk is missing or stale ([`Recipe::is_current`] is false).
    #[default]
    IfStale,
    /// Bake regardless, rewriting bakes that are already current.
    Always,
}

/// Encodes every level of an RGBA8 chain as BC7, with the slow (best) settings: the opaque modes
/// when every texel's alpha is 255, the alpha modes otherwise. A level whose extent is not a
/// multiple of four is padded by repeating its last row and column; the GPU never samples the
/// padding, because the level's extent stays what it was.
#[must_use]
pub fn encode_bc7(levels: &Levels) -> Vec<Vec<u8>> {
    let opaque = levels
        .levels
        .iter()
        .all(|level| level.as_chunks::<4>().0.iter().all(|texel| texel[3] == 255));
    let settings = if opaque {
        bc7::opaque_slow_settings()
    } else {
        bc7::alpha_slow_settings()
    };
    (0_u32..)
        .zip(&levels.levels)
        .map(|(level, texels)| {
            let (width, height) = (
                mip_extent(levels.width, level),
                mip_extent(levels.height, level),
            );
            let (padded_width, padded_height) =
                (width.next_multiple_of(4), height.next_multiple_of(4));
            let padded;
            let data = if (padded_width, padded_height) == (width, height) {
                texels.as_slice()
            } else {
                let (w, pw) = (width as usize, padded_width as usize);
                let mut rows = Vec::with_capacity(pw * padded_height as usize * 4);
                for y in 0..padded_height as usize {
                    let row = &texels[y.min(height as usize - 1) * w * 4..][..w * 4];
                    for x in 0..pw {
                        rows.extend_from_slice(&row[x.min(w - 1) * 4..][..4]);
                    }
                }
                padded = rows;
                padded.as_slice()
            };
            bc7::compress_blocks(
                &settings,
                &RgbaSurface {
                    data,
                    width: padded_width,
                    height: padded_height,
                    stride: padded_width * 4,
                },
            )
        })
        .collect()
}

/// Encodes one level of linear RGB light as BC6H unsigned half floats
/// ([`BlockCompression::Bc6hUfloat`](mulciber::BlockCompression::Bc6hUfloat)) with ISPC's basic
/// settings, a block row band per core. Alpha is dropped. Light below zero is encoded as zero
/// and above [`HDR_MAX`](crate::HDR_MAX) as it, which BC6H's unsigned floats cannot hold. A level
/// whose extent is not a multiple of four is padded by repeating its last row and column, which
/// the GPU never samples.
///
/// For a texture with mips, encode every level of an [`hdr_chain`](crate::hdr_chain).
///
/// # Errors
///
/// When `texels` is not `width` × `height` texels or holds light that is not finite.
///
/// # Panics
///
/// If an encoder thread panics, or a level is wider than `u32` bytes a row.
pub fn encode_bc6h(width: u32, height: u32, texels: &[[f32; 4]]) -> Result<Vec<u8>, TextureError> {
    if width == 0 || height == 0 || texels.len() != width as usize * height as usize {
        return Err(TextureError::new(format!(
            "{} texels do not make a {width}x{height} level",
            texels.len()
        )));
    }
    let halves = unsigned_halves(texels)?;
    let (w, h) = (width as usize, height as usize);
    let (padded_width, padded_height) = (w.next_multiple_of(4), h.next_multiple_of(4));
    // Eight bytes a texel, native-endian halves as the encoder reads them.
    let mut data = Vec::with_capacity(padded_width * padded_height * 8);
    for y in 0..padded_height {
        let row = &halves[y.min(h - 1) * w..][..w];
        for x in 0..padded_width {
            data.extend(row[x.min(w - 1)].iter().flat_map(|half| half.to_ne_bytes()));
        }
    }
    let block_rows = padded_height / 4;
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(block_rows);
    let band_rows = block_rows.div_ceil(workers);
    let row_bytes = padded_width * 8 * 4;
    let settings = bc6h::basic_settings();
    let bands: Vec<Vec<u8>> = std::thread::scope(|scope| {
        let handles: Vec<_> = data
            .chunks(band_rows * row_bytes)
            .map(|band| {
                scope.spawn(move || {
                    let surface = RgbaSurface {
                        data: band,
                        width: u32::try_from(padded_width).expect("a level's width fits u32"),
                        height: u32::try_from(band.len() / (padded_width * 8))
                            .expect("a band's height fits u32"),
                        stride: u32::try_from(padded_width * 8).expect("a row fits u32 bytes"),
                    };
                    bc6h::compress_blocks(&settings, &surface)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("no encoder thread panics"))
            .collect()
    });
    Ok(bands.concat())
}

/// Bakes `recipe` to its output, unless `mode` is [`BakeMode::IfStale`] and the bake there is
/// already current.
///
/// # Errors
///
/// When a source cannot be read or decoded, or the bake cannot be written, naming the file.
pub fn bake(recipe: &Recipe, mode: BakeMode) -> Result<Baked, TextureError> {
    if mode == BakeMode::IfStale && recipe.is_current()? {
        return Ok(Baked::Current);
    }
    let digest = recipe.digest()?;
    let levels = recipe.build()?;
    // Before the encoder's minutes, not after.
    refuse_misread_chain(levels.width, levels.height).map_err(|error| error.at(&recipe.output))?;
    let stats = Stats::of(&levels.levels[0], recipe.chain.is_srgb());
    let blocks = encode_bc7(&levels);
    let digest = format!("{digest:016x}");
    let stat_entries = stats.entries();
    let mut entries = vec![(SOURCE_DIGEST_KEY, digest.as_str())];
    entries.extend(
        stat_entries
            .iter()
            .map(|(key, value)| (*key, value.as_str())),
    );
    write_ktx2(
        &recipe.output,
        recipe.chain.compression(),
        levels.width,
        levels.height,
        &blocks,
        &entries,
    )?;
    Ok(Baked::Built)
}

/// The materials named by `paths`: each directory's materials, recursively, and each albedo image
/// itself.
fn materials(paths: &[PathBuf]) -> Result<Vec<PathBuf>, TextureError> {
    let mut albedos = Vec::new();
    for path in paths {
        if path.is_dir() {
            albedos.extend(find_materials(path)?);
        } else if is_albedo(path) && path.exists() {
            albedos.push(path.clone());
        } else {
            return Err(TextureError::new(format!(
                "{}: not a directory or a material's albedo image",
                path.display()
            )));
        }
    }
    Ok(albedos)
}

/// Bakes every texture of every material under `paths` (directories, searched recursively, or
/// albedo images), on every core, as `mode` says.
///
/// Returns every texture's output path with its own result, in recipe order (material by
/// material, each material's textures in [`MaterialMaps::recipes`] order): what was done, or why
/// that texture failed. A failed texture does not stop the others, so the caller sees both the
/// bakes that were written and the ones that were not.
///
/// # Errors
///
/// Only when the materials cannot be found: a path that is neither a directory nor an existing
/// albedo image, or a directory that cannot be read. Nothing is baked then.
///
/// # Panics
///
/// If a bake panics on its worker thread.
#[allow(clippy::type_complexity)]
pub fn bake_materials(
    paths: &[PathBuf],
    defaults: &MaterialDefaults,
    mode: BakeMode,
) -> Result<Vec<(PathBuf, Result<Baked, TextureError>)>, TextureError> {
    let recipes: Vec<Recipe> = materials(paths)?
        .iter()
        .flat_map(|albedo| {
            let maps = MaterialMaps::beside(albedo, defaults);
            maps.recipes().cloned().collect::<Vec<_>>()
        })
        .collect();
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(recipes.len()));
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    std::thread::scope(|scope| {
        for _ in 0..workers.min(recipes.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(recipe) = recipes.get(index) else {
                        break;
                    };
                    let result = bake(recipe, mode);
                    results
                        .lock()
                        .expect("no worker panics holding the lock")
                        .push((index, result));
                }
            });
        }
    });
    let mut results = results.into_inner().expect("the workers are done");
    results.sort_by_key(|(index, _)| *index);
    Ok(results
        .into_iter()
        .map(|(index, result)| (recipes[index].output.clone(), result))
        .collect())
}

/// The `mulciber-texture` command line, for a game's own bake binary to forward to as well.
///
/// - `bake <dir|albedo>... [--force]` bakes every material's textures that are stale or missing
///   (all of them with `--force`, [`BakeMode::Always`]). It prints each texture baked and each
///   that failed, with its error, then a summary, and fails if any texture failed.
/// - `check <dir|albedo>...` bakes nothing, lists every texture whose bake is stale or missing,
///   and fails if there is one.
///
/// # Errors
///
/// A usage error, a path that names no materials, any failed bake (after every texture has been
/// tried and reported), or stale bakes under `check`.
pub fn run(args: impl IntoIterator<Item = String>) -> Result<(), TextureError> {
    const USAGE: &str = "usage: mulciber-texture bake <dir|albedo image>... [--force]\n       \
                         mulciber-texture check <dir|albedo image>...";
    let mut args = args.into_iter();
    let command = args.next().ok_or(TextureError::new(USAGE))?;
    let mut mode = BakeMode::IfStale;
    let mut paths = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--force" if command == "bake" => mode = BakeMode::Always,
            flag if flag.starts_with("--") => return Err(TextureError::new(USAGE)),
            path => paths.push(PathBuf::from(path)),
        }
    }
    if paths.is_empty() {
        return Err(TextureError::new(USAGE));
    }
    let defaults = MaterialDefaults::default();
    match command.as_str() {
        "bake" => {
            let started = std::time::Instant::now();
            let results = bake_materials(&paths, &defaults, mode)?;
            let (mut built, mut current, mut failed) = (0, 0, 0);
            for (path, result) in &results {
                match result {
                    Ok(Baked::Built) => {
                        built += 1;
                        println!("baked {}", path.display());
                    }
                    Ok(Baked::Current) => current += 1,
                    Err(error) => {
                        failed += 1;
                        eprintln!("failed {}: {error}", path.display());
                    }
                }
            }
            println!(
                "{built} baked, {current} already current, {failed} failed, in {:.1?}",
                started.elapsed()
            );
            if failed == 0 {
                Ok(())
            } else {
                Err(TextureError::new(format!(
                    "{failed} of {} textures failed to bake",
                    results.len()
                )))
            }
        }
        "check" => {
            let mut stale = Vec::new();
            for albedo in materials(&paths)? {
                for recipe in MaterialMaps::beside(&albedo, &defaults).recipes() {
                    if !recipe.is_current()? {
                        stale.push(recipe.output.display().to_string());
                    }
                }
            }
            for path in &stale {
                println!("stale {path}");
            }
            if stale.is_empty() {
                Ok(())
            } else {
                Err(TextureError::new(format!(
                    "{} bakes are stale or missing; run `mulciber-texture bake`",
                    stale.len()
                )))
            }
        }
        _ => Err(TextureError::new(USAGE)),
    }
}
