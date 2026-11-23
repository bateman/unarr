//! Movie file and folder names: a port of Radarr's `FileNameBuilder`.
//!
//! The machinery is Sonarr's (`crate::naming`): a format is text with
//! `{Token}` placeholders, each with an optional prefix and suffix kept
//! only when the token has a value, a separator that replaces the blanks
//! of the value, and a custom format after a colon. Radarr's differ in
//! three ways. A token may sit in a tag, `{edition-{Edition Tags}}` or
//! `{imdb-{ImdbId}}`, which goes when the token is empty; braces count
//! as prefix and suffix, so `{{Edition Tags}}` reads `{Uncut}`; and
//! there is no `{{` escape.
//!
//! The naming examples of the settings page use it now; renaming will.

use std::sync::OnceLock;

use fancy_regex::Regex;

use super::model::{MovieMetadata, MovieNamingConfig};
use crate::model::QualityModel;
use crate::naming::{
    add_custom_format_tokens, add_media_info_tokens, clean_file_name_as, clean_folder_name, file_stem,
    is_blank, quality_real, replace_reserved_device_names, title_first_character, title_the, truncate,
    Handlers, NamingCustomFormat, NamingMediaInfo, TokenMatch,
};

macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| Regex::new($pattern).expect(stringify!($name)))
        }
    };
}

// `TitleRegex`. The original names both prefixes `prefix`, and reads the
// last one captured, which is the second: the first is part of the tag.
regex!(
    title_re,
    r"(?i)(?<tag>\{(?<tagprefix>[-{ ._\[(]*)(?:imdb(?:id)?-|edition-))?\{(?<prefix>[-{ ._\[(]*)(?<token>(?:[a-z0-9]+)(?:(?<separator>[- ._]+)(?:[a-z0-9]+))?)(?::(?<customFormat>[ ,a-z0-9|+-]+(?<![- ])))?(?<suffix>[-} ._)\]]*)\}"
);
regex!(cleanup_re, r"([- ._])(\1)+");
regex!(trim_separators_re, r"[- ._]+$");
regex!(title_prefix_re, r"(?i)^(The|An|A) (.*?)((?: *\([^)]+\))*)$");
regex!(
    edition_ordinal_re,
    r"(?i)((?:\b|_)\d{1,3}(?:st|th|rd|nd|mm)(?:\b|_))"
);
regex!(edition_upper_re, r"(?i)((?:\b|_)(?:IMAX|3D|SDR|HDR|DV)(?:\b|_))");

/// What `{Release Group}` reads for a file without one, when the token
/// stands bare.
const RELEASE_GROUP_DEFAULT: &str = "Radarr";

/// A movie as the builder reads it: its metadata, and its titles in other
/// languages, `(language id, title)`, for `{Movie Title:FR}`.
pub struct NamingMovie<'a> {
    pub metadata: &'a MovieMetadata,
    pub translations: &'a [(i64, String)],
}

/// A movie file as the builder reads it.
pub struct NamingMovieFile<'a> {
    pub quality: QualityModel,
    /// The quality definition's title, which the user may have renamed.
    pub quality_title: &'a str,
    pub relative_path: &'a str,
    pub scene_name: Option<&'a str>,
    pub release_group: Option<&'a str>,
    pub edition: Option<&'a str>,
    pub media_info: Option<&'a NamingMediaInfo>,
    /// More than one view in the video stream (`VideoMultiViewCount > 1`).
    pub three_d: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MovieNamingError {
    #[error("Standard movie format cannot be empty")]
    EmptyFormat,
}

