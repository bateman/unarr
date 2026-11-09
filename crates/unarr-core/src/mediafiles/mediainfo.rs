//! Media info through ffprobe, after the originals' `VideoFileInfoReader`:
//! the same ffprobe invocation (FFMpegCore's arguments), the same choice of
//! primary streams, the same fields in the `MediaInfoModel` JSON the
//! database keeps (schema revision 11), the same HDR detection. The API's
//! `mediaInfo` resource and the naming tokens are rendered from that JSON.
//!
//! ffprobe is not shipped: [`ffprobe_path`] takes `UNARR_FFPROBE`, then
//! `ffprobe` on the `PATH`. The originals bundle a build of their own
//! (ffprobe 5.1 "Openur"), which reports `x264` as the codec tag of streams
//! that an x264 encoder wrote where a stock ffprobe reports the container's
//! tag; the tests use that build when `make ui` left it in `.cache/sonarr`,
//! so that file names and media info compare equal to the goldens.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Map, Value};

pub const CURRENT_MEDIA_INFO_SCHEMA_REVISION: i64 = 11;
pub const MINIMUM_MEDIA_INFO_SCHEMA_REVISION: i64 = 8;

const VALID_HDR_COLOUR_PRIMARIES: &[&str] = &["bt2020"];
const HLG_TRANSFER_FUNCTIONS: &[&str] = &["arib-std-b67"];
const PQ_TRANSFER_FUNCTIONS: &[&str] = &["smpte2084"];

/// What a probe yields: the stored model and the container title the
/// quality augmenter reads (not stored, as the originals' `[JsonIgnore]`).
#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    pub model: Value,
    pub title: Option<String>,
}

impl MediaInfo {
    pub fn run_time_seconds(&self) -> f64 {
        self.model
            .get("runTime")
            .and_then(Value::as_str)
            .map(parse_timespan_seconds)
            .unwrap_or(0.0)
    }

    pub fn audio_stream_count(&self) -> i64 {
        int(&self.model, "audioStreamCount")
    }

    pub fn width(&self) -> i64 {
        int(&self.model, "width")
    }

    pub fn height(&self) -> i64 {
        int(&self.model, "height")
    }

    pub fn audio_languages(&self) -> Vec<String> {
        self.model
            .get("audioLanguages")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
            .unwrap_or_default()
    }
}

fn int(v: &Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
}

/// `[d.]hh:mm:ss[.fffffff]` to seconds.
pub fn parse_timespan_seconds(s: &str) -> f64 {
    let (days, rest) = match s.split_once('.') {
        Some((_, rest)) if !rest.contains(':') => ("0", s),
        Some((d, rest)) if d.chars().all(|c| c.is_ascii_digit()) && !d.is_empty() && rest.contains(':') => {
            (d, rest)
        }
        _ => ("0", s),
    };
    let mut parts = rest.split(':');
    let h: f64 = parts.next().and_then(|p| p.trim().parse().ok()).unwrap_or(0.0);
    let m: f64 = parts.next().and_then(|p| p.trim().parse().ok()).unwrap_or(0.0);
    let sec: f64 = parts.next().and_then(|p| p.trim().parse().ok()).unwrap_or(0.0);
    let days: f64 = days.parse().unwrap_or(0.0);
    days * 86400.0 + h * 3600.0 + m * 60.0 + sec
}

/// Seconds to the .NET `TimeSpan` default format: `[d.]hh:mm:ss[.fffffff]`,
/// the fraction only when there is one.
pub fn format_timespan(seconds: f64) -> String {
    let total_ticks = (seconds * 10_000_000.0).round() as i64;
    let days = total_ticks / (86400 * 10_000_000);
    let rem = total_ticks % (86400 * 10_000_000);
    let h = rem / (3600 * 10_000_000);
    let m = rem % (3600 * 10_000_000) / (60 * 10_000_000);
    let s = rem % (60 * 10_000_000) / 10_000_000;
    let ticks = rem % 10_000_000;
    let mut out = String::new();
    if days > 0 {
        out.push_str(&format!("{days}."));
    }
    out.push_str(&format!("{h:02}:{m:02}:{s:02}"));
    if ticks > 0 {
        out.push_str(&format!(".{ticks:07}"));
    }
    out
}

