//! Port of Radarr's `Parser`: what a release or file name says about a
//! movie. Title and year, alternative titles, edition, IMDb and TMDB ids,
//! hardcoded subtitles, release group and hash, with Radarr's own quality
//! and language parsers. The patterns are the original's, verbatim, on the
//! .NET-flavoured regex layer the series parser uses; its test cases
//! (`tests/vectors/radarr/parser`) are the specification.

pub mod language;
pub mod quality;
pub mod release_group;

use crate::net_regex;
use crate::parser::media_extensions::is_media_extension;
use crate::parser::netregex::{Lazy, NetCaptures};
use crate::parser::quality::ParsedQuality;
use crate::parser::title::remove_accent;

pub use release_group::parse_release_group;

/// `ParsedMovieInfo`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedMovieInfo {
    /// The title first, then the alternatives an `AKA` introduces.
    pub movie_titles: Vec<String>,
    pub original_title: String,
    pub release_title: String,
    pub simple_release_title: String,
    pub quality: ParsedQuality,
    pub languages: Vec<i64>,
    pub release_group: Option<String>,
    pub release_hash: String,
    pub edition: String,
    /// 0 when the name carries none.
    pub year: i64,
    pub imdb_id: String,
    /// 0 when the name carries none.
    pub tmdb_id: i64,
    pub hardcoded_subs: Option<String>,
}

impl ParsedMovieInfo {
    /// `PrimaryMovieTitle`.
    pub fn primary_movie_title(&self) -> &str {
        self.movie_titles.first().map(String::as_str).unwrap_or("")
    }
}

macro_rules! edition {
    () => {
        r"\(?\b(?<edition>(((Recut.|Extended.|Ultimate.)?(Director.?s|Collector.?s|Theatrical|Ultimate|Extended|Despecialized|(Special|Rouge|Final|Assembly|Imperial|Diamond|Signature|Hunter|Rekall)(?=(.(Cut|Edition|Version)))|\d{2,3}(th)?.Anniversary)(?:.(Cut|Edition|Version))?(.(Extended|Uncensored|Remastered|Unrated|Uncut|Open.?Matte|IMAX|Fan.?Edit))?|((Uncensored|Remastered|Unrated|Uncut|Open?.Matte|IMAX|Fan.?Edit|Restored|((2|3|4)in1))))))\b\)?"
    };
}

static REPORT_EDITION_RE: Lazy = net_regex!(concat!(r"^.+?", edition!()));

static HARDCODED_SUBS_RE: Lazy =
    net_regex!(r"\b((?<hcsub>(\w+(?<!SOFT|MULTI|HORRIBLE)SUBS?))|(?<hc>(HC|SUBBED)))\b");

/// Index of the German tracker pattern in [`REPORT_MOVIE_TITLE`].
const GERMAN_TITLE_INDEX: usize = 5;