/// `IsoLanguages`: two-letter code, country, three-letter code, language id.
const ISO_LANGUAGES: &[(&str, &str, &str, i64)] = &[
    ("en", "", "eng", 1),
    ("fr", "fr", "fra", 2),
    ("es", "", "spa", 3),
    ("de", "de", "deu", 4),
    ("it", "", "ita", 5),
    ("da", "", "dan", 6),
    ("nl", "", "nld", 7),
    ("ja", "", "jpn", 8),
    ("is", "", "isl", 9),
    ("zh", "cn", "zho", 10),
    ("ru", "", "rus", 11),
    ("pl", "", "pol", 12),
    ("vi", "", "vie", 13),
    ("sv", "", "swe", 14),
    ("no", "", "nor", 15),
    ("nb", "", "nob", 15),
    ("fi", "", "fin", 16),
    ("tr", "", "tur", 17),
    ("pt", "pt", "por", 18),
    ("el", "", "ell", 20),
    ("ko", "", "kor", 21),
    ("hu", "", "hun", 22),
    ("he", "", "heb", 23),
    ("cs", "", "ces", 25),
    ("hi", "", "hin", 26),
    ("th", "", "tha", 28),
    ("bg", "", "bul", 29),
    ("ro", "", "ron", 27),
    ("pt", "br", "", 30),
    ("ar", "", "ara", 31),
    ("uk", "", "ukr", 32),
    ("fa", "", "fas", 33),
    ("bn", "", "ben", 34),
    ("lt", "", "lit", 24),
    ("sk", "", "slk", 35),
    ("lv", "", "lav", 36),
    ("es", "mx", "spa", 37),
    ("ca", "", "cat", 38),
    ("hr", "", "hrv", 39),
    ("sr", "", "srp", 40),
    ("bs", "", "bos", 41),
    ("et", "", "est", 42),
    ("ta", "", "tam", 43),
    ("id", "", "ind", 44),
    ("te", "", "tel", 45),
    ("mk", "", "mkd", 46),
    ("sl", "", "slv", 47),
    ("ml", "", "mal", 48),
    ("kn", "", "kan", 49),
    ("sq", "", "sqi", 50),
    ("af", "", "afr", 51),
    ("mr", "", "mar", 52),
    ("tl", "", "tgl", 53),
    ("ur", "", "urd", 54),
    ("rm", "", "roh", 55),
    ("mn", "", "mon", 56),
    ("ka", "", "kat", 57),
];

/// `Iso639BTMap`: the bibliographic three-letter codes, as terminologic.
const ISO_639_BT: &[(&str, &str)] = &[
    ("alb", "sqi"),
    ("arm", "hye"),
    ("baq", "eus"),
    ("bur", "mya"),
    ("chi", "zho"),
    ("cze", "ces"),
    ("dut", "nld"),
    ("fre", "fra"),
    ("geo", "kat"),
    ("ger", "deu"),
    ("gre", "ell"),
    ("gsw", "deu"),
    ("ice", "isl"),
    ("mac", "mkd"),
    ("mao", "mri"),
    ("may", "msa"),
    ("per", "fas"),
    ("rum", "ron"),
    ("slo", "slk"),
    ("tib", "bod"),
    ("wel", "cym"),
    ("khk", "mon"),
    ("mvf", "mon"),
];

/// `IsoLanguages.Find`: the id of the language an ISO code names (`fr`,
/// `pt-br`, `ger`), lower case.
pub fn language_by_iso_code(code: &str) -> Option<i64> {
    if code == "cn" {
        return Some(10);
    }
    let mut parts = code.split('-');
    let language = parts.next().unwrap_or("");
    let country = parts.next();
    match language.len() {
        2 => {
            let named = || ISO_LANGUAGES.iter().filter(|l| l.0 == language);
            match country {
                Some(country) if named().any(|l| l.1 == country) => named().find(|l| l.1 == country),
                Some(_) => named().find(|l| l.1.is_empty()),
                None => named().next(),
            }
            .map(|l| l.3)
        }
        3 => {
            let language = ISO_639_BT
                .iter()
                .find(|(b, _)| *b == language)
                .map_or(language, |(_, t)| *t);
            ISO_LANGUAGES.iter().find(|l| l.2 == language).map(|l| l.3)
        }
        _ => None,
    }
}

/// `IsoLanguages.Get(language)?.TwoLetterCode`: the code a language is
/// written with, `None` for one the table has no row for.
pub fn iso_two_letter_code(language_id: i64) -> Option<&'static str> {
    ISO_LANGUAGES.iter().find(|l| l.3 == language_id).map(|l| l.0)
}

/// Letters the decomposition leaves whole, as `RemoveDiacritics` folds them.
fn fold_letter(c: char, out: &mut String) {
    match c {
        'Ø' => out.push('O'),
        'ø' => out.push('o'),
        'Đ' | 'Ð' => out.push('D'),
        'đ' | 'ð' => out.push('d'),
        'Ł' => out.push('L'),
        'ł' => out.push('l'),
        'Æ' => out.push_str("AE"),
        'æ' => out.push_str("ae"),
        'Œ' => out.push_str("OE"),
        'œ' => out.push_str("oe"),
        'ß' => out.push_str("ss"),
        'Þ' => out.push_str("TH"),
        'þ' => out.push_str("th"),
        other => out.push(other),
    }
}

fn remove_diacritics(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in crate::parser::title::remove_accent(text).chars() {
        fold_letter(c, &mut out);
    }
    out
}

/// Radarr's `CleanTitle`: Sonarr's, and no diacritics.
pub fn clean_title(title: &str) -> String {
    remove_diacritics(&crate::naming::clean_title(title))
}

