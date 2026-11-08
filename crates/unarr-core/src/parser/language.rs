//! Port of the originals' `LanguageParser.ParseLanguages`: the languages a
//! release name advertises, as language ids.

use crate::net_regex;
use crate::parser::netregex::Lazy;

/// Language ids, as `Language.X` names them.
pub mod lang {
    pub const ORIGINAL: i64 = -2;
    pub const UNKNOWN: i64 = 0;
    pub const ENGLISH: i64 = 1;
    pub const FRENCH: i64 = 2;
    pub const SPANISH: i64 = 3;
    pub const GERMAN: i64 = 4;
    pub const ITALIAN: i64 = 5;
    pub const DANISH: i64 = 6;
    pub const DUTCH: i64 = 7;
    pub const JAPANESE: i64 = 8;
    pub const ICELANDIC: i64 = 9;
    pub const CHINESE: i64 = 10;
    pub const RUSSIAN: i64 = 11;
    pub const POLISH: i64 = 12;
    pub const VIETNAMESE: i64 = 13;
    pub const SWEDISH: i64 = 14;
    pub const NORWEGIAN: i64 = 15;
    pub const FINNISH: i64 = 16;
    pub const TURKISH: i64 = 17;
    pub const PORTUGUESE: i64 = 18;
    pub const FLEMISH: i64 = 19;
    pub const GREEK: i64 = 20;
    pub const KOREAN: i64 = 21;
    pub const HUNGARIAN: i64 = 22;
    pub const HEBREW: i64 = 23;
    pub const LITHUANIAN: i64 = 24;
    pub const CZECH: i64 = 25;
    pub const ARABIC: i64 = 26;
    pub const HINDI: i64 = 27;
    pub const BULGARIAN: i64 = 28;
    pub const MALAYALAM: i64 = 29;
    pub const UKRAINIAN: i64 = 30;
    pub const SLOVAK: i64 = 31;
    pub const THAI: i64 = 32;
    pub const PORTUGUESE_BRAZIL: i64 = 33;
    pub const SPANISH_LATINO: i64 = 34;
    pub const ROMANIAN: i64 = 35;
    pub const LATVIAN: i64 = 36;
    pub const PERSIAN: i64 = 37;
    pub const CATALAN: i64 = 38;
    pub const CROATIAN: i64 = 39;
    pub const SERBIAN: i64 = 40;
    pub const BOSNIAN: i64 = 41;
    pub const ESTONIAN: i64 = 42;
    pub const TAMIL: i64 = 43;
    pub const INDONESIAN: i64 = 44;
    pub const MACEDONIAN: i64 = 45;
    pub const SLOVENIAN: i64 = 46;
}

use lang::*;

static CLEAN_SERIES_TITLE_RE: Lazy = net_regex!(r".*?[_. ](S\d{2}(?:E\d{2,4})*[_. ].*)");

static LANGUAGE_RE: Lazy = net_regex!(
    r"(?:\W|_)(?<english>\b(?:ing|eng)\b)|(?<italian>\b(?:ita|italian)\b)|(?<german>german\b|videomann|ger[. ]dub)|(?<flemish>flemish)|(?<greek>greek)|(?<french>(?:\W|_)(?:FR|VF|VF2|VFF|VFI|VFQ|TRUEFRENCH|FRENCH)(?:\W|_))|(?<russian>\b(?:rus|ru)\b)|(?<hungarian>\b(?:HUNDUB|HUN)\b)|(?<hebrew>\bHebDub\b)|(?<polish>\b(?:PL\W?DUB|DUB\W?PL|LEK\W?PL|PL\W?LEK)\b)|(?<chinese>\[(?:CH[ST]|BIG5|GB)\]|简|繁|字幕)|(?<bulgarian>\bbgaudio\b)|(?<spanish>\b(?:español|castellano|esp|spa(?!\(Latino\)))\b)|(?<ukrainian>\b(?:\dx?)?(?:ukr))|(?<thai>\b(?:THAI)\b)|(?<romanian>\b(?:RoDubbed|ROMANIAN)\b)|(?<catalan>[-,. ]cat[. ](?:DD|subs)|\b(?:catalan|catalán)\b)|(?<latvian>\b(?:lat|lav|lv)\b)|(?<turkish>\b(?:tur)\b)|(?<original>\b(?:orig|original)\b)"
);

static CASE_SENSITIVE_LANGUAGE_RE: Lazy = net_regex!(
    r"(?:(?i)(?<!SUB[\W|_|^]))(?:(?<lithuanian>\bLT\b)|(?<czech>\bCZ\b)|(?<polish>\bPL\b)|(?<bulgarian>\bBG\b)|(?<slovak>\bSK\b))(?:(?i)(?![\W|_|^]SUB))",
    case_sensitive
);

static GERMAN_DUAL_LANGUAGE_RE: Lazy = net_regex!(r"(?<!WEB[-_. ]?)\bDL\b");
static GERMAN_MULTI_LANGUAGE_RE: Lazy = net_regex!(r"\bML\b");