static REPORT_MOVIE_TITLE: [Lazy; 11] = [
    // Anime [Subgroup] with year explicitly in parentheses, e.g. [Sub] Title 1993 (1993) [VHS]
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+?)\s*\((?<year>(1(8|9)|20)\d{2})\).*?(?<hash>\[\w{8}\])?(?:$|\.)"
    ),
    // Anime [Subgroup] and Year
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+?)?(?:(?:[-_\W](?<![)\[!]))*(?<year>(1(8|9)|20)\d{2}(?!p|i|x|\d+|\]|\W\d+)))+.*?(?<hash>\[\w{8}\])?(?:$|\.)"
    ),
    // Anime [Subgroup] no year, versioned title, hash
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+?)((v)(?:\d{1,2})(?:([-_. ])))(\[.*)?(?:[\[(][^])])?.*?(?<hash>\[\w{8}\])(?:$|\.)"
    ),
    // Anime [Subgroup] no year, info in double sets of brackets, hash
    net_regex!(r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+?)(\[.*).*?(?<hash>\[\w{8}\])(?:$|\.)"),
    // Anime [Subgroup] no year, info in parentheses or brackets, hash
    net_regex!(
        r"^(?:\[(?<subgroup>.+?)\][-_. ]?)(?<title>(?![(\[]).+)(?:[\[(][^])]).*?(?<hash>\[\w{8}\])(?:$|\.)"
    ),
    // Some german or french tracker formats (missing year, ...). The
    // original also asks, in a look-behind of unbounded width, that no year
    // comes anywhere before the language word; `parse_movie_title` checks
    // that on the match (`gerlang`).
    net_regex!(concat!(
        r"^(?<title>(?![(\[]).+?)((\W|_))(",
        edition!(),
        r".{1,3})?(?:(?<!(?:Good|The)[_ .-])(?<gerlang>German|TrueFrench))(.+?)(?=((19|20)\d{2}|$))(?<year>(19|20)\d{2}(?!p|i|\d+|\]|\W\d+))?(\W+|_|$)(?!\\)"
    )),
    // Special, Despecialized, etc. Edition Movies, e.g: Mission.Impossible.3.Special.Edition.2011
    net_regex!(concat!(
        r"^(?<title>(?![(\[]).+?)?(?:(?:[-_\W](?<![)\[!]))*",
        edition!(),
        r".{1,3}(?<year>(1(8|9)|20)\d{2}(?!p|i|\d+|\]|\W\d+)))+(\W+|_|$)(?!\\)"
    )),
    // Normal movie format, e.g: Mission.Impossible.3.2011
    net_regex!(
        r"^(?<title>(?![(\[]).+?)?(?:(?:[-_\W](?<![)\[!]))*(?<year>(1(8|9)|20)\d{2}(?!p|i|(1(8|9)|20)\d{2}|\]|\W(1(8|9)|20)\d{2})))+(\W+|_|$)(?!\\)"
    ),
    // PassThePopcorn Torrent names: Star.Wars[PassThePopcorn]
    net_regex!(r"^(?<title>.+?)?(?:(?:[-_\W](?<![()\[!]))*(?<year>(\[\w *\])))+(\W+|_|$)(?!\\)"),
    // That did not work? Maybe some tool uses [] for years. Who would do that?
    net_regex!(
        r"^(?<title>(?![(\[]).+?)?(?:(?:[-_\W](?<![)!]))*(?<year>(1(8|9)|20)\d{2}(?!p|i|\d+|\W\d+)))+(\W+|_|$)(?!\\)"
    ),
    // As a last resort for movies that have ( or [ in their title.
    net_regex!(
        r"^(?<title>.+?)?(?:(?:[-_\W](?<![)\[!]))*(?<year>(1(8|9)|20)\d{2}(?!p|i|\d+|\]|\W\d+)))+(\W+|_|$)(?!\\)"
    ),
];

/// When year comes first: folder names only.
static REPORT_MOVIE_TITLE_FOLDER: Lazy = net_regex!(
    r"^(?:(?:[-_\W](?<![)!]))*(?<year>(19|20)\d{2}(?!p|i|\d+|\W\d+)))+(\W+|_|$)(?<title>.+?)?$",
    case_sensitive
);

static YEAR_BEFORE_RE: Lazy = net_regex!(r"(19|20)\d{2}");

static REJECT_HASHED_RELEASES: [Lazy; 9] = [
    // Generic match for md5 and mixed-case hashes.
    net_regex!(r"^[0-9a-zA-Z]{32}", case_sensitive),
    // Generic match for shorter lower-case hashes.
    net_regex!(r"^[a-z0-9]{24}$", case_sensitive),
    // Format seen on some NZBGeek releases
    net_regex!(r"^[A-Z]{11}\d{3}$", case_sensitive),
    net_regex!(r"^[a-z]{12}\d{3}$", case_sensitive),
    // Backup filename (Unknown origins)
    net_regex!(r"^Backup_\d{5,}S\d{2}-\d{2}$", case_sensitive),
    // 123 - Started appearing December 2014
    net_regex!(r"^123$", case_sensitive),
    // abc - Started appearing January 2015
    net_regex!(r"^abc$"),
    // abc - Started appearing 2020
    net_regex!(r"^abc[-_. ]xyz"),
    // b00bs - Started appearing January 2015
    net_regex!(r"^b00bs$"),
];

