//! Port of the originals' `QualityParser`: source, resolution, proper,
//! repack, real and version from a release name.

use crate::model::QualityModel;
use crate::net_regex;
use crate::parser::media_extensions::{path_extension, quality_for_extension};
use crate::parser::netregex::Lazy;
use crate::quality::{Quality, QualitySource, SONARR_QUALITIES};

/// Where a part of the quality was read from (`QualityDetectionSource`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Detection {
    #[default]
    Unknown,
    Name,
    Extension,
}

/// A quality with the revision and where each part came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuality {
    pub quality: QualityModel,
    pub source_detection: Detection,
    pub resolution_detection: Detection,
    pub revision_detection: Detection,
}

impl Default for ParsedQuality {
    fn default() -> Self {
        ParsedQuality {
            quality: QualityModel {
                quality_id: 0,
                version: 1,
                real: 0,
                is_repack: false,
            },
            source_detection: Detection::Unknown,
            resolution_detection: Detection::Unknown,
            revision_detection: Detection::Unknown,
        }
    }
}

// Quality ids, named as the originals name them.
const UNKNOWN: i64 = 0;
const SDTV: i64 = 1;
const DVD: i64 = 2;
const WEBDL1080P: i64 = 3;
const HDTV720P: i64 = 4;
const WEBDL720P: i64 = 5;
const BLURAY720P: i64 = 6;
const BLURAY1080P: i64 = 7;
const WEBDL480P: i64 = 8;
const HDTV1080P: i64 = 9;
const RAWHD: i64 = 10;
const WEBRIP480P: i64 = 12;
const BLURAY480P: i64 = 13;
const WEBRIP720P: i64 = 14;
const WEBRIP1080P: i64 = 15;
const HDTV2160P: i64 = 16;
const WEBRIP2160P: i64 = 17;
const WEBDL2160P: i64 = 18;
const BLURAY2160P: i64 = 19;
const BLURAY1080P_REMUX: i64 = 20;
const BLURAY2160P_REMUX: i64 = 21;
const BLURAY576P: i64 = 22;

static SOURCE_RE: Lazy = net_regex!(
    r"\b(?:(?<bluray>BluRay|Blu-Ray|HD-?DVD|BDMux|BD(?!$))|(?<webdl>WEB[-_. ]DL(?:mux)?|WEBDL|AmazonHD|AmazonSD|iTunesHD|MaxdomeHD|NetflixU?HD|WebHD|HBOMaxHD|DisneyHD|[. ]WEB[. ](?:[xh][ .]?26[45]|AVC|HEVC|DDP?5[. ]1)|[. ](?-i:WEB)$|(?:720|1080|2160)p[-. ]WEB[-. ]|[-. ]WEB[-. ](?:720|1080|2160)p|\b\s\/\sWEB\s\/\s\b|(?:AMZN|NF|DP)[. -]WEB[. -](?!Rip))|(?<webrip>WebRip|Web-Rip|WEBMux)|(?<hdtv>HDTV)|(?<bdrip>BDRip|BDLight)|(?<brrip>BRRip)|(?<dvd>DVD|DVDRip|NTSC|PAL|xvidvd)|(?<dsr>WS[-_. ]DSR|DSR)|(?<pdtv>PDTV)|(?<sdtv>SDTV)|(?<tvrip>TVRip))(?:\b|$|[ .])"
);
static RAW_HD_RE: Lazy = net_regex!(r"\b(?<rawhd>RawHD|Raw[-_. ]HD)\b");
static MPEG2_RE: Lazy = net_regex!(r"\b(?<mpeg2>MPEG[-_. ]?2)\b", case_sensitive);
static PROPER_RE: Lazy = net_regex!(r"\b(?<proper>proper)\b");
static REPACK_RE: Lazy = net_regex!(r"\b(?<repack>repack\d?|rerip\d?)\b");
static VERSION_RE: Lazy = net_regex!(
    r"\d[-._ ]?v(?<version>\d)[-._ ]|\[v(?<version>\d)\]|repack(?<version>\d)|rerip(?<version>\d)|(?:480|576|720|1080|2160)p[._ ]v(?<version>\d)"
);
static REAL_RE: Lazy = net_regex!(r"\b(?<real>REAL)\b", case_sensitive);
static RESOLUTION_RE: Lazy = net_regex!(
    r"\b(?:(?<R360p>360p)|(?<R480p>480p|480i|640x480|848x480)|(?<R540p>540p)|(?<R576p>576p)|(?<R720p>720p|1280x720|960p)|(?<R1080p>1080p|1920x1080|1440p|FHD|1080i|4kto1080p)|(?<R2160p>2160p|3840x2160|4k[-_. ](?:UHD|HEVC|BD|H265)|(?:UHD|HEVC|BD|H265)[-_. ]4k))\b"
);
static ALTERNATIVE_RESOLUTION_RE: Lazy = net_regex!(r"\b(?<R2160p>UHD)\b|(?<R2160p>\[4K\])");
static CODEC_RE: Lazy =
    net_regex!(r"\b(?:(?<x264>x264)|(?<h264>h264)|(?<xvidhd>XvidHD)|(?<xvid>Xvid)|(?<divx>divx))\b");
