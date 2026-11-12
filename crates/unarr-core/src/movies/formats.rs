//! Custom formats of movie files, history rows and blocklist entries, as
//! the originals' `CustomFormatCalculationService` works them out: the
//! shared matcher of [`crate::customformat`], fed what a movie has.

use crate::customformat::{evaluate, Input, MovieFacts};
use crate::model::{CustomFormat, QualityModel, ReleaseType};
use crate::movies::model::{Movie, MovieBlocklistEntry, MovieFile, MovieHistoryEntry};
use crate::movies::parser::{parse_movie_title, simplify_release_title};
use crate::movies::quality::{modifier_number, source_number, MovieQuality};

pub(crate) fn facts<'a>(quality: &QualityModel, edition: Option<&'a str>, movie: &Movie) -> MovieFacts<'a> {
    let q = MovieQuality::by_id(quality.quality_id);
    MovieFacts {
        source: source_number(q.source),
        resolution: q.resolution,
        modifier: modifier_number(q.modifier),
        edition,
        year: movie.metadata.year,
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn names(formats: Vec<&CustomFormat>) -> Vec<CustomFormat> {
    formats.into_iter().cloned().collect()
}

/// `ParseCustomFormat(MovieFile, Movie)`: the scene name, else the name of
/// the file it came from (not its folders, which would lend the file what
/// the release's folder says), else the file's own name, with what the
/// file row holds.
pub fn file_formats(formats: &[CustomFormat], file: &MovieFile, movie: &Movie) -> Vec<CustomFormat> {
    let filename = file_name(&file.relative_path);
    let present = |s: &Option<String>| s.as_deref().filter(|s| !s.trim().is_empty()).map(str::to_string);
    let scene = present(&file.scene_name)
        .or_else(|| present(&file.original_file_path).map(|path| file_name(&path).to_string()))
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

/// `ParseCustomFormat(RemoteMovie, size)`: a release the engine mapped to
/// a movie, with what its name parsed to and the languages worked out.
pub fn remote_formats(
    formats: &[CustomFormat],
    parsed: &crate::movies::parser::ParsedMovieInfo,
    languages: &[i64],
    size: i64,
    indexer_flags: i64,
    movie: &Movie,
) -> Vec<CustomFormat> {
    let quality = parsed.quality.quality;
    let input = Input {
        release_title: &parsed.simple_release_title,
        filename: None,
        release_group: parsed.release_group.as_deref(),
        quality,
        languages,
        size,
        indexer_flags,
        release_type: ReleaseType::Unknown,
        series_original_language: movie.metadata.original_language_id,
        movie: Some(facts(&quality, Some(parsed.edition.as_str()), movie)),
    };
    names(evaluate(formats, &input))
}

/// What the title parser makes of a source title, as both rows use it:
/// the simplified release title (with the movie's title cut out, when the
/// name parses), the release group and the edition.
fn parsed_source(source: &str) -> (String, Option<String>, Option<String>) {
    match parse_movie_title(source, false) {
        Some(parsed) => (
            parsed.simple_release_title,
            parsed.release_group,
            Some(parsed.edition),
        ),
        None => (simplify_release_title(source), None, None),
    }
}

/// `ParseCustomFormat(MovieHistory, Movie)`: the source title parsed
/// again, with the quality, languages, size and flags the row recorded.
pub fn history_formats(
    formats: &[CustomFormat],
    history: &MovieHistoryEntry,
    movie: &Movie,
) -> Vec<CustomFormat> {
    let (title, group, edition) = parsed_source(history.source_title.as_deref().unwrap_or(""));
    let data = |key: &str| history.data.get(key).and_then(|v| v.as_str());
    let input = Input {
        release_title: &title,
        filename: None,
        release_group: group.as_deref(),
        quality: history.quality,
        languages: &history.languages,
        size: data("size").and_then(|s| s.parse().ok()).unwrap_or(0),
        // `Enum.TryParse`: text that is not a flag list is no flag at all.
        indexer_flags: data("indexerFlags")
            .and_then(crate::movies::quality::parse_indexer_flags)
            .unwrap_or(0),
        release_type: ReleaseType::Unknown,
        series_original_language: movie.metadata.original_language_id,
        movie: Some(facts(&history.quality, edition.as_deref(), movie)),
    };
    names(evaluate(formats, &input))
}

/// `ParseCustomFormat(Blocklist, Movie)`.
pub fn blocklist_formats(
    formats: &[CustomFormat],
    blocklist: &MovieBlocklistEntry,
    movie: &Movie,
) -> Vec<CustomFormat> {
    let (title, group, edition) = parsed_source(blocklist.source_title.as_deref().unwrap_or(""));
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
        movie: Some(facts(&blocklist.quality, edition.as_deref(), movie)),
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
        use crate::movies::quality::parse_indexer_flags;
        assert_eq!(parse_indexer_flags("G_Halfleech"), Some(2));
        assert_eq!(parse_indexer_flags("g_freeleech, G_Internal"), Some(33));
        assert_eq!(parse_indexer_flags("0"), Some(0));
        // One name that is no flag spoils the list, as `Enum.TryParse` has it.
        assert_eq!(parse_indexer_flags("nonsense"), None);
        assert_eq!(parse_indexer_flags("G_Freeleech, Bogus"), None);
    }

    #[test]
    fn a_file_without_a_scene_name_is_read_by_the_name_it_came_with() {
        use crate::movies::decision::testing;
        let movie = testing::movie(1, "Movie", 2020);
        let remux = CustomFormat {
            id: 1,
            name: "Remux".into(),
            include_when_renaming: false,
            specifications: serde_json::json!([
                {"type": "ReleaseTitleSpecification", "body": {"value": "remux", "negate": false, "required": true}}
            ]),
        };
        let mut file = MovieFile {
            id: 1,
            movie_id: 1,
            relative_path: "Movie (2020).mkv".into(),
            original_file_path: Some("Movie.2020.2160p.BluRay.REMUX-GRP/abc.mkv".into()),
            size: 1,
            date_added: "2026-01-01T00:00:00Z".parse().unwrap(),
            scene_name: None,
            release_group: None,
            edition: None,
            quality: testing::quality(7, 1),
            languages: vec![1],
            media_info: None,
            indexer_flags: 0,
        };
        // The folder's name is not the file's.
        assert!(file_formats(std::slice::from_ref(&remux), &file, &movie).is_empty());
        file.original_file_path = Some("downloads/Movie.2020.2160p.BluRay.REMUX-GRP.mkv".into());
        assert_eq!(file_formats(&[remux], &file, &movie).len(), 1);
    }
}
