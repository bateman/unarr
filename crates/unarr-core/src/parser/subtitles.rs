//! What a subtitle file's name says: its language, its tags (`forced`,
//! `sdh`, ...), a title and a copy number (`LanguageParser`'s subtitle
//! half).
//!
//! The originals read these with regular expressions whose groups repeat
//! and are then asked for every capture; the expressions here find where
//! the parts are, and the parts are split by hand.

use std::sync::OnceLock;

use fancy_regex::Regex;

use crate::quality::LANGUAGES;

const TAGS: &[&str] = &["forced", "foreign", "default", "cc", "psdh", "sdh"];
const TAG_PATTERN: &str = "forced|foreign|default|cc|psdh|sdh";

/// `SubtitleTitleInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubtitleTitleInfo {
    pub language_tags: Vec<String>,
    /// The originals' language id; 0 is unknown.
    pub language: i64,
    pub raw_title: Option<String>,
    pub title: Option<String>,
    pub copy: i64,
    pub title_first: bool,
}

fn is_tag(token: &str) -> bool {
    TAGS.iter().any(|t| t.eq_ignore_ascii_case(token))
}

/// `Path.GetFileNameWithoutExtension`.
fn simple_name(file_name: &str) -> &str {
    let name = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    match name.rfind('.') {
        Some(i) => &name[..i],
        None => name,
    }
}

/// `SubtitleLanguageRegex`: `name[.tag]*.iso[.tag]*`.
fn language_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"(?i)^(?<head>.+?)(?<before>(?:[-_. ](?:{TAG_PATTERN}))*)[-_. ](?<iso>[a-z]{{2,3}})(?<after>(?:[-_. ](?:{TAG_PATTERN}))*)$"
        ))
        .expect("a valid expression")
    })
}

/// `SubtitleLanguageTitleRegex`: `name[.tag|.iso]*<sep>title[.tag|.iso]*`.
fn title_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"(?i)^(?<head>.+?)(?<before>(?:\.(?:(?:{TAG_PATTERN})|[a-z]{{2,3}}))*)[-_. ](?<title>[^.]*)(?<after>(?:\.(?:(?:{TAG_PATTERN})|[a-z]{{2,3}}))*)$"
        ))
        .expect("a valid expression")
    })
}

/// `SubtitleTitleRegex`: `[title - ]copy`.
fn copy_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:(?<title>.+) - )?(?<copy>(?<!\d)\d{1,3}(?!\d))$").expect("a valid expression")
    })
}

fn tokens(part: &str) -> impl Iterator<Item = &str> {
    part.split(['-', '_', '.', ' ']).filter(|t| !t.is_empty())
}

/// How an application names languages. Sonarr and Radarr number their
/// languages differently and do not know the same ones, so each reads a
/// subtitle's name with its own tables.
#[derive(Clone, Copy)]
pub struct LanguageLookup {
    /// `IsoLanguages.Find(code)?.Language`, the code in lower case.
    pub by_iso_code: fn(&str) -> Option<i64>,
    /// The first of `Language.All` whose name ends the text, which is in
    /// lower case.
    pub by_name_at_end: fn(&str) -> Option<i64>,
}

/// Sonarr's tables: what the functions without a lookup read names with.
pub const SERIES_LANGUAGES: LanguageLookup = LanguageLookup {
    by_iso_code: crate::mediafiles::language_from_iso,
    by_name_at_end: series_language_named_at_end,
};

fn series_language_named_at_end(lower: &str) -> Option<i64> {
    LANGUAGES
        .iter()
        .find(|l| lower.ends_with(&l.name.to_lowercase()))
        .map(|l| l.id)
        .filter(|id| *id > 0)
}

/// `IsoLanguages.Find(code)?.Language`.
fn language_of(languages: &LanguageLookup, code: &str) -> i64 {
    (languages.by_iso_code)(&code.to_ascii_lowercase()).unwrap_or(0)
}

/// `ParseSubtitleLanguage`: the language code at the end of the name, or
/// a language's name there.
pub fn parse_subtitle_language(file_name: &str) -> i64 {
    parse_subtitle_language_in(&SERIES_LANGUAGES, file_name)
}

