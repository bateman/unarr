//! File and folder naming: a port of the originals' `FileNameBuilder`.
//!
//! A naming format is a string with `{Token}` placeholders. Each token may
//! carry a prefix and a suffix inside the braces (`{[Quality Full]}`, kept
//! only when the value is not empty), a separator between its words that
//! replaces spaces in the value (`{Series.Title}`), and a custom format after
//! a colon (`{Episode CleanTitle:90}` truncates, `{episode:00}` pads). Token
//! names are matched ignoring case and punctuation; a token written in lower
//! or upper case lowers or uppers its value.
//!
//! Phase 1 uses it for the naming examples the settings page shows; phase 3
//! will use it to rename files, so it follows the original closely rather
//! than the subset the samples need.

use std::collections::HashMap;
use std::sync::OnceLock;

use fancy_regex::Regex;

use crate::model::{NamingConfig, QualityModel, SeriesType};

/// Windows' limit; the originals apply it everywhere.
const MAX_FILE_NAME_LENGTH: usize = 255;
const MAX_FILE_PATH_LENGTH: usize = 4096;

pub struct NamingSeries<'a> {
    pub title: &'a str,
    pub year: i64,
    pub series_type: SeriesType,
    pub imdb_id: Option<&'a str>,
    pub tvdb_id: i64,
    pub tv_maze_id: i64,
    pub tmdb_id: i64,
}

pub struct NamingEpisode<'a> {
    pub season_number: i64,
    pub episode_number: i64,
    pub title: &'a str,
    /// `YYYY-MM-DD`.
    pub air_date: Option<&'a str>,
    pub absolute_episode_number: Option<i64>,
}

/// Media info already formatted the way the API shows it.
#[derive(Debug, Clone, Default)]
pub struct NamingMediaInfo {
    pub video_codec: String,
    pub video_bit_depth: i64,
    pub audio_codec: String,
    pub audio_channels: f64,
    /// ISO 639-2 codes.
    pub audio_languages: Vec<String>,
    pub subtitles: Vec<String>,
    pub video_dynamic_range: String,
    pub video_dynamic_range_type: String,
}

pub struct NamingFile<'a> {
    pub quality: QualityModel,
    /// The quality definition's title, which the user may have renamed.
    pub quality_title: &'a str,
    pub relative_path: &'a str,
    pub scene_name: Option<&'a str>,
    pub release_group: Option<&'a str>,
    pub release_hash: Option<&'a str>,
    pub media_info: Option<&'a NamingMediaInfo>,
    /// A file not yet in the library (the originals' `Id == 0`).
    pub is_new: bool,
}

pub struct NamingCustomFormat<'a> {
    pub name: &'a str,
    pub include_when_renaming: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum NamingError {
    #[error("{0} episode format cannot be empty")]
    EmptyFormat(&'static str),
}

macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| Regex::new($pattern).expect(stringify!($name)))
        }
    };
}

regex!(
    title_re,
    r"(?i)(?<escaped>\{\{|\}\})|\{(?<prefix>[- ._\[(]*)(?<token>(?:[a-z0-9]+)(?:(?<separator>[- ._]+)(?:[a-z0-9]+))?)(?::(?<customFormat>[ ,a-z0-9+-]+(?<![- ])))?(?<suffix>[- ._)\]]*)\}"
);
regex!(episode_re, r"(?i)(?<episode>\{episode(?::0+)?\})");
regex!(season_re, r"(?i)(?<season>\{season(?::0+)?\})");
regex!(absolute_re, r"(?i)(?<absolute>\{absolute(?::0+)?\})");
regex!(
    season_episode_pattern_re,
    r"(?i)(?<sep1>(?<=\})[- ._]+?)?(?<seasonEpisode>s?\{season(?::0+)?\}(?<episodeSeparator>[- ._]?[ex])(?<episode>\{episode(?::0+)?\}))(?<sep2>[- ._]+?(?=\{))?"
);
regex!(
    absolute_pattern_re,
    r"(?i)(?<sep1>(?<=\})[- ._]+?)?(?<absolute>\{absolute(?::0+)?\})(?<sep2>[- ._]+?(?=\{))?"
);
regex!(air_date_re, r"(?i)\{Air(\s|\W|_)Date\}");
regex!(cleanup_re, r"([- ._])(\1)+");
regex!(trim_separators_re, r"[- ._]+$");
regex!(
    scenify_remove_re,
    r#"(?i)(?<=\s)(,|<|>|\/|\\|;|:|'|"|\||`|’|~|!|\?|@|$|%|^|\*|-|_|=){1}(?=\s)|('|`|’|:|\?|,)(?=(?:(?:s|m|t|ve|ll|d|re)\s)|\s|$)|(\(|\)|\[|\]|\{|\})"#
);
regex!(scenify_replace_re, r"[\/]");
regex!(multi_part_re, r"(?i)(?::?\s?(?:\(\d+\)|(Part|Pt\.?)\s?\d+))$");
regex!(title_prefix_re, r"(?i)^(The|An|A) (.*?)((?: *\([^)]+\))*)$");
regex!(year_re, r"\(\d{4}\)$");
regex!(
    reserved_device_re,
    r"(?i)^(?:aux|com[1-9]|con|lpt[1-9]|nul|prn)\."
);

const BAD_CHARACTERS: [&str; 8] = ["\\", "/", "<", ">", "?", "*", "|", "\""];
const GOOD_CHARACTERS: [&str; 8] = ["+", "+", "", "", "!", "-", "", ""];
const EPISODE_TITLE_TRIM: &[char] = &[' ', '.', '?'];

/// What a `{Token}` occurrence carries.
pub struct TokenMatch {
    pub prefix: String,
    pub separator: String,
    pub suffix: String,
    pub token: String,
    pub custom_format: Option<String>,
}

impl TokenMatch {
    /// The originals' fallback for a missing value: only when the token has
    /// no decoration, so `{-Release Group}` disappears rather than reading
    /// `-Sonarr`.
    pub(crate) fn default_value(&self, default: &str) -> String {
        if self.prefix.is_empty() && self.suffix.is_empty() {
            default.to_string()
        } else {
            String::new()
        }
    }
}

pub(crate) type Handler<'a> = Box<dyn Fn(&TokenMatch) -> Option<String> + 'a>;

/// Token handlers keyed by the simplified token name.
pub(crate) struct Handlers<'a>(HashMap<String, Handler<'a>>);

impl<'a> Handlers<'a> {
    pub(crate) fn new() -> Self {
        Handlers(HashMap::new())
    }

    pub(crate) fn set<F>(&mut self, token: &str, f: F)
    where
        F: Fn(&TokenMatch) -> Option<String> + 'a,
    {
        self.0.insert(simplify(token), Box::new(f));
    }

    pub(crate) fn value(&mut self, token: &str, value: String) {
        self.set(token, move |_| Some(value.clone()));
    }

    pub(crate) fn get(&self, token: &str) -> Option<&Handler<'a>> {
        self.0.get(&simplify(token))
    }
}

