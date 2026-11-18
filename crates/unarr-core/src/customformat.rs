//! Evaluation of custom formats against a release or a file, as the
//! originals' `CustomFormatCalculationService` does it: every specification
//! type of a format must have at least one match, and no `required`
//! specification may fail.

use crate::sync::Recover;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use crate::model::{CustomFormat, QualityModel, ReleaseType};
use crate::quality::{Quality, QualitySource};

/// What a specification can look at.
#[derive(Debug, Clone)]
pub struct Input<'a> {
    pub release_title: &'a str,
    /// File name, for files; the originals also match release-title regexes
    /// against it.
    pub filename: Option<&'a str>,
    pub release_group: Option<&'a str>,
    pub quality: QualityModel,
    pub languages: &'a [i64],
    pub size: i64,
    pub indexer_flags: i64,
    pub release_type: ReleaseType,
    /// The original language of the series or of the movie, for `Original`
    /// (-2) language conditions.
    pub series_original_language: i64,
    /// Set when the release or file is a movie's: Radarr numbers its
    /// sources its own way and has conditions Sonarr lacks.
    pub movie: Option<MovieFacts<'a>>,
}

/// What Radarr's specifications look at beyond what both applications
/// share: the quality's source and modifier as Radarr numbers them
/// (`QualitySource`: cam 1, telesync 2, telecine 3, workprint 4, dvd 5,
/// tv 6, webdl 7, webrip 8, bluray 9; `Modifier`: regional 1, screener 2,
/// rawhd 3, brdisk 4, remux 5), its resolution, the edition and the year.
#[derive(Debug, Clone, Copy, Default)]
pub struct MovieFacts<'a> {
    pub source: i64,
    pub resolution: i64,
    pub modifier: i64,
    pub edition: Option<&'a str>,
    pub year: i64,
}

impl Default for Input<'_> {
    fn default() -> Self {
        Input {
            release_title: "",
            filename: None,
            release_group: None,
            quality: QualityModel::default(),
            languages: &[],
            size: 0,
            indexer_flags: 0,
            release_type: ReleaseType::Unknown,
            series_original_language: 0,
            movie: None,
        }
    }
}

/// What one condition tests, decoded from its stored `{type, body}`.
#[derive(Debug, Clone)]
enum Test {
    /// `ReleaseTitleSpecification`: the release title, or the file name.
    Title(Option<Arc<fancy_regex::Regex>>),
    Group(Option<Arc<fancy_regex::Regex>>),
    Language {
        value: i64,
        except: bool,
    },
    /// In bytes.
    Size {
        min: f64,
        max: f64,
    },
    Source(i64),
    Resolution(i64),
    QualityModifier(i64),
    Edition(Option<Arc<fancy_regex::Regex>>),
    Year {
        min: i64,
        max: i64,
    },
    IndexerFlag(i64),
    ReleaseType(i64),
    Unknown,
}

#[derive(Debug, Clone)]
struct Condition {
    test: Test,
    negate: bool,
    required: bool,
}

/// One format's conditions, grouped by specification type: within a group
/// at least one must match, and every `required` one must.
#[derive(Debug, Clone)]
struct Decoded {
    groups: Vec<Vec<Condition>>,
}

fn decode(format: &CustomFormat) -> Decoded {
    let mut kinds: Vec<&str> = Vec::new();
    let mut groups: Vec<Vec<Condition>> = Vec::new();
    for spec in format.specifications.as_array().into_iter().flatten() {
        let Some(kind) = spec["type"].as_str() else {
            continue;
        };
        let body = &spec["body"];
        let condition = Condition {
            test: decode_test(kind, body),
            negate: body["negate"].as_bool().unwrap_or(false),
            required: body["required"].as_bool().unwrap_or(false),
        };
        match kinds.iter().position(|k| *k == kind) {
            Some(i) => groups[i].push(condition),
            None => {
                kinds.push(kind);
                groups.push(vec![condition]);
            }
        }
    }
    // A format fails at its first failing group: the groups without a
    // regex, which cost next to nothing, are tried first.
    groups.sort_by_key(|group| group.iter().any(Condition::has_regex));
    Decoded { groups }
}

