//! Media files: what phase 3a adds to the library side of the originals'
//! `MediaFiles` namespace. A completed download is scanned, each video file
//! parsed, matched to episodes and run through the import specifications;
//! what passes is moved (or linked, or copied) into the series folder under
//! the name the naming settings build, recorded as an `EpisodeFile` with
//! its media info, and told to whoever listens (history, the download
//! history, the SignalR relays, the notifications). Disk scans, renames,
//! manual imports and file deletions share the same pieces.
//!
//! The modules follow the originals' classes so that a divergence can be
//! read against its source: `decide` is `ImportDecisionMaker`, `specs` the
//! `EpisodeImport/Specifications`, `aggregate` the aggregation service and
//! its augmenters, `approve` is `ImportApprovedEpisodes` with the file
//! upgrader and mover, `downloaded` the `DownloadedEpisodesImportService`,
//! `completed` the `CompletedDownloadService`, `scan` the `DiskScanService`.

pub mod aggregate;
pub mod approve;
pub mod completed;
pub mod decide;
pub mod deletion;
pub mod downloaded;
pub mod events;
pub mod extras;
pub mod format;
pub mod local;
pub mod manual;
pub mod mediainfo;
pub mod rename;
pub mod sample;
pub mod scan;
pub mod specs;
pub mod transfer;

pub use events::{Events, ImportEvent};
pub use local::{
    DeleteReason, ImportDecision, ImportMode, ImportRejection, ImportResult, LocalEpisode, RejectionReason,
};

use crate::parser::media_extensions::path_extension;

/// `Path.Combine` for the forward-slash paths the library keeps.
pub fn join(base: &str, relative: &str) -> String {
    let relative = relative.trim_start_matches('/');
    if base.is_empty() {
        return relative.to_string();
    }
    format!("{}/{relative}", base.trim_end_matches('/'))
}

/// `GetRelativePath`: `child` under `parent`, without the leading slash;
/// `child` itself when it is not under `parent`.
pub fn relative_path<'a>(parent: &str, child: &'a str) -> &'a str {
    let parent = parent.trim_end_matches('/');
    match child.strip_prefix(parent) {
        Some(rest) if parent.is_empty() || rest.starts_with('/') => rest.trim_start_matches('/'),
        _ => child,
    }
}

/// A path with its `.` and `..` segments resolved by name alone, as
/// `Path.GetFullPath` does: no link is followed, and `..` at the root
/// stays at the root. Borrowed when there is nothing to resolve.
fn resolve_dots(path: &str) -> std::borrow::Cow<'_, str> {
    if !path.split('/').any(|segment| segment == "." || segment == "..") {
        return std::borrow::Cow::Borrowed(path);
    }
    let absolute = path.starts_with('/');
    let mut kept: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if kept.last().is_some_and(|s| *s != "..") {
                    kept.pop();
                } else if !absolute {
                    kept.push("..");
                }
            }
            other => kept.push(other),
        }
    }
    let joined = kept.join("/");
    std::borrow::Cow::Owned(if absolute { format!("/{joined}") } else { joined })
}

/// `IsParentPath`: whether `child` is inside `parent` (not equal to it),
/// comparing paths as the originals do, case-insensitively.
pub fn is_parent_path(parent: &str, child: &str) -> bool {
    // The originals compare `DirectoryInfo.FullName`s, which have no `.`
    // or `..` left in them: `/movies/A/../B/x.mkv` is not inside `A`.
    let (parent, child) = (resolve_dots(parent), resolve_dots(child));
    let parent = parent.trim_end_matches('/');
    let child = child.trim_end_matches('/');
    // Bytes, not `str` slices: the parent's length may fall inside a
    // character of the child (a download named in Japanese against a
    // movie folder in ASCII), where slicing a `str` panics.
    let (parent, child) = (parent.as_bytes(), child.as_bytes());
    if parent.len() >= child.len() {
        return false;
    }
    child[..parent.len()].eq_ignore_ascii_case(parent) && child[parent.len()] == b'/'
}

/// The root folder a series path belongs to: the longest root folder that is
/// a parent of the path, else the path's own parent directory.
pub fn best_root_folder_path(series_path: &str, root_folders: &[String]) -> String {
    let normalized = series_path.trim_end_matches('/');
    root_folders
        .iter()
        .filter(|r| {
            let r = r.trim_end_matches('/');
            normalized.starts_with(r) && normalized[r.len()..].starts_with('/')
        })
        .max_by_key(|r| r.len())
        .map(|r| r.trim_end_matches('/').to_string())
        .unwrap_or_else(|| match normalized.rfind('/') {
            Some(0) => "/".to_string(),
            Some(i) => normalized[..i].to_string(),
            None => String::new(),
        })
}

/// `PathEquals`.
pub fn path_equals(a: &str, b: &str) -> bool {
    a.trim_end_matches('/')
        .eq_ignore_ascii_case(b.trim_end_matches('/'))
}

