//! Port of Radarr's `QualityParser`. It shares its skeleton with the
//! series one (`crate::parser::quality`) and differs where movies do: the
//! sources a cinema release goes through (cam, telesync, telecine,
//! workprint), screeners and regional releases, DVD-R, disc images, and
//! Radarr's own numbering of the remuxes.

use crate::movies::quality::{modifier_number, source_number, MovieQuality, RADARR_QUALITIES};
use crate::net_regex;
use crate::parser::media_extensions::{
    path_extension, quality_for_extension as shared_quality_for_extension,
};
use crate::parser::netregex::Lazy;
use crate::parser::quality::{Detection, ParsedQuality};

// Quality ids, named as Radarr names them.
pub const UNKNOWN: i64 = 0;
pub const SDTV: i64 = 1;
pub const DVD: i64 = 2;
pub const WEBDL1080P: i64 = 3;
pub const HDTV720P: i64 = 4;
pub const WEBDL720P: i64 = 5;
pub const BLURAY720P: i64 = 6;
pub const BLURAY1080P: i64 = 7;
pub const WEBDL480P: i64 = 8;
pub const HDTV1080P: i64 = 9;
pub const RAWHD: i64 = 10;
pub const WEBRIP480P: i64 = 12;
pub const WEBRIP720P: i64 = 14;
pub const WEBRIP1080P: i64 = 15;
pub const HDTV2160P: i64 = 16;
pub const WEBRIP2160P: i64 = 17;
pub const WEBDL2160P: i64 = 18;
pub const BLURAY2160P: i64 = 19;
pub const BLURAY480P: i64 = 20;
pub const BLURAY576P: i64 = 21;
pub const BRDISK: i64 = 22;
pub const DVDR: i64 = 23;
pub const WORKPRINT: i64 = 24;
pub const CAM: i64 = 25;
pub const TELESYNC: i64 = 26;
pub const TELECINE: i64 = 27;
pub const DVDSCR: i64 = 28;
pub const REGIONAL: i64 = 29;
pub const REMUX1080P: i64 = 30;
pub const REMUX2160P: i64 = 31;

