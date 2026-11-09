//! `MediaInfoFormatter`: the codec names, runtime and dynamic range the
//! originals derive from a file's media info, for the API's resource, the
//! naming tokens and the notifications.

use serde_json::Value;

use crate::naming::NamingMediaInfo;

pub fn str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

pub fn int(v: &Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
}

/// The string list under `key`, as the model stores it.
pub fn list(v: &Value, key: &str) -> Vec<String> {
    match v.get(key) {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).map(String::from).collect(),
        Some(Value::String(s)) if !s.is_empty() => s.split('/').map(String::from).collect(),
        _ => Vec::new(),
    }
}

/// `FormatAudioCodec`.
pub fn audio_codec(format: Option<&str>, codec_id: &str, profile: &str) -> Option<String> {
    let format = format?;
    if format.trim().is_empty() {
        return Some(String::new());
    }
    let out = match format {
        _ if codec_id == "thd+" => "TrueHD Atmos",
        "truehd" => "TrueHD",
        "flac" => "FLAC",
        "dts" => match profile {
            "DTS:X" => "DTS-X",
            "DTS-HD MA" => "DTS-HD MA",
            "DTS-ES" => "DTS-ES",
            "DTS-HD HRA" => "DTS-HD HRA",
            "DTS Express" => "DTS Express",
            "DTS 96/24" => "DTS 96/24",
            _ => "DTS",
        },
        _ if codec_id == "ec+3" => "EAC3 Atmos",
        "eac3" => "EAC3",
        "ac3" => "AC3",
        "aac" => {
            if codec_id == "A_AAC/MPEG4/LC/SBR" {
                "HE-AAC"
            } else {
                "AAC"
            }
        }
        "mp3" => "MP3",
        "mp2" => "MP2",
        "opus" => "Opus",
        f if f.starts_with("pcm_") || f.starts_with("adpcm_") => "PCM",
        "vorbis" => "Vorbis",
        "wmav1" | "wmav2" | "wmapro" => "WMA",
        other => other,
    };
    Some(out.to_string())
}

fn scene_name_match(scene_name: Option<&str>, tokens: &[&str]) -> String {
    let name = scene_name
        .map(crate::customformat::strip_media_extension)
        .unwrap_or_default()
        .to_ascii_lowercase();
    tokens
        .iter()
        .find(|t| name.contains(&t.to_ascii_lowercase()))
        .or(tokens.last())
        .map(|t| t.to_string())
        .unwrap_or_default()
}

/// `FormatVideoCodec`.
pub fn video_codec(format: Option<&str>, codec_id: &str, scene_name: Option<&str>) -> Option<String> {
    let format = format?;
    let result = format.trim();
    if result.is_empty() {
        return Some(result.to_string());
    }
    let out = match format {
        _ if codec_id == "x264" => "x264".to_string(),
        "h264" => scene_name_match(scene_name, &["AVC", "x264", "h264"]),
        _ if codec_id == "x265" => "x265".to_string(),
        "hevc" => scene_name_match(scene_name, &["HEVC", "x265", "h265"]),
        "mpeg2video" => "MPEG2".to_string(),
        "mpeg1video" => "MPEG".to_string(),
        f if f == "mpeg4" || f.contains("msmpeg4") => match codec_id {
            "XVID" => "XviD".to_string(),
            "DIV3" | "DX50" => "DivX".to_string(),
            c if c.eq_ignore_ascii_case("divx") => "DivX".to_string(),
            _ => String::new(),
        },
        "vc1" => "VC1".to_string(),
        "av1" => "AV1".to_string(),
        f if f.contains("vp6") => "VP6".to_string(),
        "vp7" | "vp8" | "vp9" => format.to_ascii_uppercase(),
        "wmv1" | "wmv2" | "wmv3" => "WMV".to_string(),
        "qtrle" | "rpza" | "rv10" | "rv20" | "rv30" | "rv40" | "cinepak" | "rawvideo" | "msvideo1" => {
            String::new()
        }
        other => other.to_string(),
    };
    Some(out)
}

