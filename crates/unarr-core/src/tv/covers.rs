//! `MediaCoverService`: a series' images downloaded under
//! `MediaCover/{seriesId}/` and the thumbnails the UI asks for, and the
//! local URLs the API hands out for them.

use std::path::{Path, PathBuf};

use crate::metadata::augment::Augment;
use crate::model::{Image, Series};

/// The kinds the originals keep a file for (`MediaCoverTypes` minus
/// `Unknown`), with the extension of that file.
fn extension(cover_type: &str) -> &'static str {
    if cover_type == "clearlogo" {
        ".png"
    } else {
        ".jpg"
    }
}

/// `EnsureResizedCovers`: the heights of each kind's thumbnails.
fn thumbnail_heights(cover_type: &str) -> &'static [u32] {
    match cover_type {
        "poster" | "headshot" => &[500, 250],
        "banner" => &[70, 35],
        "fanart" | "screenshot" => &[360, 180],
        _ => &[],
    }
}

/// `GetCoverPath`.
pub fn cover_path(root: &Path, series_id: i64, cover_type: &str, height: Option<u32>) -> PathBuf {
    let suffix = height.map(|h| format!("-{h}")).unwrap_or_default();
    root.join(series_id.to_string())
        .join(format!("{cover_type}{suffix}{}", extension(cover_type)))
}

/// `ConvertToLocalUrls` for a series in the library: the local URL, with
/// `?lastWrite=` and the file's modification time in .NET ticks when the
/// file is there. Unknown kinds keep no URL of their own.
pub fn local_url(root: &Path, url_base: &str, series_id: i64, cover_type: &str) -> Option<String> {
    if cover_type == "unknown" {
        return None;
    }
    let mut url = format!(
        "{url_base}/MediaCover/{series_id}/{cover_type}{}",
        extension(cover_type)
    );
    if let Some(modified) = last_write(root, series_id, cover_type) {
        url.push_str(&format!("?lastWrite={}", dotnet_ticks(modified)));
    }
    Some(url)
}

/// When each cover was last written, remembered: a list of a thousand
/// series would otherwise ask the file system four thousand times. Keyed
/// by series and kind, with the folder the answer is for.
type LastWrites = std::collections::HashMap<(i64, String), (PathBuf, Option<std::time::SystemTime>)>;

static LAST_WRITES: std::sync::LazyLock<std::sync::Mutex<LastWrites>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(LastWrites::new()));

/// The modification time of a series' cover, `None` when there is no file.
/// Covers change through [`ensure_covers`] and [`delete_covers`], which
/// forget what was remembered; a file replaced by hand shows at the next
/// refresh of its series or the next start.
fn last_write(root: &Path, series_id: i64, cover_type: &str) -> Option<std::time::SystemTime> {
    let key = (series_id, cover_type.to_string());
    {
        let known = LAST_WRITES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((folder, modified)) = known.get(&key) {
            if folder == root {
                return *modified;
            }
        }
    }
    let modified = std::fs::metadata(cover_path(root, series_id, cover_type, None))
        .and_then(|m| m.modified())
        .ok();
    LAST_WRITES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (root.to_path_buf(), modified));
    modified
}

/// Forget when a series' covers were written: they are about to change,
/// or just did.
fn forget_last_writes(series_id: i64) {
    LAST_WRITES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(id, _), _| *id != series_id);
}

/// `DateTime.Ticks` of a file time as the originals read it: local time,
/// 100 ns since 0001-01-01. The container runs in UTC, as the originals'
/// images do.
fn dotnet_ticks(t: std::time::SystemTime) -> i64 {
    const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
    let since = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64 / 100)
        .unwrap_or(0);
    let offset_ticks = chrono::Local::now().offset().local_minus_utc() as i64 * 10_000_000;
    UNIX_EPOCH_TICKS + since + offset_ticks
}

/// `MediaCoverProxy.RegisterUrl`: the URL a series not in the library shows
/// its images through, `/MediaCoverProxy/{sha256 of the url}/{file name}`.
pub fn proxy_url(url_base: &str, remote_url: &str) -> String {
    let hash = ring::digest::digest(&ring::digest::SHA256, remote_url.as_bytes());
    let hex: String = hash.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    let file = remote_url.rsplit('/').next().unwrap_or("");
    format!("{url_base}/MediaCoverProxy/{hex}/{file}")
}

