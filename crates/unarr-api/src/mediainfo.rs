//! Media info as the API presents it, derived from the ffprobe summary the
//! originals store per file (`MediaInfoModel`, schema revision 11 and
//! earlier). Codec names follow the originals' `MediaInfoFormatter`.

use serde_json::Value;

use crate::resources::MediaInfoResource;

use unarr_core::mediafiles::format::{audio_channels, dynamic_range, dynamic_range_type, int, str};
pub use unarr_core::mediafiles::format::{audio_codec, format_runtime, video_codec};

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

/// Build the resource from the stored JSON. `None` when nothing was stored.
pub fn resource(raw: Option<&Value>, scene_name: Option<&str>) -> Option<MediaInfoResource> {
    let raw = raw?;
    let audio_channels = audio_channels(raw);
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
        video_dynamic_range: dynamic_range(hdr).to_string(),
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