/// The ffprobe binary: `UNARR_FFPROBE`, else `ffprobe` on the `PATH`.
pub fn ffprobe_path() -> PathBuf {
    if let Some(p) = override_path() {
        return p;
    }
    std::env::var_os("UNARR_FFPROBE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ffprobe"))
}

static OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

fn override_path() -> Option<PathBuf> {
    OVERRIDE.lock().unwrap().clone()
}

/// Point every probe at `path` (the daemon's `--ffprobe`, the tests'
/// bundled build).
pub fn set_ffprobe_path(path: Option<PathBuf>) {
    *OVERRIDE.lock().unwrap() = path;
}

/// Whether ffprobe answers at all.
pub fn ffprobe_available() -> bool {
    Command::new(ffprobe_path())
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `ffprobe -version`'s first line, for the logs and the system page.
pub fn ffprobe_version() -> Option<String> {
    let out = Command::new(ffprobe_path()).arg("-version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(str::to_string)
}

fn run_ffprobe(args: &[&str], file: &Path) -> anyhow::Result<Value> {
    let out = Command::new(ffprobe_path())
        .args(args)
        .arg(file)
        .output()
        .map_err(|e| anyhow::anyhow!("unable to start ffprobe ({}): {e}", ffprobe_path().display()))?;
    if !out.status.success() {
        anyhow::bail!(
            "ffprobe failed on {}: {}",
            file.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// `FFProbe.GetStreamJson`: FFMpegCore's arguments plus the originals' probe
/// size.
fn stream_json(file: &Path, extra: &[&str]) -> anyhow::Result<Value> {
    let mut args = vec![
        "-loglevel",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-sexagesimal",
        "-show_streams",
    ];
    args.extend_from_slice(extra);
    run_ffprobe(&args, file)
}

/// `FFProbe.GetFrameJson` of the first frame of one video stream.
fn frame_json(file: &Path, stream_index: i64) -> anyhow::Result<Value> {
    let select = format!("v:{stream_index}");
    run_ffprobe(
        &[
            "-loglevel",
            "error",
            "-print_format",
            "json",
            "-show_frames",
            "-v",
            "quiet",
            "-sexagesimal",
            "-read_intervals",
            "%+#1",
            "-select_streams",
            &select,
        ],
        file,
    )
}

/// `FFProbe.GetPixelFormats`, once: the bit depth of each pixel format.
fn pixel_format_bit_depth(name: &str) -> Option<i64> {
    static FORMATS: OnceLock<Vec<(String, i64)>> = OnceLock::new();
    let formats = FORMATS.get_or_init(|| {
        let out = Command::new(ffprobe_path())
            .args([
                "-loglevel",
                "error",
                "-print_format",
                "json",
                "-show_pixel_formats",
            ])
            .output();
        let Ok(out) = out else {
            return Vec::new();
        };
        let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else {
            return Vec::new();
        };
        v["pixel_formats"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        let name = p["name"].as_str()?.to_string();
                        let depth = p["components"]
                            .as_array()?
                            .iter()
                            .filter_map(|c| c["bit_depth"].as_i64())
                            .min()?;
                        Some((name, depth))
                    })
                    .collect()
            })
            .unwrap_or_default()
    });
    formats.iter().find(|(n, _)| n == name).map(|(_, d)| *d)
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// FFMpegCore's `ParseDuration`: the stream's `duration`, else its
/// `DURATION` tag (Matroska), as seconds.
fn stream_duration(stream: &Value) -> Option<f64> {
    if let Some(d) = str_of(stream, "duration") {
        return Some(parse_duration_text(d));
    }
    let tags = stream.get("tags")?;
    let tag = tags
        .as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("DURATION") || k.eq_ignore_ascii_case("DURATION-eng"))
        .and_then(|(_, v)| v.as_str())?;
    Some(parse_duration_text(tag))
}

/// `0:10:10.128000`, `00:10:10.000000000` or plain seconds.
fn parse_duration_text(text: &str) -> f64 {
    if text.contains(':') {
        parse_timespan_seconds(text)
    } else {
        text.trim().parse().unwrap_or(0.0)
    }
}

/// FFMpegCore's `DivideRatio(ParseRatioDouble(r_frame_rate))`.
fn frame_rate(stream: &Value) -> f64 {
    let Some(ratio) = str_of(stream, "r_frame_rate") else {
        return 0.0;
    };
    let (num, den) = ratio.split_once('/').unwrap_or((ratio, "1"));
    let num: f64 = num.trim().parse().unwrap_or(0.0);
    let den: f64 = den.trim().parse().unwrap_or(1.0);
    if den == 0.0 {
        0.0
    } else {
        num / den
    }
}

fn bit_rate(stream: &Value) -> i64 {
    str_of(stream, "bit_rate")
        .and_then(|b| b.trim().parse().ok())
        .unwrap_or(0)
}

fn language(stream: &Value) -> Option<String> {
    stream
        .get("tags")
        .and_then(|t| t.get("language"))
        .and_then(Value::as_str)
        .filter(|l| !l.trim().is_empty())
        .map(String::from)
}

/// The video stream the originals describe: the first one that is not a
/// cover image when there are several.
fn primary_video<'a>(streams: &[&'a Value]) -> Option<&'a Value> {
    let videos: Vec<&&Value> = streams.iter().filter(|s| s["codec_type"] == "video").collect();
    if videos.len() <= 1 {
        return videos.first().copied().copied();
    }
    videos
        .iter()
        .find(|s| !matches!(str_of(s, "codec_name"), Some("mjpeg") | Some("png")))
        .or(videos.first())
        .copied()
        .copied()
}