/// Radarr's `CleanTitleThe`.
pub fn clean_title_the(title: &str) -> String {
    if is_blank(title) {
        return String::new();
    }
    if let Ok(Some(c)) = title_prefix_re().captures(title) {
        let group = |i| c.get(i).map_or("", |m| m.as_str());
        return format!(
            "{}, {}{}",
            clean_title(group(2)).trim(),
            group(1),
            clean_title(group(3))
        );
    }
    clean_title(title)
}

fn the(title: &str) -> String {
    if is_blank(title) {
        return String::new();
    }
    title_the(title)
}

/// `TextInfo.ToTitleCase` of a lower-cased text: a capital where a word
/// starts, a word being letters and the apostrophes inside them.
fn title_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous: Option<char> = None;
    let mut before_previous: Option<char> = None;
    for c in text.to_lowercase().chars() {
        let in_word = match previous {
            Some(p) if p.is_alphabetic() => true,
            Some('\'') => before_previous.is_some_and(char::is_alphabetic),
            _ => false,
        };
        if c.is_alphabetic() && !in_word {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
        before_previous = previous;
        previous = Some(c);
    }
    out
}

/// `GetEditionToken`: the edition in title case, ordinals lower (`10th`),
/// the formats everybody writes in capitals upper (`IMAX`, `3D`, `HDR`).
fn edition_token(edition: &str) -> String {
    let edition = title_case(edition);
    let edition =
        edition_ordinal_re().replace_all(&edition, |c: &fancy_regex::Captures<str>| c[1].to_lowercase());
    edition_upper_re()
        .replace_all(&edition, |c: &fancy_regex::Captures<str>| c[1].to_uppercase())
        .into_owned()
}

/// `GetLanguageTitle`: the title in the first language of `codes`
/// (`FR|DE`) that is a language at all, when the movie has one there.
fn language_title<'a>(movie: &'a NamingMovie<'a>, codes: Option<&str>) -> &'a str {
    let title = movie.metadata.title.as_str();
    for code in codes.unwrap_or("").split('|').filter(|c| !is_blank(c)) {
        let Some(language) = language_by_iso_code(&code.to_lowercase()) else {
            continue;
        };
        return movie
            .translations
            .iter()
            .find(|(id, _)| *id == language)
            .map_or(title, |(_, translated)| translated.as_str());
    }
    title
}

fn add_movie_tokens<'a>(h: &mut Handlers<'a>, movie: &'a NamingMovie<'a>) {
    let md = movie.metadata;
    let format = |m: &'_ TokenMatch| m.custom_format.clone();
    h.set("{Movie Title}", move |m| {
        let cf = format(m);
        Some(truncate(language_title(movie, cf.as_deref()), cf.as_deref()))
    });
    h.set("{Movie CleanTitle}", move |m| {
        let cf = format(m);
        Some(truncate(
            &clean_title(language_title(movie, cf.as_deref())),
            cf.as_deref(),
        ))
    });
    h.set("{Movie TitleThe}", move |m| {
        Some(truncate(&the(&md.title), m.custom_format.as_deref()))
    });
    h.set("{Movie CleanTitleThe}", move |m| {
        Some(truncate(&clean_title_the(&md.title), m.custom_format.as_deref()))
    });
    h.set("{Movie TitleFirstCharacter}", move |m| {
        Some(title_first_character(&the(language_title(
            movie,
            m.custom_format.as_deref(),
        ))))
    });
    let original = md.original_title.as_deref().unwrap_or("");
    h.set("{Movie OriginalTitle}", move |m| {
        Some(truncate(original, m.custom_format.as_deref()))
    });
    h.set("{Movie CleanOriginalTitle}", move |m| {
        Some(truncate(&clean_title(original), m.custom_format.as_deref()))
    });
    h.value(
        "{Movie Certification}",
        md.certification.clone().unwrap_or_default(),
    );
    let collection = md.collection_title.as_deref().unwrap_or("");
    h.set("{Movie Collection}", move |m| {
        Some(truncate(collection, m.custom_format.as_deref()))
    });
    h.set("{Movie CollectionThe}", move |m| {
        Some(truncate(&the(collection), m.custom_format.as_deref()))
    });
    h.set("{Movie CleanCollectionThe}", move |m| {
        Some(truncate(&clean_title_the(collection), m.custom_format.as_deref()))
    });
    // A movie without a year yet has none to show.
    h.value(
        "{Release Year}",
        if md.year > 0 {
            md.year.to_string()
        } else {
            String::new()
        },
    );
    h.value("{ImdbId}", md.imdb_id.clone().unwrap_or_default());
    h.value("{TmdbId}", md.tmdb_id.to_string());
}

