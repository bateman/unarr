//! Radarr's `DetectSample`: a video file too short for its movie is a
//! sample. The runtime rule is the one the series side has
//! (`mediafiles::sample`); what differs is which files are never probed.

use crate::mediafiles::mediainfo::MediaInfo;
use crate::mediafiles::sample::{by_runtime, DetectSampleResult};
use crate::movies::model::MovieMetadata;

/// `IsSample(MovieMetadata, path)` with the probe already taken: `info` is
/// what the media info reader made of the file, `None` when it could not
/// read it. Flash and stream files and disc images are never samples and
/// never probed.
pub fn is_sample(movie: &MovieMetadata, path: &str, info: Option<&MediaInfo>) -> DetectSampleResult {
    let extension = crate::mediafiles::extension(path).to_ascii_lowercase();
    match extension.as_str() {
        ".flv" => {
            tracing::debug!("Skipping sample check for .flv file");
            return DetectSampleResult::NotSample;
        }
        ".strm" => {
            tracing::debug!("Skipping sample check for .strm file");
            return DetectSampleResult::NotSample;
        }
        ".iso" | ".img" | ".m2ts" => {
            tracing::debug!("Skipping sample check for DVD/BR image file '{path}'");
            return DetectSampleResult::NotSample;
        }
        _ => {}
    }
    let Some(info) = info else {
        tracing::error!("Failed to get runtime from the file, make sure ffprobe is available");
        return DetectSampleResult::Indeterminate;
    };
    by_runtime(path, info.run_time_seconds(), movie.runtime)
}

#[cfg(test)]
mod tests {
    //! The original's `DetectSampleFixture`.

    use serde_json::json;

    use super::*;
    use crate::mediafiles::mediainfo::format_timespan;
    use crate::movies::decision::testing;

    const PATH: &str = "/Test/30 Rock/30.rock.s01e01.avi";

    fn movie(runtime: i64) -> MovieMetadata {
        let mut movie = testing::movie(1, "30 Rock", 2006).metadata;
        movie.runtime = runtime;
        movie
    }

    fn runtime(seconds: i64) -> MediaInfo {
        MediaInfo {
            model: json!({"runTime": format_timespan(seconds as f64)}),
            title: None,
        }
    }

    fn sample(runtime_minutes: i64, path: &str, seconds: Option<i64>) -> DetectSampleResult {
        is_sample(&movie(runtime_minutes), path, seconds.map(runtime).as_ref())
    }

    #[test]
    fn should_return_false_for_flv() {
        assert_eq!(
            sample(30, "/Test/some.show.s01e01.flv", None),
            DetectSampleResult::NotSample
        );
    }

    #[test]
    fn should_return_false_for_strm() {
        assert_eq!(
            sample(30, "/Test/some.show.s01e01.strm", None),
            DetectSampleResult::NotSample
        );
    }

    #[test]
    fn should_return_false_for_iso() {
        assert_eq!(
            sample(30, "/Test/some movie (2000).iso", None),
            DetectSampleResult::NotSample
        );
    }

    #[test]
    fn should_return_false_for_img() {
        assert_eq!(
            sample(30, "/Test/some movie (2000).img", None),
            DetectSampleResult::NotSample
        );
    }

    #[test]
    fn should_return_false_for_m2ts() {
        assert_eq!(
            sample(30, "/Test/some movie (2000).M2TS", None),
            DetectSampleResult::NotSample
        );
    }

    #[test]
    fn should_use_runtime() {
        // 120 seconds of a 30 minute movie: only the runtime can say so.
        assert_eq!(sample(30, PATH, Some(120)), DetectSampleResult::Sample);
    }

    #[test]
    fn should_return_true_if_runtime_is_less_than_minimum() {
        assert_eq!(sample(30, PATH, Some(60)), DetectSampleResult::Sample);
    }

    #[test]
    fn should_return_false_if_runtime_greater_than_minimum() {
        assert_eq!(sample(30, PATH, Some(600)), DetectSampleResult::NotSample);
    }

    #[test]
    fn should_return_false_if_runtime_greater_than_webisode_minimum() {
        assert_eq!(sample(6, PATH, Some(299)), DetectSampleResult::NotSample);
    }

    #[test]
    fn should_return_false_if_runtime_greater_than_anime_short_minimum() {
        assert_eq!(sample(2, PATH, Some(60)), DetectSampleResult::NotSample);
    }

    #[test]
    fn should_return_true_if_runtime_less_than_anime_short_minimum() {
        assert_eq!(sample(2, PATH, Some(10)), DetectSampleResult::Sample);
    }

    #[test]
    fn should_return_indeterminate_if_mediainfo_result_is_null() {
        assert_eq!(sample(30, PATH, None), DetectSampleResult::Indeterminate);
    }

    #[test]
    fn a_runtime_of_zero_is_a_sample() {
        assert_eq!(sample(30, PATH, Some(0)), DetectSampleResult::Sample);
    }
}