static SOURCE_RE: Lazy = net_regex!(
    r"\b(?:(?<bluray>M?Blu[-_. ]?Ray|HD[-_. ]?DVD|BD(?!$)|UHD2?BD|BDISO|BDMux|BD25|BD50|BR[-_. ]?DISK)|(?<webdl>WEB[-_. ]?DL(?:mux)?|AmazonHD|AmazonSD|iTunesHD|MaxdomeHD|NetflixU?HD|WebHD|HBOMaxHD|DisneyHD|[. ]WEB[. ](?:[xh][ .]?26[45]|AVC|HEVC|DDP?5[. ]1)|[. ](?-i:WEB)$|(?:\d{3,4}0p)[-. ](?:Hybrid[-_. ]?)?WEB[-. ]|[-. ]WEB[-. ]\d{3,4}0p|\b\s\/\sWEB\s\/\s\b|(?:AMZN|NF|DP)[. -]WEB[. -](?!Rip))|(?<webrip>WebRip|Web-Rip|WEBMux)|(?<hdtv>HDTV)|(?<bdrip>BDRip|BDLight|HD[-_. ]?DVDRip|UHDBDRip)|(?<brrip>BRRip)|(?<dvdr>\d?x?M?DVD-?[R59])|(?<dvd>DVD(?!-R)|DVDRip|xvidvd)|(?<dsr>WS[-_. ]DSR|DSR)|(?<regional>R[0-9]{1}|REGIONAL)|(?<scr>SCR|SCREENER|DVDSCR|DVDSCREENER)|(?<ts>TS[-_. ]|TELESYNCH?|HD-TS|HDTS|PDVD|TSRip|HDTSRip)|(?<tc>TC|TELECINE|HD-TC|HDTC)|(?<cam>CAMRIP|(?:NEW)?CAM|HD-?CAM(?:Rip)?|HQCAM)|(?<wp>WORKPRINT|WP)|(?<pdtv>PDTV)|(?<sdtv>SDTV)|(?<tvrip>TVRip))(?:\b|$|[ .])"
);
static RAW_HD_RE: Lazy = net_regex!(r"\b(?<rawhd>RawHD|Raw[-_. ]HD)\b");
static MPEG2_RE: Lazy = net_regex!(r"\b(?<mpeg2>MPEG[-_. ]?2)\b", case_sensitive);
static BRDISK_RE: Lazy = net_regex!(
    r"^(?!.*\b((?<!HD[._ -]|HD)DVD|BDRip|720p|MKV|XviD|WMV|d3g|(BD)?REMUX|^(?=.*1080p)(?=.*HEVC)|[xh][-_. ]?26[45]|German.*[DM]L|((?<=\d{4}).*German.*([DM]L)?)(?=.*\b(AVC|HEVC|VC[-_. ]?1|MVC|MPEG[-_. ]?2)\b))\b)(((?=.*\b(Blu[-_. ]?ray|BD|HD[-_. ]?DVD)\b)(?=.*\b(AVC|HEVC|VC[-_. ]?1|MVC|MPEG[-_. ]?2|BDMV|ISO)\b))|^((?=.*\b(((?=.*\b((.*_)?COMPLETE.*|Dis[ck])\b)(?=.*(Blu[-_. ]?ray|HD[-_. ]?DVD)))|3D[-_. ]?BD|BR[-_. ]?DISK|Full[-_. ]?Blu[-_. ]?ray|^((?=.*((BD|UHD)[-_. ]?(25|50|66|100|ISO)))))))).*"
);
static PROPER_RE: Lazy = net_regex!(r"\b(?<proper>proper)\b");
static REPACK_RE: Lazy = net_regex!(r"\b(?<repack>repack\d?|rerip\d?)\b");
static VERSION_RE: Lazy = net_regex!(
    r"\d[-._ ]?v(?<version>\d)[-._ ]|\[v(?<version>\d)\]|repack(?<version>\d)|rerip(?<version>\d)"
);
static REAL_RE: Lazy = net_regex!(r"\b(?<real>REAL)\b", case_sensitive);
static RESOLUTION_RE: Lazy = net_regex!(
    r"\b(?:(?<R360p>360p)|(?<R480p>480p|480i|640x480|848x480)|(?<R540p>540p)|(?<R576p>576p)|(?<R720p>720p|1280x720|960p)|(?<R1080p>1080p|1920x1080|1440p|FHD|1080i|4kto1080p)|(?<R2160p>2160p|3840x2160|4k[-_. ](?:UHD|HEVC|BD|H\.?265)|(?:UHD|HEVC|BD|H\.?265)[-_. ]4k))\b"
);
static ALTERNATIVE_RESOLUTION_RE: Lazy = net_regex!(r"\b(?<R2160p>UHD)\b|(?<R2160p>\[4K\])");
static CODEC_RE: Lazy =
    net_regex!(r"\b(?:(?<x264>x264)|(?<h264>h264)|(?<xvidhd>XvidHD)|(?<xvid>X-?vid)|(?<divx>divx))\b");
static OTHER_SOURCE_RE: Lazy = net_regex!(r"(?<hdtv>HD[-_. ]TV)|(?<sdtv>SD[-_. ]TV)");
static ANIME_BLURAY_RE: Lazy = net_regex!(r"bd(?:720|1080|2160)|(?<=[-_. (\[])bd(?=[-_. )\]])");
static ANIME_WEBDL_RE: Lazy = net_regex!(r"\[WEB\]|[\[\(]WEB[ .]");
static HIGH_DEF_PDTV_RE: Lazy = net_regex!(r"hr[-_. ]ws");
static REMUX_RE: Lazy = net_regex!(
    r"(?:[_. \[]|\d{4}p-|\bHybrid-)(?<remux>(?:(BD|UHD)[-_. ]?)?Remux)\b|(?<remux>(?:(BD|UHD)[-_. ]?)?Remux[_. ]\d{4}p)"
);
static GERMAN_REMUX_RE: Lazy =
    net_regex!(r"((?<=\d{4}).*German.*([DM]L)?)(?=.*\b(AVC|HEVC|VC[_. -]?1|MVC|MPEG[_. -]?2))(?=.*Blu-?ray)");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolution {
    Unknown,
    R360p,
    R480p,
    R540p,
    R576p,
    R720p,
    R1080p,
    R2160p,
}

fn contains_ignore_case(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

/// Radarr's `MediaFileExtensions.GetQualityForExtension`: as Sonarr's,
/// except that a Matroska file is assumed a WEBDL-720p and a transport
/// stream an SDTV recording.
pub fn quality_for_extension(ext: &str) -> i64 {
    match ext.to_ascii_lowercase().as_str() {
        ".mkv" | ".mk3d" => WEBDL720P,
        ".ts" | ".wtv" => SDTV,
        _ => shared_quality_for_extension(ext),
    }
}

/// `StringExtensions.GetPathExtension`: from the last dot, wherever it is.
fn name_extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i + 1 < name.len() => &name[i..],
        _ => "",
    }
}