fn decode_test(kind: &str, body: &Value) -> Test {
    let value = &body["value"];
    let int = value.as_i64().unwrap_or(0);
    let regex = || {
        value
            .as_str()
            .filter(|pattern| !pattern.trim().is_empty())
            .and_then(compiled)
    };
    match kind {
        "ReleaseTitleSpecification" => Test::Title(regex()),
        "ReleaseGroupSpecification" => Test::Group(regex()),
        "LanguageSpecification" => Test::Language {
            value: int,
            except: body["exceptLanguage"].as_bool().unwrap_or(false),
        },
        "SizeSpecification" => {
            let gib = 1024.0 * 1024.0 * 1024.0;
            Test::Size {
                min: body["min"].as_f64().unwrap_or(0.0) * gib,
                max: body["max"].as_f64().unwrap_or(0.0) * gib,
            }
        }
        "SourceSpecification" => Test::Source(int),
        "ResolutionSpecification" => Test::Resolution(int),
        // Radarr's own conditions; a series has none of the three.
        "QualityModifierSpecification" => Test::QualityModifier(int),
        "EditionSpecification" => Test::Edition(regex()),
        "YearSpecification" => Test::Year {
            min: body["min"].as_i64().unwrap_or(0),
            max: body["max"].as_i64().unwrap_or(0),
        },
        "IndexerFlagSpecification" => Test::IndexerFlag(int),
        "ReleaseTypeSpecification" => Test::ReleaseType(int),
        other => {
            tracing::warn!(specification = other, "unknown custom format specification");
            Test::Unknown
        }
    }
}

fn regex_matches(re: &Option<Arc<fancy_regex::Regex>>, text: Option<&str>) -> bool {
    match (re, text) {
        (Some(re), Some(text)) => re.is_match(text).unwrap_or(false),
        _ => false,
    }
}

impl Test {
    fn passes(&self, input: &Input) -> bool {
        match self {
            Test::Title(re) => {
                regex_matches(re, Some(input.release_title)) || regex_matches(re, input.filename)
            }
            Test::Group(re) => regex_matches(re, input.release_group),
            Test::Language { value, except } => {
                let compared = if *value == -2 && input.series_original_language != 0 {
                    input.series_original_language
                } else {
                    *value
                };
                if *except {
                    input.languages.iter().any(|l| *l != compared)
                } else {
                    input.languages.contains(&compared)
                }
            }
            Test::Size { min, max } => {
                let size = input.size as f64;
                size > *min && size <= *max
            }
            Test::Source(value) => match input.movie {
                Some(movie) => movie.source == *value,
                None => source_ordinal(Quality::by_id(input.quality.quality_id).source) == *value,
            },
            Test::Resolution(value) => match input.movie {
                Some(movie) => movie.resolution == *value,
                None => Quality::by_id(input.quality.quality_id).resolution == *value,
            },
            Test::QualityModifier(value) => input.movie.is_some_and(|m| m.modifier == *value),
            Test::Edition(re) => input
                .movie
                .is_some_and(|m| regex_matches(re, Some(m.edition.unwrap_or("")))),
            Test::Year { min, max } => input.movie.is_some_and(|m| m.year >= *min && m.year <= *max),
            Test::IndexerFlag(value) => input.indexer_flags & value == *value && *value != 0,
            Test::ReleaseType(value) => release_type_ordinal(input.release_type) == *value,
            Test::Unknown => false,
        }
    }
}

impl Condition {
    fn has_regex(&self) -> bool {
        matches!(self.test, Test::Title(_) | Test::Group(_) | Test::Edition(_))
    }

    fn satisfied(&self, input: &Input) -> bool {
        self.test.passes(input) != self.negate
    }
}

impl Decoded {
    fn matches(&self, input: &Input) -> bool {
        !self.groups.is_empty()
            && self.groups.iter().all(|group| {
                let mut any = false;
                for condition in group {
                    let ok = condition.satisfied(input);
                    if condition.required && !ok {
                        return false;
                    }
                    any |= ok;
                }
                any
            })
    }
}

