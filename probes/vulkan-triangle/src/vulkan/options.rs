use std::num::NonZeroU64;
use std::path::PathBuf;

use super::ProbeError;

#[derive(Debug, Default)]
pub(super) struct RunOptions {
    pub(super) frame_limit: Option<NonZeroU64>,
    pub(super) abandon_acquired_frame_once: bool,
    pub(super) platform: Option<String>,
    pub(super) pipeline_cache: PipelineCacheOptions,
}

#[derive(Debug, Default)]
pub(super) struct PipelineCacheOptions {
    pub(super) path: Option<PathBuf>,
    pub(super) rebuild: bool,
    pub(super) strict: bool,
    pub(super) disabled: bool,
}

pub(super) fn parse_run_options(
    arguments: impl IntoIterator<Item = String>,
) -> Result<RunOptions, ProbeError> {
    let mut options = RunOptions::default();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--frames" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| ProbeError("--frames requires a positive integer".into()))?;
                options.frame_limit = Some(
                    value
                        .parse::<NonZeroU64>()
                        .map_err(|_| ProbeError("--frames requires a positive integer".into()))?,
                );
            }
            "--abandon-acquired-frame-once" => options.abandon_acquired_frame_once = true,
            "--platform" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| ProbeError("--platform requires a platform name".into()))?;
                options.platform = Some(value);
            }
            "--pipeline-cache" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| ProbeError("--pipeline-cache requires a file path".into()))?;
                options.pipeline_cache.path = Some(PathBuf::from(value));
            }
            "--rebuild-pipeline-cache" => options.pipeline_cache.rebuild = true,
            "--require-pipeline-cache-hits" => options.pipeline_cache.strict = true,
            "--disable-pipeline-cache" => options.pipeline_cache.disabled = true,
            _ => return Err(ProbeError(format!("unknown argument: {argument}"))),
        }
    }
    if options.pipeline_cache.rebuild && options.pipeline_cache.strict {
        return Err(ProbeError(
            "--rebuild-pipeline-cache conflicts with --require-pipeline-cache-hits".into(),
        ));
    }
    if options.pipeline_cache.disabled
        && (options.pipeline_cache.path.is_some()
            || options.pipeline_cache.rebuild
            || options.pipeline_cache.strict)
    {
        return Err(ProbeError(
            "--disable-pipeline-cache conflicts with all other pipeline-cache controls".into(),
        ));
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combines_frame_and_pipeline_cache_controls() {
        let options = parse_run_options([
            "--pipeline-cache".into(),
            "cache.bin".into(),
            "--frames".into(),
            "42".into(),
            "--abandon-acquired-frame-once".into(),
            "--require-pipeline-cache-hits".into(),
        ])
        .expect("valid options");
        assert_eq!(options.frame_limit.map(NonZeroU64::get), Some(42));
        assert!(options.abandon_acquired_frame_once);
        assert_eq!(
            options.pipeline_cache.path,
            Some(PathBuf::from("cache.bin"))
        );
        assert!(options.pipeline_cache.strict);
        assert!(!options.pipeline_cache.rebuild);
    }

    #[test]
    fn parses_platform_selection() {
        let options = parse_run_options(["--platform".into(), "x11".into()])
            .expect("valid platform selection");
        assert_eq!(options.platform.as_deref(), Some("x11"));
        assert!(
            parse_run_options(["--platform".into()])
                .expect_err("missing platform name must fail")
                .to_string()
                .contains("--platform")
        );
    }

    #[test]
    fn rejects_rebuild_with_strict_hits() {
        let error = parse_run_options([
            "--rebuild-pipeline-cache".into(),
            "--require-pipeline-cache-hits".into(),
        ])
        .expect_err("conflicting modes must fail");
        assert!(error.to_string().contains("conflicts"));
    }

    #[test]
    fn keeps_disabled_cache_mode_unambiguous() {
        let options = parse_run_options(["--disable-pipeline-cache".into()])
            .expect("standalone disabled mode");
        assert!(options.pipeline_cache.disabled);
        assert!(
            parse_run_options([
                "--disable-pipeline-cache".into(),
                "--pipeline-cache".into(),
                "cache.bin".into(),
            ])
            .is_err()
        );
    }
}