fn side_data_types(stream: &Value) -> Vec<String> {
    stream["side_data_list"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| d["side_data_type"].as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn dovi_record(stream: &Value) -> Option<&Value> {
    stream["side_data_list"]
        .as_array()?
        .iter()
        .find(|d| d["side_data_type"] == "DOVI configuration record")
}

/// `GetHdrFormat`, the enum member in the camel case the JSON carries.
pub fn hdr_format(
    bit_depth: i64,
    colour_primaries: Option<&str>,
    transfer: Option<&str>,
    side_data: &[String],
    dovi: Option<&Value>,
) -> &'static str {
    if bit_depth < 10 {
        return "none";
    }
    let has = |t: &str| side_data.iter().any(|d| d == t);
    if let Some(dovi) = dovi {
        let hdr10_plus = has("HDR Dynamic Metadata SMPTE2094-40 (HDR10+)");
        return match dovi["dv_bl_signal_compatibility_id"].as_i64().unwrap_or(0) {
            1 | 6 => {
                if hdr10_plus {
                    "dolbyVisionHdr10Plus"
                } else {
                    "dolbyVisionHdr10"
                }
            }
            2 => "dolbyVisionSdr",
            4 => "dolbyVisionHlg",
            _ => "dolbyVision",
        };
    }
    let primaries_ok = colour_primaries.is_some_and(|p| VALID_HDR_COLOUR_PRIMARIES.contains(&p));
    let transfer_ok =
        transfer.is_some_and(|t| HLG_TRANSFER_FUNCTIONS.contains(&t) || PQ_TRANSFER_FUNCTIONS.contains(&t));
    if !primaries_ok || !transfer_ok {
        return "none";
    }
    let transfer = transfer.unwrap_or("");
    if HLG_TRANSFER_FUNCTIONS.contains(&transfer) {
        return "hlg10";
    }
    if PQ_TRANSFER_FUNCTIONS.contains(&transfer) {
        if has("HDR Dynamic Metadata SMPTE2094-40 (HDR10+)") {
            return "hdr10Plus";
        }
        if has("Mastering display metadata") || has("Content light level metadata") {
            return "hdr10";
        }
        return "pq10";
    }
    "none"
}

/// `GetMediaInfo`: `None` when the file is a disk image or a stream file,
/// or when ffprobe cannot read it (logged, as the originals do).
pub fn probe(file: &Path) -> Option<MediaInfo> {
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_ascii_lowercase()))
        .unwrap_or_default();
    if crate::parser::media_extensions::is_disk_or_stream_extension(&ext) {
        return None;
    }
    match probe_inner(file) {
        Ok(info) => Some(info),
        Err(e) => {
            tracing::error!(file = %file.display(), error = %e, "unable to parse media info from file");
            None
        }
    }
}

