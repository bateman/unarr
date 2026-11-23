//! Media info through ffprobe, after the originals' `VideoFileInfoReader`:
//! the same ffprobe invocation (FFMpegCore's arguments), the same choice of
//! primary streams, the same fields in the `MediaInfoModel` JSON the
//! database keeps (schema revision 11), the same HDR detection. The API's
//! `mediaInfo` resource and the naming tokens are rendered from that JSON.
//!
//! ffprobe is not shipped: [`ffprobe_path`] takes `UNARR_FFPROBE`, then
//! `ffprobe` on the `PATH`. The originals bundle a build of their own
//! (ffprobe 5.1 "Openur"), which reports `x264` as the codec tag of an H.264
//! stream whose first packets carry x264's SEI, and `x265` for an HEVC
//! stream whose extradata carries x265's, where a stock ffprobe reports the
//! container's tag. [`encoder_tag`] finds the same with any ffprobe, so that
//! file names (`[x264]`) and media info are the originals' whichever build
//! runs; the tests still use the originals' build when `make ui` left it in
//! `.cache`.

use crate::mediafiles::format;
use crate::sync::Recover;
use crate::Personality;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Map, Value};

pub const CURRENT_MEDIA_INFO_SCHEMA_REVISION: i64 = 11;
pub const MINIMUM_MEDIA_INFO_SCHEMA_REVISION: i64 = 8;

/// Radarr's revisions since its media info keeps every audio and subtitle
/// stream (Radarr 7, `MediaInfoModel` revision 16): a file probed before is
/// probed again at the next scan.
pub const RADARR_CURRENT_MEDIA_INFO_SCHEMA_REVISION: i64 = 16;
pub const RADARR_MINIMUM_MEDIA_INFO_SCHEMA_REVISION: i64 = 16;

/// The revision a personality's probe writes.
pub fn current_schema_revision(personality: Personality) -> i64 {
    match personality {
        Personality::Radarr => RADARR_CURRENT_MEDIA_INFO_SCHEMA_REVISION,
        Personality::Sonarr | Personality::Lidarr => CURRENT_MEDIA_INFO_SCHEMA_REVISION,
    }
}

/// The oldest revision a personality still reads without probing again.
pub fn minimum_schema_revision(personality: Personality) -> i64 {
    match personality {
        Personality::Radarr => RADARR_MINIMUM_MEDIA_INFO_SCHEMA_REVISION,
        Personality::Sonarr | Personality::Lidarr => MINIMUM_MEDIA_INFO_SCHEMA_REVISION,
    }
}

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
        format::audio_stream_count(&self.model)
    }

    pub fn width(&self) -> i64 {
        int(&self.model, "width")
    }

    pub fn height(&self) -> i64 {
        int(&self.model, "height")
    }

    pub fn audio_languages(&self) -> Vec<String> {
        format::audio_languages(&self.model)
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
    OVERRIDE.lock().recover().clone()
}

