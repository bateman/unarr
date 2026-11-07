//! Evaluation of custom formats against a release or a file, as the
//! originals' `CustomFormatCalculationService` does it: every specification
//! type of a format must have at least one match, and no `required`
//! specification may fail.

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
    /// The series' original language, for `Original` (-2) language conditions.
    pub series_original_language: i64,
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
fn compiled(pattern: &str) -> Option<Arc<fancy_regex::Regex>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<fancy_regex::Regex>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().unwrap().get(pattern) {
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
    cache.lock().unwrap().insert(pattern.to_string(), built.clone());
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
        "SourceSpecification" => {
            let source = Quality::by_id(input.quality.quality_id).source;
            source_ordinal(source) == value_int
        }
        "ResolutionSpecification" => Quality::by_id(input.quality.quality_id).resolution == value_int,
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

/// Media extensions the originals strip before parsing a title.
const MEDIA_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "m4v", "mpg", "mpeg", "mov", "wmv", "ts", "m2ts", "webm", "flv", "ogm", "ogv",
    "divx", "xvid", "iso", "img", "strm", "nzb", "par2", "rar", "zip", "srt",
];

/// A release title without its media extension, as the parser sees it.
pub fn strip_media_extension(title: &str) -> &str {
    match title.rsplit_once('.') {
        Some((stem, ext)) if MEDIA_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) => stem,
        _ => title,
    }
}

/// Best-effort release group from a release title, for history rows that
/// only keep the title: the token glued to the last dash, as scene names
/// put it (`...x264-GROUP`). Titles with spaces around the dash are the
/// originals' own naming, which carries no group.
macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static fancy_regex::Regex {
            static RE: OnceLock<fancy_regex::Regex> = OnceLock::new();
            RE.get_or_init(|| fancy_regex::Regex::new($pattern).expect(stringify!($name)))
        }
    };
}

// The originals' Parser regexes for the release group, verbatim but for the
// duplicate group name, which Rust does not allow.
regex!(
    website_prefix_re,
    r"(?i)^(?:(?:\[|\()\s*)?(?:www\.)?[-a-z0-9-]{1,256}\.(?<!Naruto-Kun\.)(?:[a-z]{2,6}\.[a-z]{2,6}|xn--[a-z0-9-]{4,}|[a-z]{2,})\b(?:\s*(?:\]|\))|[ -]{2,})[ -]*"
);
regex!(
    clean_torrent_suffix_re,
    r"(?i)\[(?:ettv|rartv|rarbg|cttv|publichd)\]$"
);
regex!(
    anime_release_group_re,
    r"(?i)^(?:\[(?<subgroup>(?!\s).+?(?<!\s))\](?:_|-|\s|\.)?)"
);
regex!(
    clean_release_group_re,
    r"(?i)^(.*?[-._ ](S\d+E\d+)[-._ ])|(-(RP|1|NZBGeek|Obfuscated|Scrambled|sample|Pre|postbot|xpost|Rakuv[a-z0-9]*|WhiteRev|BUYMORE|AsRequested|AlternativeToRequested|GEROV|Z0iDS3N|Chamele0n|4P|4Planet|AlteZachen|RePACKPOST))+$"
);
regex!(
    exception_release_group_re,
    r"(?i)(?<=[._ \[])(?<releasegroup>(Silence|afm72|Panda|Ghost|MONOLITH|Tigole|Joy|ImE|UTR|t3nzin|Anime Time|Project Angel|Hakata Ramen|HONE|Vyndros|SEV|Garshasp|Kappa|Natty|RCVR|SAMPA|YOGI|r00t|EDGE2020|RZeroX)(?=\]|\)))"
);
regex!(
    exception_release_group_exact_re,
    r"(?i)(?<releasegroup>(?:D\-Z0N3|Fight-BB|VARYG|E\.N\.D|KRaLiMaRKo|BluDragon|DarQ|KCRT|BEN[_. ]THE[_. ]MEN)\b)"
);
regex!(
    release_group_re,
    r"(?i)-(?<releasegroup>[a-z0-9]+(?<part2>-[a-z0-9]+)?(?!.+?(?:480p|576p|720p|1080p|2160p)))(?<!(?:WEB-DL|Blu-Ray|480p|576p|720p|1080p|2160p|DTS-HD|DTS-X|DTS-MA|DTS-ES|-ES|-EN|-CAT|[ ._]\d{4}-\d{2}|-\d{2})(?:\k<part2>)?)(?:\b|[-._ ]|$)|[-._ ]\[(?<bracketgroup>[a-z0-9]+)\]$"
);
regex!(invalid_release_group_re, r"(?i)^([se]\d+|[0-9a-f]{8})$");

/// The release group of a release title, as the originals' parser finds it.
pub fn parse_release_group(title: &str) -> Option<String> {
    let title = strip_media_extension(title.trim());
    let title = website_prefix_re().replace(title, "").into_owned();
    let title = clean_torrent_suffix_re().replace(&title, "").into_owned();
    if let Ok(Some(c)) = anime_release_group_re().captures(title.as_str()) {
        return c.name("subgroup").map(|m| m.as_str().to_string());
    }
    let title = clean_release_group_re().replace_all(&title, "").into_owned();
    if let Some(c) = exception_release_group_re()
        .captures_iter(title.as_str())
        .flatten()
        .last()
    {
        return c.name("releasegroup").map(|m| m.as_str().to_string());
    }
    if let Some(c) = exception_release_group_exact_re()
        .captures_iter(title.as_str())
        .flatten()
        .last()
    {
        return c.name("releasegroup").map(|m| m.as_str().to_string());
    }
    let last = release_group_re()
        .captures_iter(title.as_str())
        .flatten()
        .last()?;
    let group = last
        .name("releasegroup")
        .or_else(|| last.name("bracketgroup"))
        .map(|m| m.as_str())?;
    if group.parse::<i64>().is_ok() || invalid_release_group_re().is_match(group).unwrap_or(false) {
        return None;
    }
    Some(group.to_string())
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