/// The originals compare tokens after dropping whitespace, underscores and
/// punctuation and lowering the case.
fn simplify(token: &str) -> String {
    token
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// `value.ToString(format)` for the `0+` formats the regexes allow.
fn format_number(value: i64, format: Option<&str>) -> String {
    match format {
        Some(f) if !f.is_empty() && f.chars().all(|c| c == '0') => {
            format!("{value:0width$}", width = f.len())
        }
        _ => value.to_string(),
    }
}

/// `{season:00}` → `("season", Some("00"))`.
fn split_number_token(token: &str) -> Option<&str> {
    token.trim_matches(['{', '}']).split_once(':').map(|(_, f)| f)
}

fn replace_number_token(token: &str, value: i64) -> String {
    format_number(value, split_number_token(token))
}

pub fn clean_title(title: &str) -> String {
    let title = title.replace('&', "and");
    let title = scenify_replace_re().replace_all(&title, " ");
    scenify_remove_re().replace_all(&title, "").into_owned()
}

pub fn title_the(title: &str) -> String {
    title_prefix_re().replace(title, "$2, $1$3").into_owned()
}

pub fn clean_title_the(title: &str) -> String {
    if let Ok(Some(c)) = title_prefix_re().captures(title) {
        let article = c.get(1).map_or("", |m| m.as_str());
        let rest = c.get(2).map_or("", |m| m.as_str());
        let year = c.get(3).map_or("", |m| m.as_str());
        return format!("{}, {}{}", clean_title(rest).trim(), article, clean_title(year));
    }
    clean_title(title)
}

pub fn title_year(title: &str, year: i64) -> String {
    if year == 0 || year_re().is_match(title).unwrap_or(false) {
        return title.to_string();
    }
    format!("{title} ({year})")
}

pub fn clean_title_the_year(title: &str, year: i64) -> String {
    if year == 0 {
        return clean_title_the(title);
    }
    if let Ok(Some(m)) = year_re().find(title) {
        let before = &title[..m.start()];
        let digits = &m.as_str()[1..5];
        return format!("{} {}", clean_title_the(before.trim()), digits);
    }
    format!("{} {}", clean_title_the(title), year)
}

pub fn title_without_year(title: &str) -> String {
    year_re().replace(title, "").into_owned()
}

pub fn title_first_character(title: &str) -> String {
    let mut chars = title.chars();
    for _ in 0..2 {
        match chars.next() {
            Some(c) if c.is_alphanumeric() => {
                return c
                    .to_uppercase()
                    .next()
                    .map(strip_diacritic)
                    .unwrap_or('_')
                    .to_string()
            }
            Some(_) => continue,
            None => break,
        }
    }
    "_".to_string()
}

/// The common Latin accented capitals, enough for a folder initial.
fn strip_diacritic(c: char) -> char {
    match c {
        'À'..='Å' => 'A',
        'Ç' => 'C',
        'È'..='Ë' => 'E',
        'Ì'..='Ï' => 'I',
        'Ñ' => 'N',
        'Ò'..='Ö' | 'Ø' => 'O',
        'Ù'..='Ü' => 'U',
        'Ý' => 'Y',
        other => other,
    }
}

/// Illegal characters and colons, per the naming settings.
pub fn clean_file_name(name: &str, config: &NamingConfig) -> String {
    clean_file_name_as(
        name,
        config.replace_illegal_characters,
        config.colon_replacement_format,
        &config.custom_colon_replacement_format,
    )
}

/// [`clean_file_name`] for the settings of either application: whether
/// illegal characters are replaced, and the colon's replacement (`0`
/// delete, `1` dash, `2` space dash, `3` space dash space, `4` smart, `5`
/// Sonarr's custom one).
pub(crate) fn clean_file_name_as(
    name: &str,
    replace_illegal_characters: bool,
    colon_replacement_format: i64,
    custom_colon_replacement: &str,
) -> String {
    let mut result = name.to_string();
    if replace_illegal_characters {
        match colon_replacement_format {
            // Smart: a colon followed by a space reads better as " - ".
            4 => {
                result = result.replace(": ", " - ");
                result = result.replace(':', "-");
            }
            other => {
                let replacement = match other {
                    1 => "-",
                    2 => " -",
                    3 => " - ",
                    5 => custom_colon_replacement,
                    _ => "",
                };
                result = result.replace(':', replacement);
            }
        }
    } else {
        result = result.replace(':', "");
    }
    for (bad, good) in BAD_CHARACTERS.iter().zip(GOOD_CHARACTERS) {
        result = result.replace(bad, if replace_illegal_characters { good } else { "" });
    }
    result
        .trim_start_matches([' ', '.'])
        .trim_end_matches(' ')
        .to_string()
}

pub fn clean_folder_name(name: &str) -> String {
    let name = cleanup_re().replace_all(name, "$1");
    name.trim_matches([' ', '.']).to_string()
}

pub(crate) fn replace_reserved_device_names(input: &str) -> String {
    reserved_device_re()
        .replace_all(input, |c: &fancy_regex::Captures<str>| c[0].replace('.', "_"))
        .into_owned()
}

fn max_length_from_formatter(formatter: Option<&str>) -> i64 {
    formatter.and_then(|f| f.trim().parse().ok()).unwrap_or(0)
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `{Token:90}` keeps 90 characters and an ellipsis; `{Token:-90}` keeps the end.
pub(crate) fn truncate(input: &str, formatter: Option<&str>) -> String {
    if is_blank(input) {
        return String::new();
    }
    let max = max_length_from_formatter(formatter);
    let len = input.chars().count() as i64;
    if max == 0 || len <= max.abs() {
        return input.to_string();
    }
    if max < 0 {
        let keep = (max.unsigned_abs() as usize).saturating_sub(3);
        let tail: String = input.chars().rev().take(keep).collect::<String>();
        let tail = tail
            .trim_end_matches([' ', '.'])
            .chars()
            .rev()
            .collect::<String>();
        return format!("{{ellipsis}}{tail}");
    }
    let head = take_chars(input, (max as usize).saturating_sub(3));
    format!("{}{{ellipsis}}", head.trim_end_matches([' ', '.']))
}

/// ISO 639-2 (bibliographic or terminologic) to the upper-cased 639-1 code.
fn two_letter_language(code: &str) -> Option<&'static str> {
    Some(match code.to_ascii_lowercase().as_str() {
        "eng" => "EN",
        "ger" | "deu" => "DE",
        "fre" | "fra" => "FR",
        "spa" => "ES",
        "ita" => "IT",
        "jpn" => "JA",
        "por" => "PT",
        "rus" => "RU",
        "dut" | "nld" => "NL",
        "chi" | "zho" => "ZH",
        "kor" => "KO",
        "swe" => "SV",
        "nor" => "NO",
        "nob" => "NB",
        "dan" => "DA",
        "fin" => "FI",
        "pol" => "PL",
        "tur" => "TR",
        "ara" => "AR",
        "hin" => "HI",
        "heb" => "HE",
        "hun" => "HU",
        "cze" | "ces" => "CS",
        "gre" | "ell" => "EL",
        "rum" | "ron" => "RO",
        "ukr" => "UK",
        "tha" => "TH",
        "vie" => "VI",
        "ind" => "ID",
        "cat" => "CA",
        "bul" => "BG",
        "hrv" => "HR",
        "srp" => "SR",
        "slo" | "slk" => "SK",
        "slv" => "SL",
        "lit" => "LT",
        "lav" => "LV",
        "est" => "ET",
        "ice" | "isl" => "IS",
        "per" | "fas" => "FA",
        "tam" => "TA",
        "tel" => "TE",
        "mal" => "ML",
        "ben" => "BN",
        "urd" => "UR",
        "may" | "msa" => "MS",
        "tgl" | "fil" => "TL",
        "lat" => "LA",
        "baq" | "eus" => "EU",
        "glg" => "GL",
        "wel" | "cym" => "CY",
        "gle" => "GA",
        "mac" | "mkd" => "MK",
        "alb" | "sqi" => "SQ",
        "arm" | "hye" => "HY",
        "geo" | "kat" => "KA",
        "bos" => "BS",
        "afr" => "AF",
        "swa" => "SW",
        "mon" => "MN",
        _ => return None,
    })
}

fn languages_token(codes: &[String], filter: Option<&str>, skip_english_only: bool, quoted: bool) -> String {
    let mut tokens: Vec<String> = codes
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != "und")
        .map(|s| {
            two_letter_language(s)
                .map(str::to_string)
                .unwrap_or_else(|| s.to_string())
        })
        .collect();
    tokens.dedup_by(|a, b| a == b);
    let mut distinct: Vec<String> = Vec::new();
    for t in tokens.drain(..) {
        if !distinct.contains(&t) {
            distinct.push(t);
        }
    }
    let tokens = distinct;
    let mut filtered = tokens.clone();
    if let Some(filter) = filter.filter(|f| !is_blank(f)) {
        if let Some(excluded) = filter.strip_prefix('-') {
            let excluded: Vec<&str> = excluded.split('-').collect();
            filtered.retain(|t| !excluded.contains(&t.as_str()));
        } else {
            filtered = filter
                .split('+')
                .filter(|f| tokens.iter().any(|t| t == f))
                .map(str::to_string)
                .collect();
        }
        if filter.ends_with('+') && filtered.len() != tokens.len() {
            filtered.push("--".to_string());
        }
    }
    if skip_english_only && filtered.len() == 1 && filtered[0] == "EN" {
        return String::new();
    }
    let response = filtered.join("+");
    if quoted && !is_blank(&response) {
        format!("[{response}]")
    } else {
        response
    }
}