/// `EnsureCovers` (`HandleAsync(SeriesUpdatedEvent)`): download what is
/// missing or changed size, then the thumbnails. Returns whether anything
/// was downloaded (`MediaCoversUpdatedEvent.Updated`).
pub async fn ensure_covers(root: &Path, augment: &Augment, series: &Series) -> bool {
    ensure_covers_of(root, augment, series.id, &series.title, &series.images).await
}

/// The same for whatever the folder `MediaCover/{id}/` belongs to: a
/// series here, a movie in Radarr, whose `MediaCoverService` is the same
/// code over another model.
pub async fn ensure_covers_of(
    root: &Path,
    augment: &Augment,
    id: i64,
    title: &str,
    images: &[Image],
) -> bool {
    forget_last_writes(id);
    let mut updated = false;
    let mut to_resize: Vec<(&Image, bool)> = Vec::new();
    for cover in images {
        if cover.cover_type == "unknown" {
            continue;
        }
        let Some(remote) = cover.remote_url.as_deref() else {
            continue;
        };
        let file = cover_path(root, id, &cover.cover_type, None);
        let mut already_exists = false;
        let outcome: anyhow::Result<()> = async {
            already_exists = already_exists_with_size(augment, remote, &file).await?;
            if !already_exists {
                tracing::info!("Downloading {} for {} {}", cover.cover_type, title, remote);
                let body = augment.download(remote).await?;
                write_atomically(&file, &body).await?;
                updated = true;
            }
            Ok(())
        }
        .await;
        if let Err(e) = outcome {
            tracing::warn!("Couldn't download media cover for {}. {e}", title);
        }
        to_resize.push((cover, already_exists));
    }
    for (cover, already_exists) in to_resize {
        let (root, kind) = (root.to_path_buf(), cover.cover_type.clone());
        let title = title.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            ensure_resized(&root, id, &kind, !already_exists, &title);
        })
        .await;
    }
    forget_last_writes(id);
    updated
}

/// `CoverAlreadyExistsSpecification`: the file is there and as large as the
/// server says the remote one is.
async fn already_exists_with_size(augment: &Augment, url: &str, file: &Path) -> anyhow::Result<bool> {
    let Ok(meta) = tokio::fs::metadata(file).await else {
        return Ok(false);
    };
    let length = augment.content_length(url).await?;
    Ok(length == Some(meta.len()))
}

async fn write_atomically(file: &Path, body: &[u8]) -> anyhow::Result<()> {
    if let Some(dir) = file.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let partial = file.with_extension("part");
    tokio::fs::write(&partial, body).await?;
    tokio::fs::rename(&partial, file).await?;
    Ok(())
}

/// `EnsureResizedCovers`: each thumbnail made again when the image was
/// downloaded now, or when it is missing or empty.
fn ensure_resized(root: &Path, series_id: i64, cover_type: &str, force: bool, title: &str) {
    let main = cover_path(root, series_id, cover_type, None);
    for &height in thumbnail_heights(cover_type) {
        let thumb = cover_path(root, series_id, cover_type, Some(height));
        let missing = std::fs::metadata(&thumb).map(|m| m.len() == 0).unwrap_or(true);
        if !force && !missing {
            continue;
        }
        tracing::debug!("Resizing {cover_type}-{height} for {title}");
        if let Err(e) = resize(&main, &thumb, height) {
            let _ = std::fs::remove_file(&thumb);
            tracing::debug!(
                error = %e,
                "Couldn't resize media cover {cover_type}-{height} for {title}, using full size image instead."
            );
        }
    }
}

/// `ImageResizer.Resize`: to `height`, the width following the ratio, in
/// the source's format (JPEG at quality 92).
pub fn resize(source: &Path, destination: &Path, height: u32) -> anyhow::Result<()> {
    let image = image::ImageReader::open(source)?
        .with_guessed_format()?
        .decode()?;
    let width = ((image.width() as f64) * (height as f64) / (image.height().max(1) as f64))
        .round()
        .max(1.0) as u32;
    let resized = image.resize_exact(width, height, image::imageops::FilterType::CatmullRom);
    let mut out = std::io::BufWriter::new(std::fs::File::create(destination)?);
    match destination.extension().and_then(|e| e.to_str()) {
        Some("png") => resized.write_to(&mut out, image::ImageFormat::Png)?,
        _ => {
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92);
            resized.to_rgb8().write_with_encoder(encoder)?;
        }
    }
    Ok(())
}

