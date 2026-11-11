//! Replay Radarr's parser test vectors (`tests/vectors/radarr/parser/*.json`,
//! extracted from the Radarr repository by `scripts/port-vectors.py`)
//! against `unarr_core::movies::parser`. Every test method of the original
//! fixtures is mapped to the assertion it makes; a method this file does not
//! know fails the test, so a fixture added upstream never passes by being
//! ignored.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;
use unarr_core::movies::parser::language::{lang, parse_languages};
use unarr_core::movies::parser::quality::{parse_quality, TELESYNC};
use unarr_core::movies::parser::{
    clean_movie_title, parse_movie_path, parse_movie_title, parse_release_group, scene_title, to_url_slug,
};
use unarr_core::movies::quality::MovieQuality;
use unarr_core::parser::title::normalize_imdb_id;
use unarr_core::parser::Detection;

/// Test methods of the original that exercise code outside this port, or
/// that are not data.
const SKIPPED: &[&str] = &[
    // Subtitle file names: milestone 4c (import).
    "should_parse_subtitle_language_english",
    "should_parse_subtitle_language",
    "should_parse_subtitle_language_unknown",
    "should_parse_title_and_tags",
    "should_not_parse_false_title",
    // Random input is generated in the original, not data.
    "should_not_parse_random",
    // Inside a block comment in the original (episode cases Radarr
    // inherited); the extractor reads them onto the next method.
    "should_remove_accents_from_title",
];

/// Whole fixtures about other components, or commented out upstream.
const SKIPPED_FIXTURES: &[&str] = &[
    // ISO codes of subtitle files: milestone 4c.
    "IsoLanguagesFixture",
    // The whole class is a block comment in the original.
    "ParseQualityDefinitionFixture",
];

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/radarr/parser")
}

fn s(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_default()
}

fn i(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).unwrap_or(0)
}

fn member(v: &Value) -> String {
    s(&v["member"])
}

/// `AsOsAgnostic`: `C:\Test\a\b` becomes `/Test/a/b`.
fn os_agnostic(path: &str) -> String {
    let p = path.replace('\\', "/");
    match p.strip_prefix("C:/") {
        Some(rest) => format!("/{rest}"),
        None => p,
    }
}

fn quality_by_member(name: &str) -> i64 {
    let wanted = name.to_lowercase();
    MovieQuality::all()
        .find(|q| q.name.replace('-', "").to_lowercase() == wanted)
        .map(|q| q.id)
        .unwrap_or_else(|| panic!("unknown quality {name}"))
}

/// The language a `should_parse_language_<name>` method asks for.
fn language_of(name: &str) -> Option<i64> {
    Some(match name {
        "english" => lang::ENGLISH,
        "unknown" | "polish_subbed" => lang::UNKNOWN,
        "french" => lang::FRENCH,
        "spanish" => lang::SPANISH,
        "german" => lang::GERMAN,
        "italian" => lang::ITALIAN,
        "danish" => lang::DANISH,
        "dutch" => lang::DUTCH,
        "japanese" => lang::JAPANESE,
        "icelandic" => lang::ICELANDIC,
        "chinese" => lang::CHINESE,
        "russian" => lang::RUSSIAN,
        "romanian" => lang::ROMANIAN,
        "hindi" => lang::HINDI,
        "thai" => lang::THAI,
        "bulgarian" => lang::BULGARIAN,
        "brazilian_portuguese" => lang::PORTUGUESE_BR,
        "polish" => lang::POLISH,
        "vietnamese" => lang::VIETNAMESE,
        "swedish" => lang::SWEDISH,
        "norwegian" => lang::NORWEGIAN,
        "finnish" => lang::FINNISH,
        "turkish" => lang::TURKISH,
        "portuguese" => lang::PORTUGUESE,
        "flemish" => lang::FLEMISH,
        "greek" => lang::GREEK,
        "korean" => lang::KOREAN,
        "hungarian" => lang::HUNGARIAN,
        "hebrew" => lang::HEBREW,
        "lithuanian" => lang::LITHUANIAN,
        "czech" => lang::CZECH,
        "arabic" => lang::ARABIC,
        "ukrainian" | "ukrainian_multi" => lang::UKRAINIAN,
        "persian" => lang::PERSIAN,
        "bengali" => lang::BENGALI,
        "telugu" => lang::TELUGU,
        "slovak" => lang::SLOVAK,
        "latvian" => lang::LATVIAN,
        "spanish_latino" => lang::SPANISH_LATINO,
        "catalan" => lang::CATALAN,
        "malayalam" => lang::MALAYALAM,
        "kannada" => lang::KANNADA,
        "albanian" => lang::ALBANIAN,
        "afrikaans" => lang::AFRIKAANS,
        "marathi" => lang::MARATHI,
        "tagalog" => lang::TAGALOG,
        "urdu" => lang::URDU,
        "romansh" => lang::ROMANSH,
        "mongolian" => lang::MONGOLIAN,
        "georgian" => lang::GEORGIAN,
        _ => return None,
    })
}