/// [`parse_subtitle_language`] with the tables of the application asking.
pub fn parse_subtitle_language_in(languages: &LanguageLookup, file_name: &str) -> i64 {
    let simple = simple_name(file_name);
    if let Ok(Some(found)) = language_regex().captures(simple) {
        return found
            .name("iso")
            .map(|m| language_of(languages, m.as_str()))
            .unwrap_or(0);
    }
    (languages.by_name_at_end)(&simple.to_lowercase()).unwrap_or(0)
}

/// `ParseLanguageTags`: the tags around the language code, lower-cased.
pub fn parse_language_tags(file_name: &str) -> Vec<String> {
    let simple = simple_name(file_name);
    let Ok(Some(found)) = language_regex().captures(simple) else {
        return Vec::new();
    };
    ["before", "after"]
        .iter()
        .filter_map(|group| found.name(group))
        .flat_map(|m| tokens(m.as_str()))
        .map(str::to_lowercase)
        .collect()
}

/// `ParseBasicSubtitle`: language and tags, no title.
pub fn parse_basic_subtitle(file_name: &str) -> SubtitleTitleInfo {
    parse_basic_subtitle_in(&SERIES_LANGUAGES, file_name)
}

/// [`parse_basic_subtitle`] with the tables of the application asking.
pub fn parse_basic_subtitle_in(languages: &LanguageLookup, file_name: &str) -> SubtitleTitleInfo {
    SubtitleTitleInfo {
        language_tags: parse_language_tags(file_name),
        language: parse_subtitle_language_in(languages, file_name),
        ..Default::default()
    }
}

/// `ParseSubtitleLanguageInformation`: with a title when the name has one
/// and exactly one language code.
pub fn parse_subtitle_information(file_name: &str) -> SubtitleTitleInfo {
    parse_subtitle_information_in(&SERIES_LANGUAGES, file_name)
}

/// [`parse_subtitle_information`] with the tables of the application
/// asking.
pub fn parse_subtitle_information_in(languages: &LanguageLookup, file_name: &str) -> SubtitleTitleInfo {
    let simple = simple_name(file_name);
    let Ok(Some(found)) = title_regex().captures(simple) else {
        return parse_basic_subtitle_in(languages, file_name);
    };
    let part = |name: &str| found.name(name).map(|m| m.as_str()).unwrap_or("");
    let before: Vec<&str> = part("before").split('.').filter(|t| !t.is_empty()).collect();
    let after: Vec<&str> = part("after").split('.').filter(|t| !t.is_empty()).collect();
    let codes: Vec<&str> = before
        .iter()
        .chain(after.iter())
        .copied()
        .filter(|t| !is_tag(t))
        .collect();
    if codes.len() != 1 {
        return parse_basic_subtitle_in(languages, file_name);
    }
    let tags_before: Vec<&str> = before.iter().copied().filter(|t| is_tag(t)).collect();
    let mut language_tags: Vec<String> = Vec::new();
    // `Union`: each tag once, in the order met.
    for tag in tags_before.iter().chain(after.iter().filter(|t| is_tag(t))) {
        let tag = tag.to_lowercase();
        if !language_tags.contains(&tag) {
            language_tags.push(tag);
        }
    }
    let mut info = SubtitleTitleInfo {
        // The last capture of the code, as the originals' `Group.Value`.
        language: language_of(languages, codes[0]),
        language_tags,
        raw_title: Some(part("title").to_string()),
        title: None,
        copy: 0,
        title_first: tags_before.is_empty(),
    };
    update_title_and_copy(&mut info);
    info
}