fn probe_inner(file: &Path) -> anyhow::Result<MediaInfo> {
    if !file.is_file() {
        anyhow::bail!("media file does not exist: {}", file.display());
    }
    let mut analysis = stream_json(file, &["-probesize", "50000000"])?;
    let audio_layout_missing = |a: &Value| {
        a["streams"]
            .as_array()
            .and_then(|s| s.iter().find(|s| s["codec_type"] == "audio"))
            .is_none_or(|s| str_of(s, "channel_layout").is_none())
    };
    if audio_layout_missing(&analysis) {
        analysis = stream_json(
            file,
            &["-probesize", "150000000", "-analyzeduration", "150000000"],
        )?;
    }
    let empty = Vec::new();
    let streams: Vec<&Value> = analysis["streams"].as_array().unwrap_or(&empty).iter().collect();
    let video = primary_video(&streams);
    let audio_streams: Vec<&Value> = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .copied()
        .collect();
    let subtitle_streams: Vec<&Value> = streams
        .iter()
        .filter(|s| s["codec_type"] == "subtitle")
        .copied()
        .collect();
    let audio = audio_streams.first().copied();
    let format = &analysis["format"];

    let bit_depth = video
        .and_then(|v| str_of(v, "pix_fmt"))
        .and_then(pixel_format_bit_depth)
        .unwrap_or(8);
    let colour_primaries = video.and_then(|v| str_of(v, "color_primaries"));
    let transfer = video.and_then(|v| str_of(v, "color_transfer"));
    let mut side_data = video.map(side_data_types).unwrap_or_default();
    let mut dovi = video.and_then(dovi_record).cloned();
    // PQ10 or similar: the first frame says which flavour of HDR it is.
    if transfer.is_some_and(|t| PQ_TRANSFER_FUNCTIONS.contains(&t)) {
        let index = video.and_then(|v| v["index"].as_i64()).unwrap_or(0);
        // ffprobe counts `v:N` among video streams, not all streams.
        let video_position = streams
            .iter()
            .filter(|s| s["codec_type"] == "video")
            .position(|s| s["index"].as_i64() == Some(index))
            .unwrap_or(0) as i64;
        if let Ok(frames) = frame_json(file, video_position) {
            if let Some(first) = frames["frames"].as_array().and_then(|f| f.first()) {
                side_data.extend(side_data_types(first));
                if dovi.is_none() {
                    dovi = dovi_record(first).cloned();
                }
            }
        }
    }
    let hdr = hdr_format(bit_depth, colour_primaries, transfer, &side_data, dovi.as_ref());

    let general_duration = str_of(format, "duration").map(parse_duration_text).unwrap_or(0.0);
    let run_time = best_runtime(
        audio.and_then(stream_duration),
        video.and_then(stream_duration),
        general_duration,
    );

    let mut model = Map::new();
    let mut put = |k: &str, v: Value| {
        if !v.is_null() {
            model.insert(k.to_string(), v);
        }
    };
    put("schemaRevision", json!(CURRENT_MEDIA_INFO_SCHEMA_REVISION));
    put("containerFormat", opt(str_of(format, "format_name")));
    put("videoFormat", opt(video.and_then(|v| str_of(v, "codec_name"))));
    put(
        "videoCodecID",
        opt(video.and_then(|v| str_of(v, "codec_tag_string"))),
    );
    put("videoProfile", opt(video.and_then(|v| str_of(v, "profile"))));
    put("videoBitrate", json!(video.map(bit_rate).unwrap_or(0)));
    put("videoBitDepth", json!(bit_depth));
    put("videoColourPrimaries", opt(colour_primaries));
    put("videoTransferCharacteristics", opt(transfer));
    if let Some(d) = &dovi {
        put(
            "doviConfigurationRecord",
            json!({
                "dvVersionMajor": d["dv_version_major"].as_i64().unwrap_or(0),
                "dvVersionMinor": d["dv_version_minor"].as_i64().unwrap_or(0),
                "dvProfile": d["dv_profile"].as_i64().unwrap_or(0),
                "dvLevel": d["dv_level"].as_i64().unwrap_or(0),
                "rpuPresentFlag": d["rpu_present_flag"].as_i64().unwrap_or(0),
                "elPresentFlag": d["el_present_flag"].as_i64().unwrap_or(0),
                "blPresentFlag": d["bl_present_flag"].as_i64().unwrap_or(0),
                "dvBlSignalCompatibilityId": d["dv_bl_signal_compatibility_id"].as_i64().unwrap_or(0),
            }),
        );
    }
    put("videoHdrFormat", json!(hdr));
    put(
        "height",
        json!(video.and_then(|v| v["height"].as_i64()).unwrap_or(0)),
    );
    put(
        "width",
        json!(video.and_then(|v| v["width"].as_i64()).unwrap_or(0)),
    );
    put("audioFormat", opt(audio.and_then(|a| str_of(a, "codec_name"))));
    put(
        "audioCodecID",
        opt(audio.and_then(|a| str_of(a, "codec_tag_string"))),
    );
    put("audioProfile", opt(audio.and_then(|a| str_of(a, "profile"))));
    put("audioBitrate", json!(audio.map(bit_rate).unwrap_or(0)));
    put("runTime", json!(format_timespan(run_time)));
    put("audioStreamCount", json!(audio_streams.len()));
    put(
        "audioChannels",
        json!(audio.and_then(|a| a["channels"].as_i64()).unwrap_or(0)),
    );
    put(
        "audioChannelPositions",
        opt(audio.and_then(|a| str_of(a, "channel_layout"))),
    );
    put("videoFps", json!(video.map(frame_rate).unwrap_or(0.0)));
    put(
        "audioLanguages",
        json!(audio_streams
            .iter()
            .filter_map(|s| language(s))
            .collect::<Vec<_>>()),
    );
    put(
        "subtitles",
        json!(subtitle_streams
            .iter()
            .filter_map(|s| language(s))
            .collect::<Vec<_>>()),
    );
    put("scanType", json!("Progressive"));

    let title = format
        .get("tags")
        .and_then(|t| t.as_object())
        .and_then(|t| t.iter().find(|(k, _)| k.eq_ignore_ascii_case("title")))
        .and_then(|(_, v)| v.as_str())
        .map(String::from);
    Ok(MediaInfo {
        model: Value::Object(model),
        title,
    })
}