/// Point every probe at `path` (the daemon's `--ffprobe`, the tests'
/// bundled build).
pub fn set_ffprobe_path(path: Option<PathBuf>) {
    *OVERRIDE.lock().recover() = path;
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

/// The codec tag the originals' ffprobe gives an x264 or x265 stream,
/// when the ffprobe running gave another: `x264` for an H.264 stream whose
/// first packet holds x264's SEI (`x264 - core`) and that the analysis
/// decoded, `x265` for an HEVC stream whose extradata holds x265's (read
/// when the decoder opens; an HEVC stream without extradata, as in MPEG-TS,
/// keeps its tag there too). Compared with that build on x264 and x265
/// streams in Matroska, MP4, QuickTime, AVI, MPEG-TS and M2TS, with and
/// without the encoder's SEI, and at 1 and 24 frames a second. One more
/// short ffprobe run, for those two codecs only, and none when the tag
/// already says so (the originals' build).
fn encoder_tag(file: &Path, stream: &Value, video_position: i64) -> Option<&'static str> {
    let tag = stream.get("codec_tag_string").and_then(Value::as_str);
    let select = format!("v:{video_position}");
    let (wanted, signature, bytes): (&'static str, &[u8], Vec<u8>) = match str_of(stream, "codec_name")? {
        // The originals' build tags the stream once its first access unit
        // (parameter sets, SEI, slice) went through the codec while the
        // stream was analysed; a stream the analysis did not reach that far
        // has no pixel format either, and keeps its tag there too (a 1080p
        // clip at 1 frame a second, say).
        "h264" if tag != Some("x264") && str_of(stream, "pix_fmt").is_some() => {
            ("x264", b"x264 - core", first_packet_head(file, &select)?)
        }
        "hevc" if tag != Some("x265") => {
            let dump = run_ffprobe(
                &[
                    "-loglevel",
                    "error",
                    "-print_format",
                    "json",
                    "-select_streams",
                    &select,
                    "-show_streams",
                    "-show_data",
                    "-show_entries",
                    "stream=extradata",
                ],
                file,
            )
            .ok()?;
            let extradata = dump["streams"].get(0).and_then(|s| s["extradata"].as_str())?;
            ("x265", b"x265", hex_dump_bytes(extradata))
        }
        _ => return None,
    };
    bytes
        .windows(signature.len())
        .any(|w| w == signature)
        .then_some(wanted)
}

/// How much of the first packet is read: the encoder's SEI comes before
/// the first slice, a few hundred bytes in, and a key frame can weigh
/// megabytes.
const FIRST_PACKET_HEAD: usize = 64 * 1024;

/// The first [`FIRST_PACKET_HEAD`] bytes of a stream's first packet, read
/// from the file where ffprobe says the packet is: asking ffprobe for the
/// data would dump the whole packet in hex. In MPEG-TS (and M2TS, with its
/// four-byte prefix) the packet is spread over 188-byte transport packets,
/// whose headers are taken out, so that the stream's bytes read on.
fn first_packet_head(file: &Path, select: &str) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let lookup = [
        "-loglevel",
        "error",
        "-print_format",
        "json",
        "-select_streams",
        select,
        "-read_intervals",
        "%+#1",
        "-show_packets",
        "-show_entries",
        "packet=pos,size",
    ];
    // Where the packet is needs no analysis of the streams, which decodes
    // a frame and costs as much as the probe itself on a 1080p file; an
    // ffprobe without the option is asked the long way.
    let quick: Vec<&str> = std::iter::once("-nofind_stream_info").chain(lookup).collect();
    let found = run_ffprobe(&quick, file)
        .or_else(|_| run_ffprobe(&lookup, file))
        .ok()?;
    let packet = found["packets"].get(0)?;
    let number = |v: &Value| v.as_u64().or_else(|| v.as_str()?.parse().ok());
    let pos = number(&packet["pos"])?;
    let size = number(&packet["size"])? as usize;
    let mut f = std::fs::File::open(file).ok()?;
    f.seek(SeekFrom::Start(pos)).ok()?;
    // Room for the transport packets' headers when the packet is spread.
    let mut raw = Vec::new();
    f.take((size.min(FIRST_PACKET_HEAD) * 2) as u64)
        .read_to_end(&mut raw)
        .ok()?;
    let mut head = transport_payload(&raw).unwrap_or(raw);
    head.truncate(size.min(FIRST_PACKET_HEAD));
    Some(head)
}

/// The payload of one elementary stream in MPEG-TS bytes (188-byte packets,
/// or 192 with M2TS's timestamp in front), starting at a packet of it;
/// `None` when the bytes are not transport packets.
fn transport_payload(raw: &[u8]) -> Option<Vec<u8>> {
    let (prefix, stride) = [(0, 188), (4, 192)].into_iter().find(|&(prefix, stride)| {
        raw.len() >= prefix + 2 * stride && raw[prefix] == 0x47 && raw[prefix + stride] == 0x47
    })?;
    let pid = |p: &[u8]| (u16::from(p[1] & 0x1f) << 8) | u16::from(p[2]);
    let stream = pid(&raw[prefix..]);
    let mut out = Vec::with_capacity(raw.len());
    for chunk in raw.chunks_exact(stride) {
        let p = &chunk[prefix..];
        if p[0] != 0x47 || pid(p) != stream {
            continue;
        }
        let start = match (p[3] >> 4) & 0x3 {
            0b01 => 4,
            0b11 => 5 + usize::from(p[4]),
            _ => continue,
        };
        if let Some(payload) = p.get(start..) {
            out.extend_from_slice(payload);
        }
    }
    Some(out)
}

