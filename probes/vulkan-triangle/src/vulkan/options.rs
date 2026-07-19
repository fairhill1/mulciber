use std::num::NonZeroU64;
use std::path::PathBuf;

use super::ProbeError;

#[derive(Debug, Default)]
pub(super) struct RunOptions {
    pub(super) frame_limit: Option<NonZeroU64>,
    pub(super) abandon_acquired_frame_once: bool,
    pub(super) platform: Option<String>,
    pub(super) pipeline_cache: PipelineCacheOptions,
    pub(super) pacing_csv: Option<PathBuf>,
    pub(super) load_spike: Option<LoadSpike>,
}

/// A fixed CPU stall injected before a range of presented frames for the pre-registered
/// load-spike pacing scenario.
#[derive(Clone, Copy, Debug)]
pub(super) struct LoadSpike {
    pub(super) start: u64,
    pub(super) count: u64,
    pub(super) millis: u64,
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
            "--pacing-csv" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| ProbeError("--pacing-csv requires a file path".into()))?;
                options.pacing_csv = Some(PathBuf::from(value));
            }
            "--load-spike" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| ProbeError("--load-spike requires START:COUNT:MILLIS".into()))?;
                options.load_spike = Some(parse_load_spike(&value)?);
            }
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

fn parse_load_spike(value: &str) -> Result<LoadSpike, ProbeError> {
    let mut parts = value.splitn(3, ':');
    let (Some(start), Some(count), Some(millis)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(ProbeError(
            "--load-spike requires START:COUNT:MILLIS".into(),
        ));
    };
    let parse = |part: &str, label: &str| {
        part.parse::<u64>()
            .map_err(|error| ProbeError(format!("invalid --load-spike {label}: {error}")))
    };
    let spike = LoadSpike {
        start: parse(start, "start")?,
        count: parse(count, "count")?,
        millis: parse(millis, "millis")?,
    };
    if spike.count == 0 || spike.millis == 0 {
        return Err(ProbeError(
            "--load-spike count and millis must be positive".into(),
        ));
    }
    Ok(spike)
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
    fn parses_pacing_controls() {
        let options = parse_run_options([
            "--pacing-csv".into(),
            "pacing.csv".into(),
            "--load-spike".into(),
            "120:30:40".into(),
        ])
        .expect("valid pacing controls");
        assert_eq!(options.pacing_csv, Some(PathBuf::from("pacing.csv")));
        let spike = options.load_spike.expect("parsed load spike");
        assert_eq!((spike.start, spike.count, spike.millis), (120, 30, 40));
        assert!(parse_run_options(["--load-spike".into(), "120:30".into()]).is_err());
        assert!(parse_run_options(["--load-spike".into(), "120:0:40".into()]).is_err());
        assert!(parse_run_options(["--pacing-csv".into()]).is_err());
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