fn opt(s: Option<&str>) -> Value {
    match s {
        Some(s) => Value::from(s),
        None => Value::Null,
    }
}

/// `GetBestRuntime`: the video's, else the audio's, else the container's.
fn best_runtime(audio: Option<f64>, video: Option<f64>, general: f64) -> f64 {
    match video {
        Some(v) if v > 0.0 => v,
        _ => match audio {
            Some(a) if a > 0.0 => a,
            _ => general,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timespans_round_trip() {
        assert_eq!(format_timespan(610.0), "00:10:10");
        assert_eq!(format_timespan(1743.872), "00:29:03.8720000");
        assert_eq!(format_timespan(90061.5), "1.01:01:01.5000000");
        assert_eq!(parse_timespan_seconds("00:29:03.8720000"), 1743.872);
        assert_eq!(parse_timespan_seconds("1.01:01:01.5000000"), 90061.5);
        assert_eq!(parse_timespan_seconds("0:10:10.128000"), 610.128);
        assert_eq!(parse_duration_text("12.5"), 12.5);
    }

    #[test]
    fn hdr_formats_follow_the_originals() {
        assert_eq!(
            hdr_format(8, Some("bt2020"), Some("smpte2084"), &[], None),
            "none"
        );
        assert_eq!(
            hdr_format(10, Some("bt709"), Some("smpte2084"), &[], None),
            "none"
        );
        assert_eq!(
            hdr_format(10, Some("bt2020"), Some("smpte2084"), &[], None),
            "pq10"
        );
        assert_eq!(
            hdr_format(
                10,
                Some("bt2020"),
                Some("smpte2084"),
                &["Mastering display metadata".into()],
                None
            ),
            "hdr10"
        );
        assert_eq!(
            hdr_format(
                10,
                Some("bt2020"),
                Some("smpte2084"),
                &["HDR Dynamic Metadata SMPTE2094-40 (HDR10+)".into()],
                None
            ),
            "hdr10Plus"
        );
        assert_eq!(
            hdr_format(10, Some("bt2020"), Some("arib-std-b67"), &[], None),
            "hlg10"
        );
        let dovi = json!({"dv_bl_signal_compatibility_id": 1});
        assert_eq!(hdr_format(10, None, None, &[], Some(&dovi)), "dolbyVisionHdr10");
        let dovi = json!({"dv_bl_signal_compatibility_id": 0});
        assert_eq!(hdr_format(10, None, None, &[], Some(&dovi)), "dolbyVision");
    }

    fn clip(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../tests/fixtures/media/{name}"))
    }

    #[test]
    fn probes_the_fixture_clips() {
        if !ffprobe_available() {
            eprintln!("ffprobe not available, skipping");
            return;
        }
        let info = probe(&clip("episode-dual.mkv")).expect("a readable clip");
        let m = &info.model;
        assert_eq!(m["schemaRevision"], 11);
        assert_eq!(m["containerFormat"], "matroska,webm");
        assert_eq!(m["videoFormat"], "h264");
        assert_eq!(m["width"], 64);
        assert_eq!(m["height"], 64);
        assert_eq!(m["videoBitDepth"], 8);
        assert_eq!(m["videoHdrFormat"], "none");
        assert_eq!(m["audioFormat"], "aac");
        assert_eq!(m["audioProfile"], "LC");
        assert_eq!(m["audioStreamCount"], 2);
        assert_eq!(m["audioChannels"], 1);
        assert_eq!(m["audioChannelPositions"], "mono");
        assert_eq!(m["videoFps"], 2.0);
        assert_eq!(m["runTime"], "00:10:10");
        assert_eq!(m["audioLanguages"], json!(["eng", "ita"]));
        assert_eq!(m["subtitles"], json!(["eng", "ita"]));
        assert_eq!(m["scanType"], "Progressive");
        assert_eq!(info.run_time_seconds(), 610.0);
        assert!(probe(&clip("nothing.mkv")).is_none());
        assert!(
            probe(&clip("episode.iso")).is_none(),
            "disk images are not probed"
        );
        let short = probe(&clip("sample.mkv")).unwrap();
        assert_eq!(short.run_time_seconds(), 45.0);
    }
}