static REVERSED_TITLE_RE: Lazy = net_regex!(r"(?:^|[-._ ])(p027|p0801)[-._ ]", case_sensitive);
static ALTERNATIVE_TITLE_RE: Lazy = net_regex!(r"[ ]+(?:AKA|\/)[ ]+");
static BRACKETED_ALTERNATIVE_TITLE_RE: Lazy = net_regex!(r"(.*) \([ ]*AKA[ ]+(.*)\)");
static NORMALIZE_ALTERNATIVE_TITLE_RE: Lazy = net_regex!(r"[ ]+(?:A\.K\.A\.)[ ]+");
static NORMALIZE_RE: Lazy = net_regex!(
    r"((?:\b|_)(?<!^|[^a-zA-Z0-9_']\w[^a-zA-Z0-9_'])([aà](?!$|[^a-zA-Z0-9_']\w[^a-zA-Z0-9_'])|an|the|and|or|of)(?!$)(?:\b|_))|\W|_"
);
static REPORT_IMDB_ID_RE: Lazy = net_regex!(r"(?<imdbid>tt\d{7,8})");
static REPORT_TMDB_ID_RE: Lazy = net_regex!(r"tmdb(id)?-(?<tmdbid>\d+)");
static SIMPLE_TITLE_RE: Lazy = net_regex!(
    r"(?:(480|540|576|720|1080|2160)[ip]|[xh][\W_]?26[45]|DD\W?5\W1|[<>?*]|848x480|1280x720|1920x1080|3840x2160|4096x2160|(8|10)b(it)?|10-bit)\s*?(?![a-b0-9])"
);
static SIMPLE_RELEASE_TITLE_RE: Lazy = net_regex!(r"\s*(?:[<>?*|])");
static CLEAN_QUALITY_BRACKETS_RE: Lazy = net_regex!(r"\[[a-z0-9 ._-]+\]$");
static SPECIAL_CHAR_RE: Lazy = net_regex!(r"(\&|\:|\\|\/)+", case_sensitive);
static PUNCTUATION_RE: Lazy = net_regex!(r"[^\w\s]", case_sensitive);
static ARTICLE_WORD_RE: Lazy = net_regex!(r"^(a|an|the)\s");
static SPECIAL_EPISODE_WORD_RE: Lazy = net_regex!(r"\b(part|special|edition|christmas)\b\s?");
static DUPLICATE_SPACES_RE: Lazy = net_regex!(r"\s{2,}", case_sensitive);
static REQUEST_INFO_RE: Lazy = net_regex!(r"^(?:\[.+?\])+", case_sensitive);
static MULTI_RE: Lazy = net_regex!(r"[_. ](?<multi>multi)[_. ]");
static FILE_EXTENSION_RE: Lazy = net_regex!(r"\.[a-z0-9]{2,4}$");

// `ParserCommon`, shared by the title and the release group parsers.
pub(crate) static WEBSITE_PREFIX_RE: Lazy = net_regex!(
    r"^(?:(?:\[|\()\s*)?(?:www\.)?[-a-z0-9-]{1,256}\.(?<!Naruto-Kun\.)(?:[a-z]{2,6}\.[a-z]{2,6}|xn--[a-z0-9-]{4,}|[a-z]{2,})\b(?:\s*(?:\]|\))|[ -]{2,})[ -]*"
);
pub(crate) static WEBSITE_POSTFIX_RE: Lazy =
    net_regex!(r"(?:\[\s*)?(?:www\.)?[-a-z0-9-]{1,256}\.(?:xn--[a-z0-9-]{4,}|[a-z]{2,6})\b(?:\s*\])$");
pub(crate) static CLEAN_TORRENT_SUFFIX_RE: Lazy = net_regex!(r"\[(?:ettv|rartv|rarbg|cttv|publichd)\]$");

/// Longer than any release name; past it the backtracking patterns cost
/// seconds on the database thread. Not in the original.
const MAX_TITLE_LEN: usize = 512;

/// Radarr's `FileExtensions.RemoveFileExtension`: Sonarr's, plus `.mk3d`.
pub fn remove_file_extension(title: &str) -> String {
    FILE_EXTENSION_RE.replace_all_with(title, |m| {
        let ext = m.whole().to_lowercase();
        if is_media_extension(&ext) || ext == ".mk3d" || ext == ".par2" || ext == ".nzb" {
            String::new()
        } else {
            m.whole().to_string()
        }
    })
}

fn validate_before_parsing(title: &str) -> bool {
    if title.len() > MAX_TITLE_LEN {
        return false;
    }
    let lower = title.to_lowercase();
    if lower.contains("password") && lower.contains("yenc") {
        return false;
    }
    if !title.chars().any(|c| c.is_alphanumeric()) {
        return false;
    }
    let without_extension = remove_file_extension(title);
    !REJECT_HASHED_RELEASES
        .iter()
        .any(|re| re.is_match(&without_extension))
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The byte offset in `text` of a named group that took part in a match.
fn start_of(captures: &NetCaptures<'_, '_>, name: &str) -> Option<usize> {
    let value = captures.value(name)?;
    Some(captures.end_of(name)? - value.len())
}

/// `Parser.ParseMoviePath`: a file's name, then its folder's.
pub fn parse_movie_path(path: &str) -> Option<ParsedMovieInfo> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let (dir, name) = match trimmed.rfind(['/', '\\']) {
        Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
        None => ("", trimmed),
    };
    let dir_name = dir.rsplit(['/', '\\']).next().unwrap_or("");
    let extension = match name.rfind('.') {
        Some(i) => &name[i..],
        None => "",
    };
    parse_movie_title(name, true)
        .or_else(|| parse_movie_title(&format!("{dir_name} {name}"), false))
        .or_else(|| parse_movie_title(&format!("{dir_name}{extension}"), false))
}