fn custom_formats_token(formats: &[NamingCustomFormat], filter: Option<&str>) -> String {
    let names: Vec<&str> = formats
        .iter()
        .filter(|f| f.include_when_renaming)
        .map(|f| f.name)
        .collect();
    let filtered: Vec<&str> = match filter.filter(|f| !is_blank(f)) {
        Some(f) => {
            if let Some(excluded) = f.strip_prefix('-') {
                let excluded: Vec<&str> = excluded.split(',').collect();
                names.into_iter().filter(|n| !excluded.contains(n)).collect()
            } else {
                let wanted: Vec<&str> = f.split(',').collect();
                names.into_iter().filter(|n| wanted.contains(n)).collect()
            }
        }
        None => names,
    };
    filtered.join(" ")
}

fn add_series_tokens<'a>(h: &mut Handlers<'a>, s: &'a NamingSeries<'a>) {
    h.set("{Series Title}", move |m| {
        Some(truncate(s.title, m.custom_format.as_deref()))
    });
    h.set("{Series CleanTitle}", move |m| {
        Some(truncate(&clean_title(s.title), m.custom_format.as_deref()))
    });
    h.set("{Series TitleYear}", move |m| {
        Some(truncate(&title_year(s.title, s.year), m.custom_format.as_deref()))
    });
    h.set("{Series CleanTitleYear}", move |m| {
        Some(truncate(
            &clean_title(&title_year(s.title, s.year)),
            m.custom_format.as_deref(),
        ))
    });
    h.set("{Series TitleWithoutYear}", move |m| {
        Some(truncate(&title_without_year(s.title), m.custom_format.as_deref()))
    });
    h.set("{Series CleanTitleWithoutYear}", move |m| {
        Some(truncate(
            &clean_title(&title_without_year(s.title)),
            m.custom_format.as_deref(),
        ))
    });
    h.set("{Series TitleThe}", move |m| {
        Some(truncate(&title_the(s.title), m.custom_format.as_deref()))
    });
    h.set("{Series CleanTitleThe}", move |m| {
        Some(truncate(&clean_title_the(s.title), m.custom_format.as_deref()))
    });
    h.set("{Series TitleTheYear}", move |m| {
        Some(truncate(
            &title_year(&title_the(s.title), s.year),
            m.custom_format.as_deref(),
        ))
    });
    h.set("{Series CleanTitleTheYear}", move |m| {
        Some(truncate(
            &clean_title_the_year(s.title, s.year),
            m.custom_format.as_deref(),
        ))
    });
    h.set("{Series TitleTheWithoutYear}", move |m| {
        Some(truncate(
            &title_without_year(&title_the(s.title)),
            m.custom_format.as_deref(),
        ))
    });
    h.set("{Series CleanTitleTheWithoutYear}", move |m| {
        Some(truncate(
            &clean_title_the(&title_without_year(s.title)),
            m.custom_format.as_deref(),
        ))
    });
    h.set("{Series TitleFirstCharacter}", move |m| {
        Some(truncate(
            &title_first_character(&title_the(s.title)),
            m.custom_format.as_deref(),
        ))
    });
    h.value("{Series Year}", s.year.to_string());
}

fn add_id_tokens<'a>(h: &mut Handlers<'a>, s: &'a NamingSeries<'a>) {
    h.value("{ImdbId}", s.imdb_id.unwrap_or("").to_string());
    h.value("{TvdbId}", s.tvdb_id.to_string());
    h.value(
        "{TvMazeId}",
        if s.tv_maze_id > 0 {
            s.tv_maze_id.to_string()
        } else {
            String::new()
        },
    );
    h.value(
        "{TmdbId}",
        if s.tmdb_id > 0 {
            s.tmdb_id.to_string()
        } else {
            String::new()
        },
    );
}

fn add_season_tokens(h: &mut Handlers<'_>, season_number: i64) {
    h.set("{Season}", move |m| {
        Some(format_number(season_number, m.custom_format.as_deref()))
    });
}

fn add_episode_tokens<'a>(h: &mut Handlers<'a>, episodes: &'a [NamingEpisode<'a>]) {
    match episodes[0].air_date.filter(|d| !is_blank(d)) {
        Some(d) => h.value("{Air Date}", d.replace('-', " ")),
        None => h.value("{Air Date}", "Unknown".to_string()),
    }
}

fn add_episode_title_placeholder_tokens(h: &mut Handlers<'_>) {
    h.set("{Episode Title}", |_| None);
    h.set("{Episode CleanTitle}", |_| None);
}

fn episode_titles(episodes: &[NamingEpisode<'_>]) -> Vec<String> {
    if episodes.len() == 1 {
        return vec![episodes[0].title.trim_end_matches(EPISODE_TITLE_TRIM).to_string()];
    }
    let mut titles: Vec<String> = Vec::new();
    for e in episodes {
        let t = multi_part_re()
            .replace(e.title.trim_end_matches(EPISODE_TITLE_TRIM), "")
            .trim()
            .to_string();
        if !titles.contains(&t) {
            titles.push(t);
        }
    }
    if titles.iter().all(|t| is_blank(t)) {
        titles.clear();
        for e in episodes {
            let t = e.title.trim_end_matches(EPISODE_TITLE_TRIM).to_string();
            if !titles.contains(&t) {
                titles.push(t);
            }
        }
    }
    titles
}

fn episode_title(titles: &[String], separator: &str, max_length: i64, formatter: Option<&str>) -> String {
    let mut max_length = max_length;
    let from_formatter = max_length_from_formatter(formatter);
    if from_formatter > 0 {
        max_length = max_length.min(from_formatter);
    }
    let separator = format!(" {} ", separator.trim());
    let joined = titles.join(&separator);
    if joined.len() as i64 <= max_length {
        return joined;
    }
    let first = titles[0].as_str();
    let first_len = first.len() as i64;
    if titles.len() >= 2 {
        let last = titles[titles.len() - 1].as_str();
        if first_len + last.len() as i64 + 3 <= max_length {
            return format!("{}{{ellipsis}}{}", first.trim_end_matches([' ', '.']), last);
        }
    }
    if titles.len() > 1 && first_len + 3 <= max_length {
        return format!("{}{{ellipsis}}", first.trim_end_matches([' ', '.']));
    }
    if titles.len() == 1 && first_len <= max_length {
        return first.to_string();
    }
    let keep = (max_length - 3).max(0) as usize;
    format!(
        "{}{{ellipsis}}",
        take_chars(first, keep).trim_end_matches([' ', '.'])
    )
}

fn add_episode_title_tokens<'a>(h: &mut Handlers<'a>, episodes: &'a [NamingEpisode<'a>], max_length: i64) {
    h.set("{Episode Title}", move |m| {
        Some(episode_title(
            &episode_titles(episodes),
            "+",
            max_length,
            m.custom_format.as_deref(),
        ))
    });
    h.set("{Episode CleanTitle}", move |m| {
        let titles: Vec<String> = episode_titles(episodes).iter().map(|t| clean_title(t)).collect();
        Some(episode_title(
            &titles,
            "and",
            max_length,
            m.custom_format.as_deref(),
        ))
    });
}

