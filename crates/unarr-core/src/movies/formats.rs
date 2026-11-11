//! Custom formats of movie files, history rows and blocklist entries, as
//! the originals' `CustomFormatCalculationService` works them out: the
//! shared matcher of [`crate::customformat`], fed what a movie has.

use crate::customformat::{evaluate, Input, MovieFacts};
use crate::model::{CustomFormat, QualityModel, ReleaseType};
use crate::movies::model::{Movie, MovieBlocklistEntry, MovieFile, MovieHistoryEntry};
use crate::movies::quality::{MovieQuality, RADARR_INDEXER_FLAGS};

/// `QualitySource`, as Radarr numbers it.
fn source_number(source: &str) -> i64 {
    match source {
        "cam" => 1,
        "telesync" => 2,
        "telecine" => 3,
        "workprint" => 4,
        "dvd" => 5,
        "tv" => 6,
        "webdl" => 7,
        "webrip" => 8,
        "bluray" => 9,
        _ => 0,
    }
}

/// `Modifier`, as Radarr numbers it.
fn modifier_number(modifier: &str) -> i64 {
    match modifier {
        "regional" => 1,
        "screener" => 2,
        "rawhd" => 3,
        "brdisk" => 4,
        "remux" => 5,
        _ => 0,
    }
}

fn facts<'a>(quality: &QualityModel, edition: Option<&'a str>, movie: &Movie) -> MovieFacts<'a> {
    let q = MovieQuality::by_id(quality.quality_id);
    MovieFacts {
        source: source_number(q.source),
        resolution: q.resolution,
        modifier: modifier_number(q.modifier),
        edition,
        year: movie.metadata.year,
    }
}

/// `SimplifyReleaseTitle`: the characters a file name cannot hold go,
/// with the blanks before them.
fn simplify_release_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for c in title.chars() {
        if matches!(c, '<' | '>' | '?' | '*' | '|') {
            let kept = out.trim_end().len();
            out.truncate(kept);
        } else {
            out.push(c);
        }
    }
    out
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn names(formats: Vec<&CustomFormat>) -> Vec<CustomFormat> {
    formats.into_iter().cloned().collect()
}

/// `ParseCustomFormat(MovieFile, Movie)`: the scene name, else the path it
/// came from, else the file's own name, with what the file row holds.
pub fn file_formats(formats: &[CustomFormat], file: &MovieFile, movie: &Movie) -> Vec<CustomFormat> {
    let filename = file_name(&file.relative_path);
    let present = |s: &Option<String>| s.as_deref().filter(|s| !s.trim().is_empty()).map(str::to_string);
    let scene = present(&file.scene_name)
        .or_else(|| present(&file.original_file_path))
        .unwrap_or_else(|| filename.to_string());
    let title = simplify_release_title(&scene);
    let input = Input {
        release_title: &title,
        filename: Some(filename),
        release_group: file.release_group.as_deref(),
        quality: file.quality,
        languages: &file.languages,
        size: file.size,
        indexer_flags: file.indexer_flags,
        release_type: ReleaseType::Unknown,
        series_original_language: movie.metadata.original_language_id,
        movie: Some(facts(&file.quality, file.edition.as_deref(), movie)),
    };
    names(evaluate(formats, &input))
}

/// The flags a history row keeps by name (`G_Freeleech, G_Internal`), or
/// by number.
fn parse_indexer_flags(text: &str) -> i64 {
    if let Ok(n) = text.trim().parse::<i64>() {
        return n;
    }
    text.split(',')
        .map(str::trim)
        .filter_map(|name| {
            RADARR_INDEXER_FLAGS
                .iter()
                .find(|(_, n)| n.eq_ignore_ascii_case(name))
                .map(|(bit, _)| *bit)
        })
        .fold(0, |acc, bit| acc | bit)
}

/// `ParseCustomFormat(MovieHistory, Movie)`: the source title read again,
/// with the quality, languages, size and flags the row recorded. The
/// edition is the title parser's to find and comes with it.
pub fn history_formats(
    formats: &[CustomFormat],
    history: &MovieHistoryEntry,
    movie: &Movie,
) -> Vec<CustomFormat> {
    let source = history.source_title.as_deref().unwrap_or("");
    let title = simplify_release_title(source);
    let data = |key: &str| history.data.get(key).and_then(|v| v.as_str());
    let group =
        crate::parser::parse_release_group(source).or_else(|| data("releaseGroup").map(str::to_string));
    let input = Input {
        release_title: &title,
        filename: None,
        release_group: group.as_deref(),
        quality: history.quality,
        languages: &history.languages,
        size: data("size").and_then(|s| s.parse().ok()).unwrap_or(0),
        indexer_flags: data("indexerFlags").map(parse_indexer_flags).unwrap_or(0),
        release_type: ReleaseType::Unknown,
        series_original_language: movie.metadata.original_language_id,
        movie: Some(facts(&history.quality, data("edition"), movie)),
    };
    names(evaluate(formats, &input))
}

/// `ParseCustomFormat(Blocklist, Movie)`.
pub fn blocklist_formats(
    formats: &[CustomFormat],
    blocklist: &MovieBlocklistEntry,
    movie: &Movie,
) -> Vec<CustomFormat> {
    let source = blocklist.source_title.as_deref().unwrap_or("");
    let title = simplify_release_title(source);
    let group = crate::parser::parse_release_group(source);
    let input = Input {
        release_title: &title,
        filename: None,
        release_group: group.as_deref(),
        quality: blocklist.quality,
        languages: &blocklist.languages,
        size: blocklist.size.unwrap_or(0),
        indexer_flags: blocklist.indexer_flags,
        release_type: ReleaseType::Unknown,
        series_original_language: movie.metadata.original_language_id,
        movie: Some(facts(&blocklist.quality, None, movie)),
    };
    names(evaluate(formats, &input))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_lose_what_a_file_name_cannot_hold() {
        assert_eq!(
            simplify_release_title("Movie 2020 <1080p> *x264*"),
            "Movie 20201080px264"
        );
        assert_eq!(
            simplify_release_title("Movie.2020.1080p-GRP"),
            "Movie.2020.1080p-GRP"
        );
    }

    #[test]
    fn flags_are_read_by_name_or_number() {
        assert_eq!(parse_indexer_flags("G_Halfleech"), 2);
        assert_eq!(parse_indexer_flags("G_Freeleech, G_Internal"), 33);
        assert_eq!(parse_indexer_flags("0"), 0);
        assert_eq!(parse_indexer_flags("nonsense"), 0);
    }
}