/// `QualityParser.ParseQuality`: the quality of a release name or file name.
pub fn parse_quality(name: &str) -> ParsedQuality {
    if name.trim().is_empty() {
        return ParsedQuality::default();
    }
    let name = name.trim();
    let mut result = parse_quality_name(name);
    // Based on extension. The only invalid path character on Linux is NUL.
    if result.quality.quality_id == UNKNOWN && !name.contains('\0') {
        result.quality.quality_id = quality_for_extension(path_extension(name));
        result.source_detection = Detection::Extension;
        result.resolution_detection = Detection::Extension;
    }
    result
}

/// `QualityParser.ParseQualityName`: the name only, no extension fallback.
pub fn parse_quality_name(name: &str) -> ParsedQuality {
    let normalized = name.replace('_', " ").trim().to_string();
    let normalized = normalized.as_str();
    let mut result = parse_quality_modifiers(name, normalized);

    let set = |result: &mut ParsedQuality, q: i64| result.quality.quality_id = q;

    let sources = SOURCE_RE.captures_iter(normalized);
    let source_match = sources.last();
    let resolution = parse_resolution(normalized);
    let codec = CODEC_RE.captures(normalized);
    let codec_has = |g: &str| codec.as_ref().is_some_and(|c| c.success(g));
    let remux = REMUX_RE.is_match(normalized) || GERMAN_REMUX_RE.is_match(normalized);
    let br_disk = BRDISK_RE.is_match(normalized);

    if RAW_HD_RE.is_match(normalized) && !br_disk {
        result.source_detection = Detection::Name;
        result.resolution_detection = Detection::Name;
        set(&mut result, RAWHD);
        return result;
    }

    if resolution != Resolution::Unknown {
        result.resolution_detection = Detection::Name;
    }

    if let Some(source) = &source_match {
        result.source_detection = Detection::Name;

        if source.success("bluray") {
            if br_disk {
                set(&mut result, BRDISK);
                return result;
            }
            if codec_has("xvid") || codec_has("divx") {
                set(&mut result, BLURAY480P);
                return result;
            }
            match resolution {
                Resolution::R2160p => set(&mut result, if remux { REMUX2160P } else { BLURAY2160P }),
                Resolution::R1080p => set(&mut result, if remux { REMUX1080P } else { BLURAY1080P }),
                Resolution::R720p => set(&mut result, BLURAY720P),
                Resolution::R576p => set(&mut result, BLURAY576P),
                Resolution::R360p | Resolution::R480p | Resolution::R540p => set(&mut result, BLURAY480P),
                // Treat a remux without a source as 1080p, not 720p.
                Resolution::Unknown if remux => set(&mut result, REMUX1080P),
                Resolution::Unknown => set(&mut result, BLURAY720P),
            }
            return result;
        }

        if source.success("webdl") {
            match resolution {
                Resolution::R2160p => set(&mut result, WEBDL2160P),
                Resolution::R1080p => set(&mut result, WEBDL1080P),
                Resolution::R720p => set(&mut result, WEBDL720P),
                _ if name.contains("[WEBDL]") => set(&mut result, WEBDL720P),
                _ => set(&mut result, WEBDL480P),
            }
            return result;
        }

        if source.success("webrip") {
            match resolution {
                Resolution::R2160p => set(&mut result, WEBRIP2160P),
                Resolution::R1080p => set(&mut result, WEBRIP1080P),
                Resolution::R720p => set(&mut result, WEBRIP720P),
                _ => set(&mut result, WEBRIP480P),
            }
            return result;
        }

        for (group, quality) in [
            ("scr", DVDSCR),
            ("cam", CAM),
            ("ts", TELESYNC),
            ("tc", TELECINE),
            ("wp", WORKPRINT),
            ("regional", REGIONAL),
        ] {
            if source.success(group) {
                set(&mut result, quality);
                return result;
            }
        }

        if source.success("hdtv") {
            if MPEG2_RE.is_match(normalized) {
                set(&mut result, RAWHD);
                return result;
            }
            match resolution {
                Resolution::R2160p => set(&mut result, HDTV2160P),
                Resolution::R1080p => set(&mut result, HDTV1080P),
                Resolution::R720p => set(&mut result, HDTV720P),
                _ if name.contains("[HDTV]") => set(&mut result, HDTV720P),
                _ => set(&mut result, SDTV),
            }
            return result;
        }

        if source.success("bdrip") || source.success("brrip") {
            match resolution {
                Resolution::R720p => set(&mut result, BLURAY720P),
                Resolution::R1080p => set(&mut result, BLURAY1080P),
                Resolution::R2160p => set(&mut result, BLURAY2160P),
                Resolution::R576p => set(&mut result, BLURAY576P),
                _ => set(&mut result, BLURAY480P),
            }
            return result;
        }

        if source.success("dvdr") {
            set(&mut result, DVDR);
            return result;
        }

        if source.success("dvd") {
            set(&mut result, DVD);
            return result;
        }

        if source.success("pdtv")
            || source.success("sdtv")
            || source.success("dsr")
            || source.success("tvrip")
        {
            if resolution == Resolution::R1080p || contains_ignore_case(normalized, "1080p") {
                set(&mut result, HDTV1080P);
                return result;
            }
            if resolution == Resolution::R720p || contains_ignore_case(normalized, "720p") {
                set(&mut result, HDTV720P);
                return result;
            }
            if HIGH_DEF_PDTV_RE.is_match(normalized) {
                result.resolution_detection = Detection::Name;
                set(&mut result, HDTV720P);
                return result;
            }
            set(&mut result, SDTV);
            return result;
        }
    }

    if source_match.is_none() && remux && resolution != Resolution::Unknown {
        result.source_detection = Detection::Unknown;
        let quality = match resolution {
            Resolution::R480p => Some(BLURAY480P),
            Resolution::R720p => Some(BLURAY720P),
            Resolution::R2160p => Some(REMUX2160P),
            Resolution::R1080p => Some(REMUX1080P),
            _ => None,
        };
        if let Some(quality) = quality {
            set(&mut result, quality);
            return result;
        }
    }

    let low = matches!(
        resolution,
        Resolution::R360p | Resolution::R480p | Resolution::R540p | Resolution::R576p
    );

    // Anime Bluray matching
    if ANIME_BLURAY_RE.is_match(normalized) {
        result.source_detection = Detection::Name;
        if low || contains_ignore_case(normalized, "480p") {
            result.resolution_detection = Detection::Name;
            set(&mut result, DVD);
            return result;
        }
        if resolution == Resolution::R1080p || contains_ignore_case(normalized, "1080p") {
            result.resolution_detection = Detection::Name;
            set(&mut result, if remux { REMUX1080P } else { BLURAY1080P });
            return result;
        }
        if resolution == Resolution::R2160p || contains_ignore_case(normalized, "2160p") {
            result.resolution_detection = Detection::Name;
            set(&mut result, if remux { REMUX2160P } else { BLURAY2160P });
            return result;
        }
        if remux && resolution != Resolution::R720p {
            set(&mut result, REMUX1080P);
            return result;
        }
        set(&mut result, BLURAY720P);
        return result;
    }

    if ANIME_WEBDL_RE.is_match(normalized) {
        result.source_detection = Detection::Name;
        if low || contains_ignore_case(normalized, "480p") {
            result.resolution_detection = Detection::Name;
            set(&mut result, WEBDL480P);
            return result;
        }
        if resolution == Resolution::R1080p || contains_ignore_case(normalized, "1080p") {
            result.resolution_detection = Detection::Name;
            set(&mut result, WEBDL1080P);
            return result;
        }
        if resolution == Resolution::R2160p || contains_ignore_case(normalized, "2160p") {
            result.resolution_detection = Detection::Name;
            set(&mut result, WEBDL2160P);
            return result;
        }
        set(&mut result, WEBDL720P);
        return result;
    }

    if resolution != Resolution::Unknown {
        let mut source = "unknown";
        let mut modifier = "none";
        if remux {
            result.source_detection = Detection::Name;
            source = "bluray";
            modifier = "remux";
        } else {
            let from_ext = quality_for_extension(name_extension(name));
            if from_ext != UNKNOWN {
                result.source_detection = Detection::Extension;
                source = MovieQuality::by_id(from_ext).source;
            }
        }
        let (fallback, pixels) = match resolution {
            Resolution::R2160p => (HDTV2160P, 2160),
            Resolution::R1080p => (HDTV1080P, 1080),
            Resolution::R720p => (HDTV720P, 720),
            _ => (SDTV, 480),
        };
        result.resolution_detection = Detection::Name;
        let quality = if source == "unknown" {
            fallback
        } else {
            find_by_source_and_resolution(source, pixels, modifier)
        };
        set(&mut result, quality);
        return result;
    }

    if codec_has("x264") {
        set(&mut result, SDTV);
        return result;
    }

    if contains_ignore_case(normalized, "848x480") {
        result.resolution_detection = Detection::Name;
        if normalized.contains("dvd") {
            result.source_detection = Detection::Name;
            set(&mut result, DVD);
        } else if contains_ignore_case(normalized, "bluray") {
            result.source_detection = Detection::Name;
            set(&mut result, BLURAY480P);
        } else {
            set(&mut result, SDTV);
        }
        return result;
    }

    for (size, bluray, other) in [
        ("1280x720", BLURAY720P, HDTV720P),
        ("1920x1080", BLURAY1080P, HDTV1080P),
    ] {
        if contains_ignore_case(normalized, size) {
            result.resolution_detection = Detection::Name;
            if contains_ignore_case(normalized, "bluray") {
                result.source_detection = Detection::Name;
                set(&mut result, bluray);
            } else {
                set(&mut result, other);
            }
            return result;
        }
    }

    for (needle, quality) in [
        ("bluray720p", BLURAY720P),
        ("bluray1080p", BLURAY1080P),
        ("bluray2160p", BLURAY2160P),
    ] {
        if contains_ignore_case(normalized, needle) {
            result.source_detection = Detection::Name;
            result.resolution_detection = Detection::Name;
            set(&mut result, quality);
            return result;
        }
    }

    let other = other_source_match(normalized);
    if other != UNKNOWN {
        result.source_detection = Detection::Name;
        set(&mut result, other);
    }

    result
}