pub(crate) fn file_stem(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}

fn original_file_name(file: &NamingFile<'_>, use_current_as_fallback: bool) -> String {
    if !use_current_as_fallback {
        return String::new();
    }
    file_stem(file.relative_path).to_string()
}

fn original_title(file: &NamingFile<'_>, use_current_as_fallback: bool) -> String {
    let default = NamingConfig::default();
    match file.scene_name.filter(|s| !is_blank(s)) {
        None => clean_file_name(&original_file_name(file, use_current_as_fallback), &default),
        Some(scene) => clean_file_name(scene, &default),
    }
}

fn add_episode_file_tokens<'a>(
    h: &mut Handlers<'a>,
    file: &'a NamingFile<'a>,
    use_current_as_fallback: bool,
) {
    h.set("{Original Title}", move |_| {
        Some(original_title(file, use_current_as_fallback))
    });
    h.set("{Original Filename}", move |_| {
        Some(original_file_name(file, use_current_as_fallback))
    });
    h.set("{Release Group}", move |m| {
        match file.release_group.filter(|g| !is_blank(g)) {
            None => Some(m.default_value("Sonarr")),
            Some(g) => Some(truncate(g, m.custom_format.as_deref())),
        }
    });
    h.value("{Release Hash}", file.release_hash.unwrap_or("").to_string());
}

fn quality_proper(series: &NamingSeries<'_>, quality: QualityModel) -> String {
    if quality.version > 1 {
        if series.series_type == SeriesType::Anime {
            return format!("v{}", quality.version);
        }
        return "Proper".to_string();
    }
    String::new()
}

pub(crate) fn quality_real(quality: QualityModel) -> &'static str {
    if quality.real > 0 {
        "REAL"
    } else {
        ""
    }
}

fn add_quality_tokens<'a>(h: &mut Handlers<'a>, series: &'a NamingSeries<'a>, file: &'a NamingFile<'a>) {
    let proper = quality_proper(series, file.quality);
    let real = quality_real(file.quality);
    h.value(
        "{Quality Full}",
        format!("{} {} {}", file.quality_title, proper, real),
    );
    h.value("{Quality Title}", file.quality_title.to_string());
    h.value("{Quality Proper}", proper);
    h.value("{Quality Real}", real.to_string());
}

pub(crate) fn add_media_info_tokens<'a>(h: &mut Handlers<'a>, media_info: Option<&'a NamingMediaInfo>) {
    let Some(mi) = media_info else {
        return;
    };
    let video_bit_depth = if mi.video_bit_depth > 0 {
        mi.video_bit_depth
    } else {
        8
    }
    .to_string();
    let channels = if mi.audio_channels > 0.0 {
        format!("{:.1}", mi.audio_channels)
    } else {
        String::new()
    };
    h.value("{MediaInfo Video}", mi.video_codec.clone());
    h.value("{MediaInfo VideoCodec}", mi.video_codec.clone());
    h.value("{MediaInfo VideoBitDepth}", video_bit_depth);
    h.value("{MediaInfo Audio}", mi.audio_codec.clone());
    h.value("{MediaInfo AudioCodec}", mi.audio_codec.clone());
    h.value("{MediaInfo AudioChannels}", channels);
    h.set("{MediaInfo AudioLanguages}", move |m| {
        Some(languages_token(
            &mi.audio_languages,
            m.custom_format.as_deref(),
            true,
            true,
        ))
    });
    h.set("{MediaInfo AudioLanguagesAll}", move |m| {
        Some(languages_token(
            &mi.audio_languages,
            m.custom_format.as_deref(),
            false,
            true,
        ))
    });
    h.set("{MediaInfo SubtitleLanguages}", move |m| {
        Some(languages_token(
            &mi.subtitles,
            m.custom_format.as_deref(),
            false,
            true,
        ))
    });
    h.set("{MediaInfo SubtitleLanguagesAll}", move |m| {
        Some(languages_token(
            &mi.subtitles,
            m.custom_format.as_deref(),
            false,
            true,
        ))
    });
    h.value(
        "{MediaInfo Simple}",
        format!("{} {}", mi.video_codec, mi.audio_codec),
    );
    h.set("{MediaInfo Full}", move |m| {
        Some(format!(
            "{} {}{} {}",
            mi.video_codec,
            mi.audio_codec,
            languages_token(&mi.audio_languages, m.custom_format.as_deref(), true, true),
            languages_token(&mi.subtitles, m.custom_format.as_deref(), false, true)
        ))
    });
    h.value("{MediaInfo VideoDynamicRange}", mi.video_dynamic_range.clone());
    h.value(
        "{MediaInfo VideoDynamicRangeType}",
        mi.video_dynamic_range_type.clone(),
    );
}

pub(crate) fn add_custom_format_tokens<'a>(h: &mut Handlers<'a>, formats: &'a [NamingCustomFormat<'a>]) {
    h.set("{Custom Formats}", move |m| {
        Some(custom_formats_token(formats, m.custom_format.as_deref()))
    });
    h.set("{Custom Format}", move |m| {
        let Some(name) = m.custom_format.as_deref().filter(|f| !is_blank(f)) else {
            return Some(String::new());
        };
        Some(
            formats
                .iter()
                .find(|f| f.include_when_renaming && f.name == name)
                .map(|f| f.name.to_string())
                .unwrap_or_default(),
        )
    });
}

struct EpisodeFormat {
    episode_separator: String,
    separator: String,
    episode_pattern: String,
    season_episode_pattern: String,
}

fn episode_formats(pattern: &str) -> Vec<EpisodeFormat> {
    let mut out: Vec<EpisodeFormat> = Vec::new();
    for c in season_episode_pattern_re().captures_iter(pattern).flatten() {
        let group = |name: &str| c.name(name).map(|m| m.as_str().to_string()).unwrap_or_default();
        let season_episode = group("seasonEpisode");
        if out.iter().any(|f| f.season_episode_pattern == season_episode) {
            continue;
        }
        // .NET keeps the last capture of a group that appears twice.
        let separator = if c.name("sep2").is_some() {
            group("sep2")
        } else {
            group("sep1")
        };
        out.push(EpisodeFormat {
            episode_separator: group("episodeSeparator"),
            separator,
            episode_pattern: group("episode"),
            season_episode_pattern: season_episode,
        });
    }
    out
}

struct AbsoluteFormat {
    separator: String,
    absolute_pattern: String,
}

fn absolute_formats(pattern: &str) -> Vec<AbsoluteFormat> {
    let mut out: Vec<AbsoluteFormat> = Vec::new();
    for c in absolute_pattern_re().captures_iter(pattern).flatten() {
        let group = |name: &str| c.name(name).map(|m| m.as_str().to_string()).unwrap_or_default();
        let absolute = group("absolute");
        if out.iter().any(|f| f.absolute_pattern == absolute) {
            continue;
        }
        let separator = if c.name("sep2").is_some() {
            group("sep2")
        } else {
            group("sep1")
        };
        out.push(AbsoluteFormat {
            separator: if is_blank(&separator) {
                "-".to_string()
            } else {
                separator
            },
            absolute_pattern: absolute,
        });
    }
    out
}