/// `NamingConfig.Default`'s way with a file name: illegal characters
/// replaced, the colon smartly.
fn clean_file_name_default(name: &str) -> String {
    clean_file_name_as(name, true, 4, "")
}

fn original_file_name(file: &NamingMovieFile<'_>, multiple_tokens: bool) -> String {
    if multiple_tokens {
        return String::new();
    }
    file_stem(file.relative_path).to_string()
}

fn original_title(file: &NamingMovieFile<'_>, multiple_tokens: bool) -> String {
    match file.scene_name.filter(|s| !is_blank(s)) {
        Some(scene) => clean_file_name_default(scene),
        None => clean_file_name_default(&original_file_name(file, multiple_tokens)),
    }
}

/// The codes Radarr's `Iso639BTMap` has and Sonarr's has not, as the
/// language they stand for: Swiss German, and two Mongolian ones.
const RADARR_LANGUAGE_CODES: &[(&str, &str)] = &[("gsw", "deu"), ("khk", "mon"), ("mvf", "mon")];

/// The media info with its language codes as Radarr reads them.
fn radarr_media_info(media_info: &NamingMediaInfo) -> NamingMediaInfo {
    let read = |codes: &[String]| -> Vec<String> {
        codes
            .iter()
            .map(|code| {
                let lower = code.trim().to_lowercase();
                RADARR_LANGUAGE_CODES
                    .iter()
                    .find(|(from, _)| *from == lower)
                    .map_or_else(|| code.clone(), |(_, to)| to.to_string())
            })
            .collect()
    };
    NamingMediaInfo {
        audio_languages: read(&media_info.audio_languages),
        subtitles: read(&media_info.subtitles),
        ..media_info.clone()
    }
}

fn add_file_tokens<'a>(
    h: &mut Handlers<'a>,
    file: &'a NamingMovieFile<'a>,
    media_info: Option<&'a NamingMediaInfo>,
    multiple_tokens: bool,
) {
    h.value("{Original Title}", original_title(file, multiple_tokens));
    h.value("{Original Filename}", original_file_name(file, multiple_tokens));
    h.set("{Release Group}", move |m| {
        Some(match file.release_group.filter(|g| !is_blank(g)) {
            Some(group) => truncate(group, m.custom_format.as_deref()),
            None => m.default_value(RELEASE_GROUP_DEFAULT),
        })
    });
    if let Some(edition) = file.edition.filter(|e| !is_blank(e)) {
        h.set("{Edition Tags}", move |m| {
            Some(truncate(&edition_token(edition), m.custom_format.as_deref()))
        });
    }
    let proper = if file.quality.version > 1 { "Proper" } else { "" };
    let real = quality_real(file.quality);
    h.value(
        "{Quality Full}",
        format!("{} {} {}", file.quality_title, proper, real),
    );
    h.value("{Quality Title}", file.quality_title.to_string());
    h.value("{Quality Proper}", proper.to_string());
    h.value("{Quality Real}", real.to_string());
    add_media_info_tokens(h, media_info);
    if media_info.is_some() {
        h.value(
            "{MediaInfo 3D}",
            if file.three_d {
                "3D".to_string()
            } else {
                String::new()
            },
        );
    }
}

/// `ReplaceToken`.
fn replace_token(c: &fancy_regex::Captures<str>, h: &Handlers<'_>, config: &MovieNamingConfig) -> String {
    let group = |name: &str| c.name(name).map(|m| m.as_str().to_string()).unwrap_or_default();
    let m = TokenMatch {
        prefix: group("prefix"),
        separator: group("separator"),
        suffix: group("suffix"),
        token: group("token"),
        custom_format: Some(group("customFormat")).filter(|f| !is_blank(f)),
    };
    let value = h
        .get(&m.token)
        .and_then(|handler| handler(&m))
        .unwrap_or_default();
    let mut text = value.trim().to_string();
    if m.token.chars().all(|c| !c.is_alphabetic() || c.is_lowercase()) {
        text = text.to_lowercase();
    } else if m.token.chars().all(|c| !c.is_alphabetic() || c.is_uppercase()) {
        text = text.to_uppercase();
    }
    if !is_blank(&m.separator) {
        text = text.replace(' ', &m.separator);
    }
    text = clean_file_name_as(
        &text,
        config.replace_illegal_characters,
        config.colon_replacement_format,
        "",
    );
    if is_blank(&text) {
        return text;
    }
    format!("{}{}{}{}", group("tag"), m.prefix, text, m.suffix)
}