/// Compiled specification regexes, by pattern. A library has a few hundred
/// specifications and every file and history row is matched against all of
/// them, so compiling on each call dominated response times.
type PatternCache = Mutex<HashMap<String, Option<Arc<fancy_regex::Regex>>>>;

fn pattern_cache() -> &'static PatternCache {
    static CACHE: OnceLock<PatternCache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// When a specification regex was last asked for.
fn last_used() -> &'static Mutex<Option<std::time::Instant>> {
    static LAST: OnceLock<Mutex<Option<std::time::Instant>>> = OnceLock::new();
    LAST.get_or_init(Default::default)
}

/// Forget the compiled specification regexes when none was asked for in
/// `max_idle`; they are compiled again when next matched. Returns how
/// many were let go.
pub fn release_idle(max_idle: std::time::Duration) -> usize {
    let idle = last_used()
        .lock()
        .recover()
        .is_none_or(|at| at.elapsed() >= max_idle);
    if !idle {
        return 0;
    }
    let mut cache = pattern_cache().lock().recover();
    let released = cache.len();
    cache.clear();
    cache.shrink_to_fit();
    released
}

fn compiled(pattern: &str) -> Option<Arc<fancy_regex::Regex>> {
    let cache = pattern_cache();
    *last_used().lock().recover() = Some(std::time::Instant::now());
    if let Some(hit) = cache.lock().recover().get(pattern) {
        return hit.clone();
    }
    // Seek, as for the parser's patterns (`parser::netregex`): the
    // conditions with lookarounds, a tenth of a library's, took two thirds
    // of the matching time, mostly to reject titles they cannot match.
    let built = match fancy_regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .seek(true)
        .build()
    {
        Ok(re) => Some(Arc::new(re)),
        Err(e) => {
            tracing::warn!(pattern, error = %e, "custom format regex rejected");
            None
        }
    };
    cache.lock().recover().insert(pattern.to_string(), built.clone());
    built
}

/// `QualitySource` as the originals number it.
fn source_ordinal(s: QualitySource) -> i64 {
    match s {
        QualitySource::Unknown => 0,
        QualitySource::Television => 1,
        QualitySource::TelevisionRaw => 2,
        QualitySource::Web => 3,
        QualitySource::WebRip => 4,
        QualitySource::Dvd => 5,
        QualitySource::Bluray => 6,
        QualitySource::BlurayRaw => 7,
    }
}

fn release_type_ordinal(r: ReleaseType) -> i64 {
    match r {
        ReleaseType::Unknown => 0,
        ReleaseType::SingleEpisode => 1,
        ReleaseType::MultiEpisode => 2,
        ReleaseType::SeasonPack => 3,
        ReleaseType::MultiSeasonPack => 4,
    }
}

/// Whether one format matches the input.
pub fn matches(format: &CustomFormat, input: &Input) -> bool {
    decode(format).matches(input)
}

/// The formats that match, ordered by name as the API lists them.
pub fn evaluate<'f>(formats: &'f [CustomFormat], input: &Input) -> Vec<&'f CustomFormat> {
    by_name(formats.iter().filter(|f| matches(f, input)).collect())
}

fn by_name(mut formats: Vec<&CustomFormat>) -> Vec<&CustomFormat> {
    formats.sort_by(|a, b| crate::collation::compare(&a.name, &b.name));
    formats
}

/// A library's custom formats, with their conditions decoded the first
/// time they are matched. What matches many releases, files or history
/// rows in one go holds one, so that each is matched without reading the
/// conditions' JSON or looking their regexes up again: a library has a few
/// hundred conditions, and reading them took most of a history page's
/// time. It reads as the list of formats.
#[derive(Default)]
pub struct FormatSet {
    formats: Vec<CustomFormat>,
    decoded: OnceLock<Vec<Decoded>>,
}

impl FormatSet {
    pub fn new(formats: Vec<CustomFormat>) -> FormatSet {
        FormatSet {
            formats,
            decoded: OnceLock::new(),
        }
    }

