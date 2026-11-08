//! Replay the Sonarr parser test vectors (`tests/vectors/parser/*.json`,
//! extracted from the Sonarr repository by `scripts/port-vectors.py`)
//! against `unarr_core::parser`. Every test method of the original fixtures
//! is mapped to the assertion it makes; methods about features Unarr does
//! not port (subtitle files, ISO language codes) are listed in `SKIPPED`
//! so a new fixture never passes by being ignored.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::NaiveDate;
use serde_json::Value;
use unarr_core::parser::language::{lang, parse_languages};
use unarr_core::parser::quality::parse_quality;
use unarr_core::parser::title::is_scene_title;
use unarr_core::parser::{
    clean_series_title, normalize_episode_title, parse_path, parse_release_group, parse_series_name,
    parse_title_at, Detection,
};

/// Test methods of the originals that exercise code outside this port.
const SKIPPED: &[&str] = &[
    // Subtitle file names: phase 3 (import).
    "should_parse_subtitle_language_unknown",
    "should_parse_subtitle_language_english",
    "should_parse_title_and_tags",
    "should_not_parse_false_title",
    // IsoLanguages: phase 3.
    "should_return_iso_language_for_English",
    "unknown_or_invalid_code_should_return_null",
    "should_return_portuguese",
    "should_not_return_portuguese",
    "should_lookup_cze_via_alternate_iso_code_mapping",
    // Random and md5 inputs are generated in the originals, not data.
    "should_not_parse_random",
];

/// Whole fixtures about other components: `ParsingService` maps parsed
/// names to series and episodes, which step 5 of phase 2 ports with its
/// own vectors.
const SKIPPED_FIXTURES: &[&str] = &["GetEpisodesFixture"];

/// The pinned clock for daily releases: the day the vectors were extracted.
fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 7).unwrap()
}

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/parser")
}

/// `Quality.X` and `Language.X` names to ids.
fn quality_id(member: &str) -> i64 {
    match member {
        "Unknown" => 0,
        "SDTV" => 1,
        "DVD" => 2,
        "WEBDL1080p" => 3,
        "HDTV720p" => 4,
        "WEBDL720p" => 5,
        "Bluray720p" => 6,
        "Bluray1080p" => 7,
        "WEBDL480p" => 8,
        "HDTV1080p" => 9,
        "RAWHD" => 10,
        "WEBRip480p" => 12,
        "Bluray480p" => 13,
        "WEBRip720p" => 14,
        "WEBRip1080p" => 15,
        "HDTV2160p" => 16,
        "WEBRip2160p" => 17,
        "WEBDL2160p" => 18,
        "Bluray2160p" => 19,
        "Bluray1080pRemux" => 20,
        "Bluray2160pRemux" => 21,
        "Bluray576p" => 22,
        other => panic!("unknown quality {other}"),
    }
}

fn language_id(name: &str) -> i64 {
    match name {
        "Original" => lang::ORIGINAL,
        "Unknown" => lang::UNKNOWN,
        "English" => lang::ENGLISH,
        "French" => lang::FRENCH,
        "Spanish" => lang::SPANISH,
        "German" => lang::GERMAN,
        "Italian" => lang::ITALIAN,
        "Danish" => lang::DANISH,
        "Dutch" => lang::DUTCH,
        "Japanese" => lang::JAPANESE,
        "Icelandic" => lang::ICELANDIC,
        "Chinese" => lang::CHINESE,
        "Russian" => lang::RUSSIAN,
        "Polish" => lang::POLISH,
        "Vietnamese" => lang::VIETNAMESE,
        "Swedish" => lang::SWEDISH,
        "Norwegian" => lang::NORWEGIAN,
        "Finnish" => lang::FINNISH,
        "Turkish" => lang::TURKISH,
        "Portuguese" => lang::PORTUGUESE,
        "Flemish" => lang::FLEMISH,
        "Greek" => lang::GREEK,
        "Korean" => lang::KOREAN,
        "Hungarian" => lang::HUNGARIAN,
        "Hebrew" => lang::HEBREW,
        "Lithuanian" => lang::LITHUANIAN,
        "Czech" => lang::CZECH,
        "Arabic" => lang::ARABIC,
        "Hindi" => lang::HINDI,
        "Bulgarian" => lang::BULGARIAN,
        "Malayalam" => lang::MALAYALAM,
        "Ukrainian" => lang::UKRAINIAN,
        "Slovak" => lang::SLOVAK,
        "Thai" => lang::THAI,
        "PortugueseBrazil" | "Portuguese (Brazil)" => lang::PORTUGUESE_BRAZIL,
        "SpanishLatino" | "Spanish (Latino)" => lang::SPANISH_LATINO,
        "Romanian" => lang::ROMANIAN,
        "Latvian" => lang::LATVIAN,
        "Catalan" => lang::CATALAN,
        other => panic!("unknown language {other}"),
    }
}