/// What `ParseAndVerifyQuality` is asked to find, by test method.
fn quality_expectation(method: &str) -> Option<(&'static str, i64, &'static str)> {
    Some(match method {
        "should_parse_ts" => ("telesync", 720, "none"),
        "should_parse_cam" => ("cam", 0, "none"),
        "should_parse_sdtv_quality" => ("tv", 480, "none"),
        "should_parse_dvd_quality" => ("dvd", 0, "none"),
        "should_parse_dvdr_quality" => ("dvd", 480, "remux"),
        "should_parse_webdl480p_quality" => ("webdl", 480, "none"),
        "should_parse_webrip480p_quality" => ("webrip", 480, "none"),
        "should_parse_bluray480p_quality" => ("bluray", 480, "none"),
        "should_parse_hdtv720p_quality" => ("tv", 720, "none"),
        "should_parse_hdtv1080p_quality" => ("tv", 1080, "none"),
        "should_parse_hdtv2160p_quality" => ("tv", 2160, "none"),
        "should_parse_webdl720p_quality" => ("webdl", 720, "none"),
        "should_parse_webrip720p_quality" => ("webrip", 720, "none"),
        "should_parse_webdl1080p_quality" => ("webdl", 1080, "none"),
        "should_parse_webrip1080p_quality" => ("webrip", 1080, "none"),
        "should_parse_webdl2160p_quality" => ("webdl", 2160, "none"),
        "should_parse_webrip2160p_quality" => ("webrip", 2160, "none"),
        "should_parse_bluray720p_quality" | "should_parse_remux720p_as_bluray720p_quality" => {
            ("bluray", 720, "none")
        }
        "should_parse_bluray1080p_quality" => ("bluray", 1080, "none"),
        "should_parse_bluray2160p_quality" => ("bluray", 2160, "none"),
        "should_parse_bluray576p_quality" => ("bluray", 576, "none"),
        "should_parse_remux1080p_quality" => ("bluray", 1080, "remux"),
        "should_parse_remux2160p_quality" => ("bluray", 2160, "remux"),
        "should_parse_brdisk_1080p_quality" => ("bluray", 1080, "brdisk"),
        "should_parse_raw_quality" => ("tv", 1080, "rawhd"),
        "quality_parse" => ("unknown", 0, "none"),
        _ => return None,
    })
}

/// `ParseAndVerifyQuality`: the resolution when one is asked for, the
/// source, the modifier when one is asked for, the version.
fn verify_quality(
    title: &str,
    source: &str,
    proper: bool,
    resolution: i64,
    modifier: &str,
) -> Result<(), String> {
    let parsed = parse_quality(title);
    let quality = MovieQuality::by_id(parsed.quality.quality_id);
    // The original writes the parsed resolution into its TELESYNC quality,
    // which has none; Unarr's table is constant.
    let resolution_ok = resolution == 0 || quality.resolution == resolution || quality.id == TELESYNC;
    let ok = resolution_ok
        && quality.source == source
        && (modifier == "none" || quality.modifier == modifier)
        && parsed.quality.version == if proper { 2 } else { 1 };
    if ok {
        Ok(())
    } else {
        Err(format!("{} v{}", quality.name, parsed.quality.version))
    }
}