fn replace_season_tokens(pattern: &str, season_number: i64) -> String {
    season_re()
        .replace_all(pattern, |c: &fancy_regex::Captures<str>| {
            replace_number_token(&c["season"], season_number)
        })
        .into_owned()
}

fn format_number_tokens(base: &str, format_pattern: &str, episodes: &[&NamingEpisode<'_>]) -> String {
    let mut pattern = String::new();
    for (i, e) in episodes.iter().enumerate() {
        let source = if i == 0 { base } else { format_pattern };
        pattern.push_str(
            &episode_re().replace_all(source, |c: &fancy_regex::Captures<str>| {
                replace_number_token(&c["episode"], e.episode_number)
            }),
        );
    }
    replace_season_tokens(&pattern, episodes[0].season_number)
}

fn format_absolute_number_tokens(
    base: &str,
    format_pattern: &str,
    episodes: &[&NamingEpisode<'_>],
) -> String {
    let mut pattern = String::new();
    for (i, e) in episodes.iter().enumerate() {
        let source = if i == 0 { base } else { format_pattern };
        let number = e.absolute_episode_number.unwrap_or(0);
        pattern.push_str(
            &absolute_re().replace_all(source, |c: &fancy_regex::Captures<str>| {
                replace_number_token(&c["absolute"], number)
            }),
        );
    }
    replace_season_tokens(&pattern, episodes[0].season_number)
}

fn first_and_last<'e, 'a>(episodes: &'e [NamingEpisode<'a>]) -> Vec<&'e NamingEpisode<'a>> {
    let mut eps = vec![&episodes[0]];
    if episodes.len() > 1 {
        eps.push(&episodes[episodes.len() - 1]);
    }
    eps
}

fn add_season_episode_numbering_tokens<'a>(
    pattern: &str,
    h: &mut Handlers<'a>,
    episodes: &'a [NamingEpisode<'a>],
    config: &NamingConfig,
) -> String {
    let mut pattern = pattern.to_string();
    let all: Vec<&NamingEpisode> = episodes.iter().collect();
    for (index, f) in episode_formats(&pattern).iter().enumerate() {
        let rendered = match config.multi_episode_style {
            1 => format_number_tokens(
                &f.season_episode_pattern,
                &format!("{}{}", f.separator, f.season_episode_pattern),
                &all,
            ),
            2 => format_number_tokens(
                &f.season_episode_pattern,
                &format!("{}{}", f.episode_separator, f.episode_pattern),
                &all,
            ),
            3 => format_number_tokens(
                &f.season_episode_pattern,
                &format!("-{}{}", f.episode_separator, f.episode_pattern),
                &all,
            ),
            4 => format_number_tokens(
                &f.season_episode_pattern,
                &format!("-{}", f.episode_pattern),
                &first_and_last(episodes),
            ),
            5 => format_number_tokens(
                &f.season_episode_pattern,
                &format!("-{}{}", f.episode_separator, f.episode_pattern),
                &first_and_last(episodes),
            ),
            _ => format_number_tokens(
                &f.season_episode_pattern,
                &format!("-{}", f.episode_pattern),
                &all,
            ),
        };
        let token = format!("{{Season Episode{}}}", index + 1);
        pattern = pattern.replace(&f.season_episode_pattern, &token);
        h.value(&token, rendered);
    }
    add_season_tokens(h, episodes[0].season_number);
    if episodes.len() > 1 {
        h.set("{Episode}", move |m| {
            Some(format!(
                "{}-{}",
                format_number(episodes[0].episode_number, m.custom_format.as_deref()),
                format_number(
                    episodes[episodes.len() - 1].episode_number,
                    m.custom_format.as_deref()
                )
            ))
        });
    } else {
        h.set("{Episode}", move |m| {
            Some(format_number(
                episodes[0].episode_number,
                m.custom_format.as_deref(),
            ))
        });
    }
    pattern
}

fn add_absolute_numbering_tokens<'a>(
    pattern: &str,
    h: &mut Handlers<'a>,
    series: &NamingSeries<'_>,
    episodes: &'a [NamingEpisode<'a>],
    config: &NamingConfig,
) -> String {
    let mut pattern = pattern.to_string();
    let all: Vec<&NamingEpisode> = episodes.iter().collect();
    for (index, f) in absolute_formats(&pattern).iter().enumerate() {
        if series.series_type != SeriesType::Anime
            || episodes.iter().any(|e| e.absolute_episode_number.is_none())
        {
            pattern = pattern.replace(&f.absolute_pattern, "");
            continue;
        }
        let rendered = match config.multi_episode_style {
            1 => format_absolute_number_tokens(
                &f.absolute_pattern,
                &format!("{}{}", f.separator, f.absolute_pattern),
                &all,
            ),
            2 => {
                let sep = if is_blank(f.separator.trim()) {
                    " "
                } else {
                    f.separator.trim()
                };
                format_absolute_number_tokens(
                    &f.absolute_pattern,
                    &format!("{}{}", sep, f.absolute_pattern),
                    &all,
                )
            }
            3 => {
                format_absolute_number_tokens(&f.absolute_pattern, &format!("-{}", f.absolute_pattern), &all)
            }
            4 | 5 => format_absolute_number_tokens(
                &f.absolute_pattern,
                &format!("-{}", f.absolute_pattern),
                &first_and_last(episodes),
            ),
            _ => {
                format_absolute_number_tokens(&f.absolute_pattern, &format!("-{}", f.absolute_pattern), &all)
            }
        };
        let token = format!("{{Absolute Pattern{}}}", index + 1);
        pattern = pattern.replace(&f.absolute_pattern, &token);
        h.value(&token, rendered);
    }
    pattern
}

fn pattern_has_episode_identifier(pattern: &str) -> bool {
    season_episode_pattern_re().is_match(pattern).unwrap_or(false)
        || absolute_pattern_re().is_match(pattern).unwrap_or(false)
        || air_date_re().is_match(pattern).unwrap_or(false)
}

fn requires_absolute_episode_number(anime_format: &str) -> bool {
    absolute_re().is_match(anime_format).unwrap_or(false)
}

/// `RequiresAbsoluteEpisodeNumber`: whether the anime format names the
/// absolute number, so an anime episode without one cannot be imported.
pub fn config_requires_absolute_episode_number(config: &NamingConfig) -> bool {
    requires_absolute_episode_number(&config.anime_episode_format)
}