/// `H:MM:SS` when there are hours, else `M:SS`, from a .NET TimeSpan string.
pub fn format_runtime(timespan: &str) -> String {
    let main = timespan.split('.').next().unwrap_or("");
    let parts: Vec<i64> = main.split(':').filter_map(|p| p.parse().ok()).collect();
    let (h, m, s) = match parts.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => (0, *m, *s),
        _ => (0, 0, 0),
    };
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `FormatVideoDynamicRangeType`.
pub fn dynamic_range_type(hdr: &str) -> &'static str {
    match hdr {
        "dolbyVision" => "DV",
        "dolbyVisionHdr10" => "DV HDR10",
        "dolbyVisionHdr10Plus" => "DV HDR10Plus",
        "dolbyVisionHlg" => "DV HLG",
        "dolbyVisionSdr" => "DV SDR",
        "hdr10" => "HDR10",
        "hdr10Plus" => "HDR10Plus",
        "hlg10" => "HLG",
        "pq10" => "PQ",
        "unknownHdr" => "HDR",
        _ => "",
    }
}

/// `FormatVideoDynamicRange`: `HDR` for any HDR format, else empty.
pub fn dynamic_range(hdr: &str) -> &'static str {
    if hdr != "none" && !hdr.is_empty() {
        "HDR"
    } else {
        ""
    }
}

/// `FormatAudioChannels`: positions like `5.1` win over the channel count.
pub fn audio_channels(raw: &Value) -> f64 {
    let positions = str(raw, "audioChannelPositions").unwrap_or("");
    let from_positions = positions
        .get(..3)
        .filter(|p| p.as_bytes()[0].is_ascii_digit() && &p[1..2] == "." && p.as_bytes()[2].is_ascii_digit())
        .and_then(|p| p.parse::<f64>().ok())
        .filter(|f| *f != 0.0);
    from_positions.unwrap_or(int(raw, "audioChannels") as f64)
}

/// The media info the naming tokens read, from the stored model.
pub fn naming_media_info(raw: &Value, scene_name: Option<&str>) -> NamingMediaInfo {
    let hdr = str(raw, "videoHdrFormat").unwrap_or("none");
    NamingMediaInfo {
        video_codec: video_codec(
            str(raw, "videoFormat"),
            str(raw, "videoCodecID").unwrap_or(""),
            scene_name,
        )
        .unwrap_or_default(),
        video_bit_depth: int(raw, "videoBitDepth"),
        audio_codec: audio_codec(
            str(raw, "audioFormat"),
            str(raw, "audioCodecID").unwrap_or(""),
            str(raw, "audioProfile").unwrap_or(""),
        )
        .unwrap_or_default(),
        audio_channels: audio_channels(raw),
        audio_languages: list(raw, "audioLanguages"),
        subtitles: list(raw, "subtitles"),
        video_dynamic_range: dynamic_range(hdr).to_string(),
        video_dynamic_range_type: dynamic_range_type(hdr).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_the_originals() {
        assert_eq!(format_runtime("00:11:00.0210000"), "11:00");
        assert_eq!(format_runtime("01:02:03"), "1:02:03");
        assert_eq!(audio_codec(Some("aac"), "", "LC").as_deref(), Some("AAC"));
        assert_eq!(
            audio_codec(Some("dts"), "", "DTS-HD MA").as_deref(),
            Some("DTS-HD MA")
        );
        assert_eq!(video_codec(Some("h264"), "x264", None).as_deref(), Some("x264"));
        assert_eq!(
            video_codec(Some("h264"), "", Some("Show.S01E01.1080p.WEB.H264-GRP")).as_deref(),
            Some("h264")
        );
        assert_eq!(
            video_codec(Some("hevc"), "", Some("Show.S01E01.1080p.WEB.x265-GRP")).as_deref(),
            Some("x265")
        );
        let raw = serde_json::json!({"audioChannelPositions": "5.1", "audioChannels": 6});
        assert_eq!(audio_channels(&raw), 5.1);
        let raw = serde_json::json!({"audioChannels": 2, "audioLanguages": ["eng", "ita"], "videoHdrFormat": "hdr10"});
        let n = naming_media_info(&raw, None);
        assert_eq!(n.audio_channels, 2.0);
        assert_eq!(n.audio_languages, vec!["eng", "ita"]);
        assert_eq!(n.video_dynamic_range, "HDR");
        assert_eq!(n.video_dynamic_range_type, "HDR10");
    }
}