/// `UpdateTitleAndCopyFromTitle`: a raw title of `Title - 2` or `2` is a
/// title and a copy number.
pub fn update_title_and_copy(info: &mut SubtitleTitleInfo) {
    let Some(raw) = info.raw_title.clone() else {
        info.title = None;
        info.copy = 0;
        return;
    };
    match copy_regex().captures(&raw) {
        Ok(Some(found)) => {
            info.title = found.name("title").map(|m| m.as_str().to_string());
            info.copy = found
                .name("copy")
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
        }
        _ => {
            info.title = Some(raw);
            info.copy = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::language::lang;

    // The cases are those of the originals' `LanguageParserFixture`.

    #[test]
    fn the_language_is_read_from_the_end_of_the_name() {
        assert_eq!(parse_subtitle_language("Series Title - S01E01 - Pilot.sub"), 0);
        for name in [
            "Series Title - S01E01 - Pilot.en.sub",
            "Series Title - S01E01 - Pilot.EN.sub",
            "Series Title - S01E01 - Pilot.eng.sub",
            "Series Title - S01E01 - Pilot.ENG.sub",
            "Series Title - S01E01 - Pilot.English.sub",
            "Series Title - S01E01 - Pilot.english.sub",
            "Series Title - S01E01 - Pilot.en.cc.sub",
            "Series Title - S01E01 - Pilot.en.sdh.sub",
            "Series Title - S01E01 - Pilot.en.forced.sub",
            "Series Title - S01E01 - Pilot.en.sdh.forced.sub",
        ] {
            assert_eq!(parse_subtitle_language(name), lang::ENGLISH, "{name}");
        }
        assert_eq!(
            parse_language_tags("Series Title - S01E01 - Pilot.en.sdh.forced.sub"),
            ["sdh", "forced"]
        );
        assert_eq!(
            parse_language_tags("Series Title - S01E01 - Pilot.default.EN.CC.sub"),
            ["default", "cc"]
        );
        assert!(parse_language_tags("Series Title - S01E01 - Pilot.sub").is_empty());
    }

    #[test]
    fn titles_and_tags() {
        let cases: &[(&str, &[&str], &str, i64)] = &[
            (
                "Name (2020) - S01E20 - [AAC 2.0].testtitle.default.eng.forced.ass",
                &["default", "forced"],
                "testtitle",
                lang::ENGLISH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].eng.default.testtitle.forced.ass",
                &["default", "forced"],
                "testtitle",
                lang::ENGLISH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].default.eng.testtitle.forced.ass",
                &["default", "forced"],
                "testtitle",
                lang::ENGLISH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].testtitle.forced.eng.ass",
                &["forced"],
                "testtitle",
                lang::ENGLISH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].eng.forced.testtitle.ass",
                &["forced"],
                "testtitle",
                lang::ENGLISH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].forced.eng.testtitle.ass",
                &["forced"],
                "testtitle",
                lang::ENGLISH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].testtitle.default.fra.forced.ass",
                &["default", "forced"],
                "testtitle",
                lang::FRENCH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].fra.default.testtitle.forced.ass",
                &["default", "forced"],
                "testtitle",
                lang::FRENCH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].default.fra.testtitle.forced.ass",
                &["default", "forced"],
                "testtitle",
                lang::FRENCH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].testtitle.forced.fra.ass",
                &["forced"],
                "testtitle",
                lang::FRENCH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].fra.forced.testtitle.ass",
                &["forced"],
                "testtitle",
                lang::FRENCH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].forced.fra.testtitle.ass",
                &["forced"],
                "testtitle",
                lang::FRENCH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].ru-something-else.srt",
                &[],
                "something-else",
                lang::RUSSIAN,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].Full Subtitles.eng.ass",
                &[],
                "Full Subtitles",
                lang::ENGLISH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].mytitle - 1.en.ass",
                &[],
                "mytitle",
                lang::ENGLISH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].mytitle 1.en.ass",
                &[],
                "mytitle 1",
                lang::ENGLISH,
            ),
            (
                "Name (2020) - S01E20 - [AAC 2.0].mytitle.en.ass",
                &[],
                "mytitle",
                lang::ENGLISH,
            ),
        ];
        for (name, tags, title, language) in cases {
            let info = parse_subtitle_information(name);
            let mut got = info.language_tags.clone();
            got.sort();
            let mut want: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
            want.sort();
            assert_eq!(got, want, "{name}: tags");
            assert_eq!(info.title.as_deref(), Some(*title), "{name}: title");
            assert_eq!(info.language, *language, "{name}: language");
        }
        assert_eq!(
            parse_subtitle_information("Name (2020) - S01E20 - [AAC 2.0].mytitle - 1.en.ass").copy,
            1
        );
    }

    #[test]
    fn a_name_without_a_language_has_no_title() {
        for name in [
            "Name (2020) - S01E20 - [AAC 2.0].default.forced.ass",
            "Name (2020) - S01E20 - [AAC 2.0].default.ass",
            "Name (2020) - S01E20 - [AAC 2.0].ass",
            "Name (2020) - S01E20 - [AAC 2.0].testtitle.ass",
        ] {
            let info = parse_subtitle_information(name);
            assert_eq!(info.language, 0, "{name}");
            assert!(info.language_tags.is_empty(), "{name}");
            assert_eq!(info.raw_title, None, "{name}");
        }
    }
}