static OTHER_SOURCE_RE: Lazy = net_regex!(r"(?<hdtv>HD[-_. ]TV)|(?<sdtv>SD[-_. ]TV)");
static ANIME_BLURAY_RE: Lazy = net_regex!(r"bd(?:720|1080|2160)|(?<=[-_. (\[])bd(?=[-_. )\]])");
static ANIME_WEBDL_RE: Lazy = net_regex!(r"\[WEB\]|[\[\(]WEB[ .]");
static HIGH_DEF_PDTV_RE: Lazy = net_regex!(r"hr[-_. ]ws");
static REMUX_RE: Lazy = net_regex!(
    r"(?:[_. ]|\d{4}p-|\bHybrid-)(?<remux>(?:(BD|UHD)[-_. ]?)?Remux)\b|(?<remux>(?:(BD|UHD)[-_. ]?)?Remux[_. ]\d{4}p)"
);

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

/// `QualityParser.ParseQuality`: the quality of a release name or file name.
pub fn parse_quality(name: &str) -> ParsedQuality {
    if name.trim().is_empty() {
        return ParsedQuality::default();
    }
    let name = name.trim();
    let mut result = parse_quality_name(name);
    // Based on extension. The only invalid path character on Linux is NUL.
    if result.quality.quality_id == UNKNOWN && !name.contains('\0') {
        let from_ext = quality_for_extension(path_extension(name));
        result.quality.quality_id = from_ext;
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

    if RAW_HD_RE.is_match(normalized) {
        result.source_detection = Detection::Name;
        result.resolution_detection = Detection::Name;
        set(&mut result, RAWHD);
        return result;
    }

    let sources = SOURCE_RE.captures_iter(normalized);
    let source_match = sources.last();
    let resolution = parse_resolution(normalized);
    let codec = CODEC_RE.captures(normalized);
    let codec_has = |g: &str| codec.as_ref().is_some_and(|c| c.success(g));
    let remux = REMUX_RE.is_match(normalized);

    if resolution != Resolution::Unknown {
        result.resolution_detection = Detection::Name;
    }

    if let Some(source) = &source_match {
        result.source_detection = Detection::Name;

        if source.success("bluray") {
            if codec_has("xvid") || codec_has("divx") {
                set(&mut result, BLURAY480P);
                return result;
            }
            match resolution {
                Resolution::R2160p => set(&mut result, if remux { BLURAY2160P_REMUX } else { BLURAY2160P }),
                Resolution::R1080p => set(&mut result, if remux { BLURAY1080P_REMUX } else { BLURAY1080P }),
                Resolution::R576p => set(&mut result, BLURAY576P),
                Resolution::R360p | Resolution::R480p | Resolution::R540p => set(&mut result, BLURAY480P),
                // Treat a remux without a source as 1080p, not 720p.
                _ if remux && resolution != Resolution::R720p => set(&mut result, BLURAY1080P_REMUX),
                _ => set(&mut result, BLURAY720P),
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
                _ => set(&mut result, BLURAY480P),
            }
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
        match resolution {
            Resolution::R480p => {
                set(&mut result, BLURAY480P);
                return result;
            }
            Resolution::R720p => {
                set(&mut result, BLURAY720P);
                return result;
            }
            Resolution::R2160p => {
                set(&mut result, BLURAY2160P_REMUX);
                return result;
            }
            Resolution::R1080p => {
                set(&mut result, BLURAY1080P_REMUX);
                return result;
            }
            _ => {}
        }
    }

    // Anime Bluray matching
    if ANIME_BLURAY_RE.is_match(normalized) {
        result.source_detection = Detection::Name;
        if matches!(
            resolution,
            Resolution::R360p | Resolution::R480p | Resolution::R540p | Resolution::R576p
        ) || contains_ignore_case(normalized, "480p")
        {
            result.resolution_detection = Detection::Name;
            set(&mut result, DVD);
            return result;
        }
        if resolution == Resolution::R1080p || contains_ignore_case(normalized, "1080p") {
            result.resolution_detection = Detection::Name;
            set(&mut result, if remux { BLURAY1080P_REMUX } else { BLURAY1080P });
            return result;
        }
        if resolution == Resolution::R2160p || contains_ignore_case(normalized, "2160p") {
            result.resolution_detection = Detection::Name;
            set(&mut result, if remux { BLURAY2160P_REMUX } else { BLURAY2160P });
            return result;
        }
        if remux && resolution != Resolution::R720p {
            set(&mut result, BLURAY1080P_REMUX);
            return result;
        }
        set(&mut result, BLURAY720P);
        return result;
    }

    if ANIME_WEBDL_RE.is_match(normalized) {
        result.source_detection = Detection::Name;
        if matches!(
            resolution,
            Resolution::R360p | Resolution::R480p | Resolution::R540p | Resolution::R576p
        ) || contains_ignore_case(normalized, "480p")
        {
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
        let mut source = QualitySource::Unknown;
        if remux {
            result.source_detection = Detection::Name;
            source = QualitySource::BlurayRaw;
        } else {
            let from_ext = quality_for_extension(path_extension(name));
            if from_ext != UNKNOWN {
                result.source_detection = Detection::Extension;
                source = Quality::by_id(from_ext).source;
            }
        }
        let by_source = |fallback: i64, res: i64| {
            if source == QualitySource::Unknown {
                fallback
            } else {
                find_by_source_and_resolution(source, res)
            }
        };
        match resolution {
            Resolution::R2160p => {
                result.resolution_detection = Detection::Name;
                set(&mut result, by_source(HDTV2160P, 2160));
                return result;
            }
            Resolution::R1080p => {
                result.resolution_detection = Detection::Name;
                set(&mut result, by_source(HDTV1080P, 1080));
                return result;
            }
            Resolution::R720p => {
                result.resolution_detection = Detection::Name;
                set(&mut result, by_source(HDTV720P, 720));
                return result;
            }
            Resolution::R360p | Resolution::R480p | Resolution::R540p | Resolution::R576p => {
                result.resolution_detection = Detection::Name;
                set(&mut result, by_source(SDTV, 480));
                return result;
            }
            Resolution::Unknown => {}
        }
    }

    if codec_has("x264") {
        set(&mut result, SDTV);
        return result;
    }

    if normalized.contains("848x480") {
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

    if contains_ignore_case(normalized, "1280x720") {
        result.resolution_detection = Detection::Name;
        if contains_ignore_case(normalized, "bluray") {
            result.source_detection = Detection::Name;
            set(&mut result, BLURAY720P);
        } else {
            set(&mut result, HDTV720P);
        }
        return result;
    }

    if contains_ignore_case(normalized, "1920x1080") {
        result.resolution_detection = Detection::Name;
        if contains_ignore_case(normalized, "bluray") {
            result.source_detection = Detection::Name;
            set(&mut result, BLURAY1080P);
        } else {
            set(&mut result, HDTV1080P);
        }
        return result;
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

/// `QualityFinder.FindBySourceAndResolution`.
pub fn find_by_source_and_resolution(source: QualitySource, resolution: i64) -> i64 {
    if let Some(q) = SONARR_QUALITIES
        .iter()
        .map(|d| d.quality)
        .find(|q| q.source == source && q.resolution == resolution)
    {
        return q.id;
    }
    // 576p releases with a Television or Web source stay below Bluray 576p.
    if resolution < 720 {
        match source {
            QualitySource::Television => return SDTV,
            QualitySource::Web => return WEBDL480P,
            QualitySource::WebRip => return WEBRIP480P,
            _ => {}
        }
    }
    let ordinal = |s: QualitySource| s as i64;
    let mut same_resolution: Vec<Quality> = SONARR_QUALITIES
        .iter()
        .map(|d| d.quality)
        .filter(|q| q.resolution == resolution)
        .collect();
    same_resolution.sort_by_key(|q| ordinal(q.source));
    same_resolution
        .iter()
        .find(|q| ordinal(q.source) >= ordinal(source))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_few_well_known_names() {
        let q = parse_quality("The.Pitt.S01E01.1080p.AMZN.WEB-DL.DDP5.1.H.264-FLUX");
        assert_eq!(q.quality.quality_id, WEBDL1080P);
        assert_eq!(q.quality.version, 1);
        assert_eq!(q.source_detection, Detection::Name);
        let q = parse_quality("Show.S01E01.PROPER.720p.HDTV.x264-GRP");
        assert_eq!(q.quality.quality_id, HDTV720P);
        assert_eq!(q.quality.version, 2);
        let q = parse_quality("Show.S01E01.REPACK.1080p.BluRay.REMUX.AVC-GRP");
        assert_eq!(q.quality.quality_id, BLURAY1080P_REMUX);
        assert!(q.quality.is_repack);
        let q = parse_quality("Show - S01E01 - Title.mkv");
        assert_eq!(q.quality.quality_id, HDTV720P);
        assert_eq!(q.source_detection, Detection::Extension);
        assert_eq!(find_by_source_and_resolution(QualitySource::Web, 576), WEBDL480P);
        assert_eq!(
            find_by_source_and_resolution(QualitySource::WebRip, 2160),
            WEBRIP2160P
        );
    }
}
