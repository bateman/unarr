//! Evaluation of custom formats against a release or a file, as the
//! originals' `CustomFormatCalculationService` does it: every specification
//! type of a format must have at least one match, and no `required`
//! specification may fail.

use crate::sync::Recover;
use std::collections::{BTreeMap, HashMap};
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

/// One decoded specification: `{type, body}` as stored.
struct Spec<'a> {
    kind: &'a str,
    negate: bool,
    required: bool,
    body: &'a Value,
}

fn specs(format: &CustomFormat) -> Vec<Spec<'_>> {
    format
        .specifications
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|s| {
                    let kind = s["type"].as_str()?;
                    let body = &s["body"];
                    Some(Spec {
                        kind,
                        negate: body["negate"].as_bool().unwrap_or(false),
                        required: body["required"].as_bool().unwrap_or(false),
                        body,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
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
    let built = match fancy_regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
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

fn regex_matches(pattern: &str, text: Option<&str>) -> bool {
    let Some(text) = text else {
        return false;
    };
    if pattern.trim().is_empty() {
        return false;
    }
    compiled(pattern).is_some_and(|re| re.is_match(text).unwrap_or(false))
}

fn satisfied_without_negate(spec: &Spec, input: &Input) -> bool {
    let value_str = spec.body["value"].as_str().unwrap_or("");
    let value_int = spec.body["value"].as_i64().unwrap_or(0);
    match spec.kind {
        "ReleaseTitleSpecification" => {
            regex_matches(value_str, Some(input.release_title)) || regex_matches(value_str, input.filename)
        }
        "ReleaseGroupSpecification" => regex_matches(value_str, input.release_group),
        "LanguageSpecification" => {
            let compared = if value_int == -2 && input.series_original_language != 0 {
                input.series_original_language
            } else {
                value_int
            };
            if spec.body["exceptLanguage"].as_bool().unwrap_or(false) {
                input.languages.iter().any(|l| *l != compared)
            } else {
                input.languages.contains(&compared)
            }
        }
        "SizeSpecification" => {
            let gib = 1024.0 * 1024.0 * 1024.0;
            let min = spec.body["min"].as_f64().unwrap_or(0.0) * gib;
            let max = spec.body["max"].as_f64().unwrap_or(0.0) * gib;
            let size = input.size as f64;
            size > min && size <= max
        }
        "SourceSpecification" => match input.movie {
            Some(movie) => movie.source == value_int,
            None => source_ordinal(Quality::by_id(input.quality.quality_id).source) == value_int,
        },
        "ResolutionSpecification" => match input.movie {
            Some(movie) => movie.resolution == value_int,
            None => Quality::by_id(input.quality.quality_id).resolution == value_int,
        },
        // Radarr's own conditions; a series has none of the three.
        "QualityModifierSpecification" => input.movie.is_some_and(|m| m.modifier == value_int),
        "EditionSpecification" => input
            .movie
            .is_some_and(|m| regex_matches(value_str, Some(m.edition.unwrap_or("")))),
        "YearSpecification" => input.movie.is_some_and(|m| {
            let min = spec.body["min"].as_i64().unwrap_or(0);
            let max = spec.body["max"].as_i64().unwrap_or(0);
            m.year >= min && m.year <= max
        }),
        "IndexerFlagSpecification" => input.indexer_flags & value_int == value_int && value_int != 0,
        "ReleaseTypeSpecification" => release_type_ordinal(input.release_type) == value_int,
        other => {
            tracing::warn!(specification = other, "unknown custom format specification");
            false
        }
    }
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

fn satisfied(spec: &Spec, input: &Input) -> bool {
    let m = satisfied_without_negate(spec, input);
    if spec.negate {
        !m
    } else {
        m
    }
}

/// Whether one format matches the input.
pub fn matches(format: &CustomFormat, input: &Input) -> bool {
    let specs = specs(format);
    if specs.is_empty() {
        return false;
    }
    // Group by specification type: within a group at least one must match,
    // and every `required` one must.
    let mut groups: BTreeMap<&str, Vec<(bool, bool)>> = BTreeMap::new();
    for spec in &specs {
        groups
            .entry(spec.kind)
            .or_default()
            .push((spec.required, satisfied(spec, input)));
    }
    groups.values().all(|results| {
        let required_failed = results.iter().any(|(required, ok)| *required && !ok);
        let none_matched = results.iter().all(|(_, ok)| !ok);
        !(required_failed || none_matched)
    })
}

/// The formats that match, ordered by name as the API lists them.
pub fn evaluate<'f>(formats: &'f [CustomFormat], input: &Input) -> Vec<&'f CustomFormat> {
    let mut out: Vec<&CustomFormat> = formats.iter().filter(|f| matches(f, input)).collect();
    out.sort_by(|a, b| crate::collation::compare(&a.name, &b.name));
    out
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
