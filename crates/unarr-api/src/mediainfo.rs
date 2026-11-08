//! Media info as the API presents it, derived from the ffprobe summary the
//! originals store per file (`MediaInfoModel`, schema revision 11 and
//! earlier). Codec names follow the originals' `MediaInfoFormatter`.

use serde_json::Value;

use crate::resources::MediaInfoResource;

fn str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn int(v: &Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
}

/// Whole numbers as integers, others as given (so `2` stays `2`, not `2.0`).
fn number(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        Value::from(f as i64)
    } else {
        Value::from(f)
    }
}

fn joined(v: &Value, key: &str) -> Option<String> {
    match v.get(key) {
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("/"),
        ),
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

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
        .map(unarr_core::customformat::strip_media_extension)
        .unwrap_or_default()
        .to_ascii_lowercase();
    tokens
        .iter()
        .find(|t| name.contains(&t.to_ascii_lowercase()))
        .or(tokens.last())
        .map(|t| t.to_string())
        .unwrap_or_default()
}

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

fn dynamic_range_type(hdr: &str) -> &'static str {
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

/// Build the resource from the stored JSON. `None` when nothing was stored.
pub fn resource(raw: Option<&Value>, scene_name: Option<&str>) -> Option<MediaInfoResource> {
    let raw = raw?;
    let positions = str(raw, "audioChannelPositions").unwrap_or("");
    // Positions like "5.1" win over the channel count, as in the originals.
    let from_positions = positions
        .get(..3)
        .filter(|p| p.as_bytes()[0].is_ascii_digit() && &p[1..2] == "." && p.as_bytes()[2].is_ascii_digit())
        .and_then(|p| p.parse::<f64>().ok())
        .filter(|f| *f != 0.0);
    let audio_channels = from_positions.unwrap_or(int(raw, "audioChannels") as f64);
    let hdr = str(raw, "videoHdrFormat").unwrap_or("none");
    let fps = raw
        .get("videoFps")
        .and_then(Value::as_f64)
        .map(|f| (f * 1000.0).round() / 1000.0)
        .unwrap_or(0.0);

    Some(MediaInfoResource {
        audio_bitrate: int(raw, "audioBitrate"),
        audio_channels: number(audio_channels),
        audio_codec: audio_codec(
            str(raw, "audioFormat"),
            str(raw, "audioCodecID").unwrap_or(""),
            str(raw, "audioProfile").unwrap_or(""),
        ),
        audio_languages: joined(raw, "audioLanguages"),
        audio_stream_count: int(raw, "audioStreamCount"),
        video_bit_depth: int(raw, "videoBitDepth"),
        video_bitrate: int(raw, "videoBitrate"),
        video_codec: video_codec(
            str(raw, "videoFormat"),
            str(raw, "videoCodecID").unwrap_or(""),
            scene_name,
        ),
        video_fps: number(fps),
        video_dynamic_range: if hdr != "none" && !hdr.is_empty() {
            "HDR"
        } else {
            ""
        }
        .to_string(),
        video_dynamic_range_type: dynamic_range_type(hdr).to_string(),
        resolution: format!("{}x{}", int(raw, "width"), int(raw, "height")),
        run_time: format_runtime(str(raw, "runTime").unwrap_or("00:00:00")),
        scan_type: str(raw, "scanType").map(str::to_string),
        subtitles: joined(raw, "subtitles"),
    })
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
        let raw = serde_json::json!({
            "videoFormat": "h264", "videoCodecID": "x264", "width": 1920, "height": 1080,
            "audioFormat": "aac", "audioChannels": 2, "audioChannelPositions": "stereo",
            "runTime": "00:11:00.0210000", "videoFps": 1, "videoHdrFormat": "none",
            "audioLanguages": [], "subtitles": [], "scanType": "Progressive", "audioStreamCount": 1,
            "videoBitDepth": 8,
        });
        let r = resource(Some(&raw), Some("Firefly.S01E01.1080p.BluRay.x264-DEMAND")).unwrap();
        assert_eq!(r.resolution, "1920x1080");
        assert_eq!(r.audio_channels, serde_json::json!(2));
        assert_eq!(r.audio_languages.as_deref(), Some(""));
        assert_eq!(r.run_time, "11:00");
        let surround =
            serde_json::json!({"audioChannelPositions": "5.1", "audioChannels": 6, "runTime": "00:45:00"});
        assert_eq!(
            resource(Some(&surround), None).unwrap().audio_channels,
            serde_json::json!(5.1)
        );
        assert!(resource(None, None).is_none());
    }
}
