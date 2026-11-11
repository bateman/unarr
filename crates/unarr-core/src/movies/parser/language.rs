//! Port of Radarr's `LanguageParser.ParseLanguages`. The ids are Radarr's
//! (`crate::movies::quality::RADARR_LANGUAGES`), which part from Sonarr's
//! after Bulgarian; the words looked for differ too.

use crate::net_regex;
use crate::parser::netregex::Lazy;

/// Radarr's language ids.
pub mod lang {
    pub const ANY: i64 = -1;
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
    pub const HINDI: i64 = 26;
    pub const ROMANIAN: i64 = 27;
    pub const THAI: i64 = 28;
    pub const BULGARIAN: i64 = 29;
    pub const PORTUGUESE_BR: i64 = 30;
    pub const ARABIC: i64 = 31;
    pub const UKRAINIAN: i64 = 32;
    pub const PERSIAN: i64 = 33;
    pub const BENGALI: i64 = 34;
    pub const SLOVAK: i64 = 35;
    pub const LATVIAN: i64 = 36;
    pub const SPANISH_LATINO: i64 = 37;
    pub const CATALAN: i64 = 38;
    pub const CROATIAN: i64 = 39;
    pub const SERBIAN: i64 = 40;
    pub const BOSNIAN: i64 = 41;
    pub const ESTONIAN: i64 = 42;
    pub const TAMIL: i64 = 43;
    pub const INDONESIAN: i64 = 44;
    pub const TELUGU: i64 = 45;
    pub const MACEDONIAN: i64 = 46;
    pub const SLOVENIAN: i64 = 47;
    pub const MALAYALAM: i64 = 48;
    pub const KANNADA: i64 = 49;
    pub const ALBANIAN: i64 = 50;
    pub const AFRIKAANS: i64 = 51;
    pub const MARATHI: i64 = 52;
    pub const TAGALOG: i64 = 53;
    pub const URDU: i64 = 54;
    pub const ROMANSH: i64 = 55;
    pub const MONGOLIAN: i64 = 56;
    pub const GEORGIAN: i64 = 57;
}

use lang::*;

static LANGUAGE_RE: Lazy = net_regex!(
    r"(?:\W|_|^)(?<english>\beng\b)|(?<italian>\b(?:ita|italian)\b)|(?<german>(?:swiss)?german\b|videomann|ger[. ]dub|\bger\b)|(?<flemish>flemish)|(?<bulgarian>bgaudio)|(?<romanian>rodubbed)|(?<brazilian>\b(dublado|pt-BR)\b)|(?<greek>greek)|(?<french>\b(?:FR|VO|VF|VFF|VFQ|VFI|VF2|TRUEFRENCH|FRENCH|FRE|FRA)\b)|(?<russian>\b(?:rus|ru)\b)|(?<hungarian>\b(?:HUNDUB|HUN)\b)|(?<hebrew>\b(?:HebDub|HebDubbed)\b)|(?<polish>\b(?:PL\W?DUB|DUB\W?PL|LEK\W?PL|PL\W?LEK)\b)|(?<chinese>\[(?:CH[ST]|BIG5|GB)\]|简|繁|字幕)|(?<ukrainian>(?:(?:\dx)?UKR))|(?<spanish>\b(?:español|castellano)\b)|(?<catalan>\b(?:catalan?|catalán|català)\b)|(?<latvian>\b(?:lat|lav|lv)\b)|(?<telugu>\btel\b)|(?<vietnamese>\bVIE\b)|(?<japanese>\bJAP\b)|(?<korean>\bKOR\b)|(?<urdu>\burdu\b)|(?<romansh>\b(?:romansh|rumantsch|romansch)\b)|(?<mongolian>\b(?:mongolian|khalkha)\b)|(?<georgian>\b(?:georgian|geo|ka|kat)\b)|(?<original>\b(?:orig|original)\b)"
);

static CASE_SENSITIVE_LANGUAGE_RE: Lazy = net_regex!(
    r"(?:(?i)(?<!SUB[\W|_|^]))(?:(?<english>\bEN\b)|(?<lithuanian>\bLT\b)|(?<czech>\bCZ\b)|(?<polish>\bPL\b)|(?<bulgarian>\bBG\b)|(?<slovak>\bSK\b)|(?<german>\bDE\b)|(?<spanish>\b(?<!DTS[._ -])ES\b))(?:(?i)(?![\W|_|^]SUB))",
    case_sensitive
);

static GERMAN_DUAL_LANGUAGE_RE: Lazy = net_regex!(r"(?<!WEB[-_. ]?)\bDL\b");
static GERMAN_MULTI_LANGUAGE_RE: Lazy = net_regex!(r"\bML\b");