/// `FileNameBuilderTokenEqualityComparer`: tokens compare without
/// separators, braces or case.
fn simplify_token(token: &str) -> String {
    token
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// `RequiresEpisodeTitle`: whether the format the series uses names the
/// episode title, so an episode without a title (TBA) waits for one.
/// `all_have_absolute` says whether every episode has an absolute number,
/// which is when an anime series takes the anime format.
pub fn requires_episode_title(
    series_type: SeriesType,
    all_have_absolute: bool,
    config: &NamingConfig,
) -> bool {
    if !config.rename_episodes {
        return false;
    }
    let mut pattern = config.standard_episode_format.as_str();
    if series_type == SeriesType::Daily {
        pattern = &config.daily_episode_format;
    }
    if series_type == SeriesType::Anime && all_have_absolute {
        pattern = &config.anime_episode_format;
    }
    title_re()
        .captures_iter(pattern)
        .filter_map(Result::ok)
        .filter_map(|c| c.name("token").map(|m| m.as_str().to_string()))
        .any(|token| {
            let t = simplify_token(&token);
            t == simplify_token("{Episode Title}") || t == simplify_token("{Episode CleanTitle}")
        })
}

fn replace_token(
    c: &fancy_regex::Captures<str>,
    h: &Handlers<'_>,
    config: &NamingConfig,
    escape: bool,
) -> String {
    if let Some(escaped) = c.name("escaped") {
        if escape {
            return escaped.as_str().to_string();
        }
        return match escaped.as_str() {
            "{{" => "{".to_string(),
            _ => "}".to_string(),
        };
    }
    let group = |name: &str| c.name(name).map(|m| m.as_str().to_string()).unwrap_or_default();
    let m = TokenMatch {
        prefix: group("prefix"),
        separator: group("separator"),
        suffix: group("suffix"),
        token: group("token"),
        custom_format: Some(group("customFormat")).filter(|f| !is_blank(f)),
    };
    let replacement = match h.get(&m.token) {
        Some(handler) => handler(&m),
        None => Some(String::new()),
    };
    // A handler that answers nothing keeps the token for a later pass.
    let Some(replacement) = replacement else {
        return c[0].to_string();
    };
    let mut text = replacement.trim().to_string();
    let letters: Vec<char> = m.token.chars().filter(|c| c.is_alphabetic()).collect();
    if !letters.is_empty() {
        if letters.iter().all(|c| c.is_lowercase()) {
            text = text.to_lowercase();
        } else if letters.iter().all(|c| c.is_uppercase()) {
            text = text.to_uppercase();
        }
    }
    if !is_blank(&m.separator) {
        text = text.replace(' ', &m.separator);
    }
    text = clean_file_name(&text, config);
    if !is_blank(&text) {
        text = format!("{}{}{}", m.prefix, text, m.suffix);
    }
    if escape {
        text = text.replace('{', "{{").replace('}', "}}");
    }
    text
}

fn replace_tokens(pattern: &str, h: &Handlers<'_>, config: &NamingConfig, escape: bool) -> String {
    title_re()
        .replace_all(pattern, |c: &fancy_regex::Captures<str>| {
            replace_token(c, h, config, escape)
        })
        .into_owned()
}

fn length_without_episode_title(pattern: &str, config: &NamingConfig) -> i64 {
    let mut h = Handlers::new();
    h.value("{Episode Title}", String::new());
    h.value("{Episode CleanTitle}", String::new());
    h.value("{ellipsis}", "...".to_string());
    replace_tokens(pattern, &h, config, false).len() as i64
}

/// The file name (without directory) for `episodes` of `series`, from `file`.
/// Returns the original title when renaming is off.
pub fn build_file_name(
    episodes: &[NamingEpisode<'_>],
    series: &NamingSeries<'_>,
    file: &NamingFile<'_>,
    extension: &str,
    config: &NamingConfig,
    custom_formats: &[NamingCustomFormat<'_>],
) -> Result<String, NamingError> {
    build(
        episodes,
        series,
        file,
        extension,
        MAX_FILE_PATH_LENGTH,
        config,
        custom_formats,
    )
}

fn build(
    episodes: &[NamingEpisode<'_>],
    series: &NamingSeries<'_>,
    file: &NamingFile<'_>,
    extension: &str,
    max_path: usize,
    config: &NamingConfig,
    custom_formats: &[NamingCustomFormat<'_>],
) -> Result<String, NamingError> {
    if !config.rename_episodes {
        return Ok(original_title(file, true) + extension);
    }
    if is_blank(&config.standard_episode_format) && series.series_type == SeriesType::Standard {
        return Err(NamingError::EmptyFormat("Standard"));
    }
    if is_blank(&config.daily_episode_format) && series.series_type == SeriesType::Daily {
        return Err(NamingError::EmptyFormat("Daily"));
    }
    if is_blank(&config.anime_episode_format) && series.series_type == SeriesType::Anime {
        return Err(NamingError::EmptyFormat("Anime"));
    }
    let mut sorted: Vec<&NamingEpisode> = episodes.iter().collect();
    sorted.sort_by_key(|e| (e.season_number, e.episode_number));
    let episodes: Vec<NamingEpisode> = sorted
        .iter()
        .map(|e| NamingEpisode {
            season_number: e.season_number,
            episode_number: e.episode_number,
            title: e.title,
            air_date: e.air_date,
            absolute_episode_number: e.absolute_episode_number,
        })
        .collect();
    let episodes = episodes.as_slice();

    let mut pattern = config.standard_episode_format.as_str();
    if series.series_type == SeriesType::Daily && episodes[0].season_number > 0 {
        pattern = &config.daily_episode_format;
    }
    if series.series_type == SeriesType::Anime
        && (episodes.iter().all(|e| e.absolute_episode_number.is_some())
            || !requires_absolute_episode_number(&config.anime_episode_format))
    {
        pattern = &config.anime_episode_format;
    }

    let split: Vec<&str> = pattern.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    let mut components = Vec::new();
    for (i, split_pattern) in split.iter().enumerate() {
        let mut h = Handlers::new();
        let has_identifier = pattern_has_episode_identifier(split_pattern);
        let p = add_season_episode_numbering_tokens(split_pattern, &mut h, episodes, config);
        let p = add_absolute_numbering_tokens(&p, &mut h, series, episodes, config);
        let p = p.replace("...", "{{ellipsis}}");

        add_series_tokens(&mut h, series);
        add_id_tokens(&mut h, series);
        add_episode_tokens(&mut h, episodes);
        add_episode_title_placeholder_tokens(&mut h);
        add_episode_file_tokens(&mut h, file, !has_identifier || file.is_new);
        add_quality_tokens(&mut h, series, file);
        add_media_info_tokens(&mut h, file.media_info);
        add_custom_format_tokens(&mut h, custom_formats);

        let component = replace_tokens(&p, &h, config, true).trim().to_string();
        let mut max_segment = MAX_FILE_NAME_LENGTH.min(max_path) as i64;
        if i == split.len() - 1 {
            max_segment -= extension.len() as i64;
        }
        let max_title = max_segment - length_without_episode_title(&component, config);
        add_episode_title_tokens(&mut h, episodes, max_title);
        let component = replace_tokens(&component, &h, config, false).trim().to_string();

        let component = cleanup_re().replace_all(&component, "$1").into_owned();
        let component = trim_separators_re().replace_all(&component, "").into_owned();
        let component = component.replace("{ellipsis}", "...");
        components.push(replace_reserved_device_names(&component));
    }
    Ok(components.join("/") + extension)
}

/// The series folder name.
pub fn series_folder(series: &NamingSeries<'_>, config: &NamingConfig) -> String {
    let mut h = Handlers::new();
    add_series_tokens(&mut h, series);
    add_id_tokens(&mut h, series);
    let name = replace_tokens(&config.series_folder_format, &h, config, false);
    let name = clean_folder_name(&name);
    replace_reserved_device_names(&name).replace("{ellipsis}", "...")
}

/// The season folder name; season 0 uses the specials format.
pub fn season_folder(series: &NamingSeries<'_>, season_number: i64, config: &NamingConfig) -> String {
    let mut h = Handlers::new();
    add_series_tokens(&mut h, series);
    add_id_tokens(&mut h, series);
    add_season_tokens(&mut h, season_number);
    let format = if season_number == 0 {
        &config.specials_folder_format
    } else {
        &config.season_folder_format
    };
    let name = replace_tokens(format, &h, config, false);
    let name = clean_folder_name(&name);
    replace_reserved_device_names(&name).replace("{ellipsis}", "...")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series() -> NamingSeries<'static> {
        NamingSeries {
            title: "The Series Title's!",
            year: 2010,
            series_type: SeriesType::Standard,
            imdb_id: Some("tt12345"),
            tvdb_id: 12345,
            tv_maze_id: 54321,
            tmdb_id: 11223,
        }
    }

    fn episode(n: i64, title: &'static str) -> NamingEpisode<'static> {
        NamingEpisode {
            season_number: 1,
            episode_number: n,
            title,
            air_date: Some("2013-10-30"),
            absolute_episode_number: Some(n),
        }
    }

    fn file() -> NamingFile<'static> {
        NamingFile {
            quality: QualityModel {
                quality_id: 3,
                version: 2,
                real: 0,
                is_repack: false,
            },
            quality_title: "WEBDL-1080p",
            relative_path: "The.Series.Title's!.S01E01.1080p.WEBDL.x264-EVOLVE.mkv",
            scene_name: Some("The.Series.Title's!.S01E01.1080p.WEBDL.x264-EVOLVE"),
            release_group: Some("RlsGrp"),
            release_hash: None,
            media_info: None,
            is_new: true,
        }
    }

    #[test]
    fn default_standard_format() {
        let config = NamingConfig {
            rename_episodes: true,
            ..NamingConfig::default()
        };
        let name = build_file_name(
            &[episode(1, "Episode Title (1)")],
            &series(),
            &file(),
            "",
            &config,
            &[],
        )
        .unwrap();
        assert_eq!(
            name,
            "The Series Title's! - S01E01 - Episode Title (1) WEBDL-1080p Proper"
        );
    }

    #[test]
    fn renaming_off_keeps_the_original_title() {
        let config = NamingConfig::default();
        let name = build_file_name(&[episode(1, "Pilot")], &series(), &file(), ".mkv", &config, &[]).unwrap();
        assert_eq!(name, "The.Series.Title's!.S01E01.1080p.WEBDL.x264-EVOLVE.mkv");
    }

    #[test]
    fn multi_episode_prefixed_range_and_clean_title() {
        let config = NamingConfig {
            rename_episodes: true,
            standard_episode_format: "{Series CleanTitleWithoutYear} {(Series Year)} - S{season:00}E{episode:00} - {Episode CleanTitle:90} {[Quality Full]}{-Release Group}".into(),
            ..NamingConfig::default()
        };
        let eps = [
            episode(1, "Episode Title (1)"),
            episode(2, "Episode Title (2)"),
            episode(3, "Episode Title (3)"),
        ];
        let name = build_file_name(&eps, &series(), &file(), "", &config, &[]).unwrap();
        // CleanTitle keeps the apostrophe: the originals only drop one before a space.
        assert_eq!(
            name,
            "The Series Title's! (2010) - S01E01-E03 - Episode Title [WEBDL-1080p Proper]-RlsGrp"
        );
    }

    #[test]
    fn decorated_tokens_vanish_when_empty() {
        let config = NamingConfig {
            rename_episodes: true,
            standard_episode_format:
                "{Series Title} {[Custom Formats]}{[MediaInfo VideoCodec]} {Episode Title}".into(),
            ..NamingConfig::default()
        };
        let name = build_file_name(&[episode(1, "Pilot")], &series(), &file(), ".mkv", &config, &[]).unwrap();
        assert_eq!(name, "The Series Title's! Pilot.mkv");
    }

    /// Expectations recorded from the real Sonarr for every multi-episode style.
    #[test]
    fn multi_episode_styles_match_the_originals() {
        let eps = [
            episode(1, "Episode Title (1)"),
            episode(2, "Episode Title (2)"),
            episode(3, "Episode Title (3)"),
        ];
        for (style, expected, anime) in [
            (0, "S01E01-02-03", "001-002-003"),
            (1, "S01E01 - S01E02 - S01E03", "001 - 002 - 003"),
            (2, "S01E01E02E03", "001-002-003"),
            (3, "S01E01-E02-E03", "001-002-003"),
            (4, "S01E01-03", "001-003"),
            (5, "S01E01-E03", "001-003"),
        ] {
            let config = NamingConfig {
                rename_episodes: true,
                multi_episode_style: style,
                anime_episode_format: "{Series Title} - S{season:00}E{episode:00} - {absolute:000} - {Episode Title} {Quality Full}".into(),
                ..NamingConfig::default()
            };
            let name = build_file_name(&eps, &series(), &file(), "", &config, &[]).unwrap();
            assert_eq!(
                name,
                format!("The Series Title's! - {expected} - Episode Title WEBDL-1080p Proper"),
                "style {style}"
            );
            let anime_series = NamingSeries {
                series_type: SeriesType::Anime,
                ..series()
            };
            let name = build_file_name(&eps, &anime_series, &file(), "", &config, &[]).unwrap();
            assert_eq!(
                name,
                format!("The Series Title's! - {expected} - {anime} - Episode Title WEBDL-1080p v2"),
                "anime style {style}"
            );
        }
        let config = NamingConfig {
            rename_episodes: true,
            multi_episode_style: 1,
            standard_episode_format: "{Series.Title}.S{season:00}E{episode:00}.{Episode.Title}".into(),
            ..NamingConfig::default()
        };
        let name = build_file_name(&eps, &series(), &file(), "", &config, &[]).unwrap();
        assert_eq!(name, "The.Series.Title's!.S01E01.S01E02.S01E03.Episode.Title");
    }

    /// `should_format_languagecodes_properly`, Sonarr's cases: Norwegian
    /// is not Bokmal.
    #[test]
    fn language_codes_read_as_the_original_reads_them() {
        for (code, expected) in [
            ("nob", "[NB]"),
            ("nor", "[NO]"),
            ("swe", "[SV]"),
            ("zho", "[ZH]"),
            ("chi", "[ZH]"),
            ("fre", "[FR]"),
            ("rum", "[RO]"),
            ("per", "[FA]"),
            ("ger", "[DE]"),
            ("cze", "[CS]"),
            ("ice", "[IS]"),
            ("dut", "[NL]"),
        ] {
            assert_eq!(
                languages_token(&[code.to_string()], None, false, true),
                expected,
                "{code}"
            );
        }
    }

    #[test]
    fn daily_and_media_info_tokens() {
        let config = NamingConfig {
            rename_episodes: true,
            daily_episode_format: "{Series Title} - {Air-Date} - {Episode Title} {Quality Full}".into(),
            standard_episode_format: "{Series Title} {MediaInfo VideoCodec}".into(),
            ..NamingConfig::default()
        };
        let daily = NamingSeries {
            series_type: SeriesType::Daily,
            ..series()
        };
        let name = build_file_name(
            &[episode(1, "Episode Title (1)")],
            &daily,
            &file(),
            "",
            &config,
            &[],
        )
        .unwrap();
        assert_eq!(
            name,
            "The Series Title's! - 2013-10-30 - Episode Title (1) WEBDL-1080p Proper"
        );
        // No media info: the token vanishes.
        let name = build_file_name(&[episode(1, "Pilot")], &series(), &file(), "", &config, &[]).unwrap();
        assert_eq!(name, "The Series Title's!");
    }

    #[test]
    fn folders() {
        let config = NamingConfig::default();
        assert_eq!(series_folder(&series(), &config), "The Series Title's!");
        assert_eq!(season_folder(&series(), 1, &config), "Season 1");
        assert_eq!(season_folder(&series(), 0, &config), "Specials");
        let config = NamingConfig {
            series_folder_format: "{Series TitleYear}".into(),
            season_folder_format: "S{season:00}".into(),
            specials_folder_format: "Specials {Series Title}".into(),
            ..NamingConfig::default()
        };
        assert_eq!(series_folder(&series(), &config), "The Series Title's! (2010)");
        assert_eq!(season_folder(&series(), 1, &config), "S01");
        assert_eq!(
            season_folder(&series(), 0, &config),
            "Specials The Series Title's!"
        );
    }

    /// Outputs recorded from the real Sonarr for the sample series.
    #[test]
    fn tokens_match_the_originals() {
        let eps = [
            episode(1, "Episode Title (1)"),
            episode(2, "Episode Title (2)"),
            episode(3, "Episode Title (3)"),
        ];
        let media = NamingMediaInfo {
            video_codec: "AVC".into(),
            video_bit_depth: 10,
            audio_codec: "DTS".into(),
            audio_channels: 5.1,
            audio_languages: vec!["ger".into()],
            subtitles: vec!["eng".into(), "ger".into()],
            ..NamingMediaInfo::default()
        };
        let with_media = NamingFile {
            media_info: Some(&media),
            ..file()
        };
        let formats = [
            NamingCustomFormat {
                name: "Surround Sound",
                include_when_renaming: true,
            },
            NamingCustomFormat {
                name: "x264",
                include_when_renaming: true,
            },
        ];
        for (format, single, multi) in [
            (
                "S{season:00}E{episode:00} {Series CleanTitle}|{Series TitleThe}|{Series CleanTitleThe}|{Series TitleFirstCharacter}|{Series CleanTitleYear}|{Series CleanTitleTheYear}|{Series TitleTheYear}|{Series CleanTitleTheWithoutYear}",
                "S01E01 The Series Title's!|Series Title's!, The|Series Title's!, The|S|The Series Title's! 2010|Series Title's!, The 2010|Series Title's!, The (2010)|Series Title's!, The",
                "S01E01-E03 The Series Title's!|Series Title's!, The|Series Title's!, The|S|The Series Title's! 2010|Series Title's!, The 2010|Series Title's!, The (2010)|Series Title's!, The",
            ),
            (
                "S{season:00}E{episode:00} {Episode CleanTitle}|{Episode Title:10}|{Episode Title:-10}|{Release Group:3}|{Series Title:5}|{Series Title:-5}",
                "S01E01 Episode Title 1|Episode...|Episode Title (1)|...|Th...|...s!",
                "S01E01-E03 Episode Title|Episode...|Episode Title|...|Th...|...s!",
            ),
            (
                "S{season:00}E{episode:00} {MediaInfo Full} | {MediaInfo AudioLanguages} | {MediaInfo AudioLanguagesAll} | {MediaInfo SubtitleLanguages} | {MediaInfo Simple} | {MediaInfo VideoBitDepth} | {MediaInfo AudioChannels} | {MediaInfo AudioLanguages:DE+} | {MediaInfo SubtitleLanguages:-DE} | {MediaInfo VideoDynamicRange}|{MediaInfo VideoDynamicRangeType}|{MediaInfo Video}|{MediaInfo Audio}",
                "S01E01 AVC DTS[DE] [EN+DE] | [DE] | [DE] | [EN+DE] | AVC DTS | 10 | 5.1 | [DE] | [EN] | ||AVC|DTS",
                "S01E01-E03 AVC DTS[DE] [EN+DE] | [DE] | [DE] | [EN+DE] | AVC DTS | 10 | 5.1 | [DE] | [EN] | ||AVC|DTS",
            ),
            (
                "S{season:00}E{episode:00} {Custom Formats:x264}|{Custom Formats:-x264}|{Custom Format:Surround Sound}|{Quality Title}|{Quality Proper}|{Quality Real}|{Release Hash}|{Original Title}|{Original Filename}|{ImdbId}|{TvdbId}|{TvMazeId}|{TmdbId}|{Series Year}|{Air Date}",
                "S01E01 x264|Surround Sound|Surround Sound|WEBDL-1080p|Proper|||The.Series.Title's!.S01E01.1080p.WEBDL.x264-EVOLVE|The.Series.Title's!.S01E01.1080p.WEBDL.x264-EVOLVE|tt12345|12345|54321|11223|2010|2013 10 30",
                "S01E01-E03 x264|Surround Sound|Surround Sound|WEBDL-1080p|Proper|||The.Series.Title's!.S01E01.1080p.WEBDL.x264-EVOLVE|The.Series.Title's!.S01E01.1080p.WEBDL.x264-EVOLVE|tt12345|12345|54321|11223|2010|2013 10 30",
            ),
            (
                "S{season:00}E{episode:00} {series title} {SERIES TITLE} {Series_Title} {Series-Title} {Series.Title} {{literal}} ...{Episode Title}... {Series Title}",
                "S01E01 the series title's! THE SERIES TITLE'S! The_Series_Title's! The-Series-Title's! The.Series.Title's! {literal} ...Episode Title (1)... The Series Title's!",
                "S01E01-E03 the series title's! THE SERIES TITLE'S! The_Series_Title's! The-Series-Title's! The.Series.Title's! {literal} ...Episode Title... The Series Title's!",
            ),
            (
                "S{season:00}E{episode:00} {Series Title} - {Episode Title} - {Episode CleanTitle:5} {[Release Group]} {-Release Group} {Release Group}",
                "S01E01 The Series Title's! - Episode Title (1) - Ep... [RlsGrp] -RlsGrp RlsGrp",
                "S01E01-E03 The Series Title's! - Episode Title - Ep... [RlsGrp] -RlsGrp RlsGrp",
            ),
            (
                "{Series Title} {absolute:000} - S{season:00}E{episode:00} {Episode Title}",
                "The Series Title's! - S01E01 Episode Title (1)",
                "The Series Title's! - S01E01-E03 Episode Title",
            ),
            (
                "S{season:00}E{episode:00} {Series Title}/{Episode Title}",
                "S01E01 The Series Title's!/Episode Title (1)",
                "S01E01-E03 The Series Title's!/Episode Title",
            ),
        ] {
            let config = NamingConfig {
                rename_episodes: true,
                standard_episode_format: format.into(),
                ..NamingConfig::default()
            };
            assert_eq!(
                build_file_name(&eps[..1], &series(), &with_media, "", &config, &formats).unwrap(),
                single,
                "single {format}"
            );
            assert_eq!(
                build_file_name(&eps, &series(), &with_media, "", &config, &formats).unwrap(),
                multi,
                "multi {format}"
            );
        }
        // Literal text in the format is not cleaned; token values are.
        let config = NamingConfig {
            rename_episodes: true,
            standard_episode_format:
                "{Series Title}: {Episode Title}?* S{season:00}E{episode:00} {Quality Full}".into(),
            ..NamingConfig::default()
        };
        assert_eq!(
            build_file_name(&eps[..1], &series(), &file(), "", &config, &[]).unwrap(),
            "The Series Title's!: Episode Title (1)?* S01E01 WEBDL-1080p Proper"
        );
        let anime = NamingSeries {
            series_type: SeriesType::Anime,
            ..series()
        };
        let config = NamingConfig {
            rename_episodes: true,
            anime_episode_format: "{Series Title} {absolute:000} - S{season:00}E{episode:00} {Episode Title}"
                .into(),
            ..NamingConfig::default()
        };
        assert_eq!(
            build_file_name(&eps, &anime, &file(), "", &config, &[]).unwrap(),
            "The Series Title's! 001-003 - S01E01-E03 Episode Title"
        );
    }

    #[test]
    fn title_helpers() {
        assert_eq!(clean_title("The Series Title's!"), "The Series Title's!");
        assert_eq!(clean_title("Episode Title (1)"), "Episode Title 1");
        assert_eq!(clean_title("Who's Who: Part 2"), "Whos Who Part 2");
        assert_eq!(
            title_the("The Series Title's! (2010)"),
            "Series Title's!, The (2010)"
        );
        assert_eq!(clean_title_the("The Series Title's!"), "Series Title's!, The");
        assert_eq!(
            clean_title_the_year("The Series Title's!", 2010),
            "Series Title's!, The 2010"
        );
        assert_eq!(title_year("Show (2010)", 2010), "Show (2010)");
        assert_eq!(title_year("Show", 2010), "Show (2010)");
        // The token applies TitleThe first, so "The Show" files under S.
        assert_eq!(title_first_character("The Show"), "T");
        assert_eq!(title_first_character(&title_the("The Show")), "S");
        assert_eq!(title_first_character("¡Hola!"), "H");
        assert_eq!(truncate("A long title here", Some("10")), "A long{ellipsis}");
    }
}