/// `Parser.ParseMovieTitle`. `is_dir` also tries the pattern for folder
/// names that start with the year.
pub fn parse_movie_title(title: &str, is_dir: bool) -> Option<ParsedMovieInfo> {
    if !validate_before_parsing(title) {
        return None;
    }
    // A name posted backwards: the original means to reverse it, formats
    // the character array instead of its content and goes on to parse
    // "System.Char[]", which no pattern matches.
    if REVERSED_TITLE_RE.is_match(title) {
        return None;
    }

    let release_title = remove_file_extension(title)
        .trim_matches(['-', '_'])
        .replace('【', "[")
        .replace('】', "]");

    let mut simple_title = SIMPLE_TITLE_RE.replace_all(&release_title, "");
    simple_title = WEBSITE_PREFIX_RE.replace_all(&simple_title, "");
    simple_title = WEBSITE_POSTFIX_RE.replace_all(&simple_title, "");
    simple_title = CLEAN_TORRENT_SUFFIX_RE.replace_all(&simple_title, "");
    simple_title = CLEAN_QUALITY_BRACKETS_RE.replace_all_with(&simple_title, |m| {
        if quality::parse_quality_name(m.whole()).quality.quality_id != quality::UNKNOWN {
            String::new()
        } else {
            m.whole().to_string()
        }
    });

    let folder = is_dir.then_some(&REPORT_MOVIE_TITLE_FOLDER);
    for (index, regex) in REPORT_MOVIE_TITLE.iter().chain(folder).enumerate() {
        let Some(first) = regex.captures(&simple_title) else {
            continue;
        };
        if index == GERMAN_TITLE_INDEX {
            let before = start_of(&first, "gerlang").unwrap_or(0);
            if YEAR_BEFORE_RE.is_match(&simple_title[..before]) {
                continue;
            }
        }
        let Some(mut result) = parse_match(&first) else {
            continue;
        };

        // The original cuts the title out of the release title at the
        // position it has in the simplified one, which need not be the
        // same text; past the end, it fails and nothing is parsed.
        let mut simple_release_title = SIMPLE_RELEASE_TITLE_RE.replace_all(&release_title, "");
        let title_group = first.value("title");
        let replace_string = title_group.unwrap_or_else(|| result.primary_movie_title());
        if !replace_string.trim().is_empty() {
            let placeholder = if replace_string.contains('.') {
                "A.Movie"
            } else {
                "A Movie"
            };
            match title_group {
                Some(group) => {
                    let start = utf16_len(&simple_title[..start_of(&first, "title")?]);
                    let length = utf16_len(group);
                    let mut units: Vec<u16> = simple_release_title.encode_utf16().collect();
                    if start + length > units.len() {
                        return None;
                    }
                    units.splice(start..start + length, placeholder.encode_utf16());
                    simple_release_title = String::from_utf16_lossy(&units);
                }
                None => simple_release_title = simple_release_title.replace(replace_string, placeholder),
            }
        }

        result.release_group = parse_release_group(&simple_release_title);
        if let Some(sub_group) = first.value("subgroup").filter(|g| !g.trim().is_empty()) {
            result.release_group = Some(sub_group.to_string());
        }
        result.hardcoded_subs = parse_hardcoded_subs(title);

        let language_title = match result.release_group.as_deref().filter(|g| !g.trim().is_empty()) {
            Some(group) => simple_release_title.replace(group, "RlsGrp"),
            None => simple_release_title.clone(),
        };
        result.languages = language::parse_languages(&language_title);
        result.quality = quality::parse_quality(title);
        if result.edition.trim().is_empty() {
            result.edition = parse_edition(&simple_release_title);
        }
        result.release_hash = release_hash(&first);
        result.original_title = title.to_string();
        result.imdb_id = parse_imdb_id(&simple_release_title);
        result.tmdb_id = parse_tmdb_id(&simple_release_title);
        result.release_title = release_title;
        result.simple_release_title = simple_release_title;
        return Some(result);
    }
    None
}