/// `LanguageParser.ParseLanguages`.
pub fn parse_languages(title: &str) -> Vec<i64> {
    let lower = title.to_lowercase();
    let mut languages: Vec<i64> = Vec::new();

    for (needles, language) in [
        (&["english"][..], ENGLISH),
        (&["spanish"][..], SPANISH),
        (&["danish"][..], DANISH),
        (&["dutch"][..], DUTCH),
        (&["japanese"][..], JAPANESE),
        (&["icelandic"][..], ICELANDIC),
        (&["mandarin", "cantonese", "chinese"][..], CHINESE),
        (&["korean"][..], KOREAN),
        (&["russian"][..], RUSSIAN),
        (&["romanian"][..], ROMANIAN),
        (&["hindi"][..], HINDI),
        (&["arabic"][..], ARABIC),
        (&["thai"][..], THAI),
        (&["bulgarian"][..], BULGARIAN),
        (&["polish"][..], POLISH),
        (&["vietnamese"][..], VIETNAMESE),
        (&["swedish"][..], SWEDISH),
        (&["norwegian"][..], NORWEGIAN),
        (&["finnish"][..], FINNISH),
        (&["turkish"][..], TURKISH),
        (&["portuguese"][..], PORTUGUESE),
        (&["brazilian"][..], PORTUGUESE_BR),
        (&["hungarian"][..], HUNGARIAN),
        (&["hebrew"][..], HEBREW),
        (&["ukrainian"][..], UKRAINIAN),
        (&["persian"][..], PERSIAN),
        (&["bengali"][..], BENGALI),
        (&["slovak"][..], SLOVAK),
        (&["latvian"][..], LATVIAN),
        (&["latino"][..], SPANISH_LATINO),
        (&["tamil"][..], TAMIL),
        (&["telugu"][..], TELUGU),
        (&["malayalam"][..], MALAYALAM),
        (&["kannada"][..], KANNADA),
        (&["albanian"][..], ALBANIAN),
        (&["afrikaans"][..], AFRIKAANS),
        (&["marathi"][..], MARATHI),
        (&["tagalog"][..], TAGALOG),
    ] {
        if needles.iter().any(|n| lower.contains(n)) {
            languages.push(language);
        }
    }

    for m in CASE_SENSITIVE_LANGUAGE_RE.captures_iter(title) {
        for (group, language) in [
            ("english", ENGLISH),
            ("lithuanian", LITHUANIAN),
            ("czech", CZECH),
            ("polish", POLISH),
            ("bulgarian", BULGARIAN),
            ("slovak", SLOVAK),
            ("spanish", SPANISH),
            ("german", GERMAN),
        ] {
            if m.success(group) {
                languages.push(language);
            }
        }
    }

    for m in LANGUAGE_RE.captures_iter(title) {
        // The original also asks for a `dutch` group its pattern does not
        // have; it never matches.
        for (group, language) in [
            ("english", ENGLISH),
            ("italian", ITALIAN),
            ("german", GERMAN),
            ("flemish", FLEMISH),
            ("greek", GREEK),
            ("french", FRENCH),
            ("russian", RUSSIAN),
            ("bulgarian", BULGARIAN),
            ("brazilian", PORTUGUESE_BR),
            ("hungarian", HUNGARIAN),
            ("hebrew", HEBREW),
            ("polish", POLISH),
            ("chinese", CHINESE),
            ("spanish", SPANISH),
            ("catalan", CATALAN),
            ("ukrainian", UKRAINIAN),
            ("latvian", LATVIAN),
            ("romanian", ROMANIAN),
            ("telugu", TELUGU),
            ("vietnamese", VIETNAMESE),
            ("japanese", JAPANESE),
            ("korean", KOREAN),
            ("urdu", URDU),
            ("romansh", ROMANSH),
            ("mongolian", MONGOLIAN),
            ("georgian", GEORGIAN),
            ("original", ORIGINAL),
        ] {
            if m.success(group) {
                languages.push(language);
            }
        }
    }

    if languages.is_empty() {
        languages.push(UNKNOWN);
    }
    if languages.len() == 1 && languages[0] == GERMAN {
        if GERMAN_DUAL_LANGUAGE_RE.is_match(title) {
            languages.push(ORIGINAL);
        } else if GERMAN_MULTI_LANGUAGE_RE.is_match(title) {
            languages.push(ORIGINAL);
            languages.push(ENGLISH);
        }
    }

    let mut seen = std::collections::HashSet::new();
    languages.retain(|l| seen.insert(*l));
    languages
}
