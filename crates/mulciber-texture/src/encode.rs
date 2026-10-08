//! BC7 encoding with Intel's ISPC texture compressor, and the bakes it writes.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use intel_tex_2::{RgbaSurface, bc7};

use crate::TextureError;
use crate::chain::{Levels, mip_extent};
use crate::container::{SOURCE_DIGEST_KEY, write_ktx2};
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

/// Bakes `recipe` to its output, unless the bake there is already current and `force` is off.
///
/// # Errors
///
/// When a source cannot be read or decoded, or the bake cannot be written, naming the file.
pub fn bake(recipe: &Recipe, force: bool) -> Result<Baked, TextureError> {
    if !force && recipe.is_current()? {
        return Ok(Baked::Current);
    }
    let digest = recipe.digest()?;
    let levels = recipe.build()?;
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
/// albedo images), on every core. Returns each texture's output and what was done.
///
/// # Errors
///
/// The first failure, naming its file; the other textures are still baked.
///
/// # Panics
///
/// If a bake panics on its worker thread.
pub fn bake_materials(
    paths: &[PathBuf],
    defaults: &MaterialDefaults,
    force: bool,
) -> Result<Vec<(PathBuf, Baked)>, TextureError> {
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
                    let result = bake(recipe, force);
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
    results
        .into_iter()
        .map(|(index, result)| result.map(|baked| (recipes[index].output.clone(), baked)))
        .collect()
}

/// The `mulciber-texture` command line, for a game's own bake binary to forward to as well.
///
/// - `bake <dir|albedo>... [--force]` bakes every material's textures that are stale or missing
///   (all of them with `--force`).
/// - `check <dir|albedo>...` bakes nothing, lists every texture whose bake is stale or missing,
///   and fails if there is one.
///
/// # Errors
///
/// A usage error, the first failed bake, or stale bakes under `check`.
pub fn run(args: impl IntoIterator<Item = String>) -> Result<(), TextureError> {
    const USAGE: &str = "usage: mulciber-texture bake <dir|albedo image>... [--force]\n       \
                         mulciber-texture check <dir|albedo image>...";
    let mut args = args.into_iter();
    let command = args.next().ok_or(TextureError::new(USAGE))?;
    let mut force = false;
    let mut paths = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--force" if command == "bake" => force = true,
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
            let baked = bake_materials(&paths, &defaults, force)?;
            for (path, _) in baked.iter().filter(|(_, baked)| *baked == Baked::Built) {
                println!("baked {}", path.display());
            }
            let built = baked
                .iter()
                .filter(|(_, baked)| *baked == Baked::Built)
                .count();
            println!(
                "{built} baked, {} already current, in {:.1?}",
                baked.len() - built,
                started.elapsed()
            );
            Ok(())
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