/// `LanguageParser.ParseLanguages`.
pub fn parse_languages(title: &str) -> Vec<i64> {
    let mut title = title.to_string();
    CLEAN_SERIES_TITLE_RE.try_replace(&mut title, "$1");
    let lower = title.to_lowercase();
    let mut languages: Vec<i64> = Vec::new();

    let contains = |needle: &str| lower.contains(needle);
    for (needles, language) in [
        (&["spanish"][..], SPANISH),
        (&["danish"][..], DANISH),
        (&["dutch"][..], DUTCH),
        (&["japanese"][..], JAPANESE),
        (&["icelandic"][..], ICELANDIC),
        (&["mandarin", "cantonese", "chinese"][..], CHINESE),
        (&["korean"][..], KOREAN),
        (&["russian"][..], RUSSIAN),
        (&["polish"][..], POLISH),
        (&["vietnamese"][..], VIETNAMESE),
        (&["swedish"][..], SWEDISH),
        (&["norwegian"][..], NORWEGIAN),
        (&["finnish"][..], FINNISH),
        (&["turkish"][..], TURKISH),
        (&["portuguese"][..], PORTUGUESE),
        (&["hungarian"][..], HUNGARIAN),
        (&["hebrew"][..], HEBREW),
        (&["arabic"][..], ARABIC),
        (&["hindi"][..], HINDI),
        (&["malayalam"][..], MALAYALAM),
        (&["ukrainian"][..], UKRAINIAN),
        (&["bulgarian"][..], BULGARIAN),
        (&["slovak"][..], SLOVAK),
        (&["brazilian", "dublado"][..], PORTUGUESE_BRAZIL),
        (&["latino"][..], SPANISH_LATINO),
        (&["latvian"][..], LATVIAN),
    ] {
        if needles.iter().any(|n| contains(n)) {
            languages.push(language);
        }
    }

    languages.extend(regex_languages(&title));

    if contains("english") {
        languages.push(ENGLISH);
    }
    if languages.is_empty() {
        languages.push(UNKNOWN);
    }
    if languages.len() == 1 && languages[0] == GERMAN {
        if GERMAN_DUAL_LANGUAGE_RE.is_match(&title) {
            languages.push(ORIGINAL);
        } else if GERMAN_MULTI_LANGUAGE_RE.is_match(&title) {
            languages.push(ORIGINAL);
            languages.push(ENGLISH);
        }
    }

    let mut seen = std::collections::HashSet::new();
    languages.retain(|l| seen.insert(*l));
    languages
}

fn regex_languages(title: &str) -> Vec<i64> {
    let mut out = Vec::new();
    if let Some(m) = CASE_SENSITIVE_LANGUAGE_RE.captures(title) {
        for (group, language) in [
            ("lithuanian", LITHUANIAN),
            ("czech", CZECH),
            ("polish", POLISH),
            ("bulgarian", BULGARIAN),
            ("slovak", SLOVAK),
        ] {
            if m.success(group) {
                out.push(language);
            }
        }
    }
    for m in LANGUAGE_RE.captures_iter(title) {
        for (group, language) in [
            ("english", ENGLISH),
            ("italian", ITALIAN),
            ("german", GERMAN),
            ("flemish", FLEMISH),
            ("greek", GREEK),
            ("french", FRENCH),
            ("russian", RUSSIAN),
            ("dutch", DUTCH),
            ("hungarian", HUNGARIAN),
            ("hebrew", HEBREW),
            ("polish", POLISH),
            ("chinese", CHINESE),
            ("bulgarian", BULGARIAN),
            ("ukrainian", UKRAINIAN),
            ("spanish", SPANISH),
            ("thai", THAI),
            ("romanian", ROMANIAN),
            ("catalan", CATALAN),
            ("latvian", LATVIAN),
            ("turkish", TURKISH),
            ("original", ORIGINAL),
        ] {
            if m.success(group) {
                out.push(language);
            }
        }
    }
    out
}

/// `Parser.HasMultipleLanguages`: a `MULTi` tag in the name.
pub fn has_multiple_languages(title: &str) -> bool {
    static MULTI_RE: Lazy = net_regex!(r"[_. ](?<multi>multi)[_. ]");
    MULTI_RE.is_match(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_few_well_known_names() {
        assert_eq!(parse_languages("Show.S01E01.1080p.WEB.H264-GRP"), vec![UNKNOWN]);
        assert_eq!(
            parse_languages("Show.S01E01.German.DL.1080p.BluRay.x264-GRP"),
            vec![GERMAN, ORIGINAL]
        );
        assert_eq!(
            parse_languages("Show.S01E01.ITA.ENG.1080p.WEB-DL-GRP"),
            vec![ITALIAN, ENGLISH]
        );
        assert!(has_multiple_languages("Show.S01E01.MULTi.1080p.WEB-GRP"));
    }
}