/// The bytes of ffprobe's `-show_data` dump: lines of an offset, eight
/// groups of four hex digits and the same bytes as text.
fn hex_dump_bytes(dump: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for line in dump.lines() {
        let Some((_, rest)) = line.split_once(": ") else {
            continue;
        };
        let hex = rest.split("  ").next().unwrap_or("");
        for group in hex.split(' ') {
            let digits = group.as_bytes();
            for pair in digits.chunks(2) {
                let pair = std::str::from_utf8(pair).ok().filter(|p| p.len() == 2);
                if let Some(b) = pair.and_then(|p| u8::from_str_radix(p, 16).ok()) {
                    bytes.push(b);
                }
            }
        }
    }
    bytes
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

/// `GetMediaInfo` in Sonarr's shape: see [`probe_for`].
pub fn probe(file: &Path) -> Option<MediaInfo> {
    probe_for(file, Personality::Sonarr)
}

/// `GetMediaInfo`: `None` when the file is a disk image or a stream file,
/// or when ffprobe cannot read it (logged, as the originals do). The model
/// has the shape of the personality's original: Sonarr's keeps the primary
/// audio stream's fields and the lists of languages, Radarr's one entry per
/// audio and subtitle stream.
pub fn probe_for(file: &Path, personality: Personality) -> Option<MediaInfo> {
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_ascii_lowercase()))
        .unwrap_or_default();
    if crate::parser::media_extensions::is_disk_or_stream_extension(&ext) {
        return None;
    }
    match probe_inner(file, personality) {
        Ok(info) => Some(info),
        Err(e) => {
            tracing::error!(file = %file.display(), error = %e, "unable to parse media info from file");
            None
        }
    }
}