/// Radarr's `QualityFinder.FindBySourceAndResolution`.
pub fn find_by_source_and_resolution(source: &str, resolution: i64, modifier: &str) -> i64 {
    let all = || RADARR_QUALITIES.iter().map(|d| d.quality);
    // A perfect three-way match.
    if let Some(q) =
        all().find(|q| q.source == source && q.resolution == resolution && q.modifier == modifier)
    {
        return q.id;
    }
    let wanted = source_number(source);
    // Source and modifier, for the qualities without a resolution.
    let unknown_resolution: Vec<MovieQuality> = all()
        .filter(|q| q.source == source && q.resolution == 0 && q.modifier == modifier && q.id != UNKNOWN)
        .collect();
    if unknown_resolution.len() == 1 {
        return unknown_resolution[0].id;
    }
    if let Some(q) = unknown_resolution
        .iter()
        .find(|q| source_number(q.source) >= wanted)
    {
        return q.id;
    }
    // The nearest source of that modifier and resolution. `Quality.All` is
    // in id order, and the sort by source is stable.
    let mut candidates: Vec<MovieQuality> = all()
        .filter(|q| modifier_number(q.modifier) == modifier_number(modifier) && q.resolution == resolution)
        .collect();
    candidates.sort_by_key(|q| (source_number(q.source), q.id));
    candidates
        .iter()
        .find(|q| source_number(q.source) >= wanted)
        .map(|q| q.id)
        .unwrap_or(UNKNOWN)
}