/// `ParseMovieMatchCollection`: the titles, the year and the edition of a
/// match, or nothing when the pattern caught no title.
fn parse_match(first: &NetCaptures<'_, '_>) -> Option<ParsedMovieInfo> {
    let title = first.value("title")?;
    if title == "(" {
        return None;
    }
    let name = title.replace('_', " ");
    let name = NORMALIZE_ALTERNATIVE_TITLE_RE.replace_all(&name, " AKA ");
    let name = REQUEST_INFO_RE.replace_all(&name, "");
    let name = name.trim_matches(' ');

    // Dots become spaces, except in acronyms (`S.H.I.E.L.D.`, `Dr.`).
    let parts: Vec<&str> = name.split('.').collect();
    let is_int = |s: &str| s.trim().parse::<i32>().is_ok();
    let single = |s: &str| s.chars().count() == 1;
    let mut movie_name = String::new();
    let mut previous_acronym = false;
    for (n, part) in parts.iter().enumerate() {
        let next = parts.get(n + 1).copied().unwrap_or("");
        let lower = part.to_lowercase();
        let acronym = (single(part)
            && lower != "a"
            && !is_int(part)
            && (previous_acronym || n < parts.len() - 1)
            && (previous_acronym || !single(next) || !is_int(next)))
            || (lower == "a" && (previous_acronym || single(next)))
            || lower == "dr";
        if acronym {
            movie_name.push_str(part);
            movie_name.push('.');
            previous_acronym = true;
        } else {
            if previous_acronym {
                movie_name.push(' ');
                previous_acronym = false;
            }
            movie_name.push_str(part);
            movie_name.push(' ');
        }
    }
    let movie_name = movie_name.trim_matches(' ').to_string();

    let year = first
        .value("year")
        .and_then(|y| y.trim().parse::<i64>().ok())
        .unwrap_or(0);
    let edition = first
        .value("edition")
        .map(|e| e.replace('.', " "))
        .unwrap_or_default();

    // Parentheses of the form (aka ...) go, then the name splits on AKA.
    let unbracketed = BRACKETED_ALTERNATIVE_TITLE_RE.replace_all(&movie_name, "$1 AKA $2");
    let mut movie_titles = vec![movie_name.clone()];
    movie_titles.extend(
        split(&ALTERNATIVE_TITLE_RE, &unbracketed)
            .into_iter()
            .filter(|alternative| !alternative.trim().is_empty() && *alternative != movie_name),
    );

    Some(ParsedMovieInfo {
        movie_titles,
        original_title: String::new(),
        release_title: String::new(),
        simple_release_title: String::new(),
        quality: ParsedQuality::default(),
        languages: Vec::new(),
        release_group: None,
        release_hash: String::new(),
        edition,
        year,
        imdb_id: String::new(),
        tmdb_id: 0,
        hardcoded_subs: None,
    })
}

/// `Regex.Split` for a pattern without capturing groups.
fn split(regex: &Lazy, text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut last = 0;
    for m in regex.captures_iter(text) {
        parts.push(text[last..m.start()].to_string());
        last = m.end();
    }
    parts.push(text[last..].to_string());
    parts
}

fn release_hash(first: &NetCaptures<'_, '_>) -> String {
    match first.value("hash") {
        Some(hash) => {
            let value = hash.trim_matches(['[', ']']);
            if value == "1280x720" {
                String::new()
            } else {
                value.to_string()
            }
        }
        None => String::new(),
    }
}

/// `Parser.ParseImdbId`: the `tt` id in a name, or empty.
pub fn parse_imdb_id(title: &str) -> String {
    REPORT_IMDB_ID_RE
        .captures(title)
        .and_then(|m| m.value("imdbid").map(str::to_string))
        .filter(|id| id.len() == 9 || id.len() == 10)
        .unwrap_or_default()
}

/// `Parser.ParseTmdbId`: the id after `tmdb-` or `tmdbid-`, or 0.
pub fn parse_tmdb_id(title: &str) -> i64 {
    REPORT_TMDB_ID_RE
        .captures(title)
        .and_then(|m| m.value("tmdbid").and_then(|id| id.parse::<i32>().ok()))
        .map(i64::from)
        .unwrap_or(0)
}

/// `Parser.ParseEdition`: the edition a name carries after its title.
pub fn parse_edition(title: &str) -> String {
    REPORT_EDITION_RE
        .captures(title)
        .and_then(|m| m.value("edition").map(|e| e.replace('.', " ")))
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_default()
}