fn s(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_default()
}

fn i(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).unwrap_or(0)
}

fn ints(v: &Value) -> Vec<i64> {
    v.as_array()
        .map(|a| a.iter().map(i).collect())
        .unwrap_or_default()
}

fn enum_member(v: &Value) -> String {
    s(&v["member"])
}

/// `AsOsAgnostic`: `C:\Test\a\b` becomes `/Test/a/b`, as on the Linux CI
/// the originals run on.
fn os_agnostic(path: &str) -> String {
    let p = path.replace('\\', "/");
    match p.strip_prefix("C:/") {
        Some(rest) => format!("/{rest}"),
        None => p,
    }
}

/// `string.Expand(new { year, month, day })`.
fn expand(template: &str, year: i64, month: &str, day: &str) -> String {
    template
        .replace("{year}", &year.to_string())
        .replace("{month}", month)
        .replace("{day}", day)
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
        match method {
            // --- episodes and seasons
            "should_parse_single_episode"
            | "should_parse_japanese_variety_show_format"
            | "should_parse_decimal_number_as_special"
            | "should_parse_split_episode" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.episode_numbers.len() == 1
                        && r.season_number == i(&a[2])
                        && r.episode_numbers[0] == i(&a[3])
                        && r.series_title == s(&a[1])
                        && r.absolute_episode_numbers.is_empty()
                        && !r.full_season
                        && (method != "should_parse_decimal_number_as_special" || r.special)
                        && (method != "should_parse_split_episode" || r.is_split_episode)
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_multiple_episodes" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.season_number == i(&a[2])
                        && r.episode_numbers == ints(&a[3])
                        && r.series_title == s(&a[1])
                        && r.absolute_episode_numbers.is_empty()
                        && !r.full_season
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_full_season_release"
            | "should_parse_season_extras"
            | "should_parse_season_subpack" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.season_number == i(&a[2])
                        && r.series_title == s(&a[1])
                        && r.episode_numbers.is_empty()
                        && r.absolute_episode_numbers.is_empty()
                        && r.full_season
                        && (method == "should_parse_full_season_release" || r.is_season_extra)
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_partial_season_release" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.season_number == i(&a[2])
                        && r.series_title == s(&a[1])
                        && r.episode_numbers.is_empty()
                        && r.absolute_episode_numbers.is_empty()
                        && !r.full_season
                        && r.is_partial_season
                        && r.season_part == i(&a[3])
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_multi_season_release" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.season_number == i(&a[2])
                        && r.series_title == s(&a[1])
                        && r.episode_numbers.is_empty()
                        && r.absolute_episode_numbers.is_empty()
                        && r.full_season
                        && !r.is_partial_season
                        && r.is_multi_season
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_mini_series_episode" | "should_parse_korean_series_episode" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.episode_numbers.len() == 1
                        && r.season_number == 1
                        && r.episode_numbers[0] == i(&a[2])
                        && r.series_title == s(&a[1])
                        && r.absolute_episode_numbers.is_empty()
                        && !r.full_season
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            // --- daily
            "should_parse_daily_episode" | "should_parse_daily_episode_with_multiple_parts" => {
                let r = parse_title_at(&s(&a[0]), today());
                let date = format!("{:04}-{:02}-{:02}", i(&a[2]), i(&a[3]), i(&a[4]));
                let ok = r.as_ref().is_some_and(|r| {
                    r.series_title == s(&a[1])
                        && r.air_date.as_deref() == Some(date.as_str())
                        && r.episode_numbers.is_empty()
                        && r.absolute_episode_numbers.is_empty()
                        && !r.full_season
                        && (method == "should_parse_daily_episode" || r.daily_part == Some(i(&a[5])))
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_not_accept_ancient_daily_series" => {
                let title = expand(&s(&a[0]), 1950, "10", "14");
                let r = parse_title_at(&title, today());
                out.check(r.is_none(), || {
                    format!("{} -> {:?}", label(), r.as_ref().map(brief))
                });
            }
            "should_not_accept_future_dates" => {
                let two_days = today() + chrono::Duration::days(2);
                let title = expand(
                    &s(&a[0]),
                    i64::from(two_days.format("%Y").to_string().parse::<i32>().unwrap()),
                    &two_days.format("%m").to_string(),
                    &two_days.format("%d").to_string(),
                );
                let r = parse_title_at(&title, today());
                out.check(r.is_none(), || {
                    format!("{} -> {:?}", label(), r.as_ref().map(brief))
                });
            }
            "should_not_parse_ambiguous_daily_episode"
            | "should_not_parse_crap"
            | "should_not_parse_file_name_without_proper_spacing"
            | "should_not_parse_invalid_release_name"
            | "should_not_parse_special_with_part_number" => {
                let r = parse_title_at(&s(&a[0]), today());
                out.check(r.is_none(), || {
                    format!("{} -> {:?}", label(), r.as_ref().map(brief))
                });
            }
            "should_parse_unknown_formats_without_error" | "should_clean_up_invalid_path_characters" => {
                let r = parse_title_at(&s(&a[0]), today());
                out.check(
                    method != "should_parse_unknown_formats_without_error" || r.is_some(),
                    || format!("{} -> None", label()),
                );
            }
            // --- absolute numbering
            "should_parse_absolute_numbers" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.absolute_episode_numbers == vec![i(&a[2])]
                        && r.season_number == i(&a[3])
                        && r.episode_numbers.first().copied().unwrap_or(0) == i(&a[4])
                        && r.episode_numbers.len() <= 1
                        && r.series_title == s(&a[1])
                        && !r.full_season
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_absolute_specials" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.absolute_episode_numbers == vec![i(&a[2])]
                        && r.season_number == 0
                        && r.episode_numbers.first().copied().unwrap_or(0) == 0
                        && r.episode_numbers.len() <= 1
                        && r.series_title == s(&a[1])
                        && !r.full_season
                        && r.special
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_multi_episode_absolute_numbers" => {
                let r = parse_title_at(&s(&a[0]), today());
                let expected: Vec<i64> = (i(&a[2])..=i(&a[3])).collect();
                let ok = r.as_ref().is_some_and(|r| {
                    r.absolute_episode_numbers == expected && r.series_title == s(&a[1]) && !r.full_season
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_anime_season_packs" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.absolute_episode_numbers.is_empty()
                        && r.series_title == s(&a[1])
                        && r.full_season
                        && r.season_number == i(&a[2])
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_handle_anime_recap_numbering" => {
                let r = parse_title_at(&s(&a[0]), today());
                let expected = a[2].as_f64().unwrap_or(0.0);
                let ok = r.as_ref().is_some_and(|r| {
                    r.series_title == s(&a[1])
                        && r.absolute_episode_numbers.is_empty()
                        && r.special_absolute_episode_numbers == vec![expected]
                        && !r.full_season
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_handle_ambiguously_named_anime_files_in_batch_release" => {
                let path = format!("/Test/{}/{}", s(&a[0]), s(&a[1]));
                let r = parse_path(&path);
                let ok = r.as_ref().is_some_and(|r| {
                    r.absolute_episode_numbers == vec![i(&a[3])]
                        && r.season_number == 0
                        && r.episode_numbers.is_empty()
                        && r.series_title == s(&a[2])
                        && !r.full_season
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_absolute_followed_by_standard_as_standard" => {
                let r = parse_title_at(&s(&a[0]), today());
                let special = a[2].as_f64().unwrap_or(0.0);
                let ok = r.as_ref().is_some_and(|r| {
                    r.episode_numbers.len() == 1
                        && r.season_number == i(&a[3])
                        && r.episode_numbers[0] == i(&a[4])
                        && r.series_title == s(&a[1])
                        && r.special_absolute_episode_numbers == vec![special]
                        && r.absolute_episode_numbers.is_empty()
                        && !r.full_season
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            // --- anime metadata and unicode
            "should_parse_releasegroup_and_hash" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.release_group.as_deref() == Some(s(&a[1]).as_str()) && r.release_hash == s(&a[2])
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_release_hashes_with_10b_or_8b" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| r.release_hash == s(&a[1]));
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_chinese_anime_releases"
            | "should_parse_chinese_anime_season_episode_releases"
            | "should_parse_unbracketed_chinese_anime_releases"
            | "should_parse_false_positive_chinese_anime_releases" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.release_group.as_deref() == Some(s(&a[2]).as_str())
                        && r.absolute_episode_numbers == vec![i(&a[3])]
                        && r.series_title == s(&a[1])
                        && !r.full_season
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_chinese_multiepisode_releases" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.release_group.as_deref() == Some(s(&a[2]).as_str())
                        && r.absolute_episode_numbers == ints(&a[3])
                        && r.series_title == s(&a[1])
                        && !r.full_season
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_gm_team_releases_and_files" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.absolute_episode_numbers == vec![i(&a[2])]
                        && r.series_title == s(&a[1])
                        && !r.full_season
                        && r.release_group.as_deref() == Some("GM-Team")
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_unicode_digits" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.series_title == s(&a[1])
                        && r.absolute_episode_numbers == vec![i(&a[2])]
                        && r.special_absolute_episode_numbers.is_empty()
                        && !r.full_season
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            // --- titles
            "should_parse_series_name" | "should_not_parse_url_in_name" => {
                let got = clean_series_title(&parse_series_name(&s(&a[0])));
                let want = clean_series_title(&s(&a[1]));
                out.check(got == want, || format!("{} -> {got:?}", label()));
            }
            "should_parse_series_title_info" => {
                let r = parse_title_at(&s(&a[0]), today());
                let year = a.get(2).map(i).unwrap_or(0);
                let ok = r.as_ref().is_some_and(|r| {
                    r.series_title_info.title_without_year == s(&a[1]) && r.series_title_info.year == year
                });
                out.check(ok, || {
                    format!(
                        "{} -> {:?}",
                        label(),
                        r.as_ref().map(|r| r.series_title_info.clone())
                    )
                });
            }
            "should_remove_request_info_from_title" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| r.series_title == s(&a[1]));
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_quality_from_extension" if fixture == "ParserFixture" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| {
                    r.quality.quality.quality_id != 0
                        && r.quality.source_detection == Detection::Extension
                        && r.quality.resolution_detection == Detection::Extension
                });
                out.check(ok, || {
                    format!("{} -> {:?}", label(), r.as_ref().map(|r| r.quality.clone()))
                });
            }
            "should_parse_releasetitle" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| r.release_title == s(&a[1]));
                out.check(ok, || {
                    format!("{} -> {:?}", label(), r.as_ref().map(|r| r.release_title.clone()))
                });
            }
            "should_parse_multiple_series_titles" => {
                let r = parse_title_at(&s(&a[0]), today());
                let want: Vec<String> = a[1..].iter().map(s).collect();
                let ok = r.as_ref().is_some_and(|r| {
                    let mut got = r.series_title_info.all_titles.clone();
                    let mut want = want.clone();
                    got.sort();
                    want.sort();
                    got == want
                });
                out.check(ok, || {
                    format!(
                        "{} -> {:?}",
                        label(),
                        r.as_ref().map(|r| r.series_title_info.all_titles.clone())
                    )
                });
            }
            "should_parse_language_after_parsing_title" => {
                let r = parse_title_at(&s(&a[0]), today());
                let want = language_id(&s(&a[1]));
                let ok = r.as_ref().is_some_and(|r| r.languages == vec![want]);
                out.check(ok, || {
                    format!("{} -> {:?}", label(), r.as_ref().map(|r| r.languages.clone()))
                });
            }
            "should_normalize_series_title"
            | "should_remove_special_characters_and_casing"
            | "should_not_remove_from_the_beginning_of_the_title"
            | "should_replace_percent_sign_with_percent_following_numbers" => {
                let got = clean_series_title(&s(&a[0]));
                out.check(got == s(&a[1]), || format!("{} -> {got:?}", label()));
            }
            "should_remove_common_words_from_middle_of_title" => {
                for fmt in ["word.{0}.word", "word {0} word", "word-{0}-word"] {
                    let got = clean_series_title(&fmt.replace("{0}", &s(&a[0])));
                    out.check(got == "wordword", || format!("{} {fmt} -> {got:?}", label()));
                }
            }
            "should_not_remove_common_words_from_end_of_title" => {
                for fmt in ["word.word.{0}", "word-word-{0}", "word-word {0}"] {
                    let got = clean_series_title(&fmt.replace("{0}", &s(&a[0])));
                    let want = format!("wordword{}", s(&a[0]).to_lowercase());
                    out.check(got == want, || format!("{} {fmt} -> {got:?}", label()));
                }
            }
            "should_not_remove_common_words_in_the_middle_of_word" => {
                for fmt in [
                    "word.{0}word",
                    "word {0}word",
                    "word-{0}word",
                    "word{0}.word",
                    "word{0}-word",
                ] {
                    let got = clean_series_title(&fmt.replace("{0}", &s(&a[0])));
                    let want = format!("word{}word", s(&a[0]).to_lowercase());
                    out.check(got == want, || format!("{} {fmt} -> {got:?}", label()));
                }
            }
            "should_not_clean_word_from_beginning_of_string" => {
                for fmt in ["{0}.word.word", "{0}-word-word", "{0} word word"] {
                    let got = clean_series_title(&fmt.replace("{0}", &s(&a[0])));
                    let want = format!("{}wordword", s(&a[0]));
                    out.check(got == want, || format!("{} {fmt} -> {got:?}", label()));
                }
            }
            "should_normalize_episode_title" => {
                let got = normalize_episode_title(&s(&a[0]));
                out.check(got == s(&a[1]), || format!("{} -> {got:?}", label()));
            }
            // --- release groups
            "should_parse_release_group"
            | "should_parse_exception_release_group"
            | "should_not_include_language_in_release_group"
            | "should_not_include_repost_in_release_group"
            | "should_parse_anime_release_groups"
            | "should_not_parse_url_in_group" => {
                let got = parse_release_group(&s(&a[0]));
                let want = if a[1].is_null() { None } else { Some(s(&a[1])) };
                out.check(got == want, || format!("{} -> {got:?}", label()));
            }
            "should_not_parse_anime_hash_as_release_group" => {
                let got = parse_release_group(&s(&a[0]));
                out.check(got.is_none(), || format!("{} -> {got:?}", label()));
            }
            // --- specials, scene names
            "IsPossibleSpecialEpisode_should_be_true"
            | "IsPossibleSpecialEpisode_should_be_true_if_e00_special" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| r.is_possible_special_episode());
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "IsPossibleSpecialEpisode_should_be_false_for_Special_in_title" => {
                let r = parse_title_at(&s(&a[0]), today());
                let ok = r.as_ref().is_some_and(|r| !r.is_possible_special_episode());
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_return_true_for_scene_names" => {
                let got = is_scene_title(&s(&a[0]));
                out.check(got, || format!("{} -> false", label()));
            }
            "should_return_false_for_non_scene_names" => {
                let got = is_scene_title(&s(&a[0]));
                out.check(!got, || format!("{} -> true", label()));
            }
            // --- paths
            "should_parse_from_path" => {
                let r = parse_path(&os_agnostic(&s(&a[0])));
                let ok = r.as_ref().is_some_and(|r| {
                    r.episode_numbers.len() == 1
                        && r.season_number == i(&a[1])
                        && r.episode_numbers[0] == i(&a[2])
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_parse_multi_episode_from_path" => {
                let r = parse_path(&os_agnostic(&s(&a[0])));
                let ok = r.as_ref().is_some_and(|r| {
                    r.series_title == s(&a[1])
                        && r.season_number == i(&a[2])
                        && r.episode_numbers == ints(&a[3])
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            "should_properly_parse_hashed_releases" => {
                let r = parse_path(&os_agnostic(&s(&a[0])));
                let group = if a[3].is_null() { None } else { Some(s(&a[3])) };
                let ok = r.as_ref().is_some_and(|r| {
                    r.series_title == s(&a[1])
                        && r.quality.quality.quality_id == quality_id(&enum_member(&a[2]))
                        && r.release_group == group
                });
                out.check(ok, || format!("{} -> {:?}", label(), r.as_ref().map(brief)));
            }
            // --- quality
            m if m.starts_with("should_parse_")
                && m.ends_with("_quality")
                && a.len() == 2
                && a[1].is_boolean() =>
            {
                let want = match m {
                    "should_parse_sdtv_quality" => 1,
                    "should_parse_dvd_quality" => 2,
                    "should_parse_webdl480p_quality" => 8,
                    "should_parse_bluray480p_quality" => 13,
                    "should_parse_webrip480p_quality" => 12,
                    "should_parse_bluray576p_quality" => 22,
                    "should_parse_hdtv720p_quality" => 4,
                    "should_parse_hdtv1080p_quality" => 9,
                    "should_parse_hdtv2160p_quality" => 16,
                    "should_parse_webdl720p_quality" => 5,
                    "should_parse_webrip720p_quality" => 14,
                    "should_parse_webdl1080p_quality" => 3,
                    "should_parse_webrip1080p_quality" => 15,
                    "should_parse_webdl2160p_quality" => 18,
                    "should_parse_webrip2160p_quality" => 17,
                    "should_parse_bluray720p_quality" => 6,
                    "should_parse_bluray1080p_quality" => 7,
                    "should_parse_bluray1080p_remux_quality" => 20,
                    "should_parse_bluray2160p_quality" => 19,
                    "should_parse_bluray2160p_remux_quality" => 21,
                    "should_parse_raw_quality" => 10,
                    other => panic!("unmapped quality method {other}"),
                };
                let q = parse_quality(&s(&a[0]));
                let version = if a[1].as_bool().unwrap_or(false) { 2 } else { 1 };
                out.check(
                    q.quality.quality_id == want && q.quality.version == version,
                    || format!("{} -> {:?}", label(), q.quality),
                );
            }
            "quality_parse" => {
                let q = parse_quality(&s(&a[0]));
                let version = if a[1].as_bool().unwrap_or(false) { 2 } else { 1 };
                out.check(q.quality.quality_id == 0 && q.quality.version == version, || {
                    format!("{} -> {:?}", label(), q.quality)
                });
            }
            "parsing_our_own_quality_enum_name" => {
                let id = quality_id(&enum_member(&a[0]));
                let name = unarr_core::quality::Quality::by_id(id).name;
                let q = parse_quality(&format!("My series S01E01 [{name}]"));
                out.check(q.quality.quality_id == id, || {
                    format!("{} -> {:?}", label(), q.quality)
                });
            }
            "should_parse_quality_from_other_source" => {
                let id = quality_id(&enum_member(&a[1]));
                for c in ['-', '.', ' ', '_'] {
                    let title = format!("My series S01E01 {}", s(&a[0]).replace(' ', &c.to_string()));
                    let q = parse_quality(&title);
                    out.check(q.quality.quality_id == id && q.quality.version == 1, || {
                        format!("{} {title:?} -> {:?}", label(), q.quality)
                    });
                }
            }
            "should_parse_full_quality_from_name"
            | "should_parse_resolution_from_name"
            | "should_parse_source_from_name"
            | "should_parse_quality_from_extension"
            | "should_parse_resolution_from_name_and_source_from_extension" => {
                let q = parse_quality(&s(&a[0]));
                let (source, resolution) = match method {
                    "should_parse_full_quality_from_name" => (Detection::Name, Detection::Name),
                    "should_parse_resolution_from_name" => (Detection::Unknown, Detection::Name),
                    "should_parse_source_from_name" => (Detection::Name, Detection::Unknown),
                    "should_parse_quality_from_extension" => (Detection::Extension, Detection::Extension),
                    _ => (Detection::Extension, Detection::Name),
                };
                out.check(
                    q.source_detection == source && q.resolution_detection == resolution,
                    || format!("{} -> {q:?}", label()),
                );
            }
            "should_be_able_to_parse_repack" if a.len() == 3 => {
                let q = parse_quality(&s(&a[0]));
                let ok =
                    q.quality.version == i(&a[2]) && q.quality.is_repack == a[1].as_bool().unwrap_or(false);
                out.check(ok, || format!("{} -> {:?}", label(), q.quality));
            }
            "should_be_able_to_parse_repack"
            | "should_be_able_to_parse_anime_version"
            | "should_parse_version_from_title" => {
                let q = parse_quality(&s(&a[0]));
                out.check(q.quality.version == i(&a[1]), || {
                    format!("{} -> {:?}", label(), q.quality)
                });
            }
            "should_parse_reality_from_title" => {
                let q = parse_quality(&s(&a[0]));
                out.check(q.quality.real == i(&a[1]), || {
                    format!("{} -> {:?}", label(), q.quality)
                });
            }
            // --- languages
            m if m.starts_with("should_parse_language_") => {
                let name = m.trim_start_matches("should_parse_language_");
                let want = match name {
                    "portuguese_brazil" => lang::PORTUGUESE_BRAZIL,
                    "spanish_latino" => lang::SPANISH_LATINO,
                    other => {
                        let mut c = other.chars();
                        let cap: String = c.next().unwrap().to_uppercase().chain(c).collect();
                        language_id(&cap)
                    }
                };
                let got = parse_languages(&s(&a[0]));
                out.check(got.contains(&want), || format!("{} -> {got:?}", label()));
            }
            "should_parse_hindi_and_english"
            | "should_parse_spanish_and_catalan"
            | "should_parse_english_spanish_and_catalan" => {
                let mut want: Vec<i64> = match method {
                    "should_parse_hindi_and_english" => vec![lang::HINDI, lang::ENGLISH],
                    "should_parse_spanish_and_catalan" => vec![lang::SPANISH, lang::CATALAN],
                    _ => vec![lang::ENGLISH, lang::SPANISH, lang::CATALAN],
                };
                let mut got = parse_languages(&s(&a[0]));
                want.sort();
                got.sort();
                out.check(got == want, || format!("{} -> {got:?}", label()));
            }
            "should_add_original_language_to_german_release_with_dl_tag"
            | "should_not_add_original_language_to_german_release_when_title_contains_web_dl"
            | "should_add_original_language_and_english_to_german_release_with_ml_tag"
            | "should_parse_original_title_from_release_name"
            | "should_parse_english_and_ukranian"
            | "should_parse_turkish_and_ukranian" => {
                let r = parse_title_at(&s(&a[0]), today());
                let mut want: Vec<i64> = match method {
                    "should_add_original_language_to_german_release_with_dl_tag" => {
                        vec![lang::GERMAN, lang::ORIGINAL]
                    }
                    "should_not_add_original_language_to_german_release_when_title_contains_web_dl" => {
                        vec![lang::GERMAN]
                    }
                    "should_add_original_language_and_english_to_german_release_with_ml_tag" => {
                        vec![lang::GERMAN, lang::ORIGINAL, lang::ENGLISH]
                    }
                    "should_parse_original_title_from_release_name" => vec![lang::ORIGINAL],
                    "should_parse_english_and_ukranian" => vec![lang::UKRAINIAN, lang::ENGLISH],
                    _ => vec![lang::UKRAINIAN, lang::TURKISH],
                };
                want.sort();
                let ok = r.as_ref().is_some_and(|r| {
                    let mut got = r.languages.clone();
                    got.sort();
                    got == want
                });
                out.check(ok, || {
                    format!("{} -> {:?}", label(), r.as_ref().map(|r| r.languages.clone()))
                });
            }
            _ => return false,
        }
    }
    true
}

fn brief(r: &unarr_core::parser::ParsedEpisodeInfo) -> String {
    format!(
        "title={:?} S{} E{:?} abs={:?} special_abs={:?} air={:?} full={} partial={} multi={} extra={} split={} mini={} special={} group={:?} hash={:?} q={}",
        r.series_title,
        r.season_number,
        r.episode_numbers,
        r.absolute_episode_numbers,
        r.special_absolute_episode_numbers,
        r.air_date,
        r.full_season,
        r.is_partial_season,
        r.is_multi_season,
        r.is_season_extra,
        r.is_split_episode,
        r.is_mini_series,
        r.special,
        r.release_group,
        r.release_hash,
        r.quality.quality.quality_id,
    )
}

#[test]
fn sonarr_parser_vectors_pass() {
    let mut out = Outcome {
        failures: Vec::new(),
        checked: 0,
    };
    let mut unknown: Vec<String> = Vec::new();
    let mut skipped = 0usize;
    let mut files: Vec<PathBuf> = std::fs::read_dir(vectors_dir())
        .expect("tests/vectors/parser")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no vectors: run scripts/port-vectors.py");
    for file in files {
        let fixture = file.file_stem().unwrap().to_string_lossy().to_string();
        if SKIPPED_FIXTURES.contains(&fixture.as_str()) {
            continue;
        }
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let methods: BTreeMap<String, Value> = serde_json::from_value(doc["methods"].clone()).unwrap();
        for (method, body) in methods {
            let cases = body["cases"].as_array().cloned().unwrap_or_default();
            if SKIPPED.contains(&method.as_str()) {
                skipped += cases.len();
                continue;
            }
            if !run_method(&fixture, &method, &cases, &mut out) {
                unknown.push(format!("{fixture}::{method}"));
            }
        }
    }
    eprintln!(
        "parser vectors: {} checks, {} failures, {} cases skipped on purpose",
        out.checked,
        out.failures.len(),
        skipped
    );
    assert!(unknown.is_empty(), "test methods without a mapping: {unknown:?}");
    assert!(out.checked > 1000, "only {} checks ran", out.checked);
    assert!(
        out.failures.is_empty(),
        "{} of {} vectors fail:\n{}",
        out.failures.len(),
        out.checked,
        out.failures.join("\n")
    );
}