fn replace_tokens(pattern: &str, h: &Handlers<'_>, config: &MovieNamingConfig) -> String {
    title_re()
        .replace_all(pattern, |c: &fancy_regex::Captures<str>| {
            replace_token(c, h, config)
        })
        .into_owned()
}

/// The parts of a format between slashes, each built by `component` and
/// kept when something is left of it.
fn components(pattern: &str, component: impl Fn(&str) -> String) -> String {
    pattern
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .map(|part| replace_reserved_device_names(&component(part).replace("{ellipsis}", "...")))
        .filter(|part| !is_blank(part))
        .collect::<Vec<_>>()
        .join("/")
}

/// `BuildFileName`: the name of `file` of `movie`, without directory nor
/// extension. The file's own name when renaming is off.
pub fn build_file_name(
    movie: &NamingMovie<'_>,
    file: &NamingMovieFile<'_>,
    config: &MovieNamingConfig,
    formats: &[NamingCustomFormat<'_>],
) -> Result<String, MovieNamingError> {
    if !config.rename_movies {
        return Ok(original_title(file, false));
    }
    let pattern = config.standard_movie_format.as_str();
    if is_blank(pattern) {
        return Err(MovieNamingError::EmptyFormat);
    }
    let multiple_tokens = title_re().find_iter(pattern).filter(|m| m.is_ok()).count() > 1;
    let media_info = file.media_info.map(radarr_media_info);
    let mut h = Handlers::new();
    add_movie_tokens(&mut h, movie);
    add_file_tokens(&mut h, file, media_info.as_ref(), multiple_tokens);
    add_custom_format_tokens(&mut h, formats);
    Ok(components(pattern, |part| {
        let name = replace_tokens(part, &h, config);
        let name = cleanup_re().replace_all(name.trim(), "$1");
        trim_separators_re().replace_all(&name, "").into_owned()
    }))
}

/// `GetMovieFolder`: the folder format with the movie's tokens replaced.
pub fn movie_folder(movie: &NamingMovie<'_>, config: &MovieNamingConfig) -> String {
    let mut h = Handlers::new();
    add_movie_tokens(&mut h, movie);
    components(&config.movie_folder_format, |part| {
        clean_folder_name(&replace_tokens(part, &h, config))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movies::model::MovieStatus;

    fn metadata(title: &str, year: i64, tmdb_id: i64) -> MovieMetadata {
        MovieMetadata {
            id: 1,
            tmdb_id,
            imdb_id: Some("tt0000001".into()),
            title: title.into(),
            sort_title: None,
            clean_title: None,
            original_title: None,
            clean_original_title: None,
            original_language_id: 1,
            status: MovieStatus::Released,
            overview: None,
            images: Vec::new(),
            genres: Vec::new(),
            keywords: Vec::new(),
            ratings: serde_json::json!({}),
            recommendations: Vec::new(),
            runtime: 0,
            in_cinemas: None,
            physical_release: None,
            digital_release: None,
            year,
            secondary_year: None,
            certification: None,
            youtube_trailer_id: None,
            studio: None,
            website: None,
            popularity: 0.0,
            collection_tmdb_id: 0,
            collection_title: None,
            last_info_sync: None,
        }
    }

    fn config(standard: &str, folder: &str) -> MovieNamingConfig {
        MovieNamingConfig {
            rename_movies: true,
            replace_illegal_characters: true,
            colon_replacement_format: 4,
            standard_movie_format: standard.into(),
            movie_folder_format: folder.into(),
        }
    }

    fn folder(format: &str, md: &MovieMetadata) -> String {
        let movie = NamingMovie {
            metadata: md,
            translations: &[],
        };
        movie_folder(&movie, &config("", format))
    }

    fn file<'a>() -> NamingMovieFile<'a> {
        NamingMovieFile {
            quality: QualityModel {
                quality_id: 4,
                version: 1,
                real: 0,
                is_repack: false,
            },
            quality_title: "HDTV-720p",
            relative_path: "",
            scene_name: None,
            release_group: Some("RadarrTest"),
            edition: None,
            media_info: None,
            three_d: false,
        }
    }

    /// `BuildFileName` of South Park, as the original's fixtures set it up.
    fn name_of(md: &MovieMetadata, file: &NamingMovieFile<'_>, format: &str) -> String {
        let translations = [
            (4, "German South Park".to_string()),
            (2, "French South Park".to_string()),
        ];
        let movie = NamingMovie {
            metadata: md,
            translations: &translations,
        };
        build_file_name(&movie, file, &config(format, ""), &[]).unwrap()
    }

    fn name(format: &str) -> String {
        name_of(&metadata("South Park", 2020, 124578), &file(), format)
    }

    #[test]
    fn folders_as_the_original_names_them() {
        let movie_folder = folder;
        let format = "{Movie CleanTitle} ({Release Year}) {tmdb-{TmdbId}}";
        assert_eq!(
            movie_folder(format, &metadata("Alien: Romulus", 2024, 945961)),
            "Alien Romulus (2024) {tmdb-945961}"
        );
        assert_eq!(
            movie_folder(format, &metadata("Candyman: Day of the Dead", 1999, 12485)),
            "Candyman Day of the Dead (1999) {tmdb-12485}"
        );
        // No year yet: the parentheses of the format stay, empty.
        assert_eq!(
            movie_folder(format, &metadata("Untitled Alien: Romulus Sequel", 0, 1434936)),
            "Untitled Alien Romulus Sequel () {tmdb-1434936}"
        );
        assert_eq!(
            movie_folder(
                "{Movie TitleThe} {(Release Year)}",
                &metadata("The Thing", 1982, 1091)
            ),
            "Thing, The (1982)"
        );
        assert_eq!(
            movie_folder("{Movie Title} {(Release Year)}", &metadata("Soon", 0, 1)),
            "Soon"
        );
    }

    /// From `FileNameBuilderFixture`.
    #[test]
    fn tokens_take_their_separator_and_their_case() {
        assert_eq!(name("{Movie Title}"), "South Park");
        assert_eq!(name("{Movie_Title}"), "South_Park");
        assert_eq!(name("{Movie.Title}"), "South.Park");
        assert_eq!(name("{MOVIE TITLE}"), "SOUTH PARK");
        assert_eq!(name("{mOvIe-tItLE}"), "South-Park");
        assert_eq!(name("{movie title}"), "south park");
        assert_eq!(name("{Movie Title} [{Quality Title}]"), "South Park [HDTV-720p]");
        assert_eq!(name("{Movie.Title}.{_Quality.Title_}"), "South.Park._HDTV-720p");
        assert_eq!(
            name("{Movie CleanTitle} ({Release Year}) [{Quality Title}] [tmdb-{TmdbId}]"),
            "South Park (2020) [HDTV-720p] [tmdb-124578]"
        );
        let md = metadata("South Park (1997)", 1997, 1);
        assert_eq!(name_of(&md, &file(), "{Movie.CleanTitle}"), "South.Park.1997");
        let md = metadata("Venture Bros.", 2003, 1);
        assert_eq!(name_of(&md, &file(), "{Movie.Title}"), "Venture.Bros");
    }

    #[test]
    fn a_title_is_read_in_the_language_asked_for() {
        assert_eq!(name("{Movie Title:FR}"), "French South Park");
        // Not a language, or one the movie has no title in.
        assert_eq!(name("{Movie Title:JP}"), "South Park");
        assert_eq!(name("{Movie Title:JA}"), "South Park");
        assert_eq!(name("{Movie Title:JP|FR}"), "French South Park");
        assert_eq!(name("{Movie Title:JP|CN}"), "South Park");
        assert_eq!(language_by_iso_code("pt-br"), Some(30));
        assert_eq!(language_by_iso_code("pt"), Some(18));
        assert_eq!(language_by_iso_code("ger"), Some(4));
        assert_eq!(language_by_iso_code("xx"), None);
    }

    #[test]
    fn quality_and_release_group_go_when_they_have_nothing_to_say() {
        let proper = NamingMovieFile {
            quality: QualityModel {
                quality_id: 4,
                version: 2,
                real: 0,
                is_repack: false,
            },
            ..file()
        };
        let md = metadata("South Park", 2020, 1);
        let format = "{Movie Title} [{Quality Title}] {[Quality Proper]}";
        assert_eq!(name_of(&md, &proper, format), "South Park [HDTV-720p] [Proper]");
        assert_eq!(name(format), "South Park [HDTV-720p]");
        assert_eq!(
            name_of(&md, &proper, "{Movie Title} [{Quality Full}]"),
            "South Park [HDTV-720p Proper]"
        );
        assert_eq!(name("{Movie Title} [{Quality Full}]"), "South Park [HDTV-720p]");
        for separator in [' ', '-', '.', '_'] {
            let format = format!("{{Quality{separator}Title}}{separator}{{Quality{separator}Proper}}");
            assert_eq!(name(&format), "HDTV-720p");
            let format = format!("{format}{separator}{{Movie{separator}Title}}");
            assert_eq!(name(&format), format!("HDTV-720p{separator}South{separator}Park"));
        }
        let no_group = NamingMovieFile {
            release_group: None,
            ..file()
        };
        assert_eq!(name("{Release Group}"), "RadarrTest");
        assert_eq!(name_of(&md, &no_group, "{Release Group}"), "Radarr");
        for format in [
            "{Movie Title}{-Release Group}",
            "{Movie Title}{ Release Group}",
            "{Movie Title}{ [Release Group]}",
        ] {
            assert_eq!(name_of(&md, &no_group, format), "South Park");
        }
    }

    #[test]
    fn the_file_keeps_its_name_where_the_format_asks_or_renaming_is_off() {
        let md = metadata("30 Rock", 2006, 1);
        let scene = NamingMovieFile {
            scene_name: Some("30.Rock.S01E01.xvid-LOL"),
            relative_path: "30 Rock - S01E01 - Test",
            ..file()
        };
        assert_eq!(
            name_of(&md, &scene, "{Movie Title} - {Original Title}"),
            "30 Rock - 30.Rock.S01E01.xvid-LOL"
        );
        assert_eq!(
            name_of(&md, &scene, "{Original Title}"),
            "30.Rock.S01E01.xvid-LOL"
        );
        assert_eq!(
            name_of(&md, &scene, "{Original Filename}"),
            "30 Rock - S01E01 - Test"
        );
        let no_scene = NamingMovieFile {
            relative_path: "existing.file.mkv",
            ..file()
        };
        assert_eq!(name_of(&md, &no_scene, "{Original Title}"), "existing.file");
        let movie = NamingMovie {
            metadata: &md,
            translations: &[],
        };
        let off = MovieNamingConfig {
            rename_movies: false,
            ..config("{Movie Title}", "")
        };
        let colon = NamingMovieFile {
            scene_name: Some("30.Rock.S01E01.xvid:LOL"),
            ..file()
        };
        assert_eq!(
            build_file_name(&movie, &colon, &off, &[]).unwrap(),
            "30.Rock.S01E01.xvid-LOL"
        );
        assert_eq!(
            build_file_name(&movie, &file(), &config(" ", ""), &[]),
            Err(MovieNamingError::EmptyFormat)
        );
    }

    /// `should_format_languagecodes_properly`, Radarr's cases.
    #[test]
    fn language_codes_read_as_the_original_reads_them() {
        let md = metadata("South Park", 2020, 1);
        for (code, expected) in [
            ("nob", "NB"),
            ("nor", "NO"),
            ("swe", "SV"),
            ("chi", "ZH"),
            ("fre", "FR"),
            ("ger", "DE"),
            ("gsw", "DE"),
            ("dut", "NL"),
            ("khk", "MN"),
            ("mvf", "MN"),
            ("kat", "KA"),
        ] {
            let media_info = NamingMediaInfo {
                video_codec: "H264".into(),
                audio_codec: "DTS".into(),
                audio_languages: vec!["eng".into()],
                subtitles: vec![code.into()],
                ..NamingMediaInfo::default()
            };
            let file = NamingMovieFile {
                media_info: Some(&media_info),
                ..file()
            };
            assert_eq!(
                name_of(&md, &file, "{Movie.Title}.{MEDIAINFO.FULL}"),
                format!("South.Park.H264.DTS.[{expected}]"),
                "{code}"
            );
        }
    }

    /// From `EditionTagsFixture`.
    #[test]
    fn edition_tags_are_title_cased_and_their_tag_goes_with_them() {
        let md = metadata("Movie Title", 2020, 1);
        let edition = |edition: &'static str, format: &str| {
            let file = NamingMovieFile {
                edition: Some(edition),
                ..file()
            };
            name_of(&md, &file, format)
        };
        assert_eq!(
            edition("Uncut", "{Movie Title} [{Edition Tags}]"),
            "Movie Title [Uncut]"
        );
        for format in [
            "{Movie Title} {Edition Tags}",
            "{Movie Title} {{Edition Tags}}",
            "{Movie Title} {edition-{Edition Tags}}",
            "{Movie Title} {{edition-{Edition Tags}}}",
        ] {
            assert_eq!(edition("", format), "Movie Title", "{format}");
        }
        assert_eq!(
            edition("Uncut", "{Movie Title} {{Edition Tags}}"),
            "Movie Title {Uncut}"
        );
        assert_eq!(
            edition("Uncut", "{Movie Title} {{edition-{Edition Tags}}}"),
            "Movie Title {{edition-Uncut}}"
        );
        // A film gauge stays in lower case, as an ordinal does (Radarr 7).
        for (given, format, expected) in [
            (
                "35mm edition",
                "{Movie Title} [{edition tags}]",
                "Movie Title [35mm edition]",
            ),
            (
                "35MM EDITION",
                "{Movie Title} [{edition tags}]",
                "Movie Title [35mm edition]",
            ),
            (
                "35mm edition",
                "{Movie Title} [{Edition Tags}]",
                "Movie Title [35mm Edition]",
            ),
            (
                "35MM EDITION",
                "{Movie Title} [{Edition Tags}]",
                "Movie Title [35mm Edition]",
            ),
            (
                "35mm edition",
                "{Movie Title} [{EDITION TAGS}]",
                "Movie Title [35MM EDITION]",
            ),
            (
                "35MM EDITION",
                "{Movie Title} [{EDITION TAGS}]",
                "Movie Title [35MM EDITION]",
            ),
        ] {
            assert_eq!(edition(given, format), expected, "{given} {format}");
        }
        for (given, format, expected) in [
            (
                "4th anNiverSary eDitIOn",
                "[{Edition Tags}]",
                "[4th Anniversary Edition]",
            ),
            (
                "7TH anniversary edition",
                "[{Edition Tags}]",
                "[7th Anniversary Edition]",
            ),
            (
                "10Th anniversary edition",
                "[{edition tags}]",
                "[10th anniversary edition]",
            ),
            (
                "10th anniversary edition",
                "[{EDITION TAGS}]",
                "[10TH ANNIVERSARY EDITION]",
            ),
            ("imax", "[{edition tags}]", "[imax]"),
            ("Imax", "[{Edition Tags}]", "[IMAX]"),
            ("imax edition", "[{Edition Tags}]", "[IMAX Edition]"),
            ("IMAX-edition", "[{Edition Tags}]", "[IMAX-Edition]"),
            ("IMAX_edition", "[{Edition Tags}]", "[IMAX_Edition]"),
            ("IMAX.eDiTioN", "[{Edition Tags}]", "[IMAX.Edition]"),
            ("Imax-ed.", "[{Edition Tags}]", "[IMAX-Ed.]"),
            ("3d", "[{edition tags}]", "[3d]"),
            ("3d", "[{Edition Tags}]", "[3D]"),
            ("director's cut", "[{Edition Tags}]", "[Director's Cut]"),
        ] {
            assert_eq!(
                edition(given, &format!("{{Movie Title}} {format}")),
                format!("Movie Title {expected}"),
                "{given}"
            );
        }
    }

    /// From the title fixtures: `TitleThe`, `CleanTitleThe`, the colon.
    #[test]
    fn titles_are_cleaned_and_turned_as_the_original_does() {
        let title = |title: &str, format: &str| name_of(&metadata(title, 2020, 1), &file(), format);
        assert_eq!(title("The Mist", "{Movie TitleThe}"), "Mist, The");
        assert_eq!(
            title("A Place to Call Home", "{Movie TitleThe}"),
            "Place to Call Home, A"
        );
        assert_eq!(
            title("An Adventure in Space and Time", "{Movie TitleThe}"),
            "Adventure in Space and Time, An"
        );
        assert_eq!(title("The Flash (2010)", "{Movie TitleThe}"), "Flash, The (2010)");
        assert_eq!(title("Batman", "{Movie TitleThe}"), "Batman");
        assert_eq!(title("The Mist", "{Movie CleanTitleThe}"), "Mist, The");
        assert_eq!(
            title("The Flash (2010)", "{Movie CleanTitleThe}"),
            "Flash, The 2010"
        );
        assert_eq!(
            title("Mission: Impossible", "{Movie CleanTitle}"),
            "Mission Impossible"
        );
        assert_eq!(title("Amélie", "{Movie CleanTitle}"), "Amelie");
        assert_eq!(
            title("Mission: Impossible", "{Movie Title}"),
            "Mission - Impossible"
        );
        assert_eq!(title("The Mist", "{Movie TitleFirstCharacter}"), "M");
        assert_eq!(title("'71", "{Movie TitleFirstCharacter}"), "7");
        // Truncated to the length asked for, the ellipsis counted in.
        assert_eq!(
            title("The Fantastic Life of Mr. Sisko", "{Movie Title:16}"),
            "The Fantastic..."
        );
        assert_eq!(
            title("The Fantastic Life of Mr. Sisko", "{Movie TitleThe:17}"),
            "Fantastic Life..."
        );
        assert_eq!(
            title("The Fantastic Life of Mr. Sisko", "{Movie CleanTitle:-13}"),
            "...Mr. Sisko"
        );
        // Ids in their tags, which go with them.
        assert_eq!(title("X", "{Movie Title} {imdb-{ImdbId}}"), "X {imdb-tt0000001}");
        assert_eq!(title("X", "{Movie Title} {tmdb-{TmdbId}}"), "X {tmdb-1}");
    }
}