/// `Parser.ParseHardcodeSubs`: the kind of burnt-in subtitles, if any.
pub fn parse_hardcoded_subs(title: &str) -> Option<String> {
    let last = HARDCODED_SUBS_RE.captures_iter(title).into_iter().last()?;
    if let Some(sub) = last.value("hcsub") {
        return Some(sub.to_string());
    }
    last.success("hc").then(|| "Generic Hardcoded Subs".to_string())
}

/// `Parser.HasMultipleLanguages`: a `MULTi` tag in the name.
pub fn has_multiple_languages(title: &str) -> bool {
    MULTI_RE.is_match(title)
}

/// `Parser.ReplaceGermanUmlauts`.
pub fn replace_german_umlauts(text: &str) -> String {
    text.replace('ä', "ae")
        .replace('ö', "oe")
        .replace('ü', "ue")
        .replace('Ä', "Ae")
        .replace('Ö', "Oe")
        .replace('Ü', "Ue")
        .replace('ß', "ss")
}

/// `Parser.CleanMovieTitle`: the form two titles are compared in.
pub fn clean_movie_title(title: &str) -> String {
    if title.trim().is_empty() {
        return title.to_string();
    }
    // A title made of digits only stays as it is.
    if title.trim().parse::<i64>().is_ok() {
        return title.to_string();
    }
    remove_accent(&replace_german_umlauts(
        &NORMALIZE_RE.replace_all(title, "").to_lowercase(),
    ))
}

/// `Parser.NormalizeMovieTitle`.
pub fn normalize_movie_title(title: &str) -> String {
    let title = SPECIAL_EPISODE_WORD_RE.replace_all(title, "");
    let title = PUNCTUATION_RE.replace_all(&title, " ");
    let title = DUPLICATE_SPACES_RE.replace_all(&title, " ");
    title.trim().to_lowercase()
}

/// `Parser.NormalizeTitle`.
pub fn normalize_title(title: &str) -> String {
    let title = PUNCTUATION_RE.replace_all(title, "");
    let title = ARTICLE_WORD_RE.replace_all(&title, "");
    let title = DUPLICATE_SPACES_RE.replace_all(&title, " ");
    let title = SPECIAL_CHAR_RE.replace_all(&title, "");
    title.trim().to_lowercase()
}

/// `Parser.SimplifyReleaseTitle`.
pub fn simplify_release_title(title: &str) -> String {
    SIMPLE_RELEASE_TITLE_RE.replace_all(title, "")
}

/// `SceneChecker.GetSceneTitle`: the release title when the name is a
/// scene name. Prefers false negatives, as the original says.
pub fn scene_title(title: &str) -> Option<String> {
    if !title.contains('.') || title.contains(' ') {
        return None;
    }
    let parsed = parse_movie_title(title, false)?;
    if parsed.release_group.is_none()
        || parsed.quality.quality.quality_id == quality::UNKNOWN
        || parsed.primary_movie_title().trim().is_empty()
        || parsed.release_title.trim().is_empty()
    {
        return None;
    }
    Some(parsed.release_title)
}

/// `Parser.ToUrlSlug`: a title as a URL slug. `trim_end_chars` are trimmed
/// from both ends, as the original's `Trim` does despite the name.
pub fn to_url_slug(
    value: &str,
    invalid_dash_replacement: bool,
    trim_end_chars: &str,
    deduplicate_chars: &str,
) -> String {
    static WHITESPACE_RE: Lazy = net_regex!(r"\s", case_sensitive);
    static INVALID_RE: Lazy = net_regex!(r"[^a-z0-9\s\-_]", case_sensitive);
    let value = remove_accent(&value.to_lowercase());
    let value = WHITESPACE_RE.replace_all(&value, "-");
    let mut value = INVALID_RE.replace_all(&value, if invalid_dash_replacement { "-" } else { "" });
    if !trim_end_chars.is_empty() {
        let trim: Vec<char> = trim_end_chars.chars().collect();
        value = value.trim_matches(&trim[..]).to_string();
    }
    if !deduplicate_chars.is_empty() {
        // `([chars]){2,}` replaced by `$1`: a run of them leaves its last.
        let chars: Vec<char> = value.chars().collect();
        let mut out = String::with_capacity(value.len());
        let mut i = 0;
        while i < chars.len() {
            let mut j = i;
            while j < chars.len() && deduplicate_chars.contains(chars[j]) {
                j += 1;
            }
            if j - i >= 2 {
                out.push(chars[j - 1]);
                i = j;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        value = out;
    }
    value
}