/// `Path.GetDirectoryName`.
pub fn parent_path(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    let i = trimmed.rfind('/')?;
    Some(if i == 0 { "/" } else { &trimmed[..i] })
}

/// `Path.GetFileName`.
pub fn file_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// `Path.GetFileNameWithoutExtension`.
pub fn file_stem(path: &str) -> &str {
    let name = file_name(path);
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

/// `Path.GetExtension`, with its dot.
pub fn extension(path: &str) -> &str {
    path_extension(file_name(path))
}

/// `GetLongestCommonPath` of the originals: the deepest folder every path
/// shares.
pub fn longest_common_path(paths: &[String]) -> String {
    let Some(first) = paths.first() else {
        return String::new();
    };
    let mut common: Vec<&str> = parent_path(first).unwrap_or("").split('/').collect();
    for p in &paths[1..] {
        let parts: Vec<&str> = parent_path(p).unwrap_or("").split('/').collect();
        let keep = common
            .iter()
            .zip(parts.iter())
            .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
            .count();
        common.truncate(keep);
    }
    let joined = common.join("/");
    if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    }
}

/// ISO 639 codes to the originals' language ids (`IsoLanguages`), for the
/// audio tracks media info names.
pub fn language_from_iso(code: &str) -> Option<i64> {
    use crate::parser::language::lang;
    let whole = code.trim().to_ascii_lowercase();
    // `Find` looks at the part before a dash (`pt-br`); the country part
    // only narrows a two-letter match, which this table has no row for.
    let code = whole.split('-').next().unwrap_or("").to_string();
    // `AlternateIsoCodeMappings`, checked on the whole code.
    let alternate = match whole.as_str() {
        "cze" => Some(25),
        "dut" => Some(lang::DUTCH),
        "mac" => Some(45),
        "rum" => Some(35),
        "yue" | "zhtw" => Some(lang::CHINESE),
        _ => None,
    };
    if code.len() != 2 && alternate.is_some() {
        return alternate;
    }
    // `FileNameBuilder.Iso639BTMap`: bibliographic codes to terminologic.
    let code = match code.as_str() {
        "alb" => "sqi",
        "arm" => "hye",
        "baq" => "eus",
        "bur" => "mya",
        "chi" => "zho",
        "cze" => "ces",
        "dut" => "nld",
        "fre" => "fra",
        "geo" => "kat",
        "ger" => "deu",
        "gre" => "ell",
        "ice" => "isl",
        "mac" => "mkd",
        "mao" => "mri",
        "may" => "msa",
        "per" => "fas",
        "rum" => "ron",
        "slo" => "slk",
        "tib" => "bod",
        "wel" => "cym",
        other => other,
    }
    .to_string();
    let table: &[(&str, &str, i64)] = &[
        ("en", "eng", lang::ENGLISH),
        ("fr", "fra", lang::FRENCH),
        ("es", "spa", lang::SPANISH),
        ("de", "deu", lang::GERMAN),
        ("it", "ita", lang::ITALIAN),
        ("da", "dan", lang::DANISH),
        ("nl", "nld", lang::DUTCH),
        ("ja", "jpn", lang::JAPANESE),
        ("is", "isl", lang::ICELANDIC),
        ("zh", "zho", lang::CHINESE),
        ("ru", "rus", lang::RUSSIAN),
        ("pl", "pol", lang::POLISH),
        ("vi", "vie", lang::VIETNAMESE),
        ("sv", "swe", lang::SWEDISH),
        ("no", "nor", lang::NORWEGIAN),
        ("nb", "nob", lang::NORWEGIAN),
        ("fi", "fin", lang::FINNISH),
        ("tr", "tur", lang::TURKISH),
        ("pt", "por", lang::PORTUGUESE),
        ("el", "ell", 20),
        ("ko", "kor", 21),
        ("hu", "hun", 22),
        ("he", "heb", 23),
        ("lt", "lit", 24),
        ("cs", "ces", 25),
        ("ar", "ara", 26),
        ("hi", "hin", 27),
        ("bg", "bul", 28),
        ("ml", "mal", 29),
        ("uk", "ukr", 30),
        ("sk", "slk", 31),
        ("th", "tha", 32),
        ("ro", "ron", 35),
        ("lv", "lav", 36),
        ("fa", "fas", 37),
        ("ca", "cat", 38),
        ("hr", "hrv", 39),
        ("sr", "srp", 40),
        ("bs", "bos", 41),
        ("et", "est", 42),
        ("ta", "tam", 43),
        ("id", "ind", 44),
        ("mk", "mkd", 45),
        ("sl", "slv", 46),
    ];
    // The originals look the two-letter code up first, then the three-letter
    // one, taking the first row that matches (`fr`/`fra` are French, not
    // the later `pt-br` style rows).
    // The rows of `IsoLanguages.All` with a country code; a two-letter code
    // with a country picks among them, falling back to the rows without.
    let countries: &[(&str, &str, i64)] = &[
        ("fr", "fr", lang::FRENCH),
        ("de", "de", lang::GERMAN),
        ("zh", "cn", lang::CHINESE),
        ("pt", "pt", lang::PORTUGUESE),
        ("th", "th", 32),
        ("pt", "br", lang::PORTUGUESE_BRAZIL),
        ("es", "mx", 34),
    ];
    if let (2, Some(country)) = (code.len(), whole.split('-').nth(1)) {
        if let Some((_, _, id)) = countries.iter().find(|(two, c, _)| *two == code && *c == country) {
            return Some(*id);
        }
        let has_country = |id: i64| countries.iter().any(|(two, _, i)| *two == code && *i == id);
        return table
            .iter()
            .find(|(two, _, id)| *two == code && !has_country(*id))
            .map(|(_, _, id)| *id);
    }
    match code.len() {
        2 => table.iter().find(|(two, _, _)| *two == code),
        3 => table.iter().find(|(_, three, _)| *three == code),
        _ => None,
    }
    .map(|(_, _, id)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_paths_are_compared_at_any_byte() {
        assert!(is_parent_path(
            "/movies/Amelie (2001)",
            "/movies/amelie (2001)/Amelie.mkv"
        ));
        assert!(!is_parent_path("/movies/Amelie (2001)", "/movies/Amelie (2001)"));
        assert!(!is_parent_path(
            "/movies/Amelie",
            "/movies/Amelie (2001)/Amelie.mkv"
        ));
        // The parent's 21 bytes end inside a three-byte character here.
        assert!(!is_parent_path(
            "/movies/Amelie (2001)",
            "/downloads/complete/アメリ.2001.1080p.BluRay.x264-GRP/アメリ.mkv"
        ));
        assert!(is_parent_path(
            "/movies/アメリ (2001)",
            "/movies/アメリ (2001)/アメリ.mkv"
        ));
        assert!(!is_parent_path(
            "/movies/アメリ (2001)",
            "/movies/アメル (2001)/アメリ.mkv"
        ));
    }

    #[test]
    fn parent_paths_are_compared_after_resolving_dots() {
        assert!(!is_parent_path(
            "/movies/A (2019)",
            "/movies/A (2019)/../Other (2019)/x.mkv"
        ));
        assert!(is_parent_path(
            "/movies/A (2019)",
            "/movies/A (2019)/./extras/../x.mkv"
        ));
        assert!(is_parent_path("/movies/B/../A (2019)", "/movies/A (2019)/x.mkv"));
        assert_eq!(resolve_dots("/a/../../b"), "/b");
        assert_eq!(resolve_dots("a/../../b"), "../b");
        assert_eq!(resolve_dots("/movies/A/x.mkv"), "/movies/A/x.mkv");
    }

    #[test]
    fn path_helpers_follow_the_originals() {
        assert_eq!(join("/tv/Show", "Season 01/ep.mkv"), "/tv/Show/Season 01/ep.mkv");
        assert_eq!(
            relative_path("/tv/Show", "/tv/Show/Season 01/ep.mkv"),
            "Season 01/ep.mkv"
        );
        assert_eq!(relative_path("/tv/Show", "/other/ep.mkv"), "/other/ep.mkv");
        assert!(is_parent_path("/tv/Show", "/tv/Show/Season 01/ep.mkv"));
        assert!(is_parent_path("/tv/show", "/tv/Show/ep.mkv"));
        assert!(!is_parent_path("/tv/Show", "/tv/Show"));
        assert!(!is_parent_path("/tv/Show", "/tv/Showtime/ep.mkv"));
        assert_eq!(parent_path("/tv/Show/ep.mkv"), Some("/tv/Show"));
        assert_eq!(parent_path("/ep.mkv"), Some("/"));
        assert_eq!(file_name("/tv/Show/ep.mkv"), "ep.mkv");
        assert_eq!(file_stem("/tv/Show/ep.mkv"), "ep");
        assert_eq!(extension("/tv/Show/ep.mkv"), ".mkv");
        assert_eq!(
            longest_common_path(&["/d/a/x/1.mkv".into(), "/d/a/y/2.mkv".into()]),
            "/d/a"
        );
        assert_eq!(longest_common_path(&["/d/a/1.mkv".into()]), "/d/a");
    }

    fn lang_portuguese() -> i64 {
        crate::parser::language::lang::PORTUGUESE
    }

    #[test]
    fn iso_codes_map_to_language_ids() {
        assert_eq!(language_from_iso("eng"), Some(1));
        assert_eq!(language_from_iso("ita"), Some(5));
        assert_eq!(language_from_iso("fr"), Some(2));
        assert_eq!(language_from_iso("und"), None);
        assert_eq!(language_from_iso("ger"), Some(4));
        assert_eq!(language_from_iso("cze"), Some(25));
        assert_eq!(language_from_iso("pt-br"), Some(33));
        assert_eq!(language_from_iso("pt-xx"), None);
        assert_eq!(language_from_iso("es-ar"), Some(3));
        assert_eq!(language_from_iso("pt"), Some(lang_portuguese()));
    }
}