fn parse_resolution(name: &str) -> Resolution {
    let alt = ALTERNATIVE_RESOLUTION_RE.captures(name);
    if let Some(m) = RESOLUTION_RE.captures(name) {
        for (group, res) in [
            ("R360p", Resolution::R360p),
            ("R480p", Resolution::R480p),
            ("R540p", Resolution::R540p),
            ("R576p", Resolution::R576p),
            ("R720p", Resolution::R720p),
            ("R1080p", Resolution::R1080p),
            ("R2160p", Resolution::R2160p),
        ] {
            if m.success(group) {
                return res;
            }
        }
    }
    if alt.is_some_and(|a| a.success("R2160p")) {
        return Resolution::R2160p;
    }
    Resolution::Unknown
}

fn other_source_match(name: &str) -> i64 {
    match OTHER_SOURCE_RE.captures(name) {
        Some(m) if m.success("sdtv") => SDTV,
        Some(m) if m.success("hdtv") => HDTV720P,
        _ => UNKNOWN,
    }
}

fn parse_quality_modifiers(name: &str, normalized: &str) -> ParsedQuality {
    let mut result = ParsedQuality::default();
    let version = VERSION_RE
        .captures(normalized)
        .and_then(|c| c.value("version").and_then(|v| v.parse::<i64>().ok()));
    if let Some(v) = version {
        result.quality.version = v;
        result.revision_detection = Detection::Name;
    }
    if PROPER_RE.is_match(normalized) {
        result.quality.version = version.map(|v| v + 1).unwrap_or(2);
        result.revision_detection = Detection::Name;
    }
    if REPACK_RE.is_match(normalized) {
        result.quality.version = version.map(|v| v + 1).unwrap_or(2);
        result.quality.is_repack = true;
        result.revision_detection = Detection::Name;
    }
    let reals = REAL_RE.captures_iter(name).len() as i64;
    if reals > 0 {
        result.quality.real = reals;
        result.revision_detection = Detection::Name;
    }
    result
}