    /// The formats that match, ordered by name as the API lists them.
    pub fn evaluate(&self, input: &Input) -> Vec<&CustomFormat> {
        let decoded = self
            .decoded
            .get_or_init(|| self.formats.iter().map(decode).collect());
        by_name(
            self.formats
                .iter()
                .zip(decoded)
                .filter(|(_, d)| d.matches(input))
                .map(|(f, _)| f)
                .collect(),
        )
    }

    pub fn into_vec(self) -> Vec<CustomFormat> {
        self.formats
    }
}

impl From<Vec<CustomFormat>> for FormatSet {
    fn from(formats: Vec<CustomFormat>) -> FormatSet {
        FormatSet::new(formats)
    }
}

impl std::ops::Deref for FormatSet {
    type Target = [CustomFormat];
    fn deref(&self) -> &[CustomFormat] {
        &self.formats
    }
}

impl Clone for FormatSet {
    fn clone(&self) -> FormatSet {
        FormatSet {
            formats: self.formats.clone(),
            decoded: self.decoded.clone(),
        }
    }
}

impl PartialEq for FormatSet {
    fn eq(&self, other: &FormatSet) -> bool {
        self.formats == other.formats
    }
}

impl std::fmt::Debug for FormatSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(&self.formats).finish()
    }
}

/// A release title without its media extension, as the parser sees it.
pub fn strip_media_extension(title: &str) -> String {
    crate::parser::remove_file_extension(title)
}