fn probe_inner(file: &Path, personality: Personality) -> anyhow::Result<MediaInfo> {
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
    let index = video.and_then(|v| v["index"].as_i64()).unwrap_or(0);
    // ffprobe counts `v:N` among video streams, not all streams.
    let video_position = streams
        .iter()
        .filter(|s| s["codec_type"] == "video")
        .position(|s| s["index"].as_i64() == Some(index))
        .unwrap_or(0) as i64;
    // PQ10 or similar: the first frame says which flavour of HDR it is.
    if transfer.is_some_and(|t| PQ_TRANSFER_FUNCTIONS.contains(&t)) {
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
    put("schemaRevision", json!(current_schema_revision(personality)));
    put("containerFormat", opt(str_of(format, "format_name")));
    put("videoFormat", opt(video.and_then(|v| str_of(v, "codec_name"))));
    put(
        "videoCodecID",
        opt(video
            .and_then(|v| encoder_tag(file, v, video_position).or_else(|| str_of(v, "codec_tag_string")))),
    );
    put("videoProfile", opt(video.and_then(|v| str_of(v, "profile"))));
    put("videoBitrate", json!(video.map(bit_rate).unwrap_or(0)));
    put(
        "videoMultiViewCount",
        json!(video.map(multi_view_count).unwrap_or(1)),
    );
    put("videoBitDepth", json!(bit_depth));
    put("videoColourPrimaries", opt(colour_primaries));
    put("videoTransferCharacteristics", opt(transfer));
    // Radarr 7 (FFMpegCore 5.4) keeps the record out of the model: it is
    // read for the HDR format alone.
    if let Some(d) = dovi.as_ref().filter(|_| personality != Personality::Radarr) {
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
    let streams_shape = personality == Personality::Radarr;
    if !streams_shape {
        put("audioFormat", opt(audio.and_then(|a| str_of(a, "codec_name"))));
        put(
            "audioCodecID",
            opt(audio.and_then(|a| str_of(a, "codec_tag_string"))),
        );
        put("audioProfile", opt(audio.and_then(|a| str_of(a, "profile"))));
        put("audioBitrate", json!(audio.map(bit_rate).unwrap_or(0)));
    }
    put("runTime", json!(format_timespan(run_time)));
    if !streams_shape {
        put("audioStreamCount", json!(audio_streams.len()));
        put(
            "audioChannels",
            json!(audio.and_then(|a| a["channels"].as_i64()).unwrap_or(0)),
        );
        put(
            "audioChannelPositions",
            opt(audio.and_then(|a| str_of(a, "channel_layout"))),
        );
    }
    put("videoFps", json!(video.map(frame_rate).unwrap_or(0.0)));
    if streams_shape {
        put("audioStreams", Value::Array(audio_stream_models(&audio_streams)));
        put(
            "subtitleStreams",
            Value::Array(subtitle_stream_models(&subtitle_streams)),
        );
    } else {
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
    }
    put("scanType", json!(scan_type(personality, video)));

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

/// `ScanType`: Radarr 7 reads the video's field order (FFMpegCore 5.4);
/// Sonarr, at its pin, writes every file as progressive.
fn scan_type(personality: Personality, video: Option<&Value>) -> &'static str {
    let interlaced = personality == Personality::Radarr
        && video
            .and_then(|v| str_of(v, "field_order"))
            .is_some_and(|o| matches!(o, "tt" | "bb" | "tb" | "bt"));
    if interlaced {
        "Interlaced"
    } else {
        "Progressive"
    }
}

/// A stream's `title` tag, trimmed, when it has one.
fn stream_title(stream: &Value) -> Option<String> {
    let tags = stream.get("tags")?.as_object()?;
    tags.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("title"))
        .and_then(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from)
}

/// A stream's disposition flag, when ffprobe reports it.
fn disposition(stream: &Value, flag: &str) -> Option<bool> {
    stream
        .get("disposition")
        .and_then(|d| d.get(flag))
        .and_then(Value::as_i64)
        .map(|v| v != 0)
}

/// Radarr's `MediaInfoAudioStreamModel` list: every audio stream, in the
/// file's order, `und` for one without a language.
fn audio_stream_models(streams: &[&Value]) -> Vec<Value> {
    let mut sorted: Vec<&&Value> = streams.iter().collect();
    sorted.sort_by_key(|s| s["index"].as_i64().unwrap_or(0));
    sorted
        .into_iter()
        .map(|s| {
            let mut m = Map::new();
            m.insert(
                "language".into(),
                json!(language(s).unwrap_or_else(|| "und".into())),
            );
            m.insert("format".into(), opt(str_of(s, "codec_name")));
            m.insert("codecId".into(), opt(str_of(s, "codec_tag_string")));
            m.insert("profile".into(), opt(str_of(s, "profile")));
            m.insert("bitrate".into(), json!(bit_rate(s)));
            m.insert("channels".into(), json!(s["channels"].as_i64().unwrap_or(0)));
            m.insert("channelPositions".into(), opt(str_of(s, "channel_layout")));
            if let Some(title) = stream_title(s) {
                m.insert("title".into(), json!(title));
            }
            // The original's database serializer leaves nulls out.
            m.retain(|_, v| !v.is_null());
            Value::Object(m)
        })
        .collect()
}

/// Radarr's `MediaInfoSubtitleStreamModel` list: the subtitle streams with
/// a language, in the file's order.
fn subtitle_stream_models(streams: &[&Value]) -> Vec<Value> {
    let mut sorted: Vec<&&Value> = streams.iter().filter(|s| language(s).is_some()).collect();
    sorted.sort_by_key(|s| s["index"].as_i64().unwrap_or(0));
    sorted
        .into_iter()
        .map(|s| {
            let mut m = Map::new();
            m.insert("language".into(), opt(language(s).as_deref()));
            m.insert("format".into(), opt(str_of(s, "codec_name")));
            if let Some(title) = stream_title(s) {
                m.insert("title".into(), json!(title));
            }
            if let Some(forced) = disposition(s, "forced") {
                m.insert("forced".into(), json!(forced));
            }
            if let Some(hi) = disposition(s, "hearing_impaired") {
                m.insert("hearingImpaired".into(), json!(hi));
            }
            m.retain(|_, v| !v.is_null());
            Value::Object(m)
        })
        .collect()
}

/// `VideoMultiViewCount`: two for a stream that carries a `stereo_mode`
/// tag (what Matroska calls a 3D layout), one otherwise. The originals'
/// probe library reads tag names without regard to case.
fn multi_view_count(stream: &Value) -> i64 {
    let stereo = stream["tags"]
        .as_object()
        .is_some_and(|tags| tags.keys().any(|k| k.eq_ignore_ascii_case("stereo_mode")));
    if stereo {
        2
    } else {
        1
    }
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
    fn a_stereo_mode_tag_makes_two_views() {
        assert_eq!(multi_view_count(&json!({"codec_name": "h264"})), 1);
        assert_eq!(multi_view_count(&json!({"tags": {"language": "eng"}})), 1);
        assert_eq!(
            multi_view_count(&json!({"tags": {"stereo_mode": "left_right"}})),
            2
        );
        assert_eq!(
            multi_view_count(&json!({"tags": {"STEREO_MODE": "top_bottom"}})),
            2
        );
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

    /// Radarr 7's shape: one entry per audio and subtitle stream, no flat
    /// audio fields, revision 16; Sonarr's probe of the same clip keeps its
    /// own shape (above).
    #[test]
    fn radarr_probes_every_stream() {
        if !ffprobe_available() {
            eprintln!("ffprobe not available, skipping");
            return;
        }
        let info = probe_for(&clip("episode-dual.mkv"), Personality::Radarr).expect("a readable clip");
        let m = &info.model;
        assert_eq!(m["schemaRevision"], 16);
        for gone in [
            "audioFormat",
            "audioLanguages",
            "subtitles",
            "audioStreamCount",
            "audioChannels",
        ] {
            assert!(m.get(gone).is_none(), "{gone}");
        }
        let audio = m["audioStreams"].as_array().unwrap();
        assert_eq!(audio.len(), 2);
        assert_eq!(audio[0]["language"], "eng");
        assert_eq!(audio[0]["format"], "aac");
        assert_eq!(audio[0]["profile"], "LC");
        assert_eq!(audio[0]["channels"], 1);
        assert_eq!(audio[0]["channelPositions"], "mono");
        assert_eq!(audio[1]["language"], "ita");
        let subs = m["subtitleStreams"].as_array().unwrap();
        assert_eq!(
            subs.iter().map(|s| s["language"].clone()).collect::<Vec<_>>(),
            [json!("eng"), json!("ita")]
        );
        assert!(subs[0]["format"].is_string());
        assert!(subs[0].get("forced").is_some_and(Value::is_boolean));
        assert_eq!(info.audio_stream_count(), 2);
        assert_eq!(info.audio_languages(), vec!["eng", "ita"]);
        assert_eq!(m["videoFormat"], "h264");
        assert_eq!(m["runTime"], "00:10:10");
        assert_eq!(m["scanType"], "Progressive");
        assert!(m.get("doviConfigurationRecord").is_none());
    }

    #[test]
    fn radarr_reads_the_field_order() {
        for (order, radarr) in [
            ("tt", "Interlaced"),
            ("bb", "Interlaced"),
            ("tb", "Interlaced"),
            ("bt", "Interlaced"),
            ("progressive", "Progressive"),
            ("unknown", "Progressive"),
        ] {
            let video = json!({"codec_type": "video", "field_order": order});
            assert_eq!(scan_type(Personality::Radarr, Some(&video)), radarr, "{order}");
            assert_eq!(
                scan_type(Personality::Sonarr, Some(&video)),
                "Progressive",
                "{order}"
            );
        }
        assert_eq!(scan_type(Personality::Radarr, None), "Progressive");
    }

    /// A stream's title is kept trimmed; a missing disposition is no flag.
    #[test]
    fn stream_models_keep_titles_and_flags() {
        let audio = json!({"index": 2, "codec_type": "audio", "codec_name": "ac3",
            "channels": 6, "channel_layout": "5.1(side)", "tags": {"language": "eng", "title": " Main "}});
        let untagged = json!({"index": 1, "codec_type": "audio", "codec_name": "aac"});
        let models = audio_stream_models(&[&audio, &untagged]);
        assert_eq!(models.len(), 2, "in the file's order");
        assert_eq!(
            models[0]["language"], "und",
            "a stream without a language is `und`"
        );
        assert_eq!(models[0]["format"], "aac");
        assert_eq!(models[1]["title"], "Main");
        assert!(models[1].get("profile").is_none(), "nulls are left out");
        let sub = json!({"index": 3, "codec_type": "subtitle", "codec_name": "subrip",
            "tags": {"language": "ita"}, "disposition": {"forced": 1, "hearing_impaired": 0}});
        let models = subtitle_stream_models(&[&sub]);
        assert_eq!(
            models[0],
            json!({"language": "ita", "format": "subrip", "forced": true, "hearingImpaired": false})
        );
    }

    /// Whichever ffprobe runs, an x264 stream is `x264`, as the originals'
    /// build reports it (the clips were encoded with libx264).
    #[test]
    fn an_x264_stream_is_x264_with_any_ffprobe() {
        if !ffprobe_available() {
            eprintln!("ffprobe not available, skipping");
            return;
        }
        for name in ["episode.mkv", "episode-dual.mkv", "sample.mkv"] {
            let info = probe(&clip(name)).expect("a readable clip");
            assert_eq!(info.model["videoCodecID"], "x264", "{name}");
        }
        // At one frame a second the analysis does not reach the first
        // frame: the originals' build leaves the tag as it is.
        let info = probe(&clip("episode-1080.mkv")).expect("a readable clip");
        assert_eq!(info.model["videoCodecID"], "[0][0][0][0]");
    }

    /// A text split over two transport packets reads on once their
    /// headers are out; another stream's packet in between is skipped.
    #[test]
    fn transport_packets_are_read_as_one_stream() {
        let packet = |pid: u16, payload: &[u8]| {
            let mut p = vec![0x47, (pid >> 8) as u8 & 0x1f, pid as u8, 0x10];
            p.extend_from_slice(payload);
            p.resize(188, 0xff);
            p
        };
        let mut first = vec![0u8; 184 - 5];
        first.extend_from_slice(b"x264 ");
        let raw = [
            packet(0x100, &first),
            packet(0x101, b"audio"),
            packet(0x100, b"- core 164"),
        ]
        .concat();
        let payload = transport_payload(&raw).unwrap();
        assert!(payload.windows(11).any(|w| w == b"x264 - core"));
        assert!(!payload.windows(5).any(|w| w == b"audio"));
        let m2ts: Vec<u8> = raw
            .chunks(188)
            .flat_map(|p| [&[0u8; 4][..], p].concat())
            .collect();
        assert_eq!(transport_payload(&m2ts), Some(payload));
        assert_eq!(transport_payload(b"not transport packets"), None);
    }

    #[test]
    fn the_data_dump_is_read_back_to_bytes() {
        let dump = "\n00000000: 0000 02ae 0605 ffff aadc 45e9 bde6 d948  ..........E....H\n\
                    00000010: b796 2cd8 20d9 23ee ef78 3236 3420 2d20  ..,. .#..x264 - \n\
                    00000020: 636f 7265 20                             core \n";
        let bytes = hex_dump_bytes(dump);
        assert_eq!(bytes.len(), 37);
        assert_eq!(&bytes[..4], &[0, 0, 2, 0xae]);
        assert!(bytes.windows(11).any(|w| w == b"x264 - core"));
    }
}
