//! `CustomFormatCalculationService` for what the engine compares: a release,
//! an existing file, a history row, a blocklist row or a queued download.

use serde_json::Value;

use crate::customformat::Input;
use crate::decision::lookup::{Catalog, QueuedRelease};
use crate::decision::RemoteEpisode;
use crate::model::{CustomFormat, EpisodeFile, HistoryEntry, ReleaseType, Series};
use crate::parser;

fn evaluate(catalog: &Catalog, input: &Input<'_>) -> Vec<CustomFormat> {
    catalog.formats.evaluate(input).into_iter().cloned().collect()
}

/// `ParseCustomFormat(RemoteEpisode, size)`.
pub fn release_formats(catalog: &Catalog, remote: &RemoteEpisode, size: i64) -> Vec<CustomFormat> {
    let parsed = remote.parsed_or_default();
    let series_language = remote
        .series
        .as_ref()
        .map(|s| s.original_language_id)
        .unwrap_or(0);
    let input = Input {
        release_title: &parsed.release_title,
        filename: None,
        release_group: parsed.release_group.as_deref(),
        quality: parsed.quality.quality,
        languages: &remote.languages,
        size,
        indexer_flags: remote.release.indexer_flags,
        release_type: parsed.release_type(),
        series_original_language: series_language,
        movie: None,
    };
    evaluate(catalog, &input)
}

/// `ParseCustomFormat(EpisodeFile, Series)`.
pub fn file_formats(catalog: &Catalog, file: &EpisodeFile, series: &Series) -> Vec<CustomFormat> {
    let filename = file
        .relative_path
        .rsplit('/')
        .next()
        .unwrap_or(&file.relative_path);
    let release_title = file
        .scene_name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            file.original_file_path
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(|p| p.rsplit('/').next().unwrap_or(p))
        })
        .unwrap_or(filename);
    let input = Input {
        release_title,
        filename: Some(filename),
        release_group: file.release_group.as_deref(),
        quality: file.quality,
        languages: &file.languages,
        size: file.size,
        indexer_flags: file.indexer_flags,
        release_type: file.release_type,
        series_original_language: series.original_language_id,
        movie: None,
    };
    evaluate(catalog, &input)
}

/// `ParseCustomFormat(EpisodeHistory, Series)`: the source title parsed
/// again, with the quality, languages, size and flags the row recorded.
pub fn history_formats(catalog: &Catalog, history: &HistoryEntry, series: &Series) -> Vec<CustomFormat> {
    let source = history.source_title.as_deref().unwrap_or("");
    let parsed = parser::parse_title(source);
    let release_title = parsed
        .as_ref()
        .map(|p| p.release_title.clone())
        .unwrap_or_else(|| source.to_string());
    let data_str = |key: &str| history.data.get(key).and_then(Value::as_str).map(str::to_string);
    let size = data_str("size")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0);
    let indexer_flags = data_str("indexerFlags")
        .map(|s| crate::indexers::rss::flags::from_names(&s))
        .unwrap_or(0);
    let release_type = data_str("releaseType")
        .and_then(|s| ReleaseType::parse(&lower_first(&s)))
        .unwrap_or(ReleaseType::Unknown);
    let input = Input {
        release_title: &release_title,
        filename: None,
        release_group: parsed.as_ref().and_then(|p| p.release_group.as_deref()),
        quality: history.quality,
        languages: &history.languages,
        size,
        indexer_flags,
        release_type,
        series_original_language: series.original_language_id,
        movie: None,
    };
    evaluate(catalog, &input)
}

/// `ParseCustomFormat(RemoteEpisode, size)` for a queued download.
pub fn queued_formats(catalog: &Catalog, queued: &QueuedRelease, series: &Series) -> Vec<CustomFormat> {
    let parsed = parser::parse_title(&queued.title);
    let release_title = parsed
        .as_ref()
        .map(|p| p.release_title.clone())
        .unwrap_or_else(|| queued.title.clone());
    let input = Input {
        release_title: &release_title,
        filename: None,
        release_group: parsed.as_ref().and_then(|p| p.release_group.as_deref()),
        quality: queued.quality,
        languages: &queued.languages,
        size: queued.size,
        indexer_flags: queued.indexer_flags,
        release_type: queued.release_type,
        series_original_language: series.original_language_id,
        movie: None,
    };
    evaluate(catalog, &input)
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Format names joined as `ConcatToString` does.
pub fn names(formats: &[CustomFormat]) -> String {
    formats
        .iter()
        .map(|f| f.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}
