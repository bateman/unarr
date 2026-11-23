//! Media info as the API presents it, derived from the ffprobe summary the
//! originals store per file (`MediaInfoModel`). Codec names follow the
//! originals' `MediaInfoFormatter`. Radarr 7 stores one entry per audio and
//! subtitle stream and answers them as `audioStreams` and
//! `subtitleStreams`, beside the older fields it now derives from the
//! primary stream; Sonarr, at its pin, has neither.

use serde_json::{Map, Value};

use crate::resources::MediaInfoResource;

use unarr_core::mediafiles::format::{
    self, audio_channels, dynamic_range, dynamic_range_type, int, str, AudioStream,
};
pub use unarr_core::mediafiles::format::{audio_codec, format_runtime, video_codec};
use unarr_core::Personality;

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

/// The languages of a list of streams, joined as the originals'
/// `ConcatToString("/")` does; `None` when the list itself is missing.
fn joined_languages(streams: Option<&Vec<Value>>) -> Option<String> {
    streams.map(|s| {
        s.iter()
            .filter_map(|x| str(x, "language"))
            .collect::<Vec<_>>()
            .join("/")
    })
}

/// A map without its nulls: the API leaves them out.
fn without_nulls(mut m: Map<String, Value>) -> Value {
    m.retain(|_, v| !v.is_null());
    Value::Object(m)
}

/// `MediaInfoAudioStreamResource`.
fn audio_stream_resource(stream: &Value) -> Value {
    let a = AudioStream::of_stream(stream);
    let mut m = Map::new();
    m.insert(
        "language".into(),
        stream.get("language").cloned().unwrap_or(Value::Null),
    );
    m.insert("codec".into(), a.codec().map(Value::from).unwrap_or(Value::Null));
    m.insert(
        "codecId".into(),
        a.codec_id.map(Value::from).unwrap_or(Value::Null),
    );
    m.insert(
        "profile".into(),
        a.profile.map(Value::from).unwrap_or(Value::Null),
    );
    m.insert("bitrate".into(), Value::from(a.bitrate));
    m.insert("channels".into(), number(a.channels_formatted()));
    m.insert(
        "channelPositions".into(),
        a.channel_positions.map(Value::from).unwrap_or(Value::Null),
    );
    m.insert(
        "title".into(),
        stream.get("title").cloned().unwrap_or(Value::Null),
    );
    without_nulls(m)
}

/// `MediaInfoSubtitleStreamResource`.
fn subtitle_stream_resource(stream: &Value) -> Value {
    let mut m = Map::new();
    for key in ["language", "format", "title", "forced", "hearingImpaired"] {
        m.insert(key.into(), stream.get(key).cloned().unwrap_or(Value::Null));
    }
    without_nulls(m)
}

/// Build the resource from the stored JSON. `None` when nothing was stored.
/// `personality` picks the original whose resource it is: Radarr's has the
/// streams, Sonarr's not.
pub fn resource(
    raw: Option<&Value>,
    scene_name: Option<&str>,
    personality: Personality,
) -> Option<MediaInfoResource> {
    let raw = raw?;
    let primary = format::primary_audio(raw);
    let audio_streams = format::audio_streams(raw);
    let subtitle_streams = format::subtitle_streams(raw);
    let streams_shape = personality == Personality::Radarr;
    let audio_channels = audio_channels(raw);
    let hdr = str(raw, "videoHdrFormat").unwrap_or("none");
    let fps = raw
        .get("videoFps")
        .and_then(Value::as_f64)
        .map(|f| (f * 1000.0).round() / 1000.0)
        .unwrap_or(0.0);

    Some(MediaInfoResource {
        audio_bitrate: primary.bitrate,
        audio_channels: number(audio_channels),
        audio_codec: primary.codec(),
        audio_languages: match audio_streams {
            Some(_) => joined_languages(audio_streams),
            None => joined(raw, "audioLanguages"),
        },
        audio_stream_count: format::audio_stream_count(raw),
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
        subtitles: match subtitle_streams {
            Some(_) => joined_languages(subtitle_streams),
            None => joined(raw, "subtitles"),
        },
        audio_streams: audio_streams
            .filter(|_| streams_shape)
            .map(|s| s.iter().map(audio_stream_resource).collect()),
        subtitle_streams: subtitle_streams
            .filter(|_| streams_shape)
            .map(|s| s.iter().map(subtitle_stream_resource).collect()),
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
        let r = resource(
            Some(&raw),
            Some("Firefly.S01E01.1080p.BluRay.x264-DEMAND"),
            Personality::Sonarr,
        )
        .unwrap();
        assert_eq!(r.resolution, "1920x1080");
        assert_eq!(r.audio_channels, serde_json::json!(2));
        assert_eq!(r.audio_languages.as_deref(), Some(""));
        assert_eq!(r.run_time, "11:00");
        let surround =
            serde_json::json!({"audioChannelPositions": "5.1", "audioChannels": 6, "runTime": "00:45:00"});
        assert_eq!(
            resource(Some(&surround), None, Personality::Sonarr)
                .unwrap()
                .audio_channels,
            serde_json::json!(5.1)
        );
        assert!(resource(None, None, Personality::Sonarr).is_none());
    }

    /// Radarr 7 answers the streams beside the older fields, which it now
    /// derives from the first audio stream (moviefile-1 of the goldens).
    #[test]
    fn radarr_answers_the_streams() {
        let raw = serde_json::json!({
            "schemaRevision": 14, "videoFormat": "hevc", "width": 3840, "height": 2160,
            "runTime": "01:47:06.7200000", "audioStreamCount": 1,
            "audioStreams": [{"language": "eng", "format": "eac3", "codecId": "[0][0][0][0]",
                "profile": null, "bitrate": 256000, "channels": 6, "channelPositions": "5.1(side)"}],
            "subtitleStreams": [{"language": "eng"}, {"language": "eng"}],
        });
        let r = serde_json::to_value(resource(Some(&raw), None, Personality::Radarr).unwrap()).unwrap();
        assert_eq!(r["audioCodec"], "EAC3");
        assert_eq!(r["audioBitrate"], 256000);
        assert_eq!(r["audioChannels"], 5.1);
        assert_eq!(r["audioLanguages"], "eng");
        assert_eq!(r["audioStreamCount"], 1);
        assert_eq!(r["subtitles"], "eng/eng");
        assert_eq!(
            r["audioStreams"],
            serde_json::json!([{"language": "eng", "codec": "EAC3", "codecId": "[0][0][0][0]",
                "bitrate": 256000, "channels": 5.1, "channelPositions": "5.1(side)"}])
        );
        assert_eq!(
            r["subtitleStreams"],
            serde_json::json!([{"language": "eng"}, {"language": "eng"}])
        );
    }

    /// Sonarr, at its pin, has no streams in its answer.
    #[test]
    fn sonarr_answers_no_streams() {
        let raw = serde_json::json!({"audioFormat": "aac", "audioLanguages": ["eng"], "subtitles": []});
        let r = serde_json::to_value(resource(Some(&raw), None, Personality::Sonarr).unwrap()).unwrap();
        assert!(r.get("audioStreams").is_none() && r.get("subtitleStreams").is_none());
        assert_eq!(r["audioCodec"], "AAC");
        assert_eq!(r["audioLanguages"], "eng");
    }
}