struct Outcome {
    failures: Vec<String>,
    checked: usize,
}

impl Outcome {
    fn check(&mut self, ok: bool, what: impl FnOnce() -> String) {
        self.checked += 1;
        if !ok {
            self.failures.push(what());
        }
    }
}

/// Run one test method's cases. Returns `false` when the method is unknown.
fn run_method(fixture: &str, method: &str, cases: &[Value], out: &mut Outcome) -> bool {
    for case in cases {
        let a = case.as_array().cloned().unwrap_or_default();
        let label = || {
            format!(
                "{fixture}::{method}{}",
                serde_json::to_string(case).unwrap_or_default()
            )
        };
        let title = a.first().map(s).unwrap_or_default();
        let parsed = || parse_movie_title(&title, false);
        let parsed_dir = || parse_movie_title(&title, true);

        if let Some((source, resolution, modifier)) = quality_expectation(method) {
            let proper = a.get(1).and_then(Value::as_bool).unwrap_or(false);
            let r = verify_quality(&title, source, proper, resolution, modifier);
            out.check(r.is_ok(), || format!("{} -> {:?}", label(), r));
            continue;
        }
        if let Some(language) = method
            .strip_prefix("should_parse_language_")
            .filter(|name| *name != "french_english")
            .and_then(language_of)
        {
            // These ask the language parser itself; the others parse the
            // name as a movie's first. Two of those parse it as a release
            // rather than as a folder; no case of theirs starts with a year.
            const DIRECT: &[&str] = &[
                "spanish_latino",
                "malayalam",
                "kannada",
                "albanian",
                "afrikaans",
                "marathi",
                "tagalog",
                "urdu",
                "romansh",
                "mongolian",
                "georgian",
            ];
            let name = method.trim_start_matches("should_parse_language_");
            let r = if DIRECT.contains(&name) {
                Some(parse_languages(&title))
            } else {
                parsed_dir().map(|p| p.languages)
            };
            let ok = r.as_ref().is_some_and(|l| {
                if name == "spanish_latino" {
                    l.first() == Some(&language)
                } else {
                    l.contains(&language)
                }
            });
            out.check(ok, || format!("{} -> {:?}", label(), r));
            continue;
        }

        match method {
            // --- titles
            "should_parse_movie_title" => {
                let r = parsed().map(|p| p.primary_movie_title().to_string());
                out.check(r.as_deref() == Some(&s(&a[1])), || {
                    format!("{} -> {:?}", label(), r)
                });
            }
            "should_parse_movie_folder_name" => {
                let r = parsed_dir().map(|p| p.primary_movie_title().to_string());
                out.check(r.as_deref() == Some(&s(&a[1])), || {
                    format!("{} -> {:?}", label(), r)
                });
            }
            "should_parse_anime_movie_title" | "should_parse_anime_movie_title_without_year" => {
                let r = parsed();
                let ok = r.as_ref().is_some_and(|p| {
                    p.primary_movie_title() == s(&a[1])
                        && p.release_group.as_deref() == Some(&s(&a[2]))
                        && (a.len() < 4 || p.year == i(&a[3]))
                });
                out.check(ok, || format!("{} -> {:?}", label(), r));
            }
            "should_parse_german_movie" => {
                let r = parsed();
                let ok = r.as_ref().is_some_and(|p| {
                    p.primary_movie_title() == s(&a[1]) && p.edition == s(&a[2]) && p.year == i(&a[3])
                });
                out.check(ok, || format!("{} -> {:?}", label(), r));
            }
            "should_parse_movie_alternative_titles" => {
                let expected: Vec<String> = a[1]
                    .as_array()
                    .map(|x| x.iter().map(s).collect())
                    .unwrap_or_default();
                let r = parsed_dir().map(|p| p.movie_titles);
                out.check(r.as_ref() == Some(&expected), || {
                    format!("{} -> {:?}", label(), r)
                });
            }
            "should_parse_movie_year" => {
                let r = parsed().map(|p| p.year);
                out.check(r == Some(i(&a[1])), || format!("{} -> {:?}", label(), r));
            }
            "should_parse_tmdb_id" => {
                let r = parsed().map(|p| p.tmdb_id);
                out.check(r == Some(i(&a[1])), || format!("{} -> {:?}", label(), r));
            }
            "should_parse_imdb_in_title" => {
                let r = parsed_dir().map(|p| p.imdb_id);
                out.check(r.as_deref() == Some(&s(&a[1])), || {
                    format!("{} -> {:?}", label(), r)
                });
            }
            "should_parse_hardcoded_subs" => {
                let r = parsed().map(|p| p.hardcoded_subs);
                let expected = a[1].as_str().map(str::to_string);
                out.check(r.as_ref() == Some(&expected), || {
                    format!("{} -> {:?}", label(), r)
                });
            }
            "should_normalize_imdbid" => {
                let r = normalize_imdb_id(&title);
                out.check(r == a[1].as_str().map(str::to_string), || {
                    format!("{} -> {:?}", label(), r)
                });
            }
            "should_parse_edition" | "should_not_parse_edition" => {
                let expected = a.get(1).map(s).unwrap_or_default();
                let r = parsed().map(|p| p.edition);
                out.check(r.as_deref() == Some(&expected), || {
                    format!("{} -> {:?}", label(), r)
                });
            }
            "should_not_parse_crap"
            | "should_not_parse_file_name_without_proper_spacing"
            | "should_not_parse_invalid_release_name" => {
                let r = parsed();
                out.check(r.is_none(), || format!("{} -> {:?}", label(), r));
            }
            "should_not_parse_url_in_name" => {
                let r = parsed().map(|p| clean_movie_title(p.primary_movie_title()));
                let expected = clean_movie_title(&s(&a[1]));
                out.check(r.as_deref() == Some(&expected), || {
                    format!("{} -> {:?}", label(), r)
                });
            }
            "should_properly_parse_hashed_releases" => {
                let r = parse_movie_path(&os_agnostic(&title));
                let ok = r.as_ref().is_some_and(|p| {
                    p.primary_movie_title() == s(&a[1])
                        && p.quality.quality.quality_id == quality_by_member(&member(&a[2]))
                        && p.release_group == a[3].as_str().map(str::to_string)
                });
                out.check(ok, || format!("{} -> {:?}", label(), r));
            }

            // --- languages
            "should_parse_language_french_english" => {
                let r = parsed_dir().map(|p| p.languages);
                let ok = r
                    .as_ref()
                    .is_some_and(|l| l.contains(&lang::FRENCH) && l.contains(&lang::ENGLISH));
                out.check(ok, || format!("{} -> {:?}", label(), r));
            }
            "should_not_parse_wrong_language_in_title" => {
                let r = parsed_dir().map(|p| p.languages);
                out.check(r == Some(vec![lang::UNKNOWN]), || {
                    format!("{} -> {:?}", label(), r)
                });
            }
            "should_add_original_language_to_german_release_with_dl_tag" => {
                let r = parsed().map(|p| p.languages);
                out.check(r == Some(vec![lang::GERMAN, lang::ORIGINAL]), || {
                    format!("{} -> {:?}", label(), r)
                });
            }
            "should_add_original_language_and_english_to_german_release_with_ml_tag" => {
                let r = parsed().map(|p| p.languages);
                let ok = r == Some(vec![lang::GERMAN, lang::ORIGINAL, lang::ENGLISH]);
                out.check(ok, || format!("{} -> {:?}", label(), r));
            }
            "should_not_add_original_language_to_german_release_when_title_contains_web_dl" => {
                let r = parsed().map(|p| p.languages);
                out.check(r == Some(vec![lang::GERMAN]), || {
                    format!("{} -> {:?}", label(), r)
                });
            }
            "should_parse_original_title_from_release_name" => {
                let r = parsed().map(|p| p.languages);
                out.check(r == Some(vec![lang::ORIGINAL]), || {
                    format!("{} -> {:?}", label(), r)
                });
            }

            // --- quality
            "should_parse_quality_from_other_source" => {
                let resolution = match member(&a[2]).as_str() {
                    "Unknown" => 0,
                    other => other
                        .trim_start_matches('R')
                        .trim_end_matches('p')
                        .parse()
                        .unwrap(),
                };
                let source = member(&a[1]).to_lowercase();
                let modifier = member(&a[3]).to_lowercase();
                for c in ['-', '.', ' ', '_'] {
                    let name = format!("My series S01E01 {}", title.replace(' ', &c.to_string()));
                    let r = verify_quality(&name, &source, false, resolution, &modifier);
                    out.check(r.is_ok(), || format!("{} as {name:?} -> {:?}", label(), r));
                }
            }
            "should_parse_full_quality_from_name"
            | "should_parse_resolution_from_name"
            | "should_parse_source_from_name"
            | "should_parse_quality_from_extension"
            | "should_parse_resolution_from_name_and_source_from_extension" => {
                let (source, resolution) = match method {
                    "should_parse_full_quality_from_name" => (Detection::Name, Detection::Name),
                    "should_parse_resolution_from_name" => (Detection::Unknown, Detection::Name),
                    "should_parse_source_from_name" => (Detection::Name, Detection::Unknown),
                    "should_parse_quality_from_extension" => (Detection::Extension, Detection::Extension),
                    _ => (Detection::Extension, Detection::Name),
                };
                let r = parse_quality(&title);
                let ok = r.source_detection == source && r.resolution_detection == resolution;
                out.check(ok, || format!("{} -> {:?}", label(), r));
            }
            "should_be_able_to_parse_repack" => {
                let r = parse_quality(&title);
                // AnimeVersionFixture gives the version only.
                let ok = if a.len() == 2 {
                    r.quality.version == i(&a[1])
                } else {
                    r.quality.is_repack == a[1].as_bool().unwrap_or(false) && r.quality.version == i(&a[2])
                };
                out.check(ok, || format!("{} -> {:?}", label(), r));
            }
            "should_parse_reality_from_title" => {
                let r = parse_quality(&title);
                out.check(r.quality.real == i(&a[1]), || format!("{} -> {:?}", label(), r));
            }
            "should_parse_version_from_title" => {
                let r = parse_quality(&title);
                out.check(r.quality.version == i(&a[1]), || {
                    format!("{} -> {:?}", label(), r)
                });
            }
            "should_parse_ultrahd_from_title" => {
                let r = MovieQuality::by_id(parse_quality(&title).quality.quality_id);
                out.check(r.resolution == 2160, || format!("{} -> {:?}", label(), r));
            }

            // --- release groups
            "should_parse_release_group"
            | "should_parse_exception_release_group"
            | "should_not_include_extension_in_release_group"
            | "should_not_include_language_in_release_group"
            | "should_not_include_bad_suffix_in_release_group"
            | "should_parse_anime_release_groups"
            | "should_not_parse_anime_hash_as_release_group"
            | "should_not_parse_url_in_group" => {
                let expected = a.get(1).and_then(Value::as_str).map(str::to_string);
                let r = parse_release_group(&title);
                out.check(r == expected, || format!("{} -> {:?}", label(), r));
            }

            // --- scene names
            "should_return_true_for_scene_names" | "should_return_false_for_non_scene_names" => {
                let r = scene_title(&title);
                let wanted = method == "should_return_true_for_scene_names";
                out.check(r.is_some() == wanted, || format!("{} -> {:?}", label(), r));
            }
            "should_correctly_parse_scene_names" => {
                let r = scene_title(&title);
                out.check(r.as_deref() == Some(&s(&a[1])), || {
                    format!("{} -> {:?}", label(), r)
                });
            }

            // --- title cleaning
            "should_normalize_series_title"
            | "should_remove_special_characters_and_casing"
            | "should_not_remove_from_the_beginning_of_the_title" => {
                let r = clean_movie_title(&title);
                out.check(r == s(&a[1]), || format!("{} -> {r:?}", label()));
            }
            "should_remove_common_words_from_middle_of_title" => {
                for format in ["word.{0}.word", "word {0} word", "word-{0}-word"] {
                    let r = clean_movie_title(&format.replace("{0}", &title));
                    out.check(r == "wordword", || format!("{} as {format} -> {r:?}", label()));
                }
            }
            "should_not_remove_common_words_from_end_of_title" => {
                for format in ["word.word.{0}", "word-word-{0}", "word-word {0}"] {
                    let r = clean_movie_title(&format.replace("{0}", &title));
                    let expected = format!("wordword{}", title.to_lowercase());
                    out.check(r == expected, || format!("{} as {format} -> {r:?}", label()));
                }
            }
            "should_not_remove_common_words_in_the_middle_of_word" => {
                for format in [
                    "word.{0}word",
                    "word {0}word",
                    "word-{0}word",
                    "word{0}.word",
                    "word{0}-word",
                ] {
                    let r = clean_movie_title(&format.replace("{0}", &title));
                    let expected = format!("word{}word", title.to_lowercase());
                    out.check(r == expected, || format!("{} as {format} -> {r:?}", label()));
                }
            }
            "should_not_clean_word_from_beginning_of_string" => {
                for format in ["{0}.word.word", "{0}-word-word", "{0} word word"] {
                    let r = clean_movie_title(&format.replace("{0}", &title));
                    let expected = format!("{title}wordword");
                    out.check(r == expected, || format!("{} as {format} -> {r:?}", label()));
                }
            }

            // --- slugs
            "should_replace_accents" | "should_replace_double_occurence" => {
                let r = to_url_slug(&title, false, "-_", "-_");
                out.check(r == s(&a[1]), || format!("{} -> {r:?}", label()));
            }
            "should_replace_special_characters"
            | "should__not_replace_special_characters_with_dash_when_disabled"
            | "should_lowercase_capitals"
            | "should_trim_trailing_dashes_and_underscores"
            | "should_replace_spaces_with_dash"
            | "should_replace_special_characters_with_dash_when_enabled" => {
                let expected = match method {
                    "should_lowercase_capitals" => "this-is-a-mixed-case-sensitive-value",
                    "should_trim_trailing_dashes_and_underscores" => "test",
                    "should_replace_spaces_with_dash"
                    | "should_replace_special_characters_with_dash_when_enabled" => "test-result",
                    _ => "testresult",
                };
                let dash = method == "should_replace_special_characters_with_dash_when_enabled";
                let r = to_url_slug(&title, dash, "-_", "-_");
                out.check(r == expected, || format!("{} -> {r:?}", label()));
            }
            "should_trim_trailing_dashes_and_underscores_based_on_list" => {
                let r = to_url_slug(&title, false, &s(&a[1]), "");
                out.check(r == s(&a[2]), || format!("{} -> {r:?}", label()));
            }
            "should_replace_duplicate_characters_based_on_list" => {
                let r = to_url_slug(&title, false, "", &s(&a[1]));
                out.check(r == s(&a[2]), || format!("{} -> {r:?}", label()));
            }
            _ => return false,
        }
    }
    true
}

#[test]
fn radarr_parser_vectors() {
    let mut out = Outcome {
        failures: Vec::new(),
        checked: 0,
    };
    let mut unknown = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(vectors_dir())
        .expect("tests/vectors/radarr/parser")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no vectors found");
    for path in files {
        let fixture = path.file_stem().unwrap().to_string_lossy().to_string();
        if SKIPPED_FIXTURES.contains(&fixture.as_str()) {
            continue;
        }
        let data: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let methods: BTreeMap<String, Value> = serde_json::from_value(data["methods"].clone()).unwrap();
        for (method, body) in methods {
            if SKIPPED.contains(&method.as_str()) {
                continue;
            }
            let cases = body["cases"].as_array().cloned().unwrap_or_default();
            if !run_method(&fixture, &method, &cases, &mut out) {
                unknown.push(format!("{fixture}::{method}"));
            }
        }
    }
    assert!(unknown.is_empty(), "test methods without a replay: {unknown:#?}");
    assert!(
        out.failures.is_empty(),
        "{} of {} vectors fail:\n{}",
        out.failures.len(),
        out.checked,
        out.failures.join("\n")
    );
    println!("{} vectors pass", out.checked);
}