/// The release group of a release title, as the originals' parser finds it.
pub fn parse_release_group(title: &str) -> Option<String> {
    crate::parser::parse_release_group(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn format(name: &str, specs: Value) -> CustomFormat {
        CustomFormat {
            id: 1,
            name: name.into(),
            include_when_renaming: false,
            specifications: specs,
        }
    }

    #[test]
    fn a_format_set_matches_as_each_format_does() {
        let formats = vec![
            format(
                "x265",
                json!([{"type": "ReleaseTitleSpecification", "body": {"value": "[xh]\\.?265|HEVC", "negate": false, "required": false}}]),
            ),
            format(
                "Bluray not remux",
                json!([
                    {"type": "ReleaseTitleSpecification", "body": {"value": "remux", "negate": true, "required": true}},
                    {"type": "SourceSpecification", "body": {"value": 6, "negate": false, "required": false}},
                ]),
            ),
            format(
                "Anime group",
                json!([{"type": "ReleaseGroupSpecification", "body": {"value": "^(SubsPlease)$", "negate": false, "required": false}}]),
            ),
            format("Empty", json!([])),
            format(
                "Unknown",
                json!([{"type": "NoSuchSpecification", "body": {"value": 1}}]),
            ),
        ];
        let set = FormatSet::from(formats.clone());
        let titles = [
            ("Show.S01E01.1080p.BluRay.x265-GRP", Some("GRP"), 7),
            ("Show.S01E01.1080p.BluRay.REMUX.HEVC-GRP", Some("GRP"), 7),
            ("[SubsPlease] Show - 01 (1080p)", Some("SubsPlease"), 3),
            ("Show.S01E01.720p.WEB.H264-GRP", None, 5),
        ];
        for (title, group, quality_id) in titles {
            let input = Input {
                release_title: title,
                release_group: group,
                quality: QualityModel {
                    quality_id,
                    ..QualityModel::default()
                },
                ..Input::default()
            };
            let names =
                |list: Vec<&CustomFormat>| list.into_iter().map(|f| f.name.clone()).collect::<Vec<_>>();
            // Twice: decoded on the first call, reused on the second.
            assert_eq!(
                names(set.evaluate(&input)),
                names(evaluate(&formats, &input)),
                "{title}"
            );
            assert_eq!(
                names(set.evaluate(&input)),
                names(evaluate(&formats, &input)),
                "{title}"
            );
        }
        assert_eq!(&*set, &formats[..]);
    }

    #[test]
    fn release_title_regex_is_case_insensitive() {
        let x265 = format(
            "x265",
            json!([{"type": "ReleaseTitleSpecification", "body": {"value": "[xh]\\.?265|HEVC", "negate": false, "required": false}}]),
        );
        let hit = Input {
            release_title: "Cowboy.Bebop.S01E01.1080p.BluRay.x265.FLAC-Group",
            ..Input::default()
        };
        let miss = Input {
            release_title: "Firefly.S01E01.1080p.BluRay.x264-DEMAND",
            ..Input::default()
        };
        assert!(matches(&x265, &hit));
        assert!(!matches(&x265, &miss));
        assert_eq!(evaluate(&[x265], &hit).len(), 1);
    }

    #[test]
    fn groups_required_and_negate() {
        let f = format(
            "f",
            json!([
                {"type": "ReleaseTitleSpecification", "body": {"value": "remux", "negate": true, "required": true}},
                {"type": "ResolutionSpecification", "body": {"value": 1080, "negate": false, "required": false}},
                {"type": "ResolutionSpecification", "body": {"value": 2160, "negate": false, "required": false}},
                {"type": "LanguageSpecification", "body": {"value": -2, "negate": false, "required": false}},
            ]),
        );
        let input = Input {
            release_title: "Show.S01E01.1080p.WEB.H264-GRP",
            quality: QualityModel {
                quality_id: 3,
                ..QualityModel::default()
            },
            languages: &[1],
            series_original_language: 1,
            movie: None,
            ..Input::default()
        };
        assert!(matches(&f, &input));
        let remux = Input {
            release_title: "Show.S01E01.1080p.Remux-GRP",
            ..input.clone()
        };
        assert!(!matches(&f, &remux));
        let sd = Input {
            quality: QualityModel {
                quality_id: 1,
                ..QualityModel::default()
            },
            ..input.clone()
        };
        assert!(!matches(&f, &sd));
    }

    #[test]
    fn size_and_source_and_release_type() {
        let f = format(
            "big bluray pack",
            json!([
                {"type": "SizeSpecification", "body": {"min": 1.0, "max": 50.0}},
                {"type": "SourceSpecification", "body": {"value": 6}},
                {"type": "ReleaseTypeSpecification", "body": {"value": 3}},
            ]),
        );
        let input = Input {
            size: 2 * 1024 * 1024 * 1024,
            quality: QualityModel {
                quality_id: 7,
                ..QualityModel::default()
            },
            release_type: ReleaseType::SeasonPack,
            ..Input::default()
        };
        assert!(matches(&f, &input));
        assert!(!matches(
            &f,
            &Input {
                size: 1000,
                ..input.clone()
            }
        ));
        assert_eq!(
            parse_release_group("Firefly.S01E01.720p.HDTV.x264-CTU"),
            Some("CTU".into())
        );
        assert_eq!(parse_release_group("Firefly - S01E01 - Title.mkv"), None);
        // Suffixes the originals strip, bracketed anime groups, exceptions.
        assert_eq!(
            parse_release_group("The.Pitt.S01E15.1080p.AMZN.WEB-DL.DDP5.1.H.264-FLUX-AsRequested").as_deref(),
            Some("FLUX")
        );
        assert_eq!(
            parse_release_group("Silicon.Valley.S04E07.1080p.AMZN.WEBRip.DD5.1.x264-NTb").as_deref(),
            Some("NTb")
        );
        assert_eq!(
            parse_release_group("[SubsPlease] Show - 01 (1080p) [ABCDEF12].mkv").as_deref(),
            Some("SubsPlease")
        );
        assert!(parse_release_group("Show.S01E01.1080p.WEB-DL.x264-2024-01").is_none());
        assert_eq!(
            parse_release_group("Show.S01E01.1080p.BluRay.x264-Tigole[QxR]").as_deref(),
            Some("Tigole")
        );
        assert_eq!(strip_media_extension("Show.S01E01.mkv"), "Show.S01E01");
        assert_eq!(
            strip_media_extension("Show.S01E01.x264-GRP"),
            "Show.S01E01.x264-GRP"
        );
    }
}
