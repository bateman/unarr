//! `DetectSample`: a video file too short for its series is a sample.

use super::local::LocalEpisode;
use super::mediainfo::{self, MediaInfo};
use crate::model::Series;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectSampleResult {
    Sample,
    NotSample,
    Indeterminate,
}

/// `GetMinimumAllowedRuntime`, in seconds, for a series or episode runtime
/// in minutes.
pub fn minimum_allowed_runtime(runtime_minutes: i64) -> f64 {
    if runtime_minutes <= 3 {
        // Anime short
        15.0
    } else if runtime_minutes <= 10 {
        // Webisodes
        90.0
    } else if runtime_minutes <= 30 {
        300.0
    } else {
        600.0
    }
}

/// The checks that need no probe: specials and `.flv`/`.strm` are never
/// samples.
fn by_path(path: &str, is_special: bool) -> DetectSampleResult {
    if is_special {
        return DetectSampleResult::NotSample;
    }
    let ext = super::extension(path).to_ascii_lowercase();
    if ext == ".flv" || ext == ".strm" {
        return DetectSampleResult::NotSample;
    }
    DetectSampleResult::Indeterminate
}

fn by_runtime(path: &str, file_runtime_seconds: f64, expected_runtime_minutes: i64) -> DetectSampleResult {
    let minimum = minimum_allowed_runtime(expected_runtime_minutes);
    if file_runtime_seconds == 0.0 {
        tracing::error!("[{path}] has a runtime of 0, is it a valid video file?");
        return DetectSampleResult::Sample;
    }
    if file_runtime_seconds < minimum {
        tracing::debug!(
            "[{path}] appears to be a sample. Runtime: {file_runtime_seconds} seconds. Expected at least: {minimum} seconds"
        );
        return DetectSampleResult::Sample;
    }
    DetectSampleResult::NotSample
}

/// `IsSample(series, path, isSpecial)`: probes the file itself.
pub fn is_sample_file(series: &Series, path: &str, is_special: bool) -> DetectSampleResult {
    let by_path = by_path(path, is_special);
    if by_path != DetectSampleResult::Indeterminate {
        return by_path;
    }
    match mediainfo::probe(std::path::Path::new(path)) {
        Some(info) => by_runtime(path, info.run_time_seconds(), series.runtime),
        None => {
            tracing::error!("Failed to get runtime from the file, make sure ffprobe is available");
            DetectSampleResult::Indeterminate
        }
    }
}

/// `IsSample(localEpisode)`: the media info already probed, the runtime of
/// the episodes it carries (the series' when an episode has none, 45
/// minutes when both say nothing).
pub fn is_sample(local: &LocalEpisode) -> DetectSampleResult {
    let by_path = by_path(&local.path, local.is_special());
    if by_path != DetectSampleResult::Indeterminate {
        return by_path;
    }
    let series_runtime = local.series.as_ref().map(|s| s.runtime).unwrap_or(0);
    let mut runtime: i64 = local
        .episodes
        .iter()
        .map(|e| if e.runtime > 0 { e.runtime } else { series_runtime })
        .sum();
    let Some(info) = &local.media_info else {
        tracing::error!("Failed to get runtime from the file, make sure ffprobe is available");
        return DetectSampleResult::Indeterminate;
    };
    if runtime == 0 {
        tracing::debug!("Series runtime is 0, defaulting runtime to 45 minutes");
        runtime = 45;
    }
    by_runtime(&local.path, info.run_time_seconds(), runtime)
}

/// `is_sample` with a probe result standing in for the file's.
pub fn is_sample_with(local: &LocalEpisode, info: Option<&MediaInfo>) -> DetectSampleResult {
    let mut copy = local.clone();
    copy.media_info = info.cloned();
    is_sample(&copy)
}

/// `IsSample(series, path, isSpecial)` with the probe already taken.
pub fn is_sample_probe(
    series: &Series,
    path: &str,
    is_special: bool,
    info: Option<&MediaInfo>,
) -> DetectSampleResult {
    let by_path = by_path(path, is_special);
    if by_path != DetectSampleResult::Indeterminate {
        return by_path;
    }
    match info {
        Some(info) => by_runtime(path, info.run_time_seconds(), series.runtime),
        None => {
            tracing::error!("Failed to get runtime from the file, make sure ffprobe is available");
            DetectSampleResult::Indeterminate
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimum_runtimes_follow_the_originals() {
        assert_eq!(minimum_allowed_runtime(3), 15.0);
        assert_eq!(minimum_allowed_runtime(10), 90.0);
        assert_eq!(minimum_allowed_runtime(30), 300.0);
        assert_eq!(minimum_allowed_runtime(45), 600.0);
    }

    #[test]
    fn runtime_decides_except_for_specials_and_streams() {
        assert_eq!(by_path("/a/b.flv", false), DetectSampleResult::NotSample);
        assert_eq!(by_path("/a/b.strm", false), DetectSampleResult::NotSample);
        assert_eq!(by_path("/a/b.mkv", true), DetectSampleResult::NotSample);
        assert_eq!(by_path("/a/b.mkv", false), DetectSampleResult::Indeterminate);
        assert_eq!(by_runtime("x", 0.0, 45), DetectSampleResult::Sample);
        assert_eq!(by_runtime("x", 120.0, 45), DetectSampleResult::Sample);
        assert_eq!(by_runtime("x", 610.0, 45), DetectSampleResult::NotSample);
        assert_eq!(by_runtime("x", 120.0, 5), DetectSampleResult::NotSample);
    }
}