/// `HandleAsync(SeriesDeletedEvent)`: the series' folder of covers, gone.
pub async fn delete_covers(root: &Path, series_id: i64) {
    forget_last_writes(series_id);
    let dir = root.join(series_id.to_string());
    if tokio::fs::metadata(&dir).await.is_ok() {
        if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(error = %e, "Couldn't delete the media covers of series {series_id}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_urls_follow_the_original() {
        let root = Path::new("/config/MediaCover");
        assert_eq!(
            cover_path(root, 5, "poster", Some(250)),
            Path::new("/config/MediaCover/5/poster-250.jpg")
        );
        assert_eq!(
            cover_path(root, 5, "clearlogo", None),
            Path::new("/config/MediaCover/5/clearlogo.png")
        );
        assert_eq!(
            local_url(root, "/sonarr", 5, "banner").as_deref(),
            Some("/sonarr/MediaCover/5/banner.jpg")
        );
        assert_eq!(local_url(root, "", 5, "unknown"), None);
        // Recorded from the original for Breaking Bad's banner.
        assert_eq!(
            proxy_url("", "https://artworks.thetvdb.com/banners/graphical/81189-g21.jpg"),
            "/MediaCoverProxy/11d8491574928723a7cb4ad413a946048a3b5fa113294a9ad48bb9888919ad4d/81189-g21.jpg"
        );
    }

    #[test]
    fn thumbnails_keep_the_ratio() {
        let dir = std::env::temp_dir().join(format!("unarr_covers_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fixtures =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/metadata/artwork");
        let thumb = dir.join("poster-250.jpg");
        resize(&fixtures.join("poster.jpg"), &thumb, 250).unwrap();
        let made = image::open(&thumb).unwrap();
        assert_eq!((made.width(), made.height()), (170, 250));
        let logo = dir.join("clearlogo-70.png");
        resize(&fixtures.join("clearlogo.png"), &logo, 70).unwrap();
        assert_eq!(image::open(&logo).unwrap().height(), 70);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Radarr 7 limits the decoders of its resizer; Unarr's decodes JPEG and
    /// PNG alone (the `image` crate's features), so a cover in another
    /// format, a GIF here, is not resized and the full image is served.
    #[test]
    fn a_cover_in_another_format_is_not_resized() {
        let dir = std::env::temp_dir().join(format!("unarr_covers_gif_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A GIF of one pixel.
        let gif: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";
        let source = dir.join("poster.jpg");
        std::fs::write(&source, gif).unwrap();
        let err = resize(&source, &dir.join("poster-250.jpg"), 250).unwrap_err();
        assert!(err.to_string().contains("Gif"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_last_write_is_remembered_until_the_covers_change() {
        let root = std::env::temp_dir().join(format!("unarr_cover_times_{}", std::process::id()));
        let id = 987_654;
        let file = cover_path(&root, id, "poster", None);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        // No file yet: a plain URL, and that is remembered too.
        assert_eq!(
            local_url(&root, "", id, "poster").unwrap(),
            format!("/MediaCover/{id}/poster.jpg")
        );
        std::fs::write(&file, b"jpeg").unwrap();
        assert!(!local_url(&root, "", id, "poster").unwrap().contains("lastWrite"));
        // What writes covers forgets, and the next list asks the disk.
        forget_last_writes(id);
        let first = local_url(&root, "", id, "poster").unwrap();
        assert!(first.contains("?lastWrite="), "{first}");
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(90);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .and_then(|f| f.set_modified(later))
            .unwrap();
        assert_eq!(local_url(&root, "", id, "poster").unwrap(), first);
        forget_last_writes(id);
        assert_ne!(local_url(&root, "", id, "poster").unwrap(), first);
        // Another folder is another answer.
        let elsewhere = root.join("elsewhere");
        assert!(!local_url(&elsewhere, "", id, "poster")
            .unwrap()
            .contains("lastWrite"));
        forget_last_writes(id);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
