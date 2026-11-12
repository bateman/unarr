//! The files that go with a movie file: subtitles, the files a metadata
//! consumer wrote and whatever else the settings name (`ExtraService`,
//! `ExistingExtraFileService`, `ImportExistingExtraFilesBase`,
//! `ExtraFileManager`, `ExtraFileService`, `SubtitleService`,
//! `ExistingSubtitleImporter`, `OtherExtraService`,
//! `ExistingOtherExtraImporter`, `ExistingMetadataImporter`). The twin of
//! the series side's `mediafiles::extras`.
//!
//! With "Import Extra Files" on, the files next to a download whose
//! extension is listed come into the library with the movie. Whether or
//! not that is on, the extras a scan finds in the movie's folder are
//! remembered, so that they are renamed with the movie file and go to the
//! recycling bin with it. The metadata consumers are not driven: the files
//! they wrote are recognised by a scan, as the original recognises them
//! whether or not a consumer is enabled, and nothing writes, renames or
//! refreshes them.

use std::path::Path;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use fancy_regex::Regex;
use rusqlite::Connection;

use super::aggregate::{self, MOVIE_SUBTITLE_LANGUAGES};
use super::events::MovieImportEvent;
use super::local::LocalMovie;
use super::sample;
use crate::db::Db;
use crate::mediafiles::local::DeleteReason;
use crate::mediafiles::sample::DetectSampleResult;
use crate::mediafiles::scan::{files_in, video_files};
use crate::mediafiles::transfer::{self, TransferError, TransferMode};
use crate::mediafiles::{
    extension, file_name, file_stem, join, mediainfo, parent_path, path_equals, relative_path,
};
use crate::movies::model::{ExtraKind, Movie, MovieExtraFile, MovieFile};
use crate::movies::naming::iso_two_letter_code;
use crate::movies::parser::{parse_movie_path, parse_movie_title};
use crate::movies::store;
use crate::parser::subtitles;
use crate::store::config::MediaManagementConfig;

/// `SubtitleFileExtensions` as Radarr has them: Sonarr's and `.sup`.
const SUBTITLE_EXTENSIONS: &[&str] = &[
    ".aqt", ".ass", ".idx", ".jss", ".psb", ".rt", ".smi", ".srt", ".ssa", ".sub", ".sup", ".txt", ".utf",
    ".utf8", ".utf-8", ".vtt",
];

pub(crate) fn is_subtitle_extension(extension: &str) -> bool {
    SUBTITLE_EXTENSIONS
        .iter()
        .any(|e| e.eq_ignore_ascii_case(extension))
}

/// What the original raises where it reads something that is not there.
const NULL_REFERENCE: &str = "Object reference not set to an instance of an object.";

fn new_extra(
    kind: ExtraKind,
    movie_id: i64,
    movie_file_id: i64,
    relative_path: String,
    extension: &str,
) -> MovieExtraFile {
    MovieExtraFile {
        kind,
        id: 0,
        movie_id,
        movie_file_id: Some(movie_file_id),
        relative_path,
        extension: extension.to_string(),
        added: None,
        last_updated: None,
        language: 0,
        language_tags: Vec::new(),
        title: None,
        copy: 0,
        consumer: None,
        metadata_type: None,
        hash: None,
    }
}

/// `SubtitleFile.AggregateString`: what makes two subtitles of a file
/// copies of each other.
fn aggregate_string(e: &MovieExtraFile) -> String {
    format!(
        "{}{}{}{}",
        e.language,
        e.title.as_deref().unwrap_or(""),
        e.language_tags.join("."),
        e.extension
    )
}

/// `GroupBy(s => s.AggregateString)`: the keys in the order first met.
fn aggregate_keys<'a>(files: impl Iterator<Item = &'a MovieExtraFile>) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for file in files {
        let key = aggregate_string(file);
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

fn full_path(movie: &Movie, extra: &MovieExtraFile) -> String {
    join(&movie.path, &extra.relative_path)
}

// --- the extras a scan finds

/// `IntersectBy` over paths: the rows whose file is among `paths`, one row
/// for each path.
fn intersect_by<'a>(movie: &Movie, rows: &'a [MovieExtraFile], paths: &[String]) -> Vec<&'a MovieExtraFile> {
    let mut keys: Vec<&str> = paths.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    for row in rows {
        let path = full_path(movie, row);
        if let Some(i) = keys.iter().position(|k| path_equals(k, &path)) {
            keys.swap_remove(i);
            out.push(row);
        }
    }
    out
}

/// `ExceptBy` over paths: the rows whose file is not among `paths`, one row
/// for each path.
fn except_by<'a>(movie: &Movie, rows: &'a [MovieExtraFile], paths: &[String]) -> Vec<&'a MovieExtraFile> {
    let mut matched: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for row in rows {
        let path = full_path(movie, row);
        if !paths.iter().any(|p| path_equals(p, &path)) && !matched.iter().any(|p| path_equals(p, &path)) {
            matched.push(path);
            out.push(row);
        }
    }
    out
}

/// `ImportExistingExtraFileFilterResult`.
struct FilterResult {
    previously_imported: Vec<MovieExtraFile>,
    files_on_disk: Vec<String>,
}

/// `ImportExistingExtraFilesBase.Filter`: the rows whose file is still
/// there, and the files that are nobody's yet.
fn filter(
    movie: &Movie,
    files_on_disk: &[String],
    imported_files: &[String],
    rows: &[MovieExtraFile],
) -> FilterResult {
    let previously_imported: Vec<MovieExtraFile> = intersect_by(movie, rows, files_on_disk)
        .into_iter()
        .cloned()
        .collect();
    let known: Vec<String> = previously_imported.iter().map(|e| full_path(movie, e)).collect();
    let mut filtered: Vec<String> = Vec::new();
    for file in files_on_disk {
        let taken = |paths: &[String]| paths.iter().any(|p| path_equals(p, file));
        if !taken(&known) && !taken(imported_files) && !taken(&filtered) {
            filtered.push(file.clone());
        }
    }
    // Files that are already imported are returned so that they are not
    // imported again by the other importers.
    FilterResult {
        previously_imported,
        files_on_disk: filtered,
    }
}

/// `ImportExistingExtraFilesBase.FilterAndClean`: the rows of a kind whose
/// file another importer took or the disk no longer has are removed; with
/// `keep_existing_entries` (an import script renamed the file) the rows of
/// the files on disk are removed instead, to be read again.
fn filter_and_clean(
    conn: &Connection,
    movie: &Movie,
    kind: ExtraKind,
    files_on_disk: &[String],
    imported_files: &[String],
    keep_existing_entries: bool,
) -> anyhow::Result<FilterResult> {
    let rows: Vec<MovieExtraFile> = store::extra_files_of(conn, movie.id)?
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect();
    if keep_existing_entries {
        for incomplete in intersect_by(movie, &rows, files_on_disk) {
            store::delete_extra_file(conn, kind, incomplete.id)?;
        }
        return Ok(filter(movie, files_on_disk, imported_files, &[]));
    }
    // `Clean`.
    let already_imported = intersect_by(movie, &rows, imported_files);
    let deleted = except_by(movie, &rows, files_on_disk);
    for row in already_imported.into_iter().chain(deleted) {
        store::delete_extra_file(conn, kind, row.id)?;
    }
    Ok(filter(movie, files_on_disk, imported_files, &rows))
}

/// The metadata consumers, in the order the original asks them about a
/// file: a file two of them recognise gets a row for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Consumer {
    Xbmc,
    Wdtv,
    Roksbox,
    MediaBrowser,
    Kometa,
}

const CONSUMERS: &[Consumer] = &[
    Consumer::Xbmc,
    Consumer::Wdtv,
    Consumer::Roksbox,
    Consumer::MediaBrowser,
    Consumer::Kometa,
];

impl Consumer {
    /// `GetType().Name`, as `MetadataFile.Consumer` keeps it.
    fn type_name(self) -> &'static str {
        match self {
            Consumer::Xbmc => "XbmcMetadata",
            Consumer::Wdtv => "WdtvMetadata",
            Consumer::Roksbox => "RoksboxMetadata",
            Consumer::MediaBrowser => "MediaBrowserMetadata",
            Consumer::Kometa => "KometaMetadata",
        }
    }
}

/// `MetadataType`.
const MOVIE_METADATA: i64 = 1;
const MOVIE_IMAGE: i64 = 2;

macro_rules! regex {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| Regex::new($pattern).expect("a valid pattern"))
        }
    };
}

// `XbmcMetadata.MovieImagesRegex` and `MovieFileImageRegex`.
regex!(
    xbmc_movie_images,
    r"(?i)^(?<type>poster|banner|fanart|clearart|discart|keyart|landscape|logo|backdrop|clearlogo)\.(?:png|jpe?g)"
);
regex!(
    xbmc_movie_file_image,
    r"(?i)(?<type>-thumb|-poster|-banner|-fanart|-clearart|-discart|-keyart|-landscape|-logo|-backdrop|-clearlogo)\.(?:png|jpe?g)"
);
// `KometaMetadata.MovieImagesRegex`.
regex!(kometa_movie_images, r"(?i)^(?:poster|background)\.(?:png|jpe?g)$");
// `XbmcNfoDetector`.
regex!(
    xbmc_nfo_content,
    r"<(movie|tvshow|episodedetails|artist|album|musicvideo)>"
);

fn matches(re: &Regex, s: &str) -> bool {
    re.is_match(s).unwrap_or(false)
}

/// `XbmcNfoDetector.IsXbmcNfoFile`: a file of at most ten megabytes that
/// holds one of the tags Kodi writes.
fn is_xbmc_nfo_file(path: &str) -> bool {
    const TEN_MEGABYTES: u64 = 10 * 1024 * 1024;
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() <= TEN_MEGABYTES => {}
        _ => return false,
    }
    std::fs::read(path)
        .map(|content| matches(xbmc_nfo_content(), &String::from_utf8_lossy(&content)))
        .unwrap_or(false)
}

/// `IMetadata.FindMetadataFile`: the type of metadata a consumer takes the
/// file for, `None` when it is not one of its files.
fn find_metadata_file(consumer: Consumer, path: &str) -> Option<i64> {
    let filename = file_name(path);
    let parsed = || parse_movie_title(filename, false).is_some();
    let extension = extension(filename).to_lowercase();
    match consumer {
        Consumer::Xbmc => {
            if matches(xbmc_movie_images(), filename) || matches(xbmc_movie_file_image(), filename) {
                return Some(MOVIE_IMAGE);
            }
            if filename.eq_ignore_ascii_case("movie.nfo") && is_xbmc_nfo_file(path) {
                return Some(MOVIE_METADATA);
            }
            (parsed() && extension == ".nfo" && is_xbmc_nfo_file(path)).then_some(MOVIE_METADATA)
        }
        Consumer::Wdtv => {
            if filename.eq_ignore_ascii_case("folder.jpg") {
                return Some(MOVIE_IMAGE);
            }
            if !parsed() {
                return None;
            }
            match extension.as_str() {
                ".xml" => Some(MOVIE_METADATA),
                ".metathumb" => Some(MOVIE_IMAGE),
                _ => None,
            }
        }
        Consumer::Roksbox => {
            if !parsed() {
                return None;
            }
            match extension.as_str() {
                ".xml" => Some(MOVIE_METADATA),
                ".jpg" => {
                    let parent = parent_path(path).map(file_name).unwrap_or("");
                    (file_stem(filename).eq_ignore_ascii_case(parent)
                        && !parent.eq_ignore_ascii_case("metadata"))
                    .then_some(MOVIE_IMAGE)
                }
                _ => None,
            }
        }
        Consumer::MediaBrowser => filename
            .eq_ignore_ascii_case("movie.xml")
            .then_some(MOVIE_METADATA),
        Consumer::Kometa => matches(kometa_movie_images(), filename).then_some(MOVIE_IMAGE),
    }
}

fn upsert_all(conn: &Connection, rows: &mut [MovieExtraFile], now: DateTime<Utc>) -> anyhow::Result<()> {
    for row in rows {
        store::upsert_extra_file(conn, row, now)?;
    }
    Ok(())
}

/// The rows an importer answers with: the ones it just wrote, then the
/// ones it had, so that no other importer takes their files.
fn with_previous(mut found: Vec<MovieExtraFile>, result: FilterResult) -> Vec<MovieExtraFile> {
    found.extend(result.previously_imported);
    found
}

/// `ExistingMetadataImporter.ProcessFiles`: the files a metadata consumer
/// would have written, whichever consumer and whether or not it is on.
fn process_metadata_files(
    conn: &Connection,
    movie: &Movie,
    files_on_disk: &[String],
    imported_files: &[String],
    file_name_before_rename: Option<&str>,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<MovieExtraFile>> {
    tracing::debug!("Looking for existing metadata in {}", movie.path);
    let result = filter_and_clean(
        conn,
        movie,
        ExtraKind::Metadata,
        files_on_disk,
        imported_files,
        file_name_before_rename.is_some(),
    )?;
    let mut found = Vec::new();
    for file in &result.files_on_disk {
        // Subtitles are the next importer's.
        if is_subtitle_extension(extension(file)) {
            continue;
        }
        for consumer in CONSUMERS {
            let Some(metadata_type) = find_metadata_file(*consumer, file) else {
                continue;
            };
            if parse_movie_path(file).is_none() {
                tracing::debug!("Unable to parse extra file: {file}");
                continue;
            }
            let mut row = new_extra(
                ExtraKind::Metadata,
                movie.id,
                movie.movie_file_id,
                relative_path(&movie.path, file).to_string(),
                extension(file),
            );
            row.consumer = Some(consumer.type_name().to_string());
            row.metadata_type = Some(metadata_type);
            found.push(row);
        }
    }
    tracing::info!("Found {} existing metadata files", found.len());
    upsert_all(conn, &mut found, now)?;
    Ok(with_previous(found, result))
}

/// `ExistingSubtitleImporter.ProcessFiles`. A subtitle belongs to the file
/// the movie had when the scan was handed it (`movie.MovieFileId`); what
/// its name says is read against the file the movie has now.
fn process_subtitle_files(
    conn: &Connection,
    movie: &Movie,
    files_on_disk: &[String],
    imported_files: &[String],
    file_name_before_rename: Option<&str>,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<MovieExtraFile>> {
    tracing::debug!("Looking for existing subtitle files in {}", movie.path);
    let result = filter_and_clean(
        conn,
        movie,
        ExtraKind::Subtitle,
        files_on_disk,
        imported_files,
        file_name_before_rename.is_some(),
    )?;
    let mut found = Vec::new();
    for file in &result.files_on_disk {
        let extension = extension(file);
        if !is_subtitle_extension(extension) {
            continue;
        }
        let Some(minimal_info) = parse_movie_path(file) else {
            tracing::debug!("Unable to parse subtitle file: {file}");
            continue;
        };
        let mut local = LocalMovie {
            file_movie_info: Some(minimal_info),
            movie: Some(movie.clone()),
            path: file.clone(),
            file_name_before_rename: file_name_before_rename.map(str::to_string),
            ..Default::default()
        };
        if aggregate::augment(conn, &mut local, None, None).is_err() {
            tracing::debug!("Unable to parse extra file: {file}");
            continue;
        }
        // The aggregator gave up on a movie without a file, and the
        // original goes on to read what it did not set: the handler ends
        // here, the rows of this kind unwritten and the other extras
        // unread.
        let Some(info) = local.subtitle_info else {
            anyhow::bail!("Unable to read the subtitle file {file}: {NULL_REFERENCE}");
        };
        let mut row = new_extra(
            ExtraKind::Subtitle,
            movie.id,
            movie.movie_file_id,
            relative_path(&movie.path, file).to_string(),
            extension,
        );
        row.language = info.language;
        row.language_tags = info.language_tags;
        row.title = info.title;
        row.copy = info.copy;
        found.push(row);
    }
    tracing::info!("Found {} existing subtitle files", found.len());
    upsert_all(conn, &mut found, now)?;
    Ok(with_previous(found, result))
}

/// `ExistingOtherExtraImporter.ProcessFiles`: whatever is left that has an
/// extension and a name, or a folder, that reads as a movie.
fn process_other_files(
    conn: &Connection,
    movie: &Movie,
    files_on_disk: &[String],
    imported_files: &[String],
    file_name_before_rename: Option<&str>,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<MovieExtraFile>> {
    tracing::debug!("Looking for existing extra files in {}", movie.path);
    let result = filter_and_clean(
        conn,
        movie,
        ExtraKind::Other,
        files_on_disk,
        imported_files,
        file_name_before_rename.is_some(),
    )?;
    let mut found = Vec::new();
    for file in &result.files_on_disk {
        let extension = extension(file);
        if extension.trim().is_empty() {
            tracing::debug!("No extension for file: {file}");
            continue;
        }
        if parse_movie_path(file).is_none() {
            tracing::debug!("Unable to parse extra file: {file}");
            continue;
        }
        found.push(new_extra(
            ExtraKind::Other,
            movie.id,
            movie.movie_file_id,
            relative_path(&movie.path, file).to_string(),
            extension,
        ));
    }
    tracing::info!("Found {} existing other extra files", found.len());
    upsert_all(conn, &mut found, now)?;
    Ok(with_previous(found, result))
}

/// `ExistingExtraFileService.ImportExtraFiles`: the importers in their
/// order (metadata, subtitles, others), each given what the ones before it
/// took. Returns the paths that are somebody's, the rows that were already
/// there included.
pub fn import_extra_files(
    conn: &Connection,
    movie: &Movie,
    possible_extra_files: &[String],
    file_name_before_rename: Option<&str>,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<String>> {
    tracing::debug!("Looking for existing extra files in {}", movie.path);
    let mut imported_files: Vec<String> = Vec::new();
    for process in [
        process_metadata_files,
        process_subtitle_files,
        process_other_files,
    ] {
        let imported = process(
            conn,
            movie,
            possible_extra_files,
            &imported_files,
            file_name_before_rename,
            now,
        )?;
        imported_files.extend(imported.iter().map(|e| full_path(movie, e)));
    }
    Ok(imported_files)
}

/// `ExistingExtraFileService.Handle(MovieScannedEvent)`: the extras a scan
/// found in the movie's folder are remembered, the rows whose file is gone
/// removed. `movie` is the movie as the scan was handed it, not as the
/// scan left it: the rows take its `movie_file_id`, which a file the scan
/// itself imported has not reached. Returns the paths that are somebody's.
pub fn import_existing(
    conn: &Connection,
    movie: &Movie,
    possible_extra_files: &[String],
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<String>> {
    let imported_files = import_extra_files(conn, movie, possible_extra_files, None, now)?;
    tracing::info!(
        "Found {} possible extra files, imported {} files.",
        possible_extra_files.len(),
        imported_files.len()
    );
    Ok(imported_files)
}

/// The `MovieScannedEvent`s among what a scan raised, handled as
/// [`import_existing`] handles one. `movie` is the movie the scan was
/// given. A handler that fails is logged, as the original's event
/// aggregator logs it, and the scan stands.
pub fn handle_scanned(db: &Db, movie: &Movie, events: &[MovieImportEvent], now: DateTime<Utc>) {
    for event in events {
        let MovieImportEvent::Scanned {
            movie_id,
            possible_extra_files,
        } = event
        else {
            continue;
        };
        if *movie_id != movie.id {
            continue;
        }
        let (movie2, files) = (movie.clone(), possible_extra_files.clone());
        if let Err(e) = db.call_blocking(move |conn| import_existing(conn, &movie2, &files, now)) {
            tracing::error!(
                error = %format!("{e:#}"),
                "ExistingExtraFileService failed while processing [MovieScannedEvent]"
            );
        }
    }
}

// --- the extras of a download

/// `SubtitleService.GetSuffix`: what comes between the movie file's name
/// and the subtitle's extension. `None` when the language has no code to
/// write, which the original fails on.
fn suffix(
    language: i64,
    copy: i64,
    tags: &[String],
    multiple_copies: bool,
    title: Option<&str>,
) -> Option<String> {
    let mut out = String::new();
    match title {
        Some(title) => {
            out.push('.');
            out.push_str(title);
            if multiple_copies {
                out.push_str(&format!(" - {copy}"));
            }
        }
        None if multiple_copies => out.push_str(&format!(".{copy}")),
        None => {}
    }
    if language != 0 {
        out.push('.');
        out.push_str(iso_two_letter_code(language)?);
    }
    if !tags.is_empty() {
        out.push('.');
        out.push_str(&tags.join("."));
    }
    Some(out)
}

/// Where an extra of `movie_file` belongs: next to it, under its name.
fn target_path(movie: &Movie, movie_file: &MovieFile, suffix: Option<&str>, extension: &str) -> String {
    let video = join(&movie.path, &movie_file.relative_path);
    let folder = parent_path(&video).unwrap_or(&movie.path);
    let suffix = suffix.filter(|s| !s.trim().is_empty()).unwrap_or("");
    join(
        folder,
        &format!("{}{suffix}{extension}", file_stem(&movie_file.relative_path)),
    )
}

/// `ExtraFileManager.ImportFile`: the file brought next to the movie file,
/// moved, or copied (hardlinked when the settings say so) when the source
/// must stay. A file already there is replaced. `None` when the name it
/// would take is the movie file's own.
fn import_file(
    config: &MediaManagementConfig,
    movie: &Movie,
    movie_file: &MovieFile,
    source: &str,
    read_only: bool,
    extension: &str,
    suffix: Option<&str>,
) -> Result<Option<String>, TransferError> {
    let movie_file_path = join(&movie.path, &movie_file.relative_path);
    let target = target_path(movie, movie_file, suffix, extension);
    if target == movie_file_path {
        tracing::debug!("Extra file {source} not imported, due to naming interference with movie file");
        return Ok(None);
    }
    let mode = match (read_only, config.copy_using_hardlinks) {
        (false, _) => TransferMode::Move,
        (true, true) => TransferMode::HardLinkOrCopy,
        (true, false) => TransferMode::Copy,
    };
    if Path::new(&target).is_file() && Path::new(&target) != Path::new(source) {
        std::fs::remove_file(&target)?;
    }
    transfer::transfer_file(Path::new(source), Path::new(&target), mode)?;
    Ok(Some(relative_path(&movie.path, &target).to_string()))
}

/// Whether an extra next to a download is the movie's: `Some(true)` when
/// its name starts with the video's or reads as the same title and year,
/// `None` when the video's own name was never read, which the original
/// fails on.
fn belongs_to(local: &LocalMovie, source_stem: &str, file: &str) -> Option<bool> {
    // Filename match.
    if file_stem(file)
        .to_lowercase()
        .starts_with(&source_stem.to_lowercase())
    {
        return Some(true);
    }
    // Movie match.
    let Some(info) = parse_movie_path(file).filter(|i| !i.movie_titles.is_empty()) else {
        return Some(false);
    };
    let own = local.file_movie_info.as_ref()?;
    Some(own.movie_titles.first() == info.movie_titles.first() && own.year == info.year)
}

fn store_extra(db: &Db, mut row: MovieExtraFile, now: DateTime<Utc>) -> anyhow::Result<MovieExtraFile> {
    db.call_blocking(move |conn| {
        store::upsert_extra_file(conn, &mut row, now)?;
        Ok(row)
    })
}

/// `SubtitleService.ImportFiles`: the subtitles among `files` that are the
/// movie's, under the movie file's name with their language and tags,
/// numbered where two would share a name. `is_sample` is `DetectSample`
/// for a video file of the download's folder.
#[allow(clippy::too_many_arguments)]
fn import_subtitle_files(
    db: &Db,
    config: &MediaManagementConfig,
    local: &LocalMovie,
    movie_file: &MovieFile,
    files: &[String],
    read_only: bool,
    is_sample: &dyn Fn(&str) -> DetectSampleResult,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<MovieExtraFile>> {
    let movie = local.movie_ref();
    let mut imported = Vec::new();
    let filtered: Vec<&String> = files
        .iter()
        .filter(|f| is_subtitle_extension(extension(f)))
        .collect();
    let source_folder = parent_path(&local.path).unwrap_or("").to_string();
    let source_stem = file_stem(&local.path).to_string();

    let mut matching: Vec<&String> = Vec::new();
    for file in &filtered {
        match belongs_to(local, &source_stem, file) {
            Some(true) => matching.push(file),
            Some(false) => {}
            None => tracing::warn!(error = NULL_REFERENCE, "Failed to import subtitle file: {file}"),
        }
    }
    // Use any sub if only episode in folder.
    if matching.is_empty() && !filtered.is_empty() {
        let videos = video_files(&source_folder, true);
        if videos.len() > 2 {
            return Ok(imported);
        }
        let real = videos
            .iter()
            .filter(|v| is_sample(v) != DetectSampleResult::Sample)
            .count();
        if real == 1 {
            matching.extend(filtered.iter().copied());
            tracing::warn!("Imported any available subtitle file for movie: {}", local.path);
        }
    }

    let subtitle_files: Vec<(String, MovieExtraFile)> = matching
        .into_iter()
        .map(|file| {
            let mut row = new_extra(
                ExtraKind::Subtitle,
                movie.id,
                movie_file.id,
                String::new(),
                extension(file),
            );
            row.language = subtitles::parse_subtitle_language_in(&MOVIE_SUBTITLE_LANGUAGES, file);
            row.language_tags = subtitles::parse_language_tags(file);
            (file.clone(), row)
        })
        .collect();
    for key in aggregate_keys(subtitle_files.iter().map(|(_, row)| row)) {
        let group: Vec<&(String, MovieExtraFile)> = subtitle_files
            .iter()
            .filter(|(_, row)| aggregate_string(row) == key)
            .collect();
        let mut copy = 1;
        for (path, row) in &group {
            // The original builds the suffix outside of what it catches: a
            // language without a code ends the import of the extras.
            let Some(suffix) = suffix(row.language, copy, &row.language_tags, group.len() > 1, None) else {
                anyhow::bail!("Failed to import subtitle file: {path}: {NULL_REFERENCE}");
            };
            match import_file(
                config,
                movie,
                movie_file,
                path,
                read_only,
                &row.extension,
                Some(&suffix),
            ) {
                Ok(Some(relative)) => {
                    let mut row = row.clone();
                    row.relative_path = relative;
                    // The permissions go to the path the file came from,
                    // as the original applies them.
                    transfer::set_permissions(config, Path::new(path));
                    imported.push(store_extra(db, row, now)?);
                    copy += 1;
                }
                Ok(None) => tracing::warn!(error = NULL_REFERENCE, "Failed to import subtitle file: {path}"),
                Err(e) => tracing::warn!(error = %e, "Failed to import subtitle file: {path}"),
            }
        }
    }
    Ok(imported)
}

/// `OtherExtraService.ImportFiles`: the other files among `files` that are
/// the movie's, under the movie file's name; of several nfo files the
/// first.
fn import_other_files(
    db: &Db,
    config: &MediaManagementConfig,
    local: &LocalMovie,
    movie_file: &MovieFile,
    files: &[String],
    read_only: bool,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<MovieExtraFile>> {
    let movie = local.movie_ref();
    let source_stem = file_stem(&local.path).to_string();
    let mut imported = Vec::new();
    let mut matching: Vec<&String> = Vec::new();
    let mut has_nfo = false;
    for file in files {
        // Filter out duplicate NFO files.
        if file.to_lowercase().ends_with(".nfo") {
            if has_nfo {
                continue;
            }
            has_nfo = true;
        }
        match belongs_to(local, &source_stem, file) {
            Some(true) => matching.push(file),
            Some(false) => {}
            None => tracing::warn!(error = NULL_REFERENCE, "Failed to import extra file: {file}"),
        }
    }
    for file in matching {
        let extension = extension(file);
        match import_file(config, movie, movie_file, file, read_only, extension, None) {
            Ok(Some(relative)) => {
                transfer::set_permissions(config, Path::new(file));
                let row = new_extra(ExtraKind::Other, movie.id, movie_file.id, relative, extension);
                imported.push(store_extra(db, row, now)?);
            }
            Ok(None) => tracing::warn!(error = NULL_REFERENCE, "Failed to import extra file: {file}"),
            Err(e) => tracing::warn!(error = %e, "Failed to import extra file: {file}"),
        }
    }
    Ok(imported)
}

/// `ExtraService.ImportExtraFiles` with the folder listed already: the
/// files whose extension the settings list, each given to the first
/// manager that takes it (the subtitle one, then the one for everything
/// else; the metadata one takes none).
#[allow(clippy::too_many_arguments)]
fn import_listed_files(
    db: &Db,
    config: &MediaManagementConfig,
    local: &LocalMovie,
    movie_file: &MovieFile,
    files: &[String],
    read_only: bool,
    is_sample: &dyn Fn(&str) -> DetectSampleResult,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<MovieExtraFile>> {
    let wanted: Vec<String> = config
        .extra_file_extensions
        .split(',')
        .filter(|e| !e.is_empty())
        .map(|e| format!(".{}", e.trim_matches([' ', '.'])))
        .collect();
    let (subtitle_files, other_files): (Vec<String>, Vec<String>) = files
        .iter()
        .filter(|f| wanted.iter().any(|w| w == extension(f)))
        .cloned()
        .partition(|f| is_subtitle_extension(extension(f)));
    let mut imported = import_subtitle_files(
        db,
        config,
        local,
        movie_file,
        &subtitle_files,
        read_only,
        is_sample,
        now,
    )?;
    imported.extend(import_other_files(
        db,
        config,
        local,
        movie_file,
        &other_files,
        read_only,
        now,
    )?);
    Ok(imported)
}

/// `ExtraService.ImportMovie`: with "Import Extra Files" on, the files
/// next to the imported video whose extension the settings list come with
/// it: subtitles under the movie file's name plus language and tags,
/// anything else under its name. A download that was a folder is searched
/// all the way down, a single file only where it lies. Returns what was
/// imported. (`CreateAfterMovieImport` is the metadata consumers', which
/// are not driven.)
pub fn import_movie(
    db: &Db,
    config: &MediaManagementConfig,
    local: &LocalMovie,
    movie_file: &MovieFile,
    read_only: bool,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<MovieExtraFile>> {
    if !config.import_extra_files {
        return Ok(Vec::new());
    }
    let Some(movie) = &local.movie else {
        return Ok(Vec::new());
    };
    let folder_search_option = local.folder_movie_info.is_some();
    let source_folder = parent_path(&local.path).unwrap_or("");
    let files = files_in(source_folder, folder_search_option);
    let metadata = movie.metadata.clone();
    let is_sample = move |file: &str| {
        let info = mediainfo::probe(Path::new(file));
        sample::is_sample(&metadata, file, info.as_ref())
    };
    import_listed_files(db, config, local, movie_file, &files, read_only, &is_sample, now)
}

// --- the extras of a file renamed or deleted

/// `ExtraFileManager.MoveFile`: the extra under the movie file's present
/// name; `true` when it moved.
fn move_file(
    movie: &Movie,
    movie_file: &MovieFile,
    extra: &mut MovieExtraFile,
    suffix: Option<&str>,
) -> bool {
    let existing = full_path(movie, extra);
    let target = target_path(movie, movie_file, suffix, &extra.extension);
    if path_equals(&existing, &target) {
        return false;
    }
    tracing::trace!(
        "Renaming extra file: [{}] {} to {target}",
        extra.id,
        extra.relative_path
    );
    let moved = (|| -> std::io::Result<()> {
        if let Some(folder) = Path::new(&target).parent() {
            std::fs::create_dir_all(folder)?;
        }
        if Path::new(&target).exists() {
            return Err(std::io::Error::other(
                "there is already a file at the destination",
            ));
        }
        std::fs::rename(&existing, &target)
    })();
    match moved {
        Ok(()) => {
            extra.relative_path = relative_path(&movie.path, &target).to_string();
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "Unable to move file after rename: {existing}");
            false
        }
    }
}

/// `ExtraService.MoveFilesAfterRename(movie, movieFile)`: the subtitles
/// and the other extras of the movie file follow it to its name, subtitles
/// that would otherwise share a name numbered. The metadata files are
/// their consumers' to move, which are not driven. Returns what moved.
/// (The settings are taken for the callers' sake; a rename reads none.)
pub fn move_after_rename(
    conn: &Connection,
    _config: &MediaManagementConfig,
    movie: &Movie,
    movie_file: &MovieFile,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<MovieExtraFile>> {
    let own: Vec<MovieExtraFile> = store::extra_files_of(conn, movie.id)?
        .into_iter()
        .filter(|e| e.movie_file_id == Some(movie_file.id))
        .collect();
    let mut moved: Vec<MovieExtraFile> = Vec::new();

    // `SubtitleService.MoveFilesAfterRename`.
    let subtitle_files = || own.iter().filter(|e| e.kind == ExtraKind::Subtitle);
    for key in aggregate_keys(subtitle_files()) {
        let mut group: Vec<MovieExtraFile> = subtitle_files()
            .filter(|e| aggregate_string(e) == key)
            .cloned()
            .collect();
        let multiple_copies = group.len() > 1;
        let mut copy = group[0].copy;
        group.sort_by_key(|e| -e.copy);
        for mut subtitle in group {
            if multiple_copies && subtitle.copy == 0 {
                copy += 1;
                subtitle.copy = copy;
            }
            let Some(suffix) = suffix(
                subtitle.language,
                subtitle.copy,
                &subtitle.language_tags,
                multiple_copies,
                subtitle.title.as_deref(),
            ) else {
                anyhow::bail!(
                    "Unable to move file after rename: {}: {NULL_REFERENCE}",
                    full_path(movie, &subtitle)
                );
            };
            if move_file(movie, movie_file, &mut subtitle, Some(&suffix)) {
                moved.push(subtitle);
            }
            copy += 1;
        }
    }

    // `OtherExtraService.MoveFilesAfterRename`.
    for extra in own.iter().filter(|e| e.kind == ExtraKind::Other) {
        let mut extra = extra.clone();
        if move_file(movie, movie_file, &mut extra, None) {
            moved.push(extra);
        }
    }

    upsert_all(conn, &mut moved, now)?;
    Ok(moved)
}

/// `ExtraFileService.HandleAsync(MovieFileDeletedEvent)`, which the
/// subtitle, the metadata and the other extra file services each do for
/// their kind: the extras of a movie file leave with it, to the recycling
/// bin when there is one. When the file's row is only being tidied away,
/// what is on disk stays.
pub fn movie_file_deleted(
    conn: &Connection,
    config: &MediaManagementConfig,
    movie: &Movie,
    movie_file_id: i64,
    reason: DeleteReason,
) -> anyhow::Result<()> {
    if reason == DeleteReason::NoLinkedEpisodes {
        tracing::debug!(
            "Removing movie file from DB as part of cleanup routine, not deleting extra files from disk."
        );
    } else {
        let root = parent_path(&movie.path).unwrap_or("");
        for extra in store::extra_files_of_movie_file(conn, movie_file_id)? {
            let path = full_path(movie, &extra);
            if !Path::new(&path).is_file() {
                continue;
            }
            // Send to the recycling bin so they can be recovered if necessary.
            let subfolder = relative_path(root, parent_path(&path).unwrap_or("")).to_string();
            if let Err(e) = transfer::recycle_file(config, Path::new(&path), &subfolder) {
                tracing::warn!(error = %e, "Unable to delete extra file: {path}");
            }
        }
    }
    tracing::debug!("Deleting Extra from database for movie file: {movie_file_id}");
    store::delete_extra_files_of_movie_file(conn, movie_file_id)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The original's fixtures under `NzbDrone.Core.Test/Extras`, each
    //! test under the original's name, over a library and a release folder
    //! in a temporary directory; then the four entry points and the scan
    //! the original was recorded doing.

    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::movies::decision::testing;
    use crate::Personality;

    struct Library {
        root: PathBuf,
        db: Db,
        movie: Movie,
        file: MovieFile,
        release_folder: String,
    }

    impl Drop for Library {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn movie_file(id: i64, movie_id: i64, relative_path: &str) -> MovieFile {
        MovieFile {
            id,
            movie_id,
            relative_path: relative_path.to_string(),
            original_file_path: None,
            size: 5,
            date_added: Utc::now(),
            scene_name: None,
            release_group: None,
            edition: None,
            quality: testing::quality(7, 1),
            languages: vec![1],
            media_info: None,
            indexer_flags: 0,
        }
    }

    /// `Movie Title` with its file `Movie Title - 2022.mkv`, and the
    /// release folder `Movie.Title.2022` the fixtures import from.
    fn library(test: &str) -> Library {
        library_of(test, 1, "Movie Title", 2022, 10, "Movie Title - 2022.mkv")
    }

    fn library_of(
        test: &str,
        movie_id: i64,
        title: &str,
        year: i64,
        file_id: i64,
        file_name: &str,
    ) -> Library {
        let root = std::env::temp_dir().join(format!("unarr-movie-extras-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let mut movie = testing::movie(movie_id, title, year);
        movie.path = root
            .join("Movies")
            .join(format!("{title} ({year})"))
            .to_string_lossy()
            .into_owned();
        movie.movie_file_id = file_id;
        let release_folder = root
            .join("Unsorted Movies")
            .join("Movie.Title.2022")
            .to_string_lossy()
            .into_owned();
        fs::create_dir_all(&movie.path).unwrap();
        fs::create_dir_all(&release_folder).unwrap();
        let file = movie_file(file_id, movie.id, file_name);
        fs::write(join(&movie.path, &file.relative_path), b"video").unwrap();
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        let (movie2, file2) = (movie.clone(), file.clone());
        db.call_blocking(move |conn| {
            store::insert_metadata(conn, &movie2.metadata)?;
            store::insert_movie(conn, &movie2)?;
            store::insert_movie_file(conn, &file2)
        })
        .unwrap();
        Library {
            root,
            db,
            movie,
            file,
            release_folder,
        }
    }

    fn write(path: &str, body: &str) {
        fs::create_dir_all(parent_path(path).unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn settings(extensions: &str) -> MediaManagementConfig {
        MediaManagementConfig {
            import_extra_files: true,
            extra_file_extensions: extensions.into(),
            ..Default::default()
        }
    }

    fn not_sample(_: &str) -> DetectSampleResult {
        DetectSampleResult::NotSample
    }

    impl Library {
        /// Files of the release folder, written; their paths.
        fn release(&self, names: &[&str]) -> Vec<String> {
            names
                .iter()
                .map(|name| {
                    let path = join(&self.release_folder, name);
                    write(&path, name);
                    path
                })
                .collect()
        }

        /// Files of the movie's folder, written; their paths.
        fn in_folder(&self, names: &[&str]) -> Vec<String> {
            names
                .iter()
                .map(|name| {
                    let path = join(&self.movie.path, name);
                    write(&path, name);
                    path
                })
                .collect()
        }

        /// The video of the release, as the import decided on it.
        fn local(&self) -> LocalMovie {
            let path = self.release(&["Movie.Title.2022.mkv"]).remove(0);
            LocalMovie {
                file_movie_info: parse_movie_title("Movie.Title.2022.mkv", true),
                movie: Some(self.movie.clone()),
                path,
                ..Default::default()
            }
        }

        fn import_subtitles(&self, files: &[String]) -> Vec<String> {
            self.import_subtitles_with(files, &not_sample)
        }

        fn import_subtitles_with(
            &self,
            files: &[String],
            is_sample: &dyn Fn(&str) -> DetectSampleResult,
        ) -> Vec<String> {
            import_subtitle_files(
                &self.db,
                &settings("srt"),
                &self.local(),
                &self.file,
                files,
                true,
                is_sample,
                Utc::now(),
            )
            .unwrap()
            .into_iter()
            .map(|e| e.relative_path)
            .collect()
        }

        fn import_others(&self, files: &[String]) -> Vec<String> {
            import_other_files(
                &self.db,
                &settings("nfo"),
                &self.local(),
                &self.file,
                files,
                true,
                Utc::now(),
            )
            .unwrap()
            .into_iter()
            .map(|e| e.relative_path)
            .collect()
        }

        /// `ExtraService.ImportMovie` over the release folder as it is.
        fn import_movie(&self, config: &MediaManagementConfig, local: &LocalMovie) -> Vec<MovieExtraFile> {
            import_movie(&self.db, config, local, &self.file, true, Utc::now()).unwrap()
        }

        fn rows(&self) -> Vec<MovieExtraFile> {
            let id = self.movie.id;
            self.db
                .call_blocking(move |conn| store::extra_files_of(conn, id))
                .unwrap()
        }

        fn paths(&self) -> Vec<String> {
            let mut paths: Vec<String> = self.rows().into_iter().map(|e| e.relative_path).collect();
            paths.sort();
            paths
        }

        fn on_disk(&self, relative: &str) -> bool {
            Path::new(&join(&self.movie.path, relative)).is_file()
        }

        /// The files of the movie's folder that are not videos, as a scan
        /// lists them.
        fn possible_extra_files(&self) -> Vec<String> {
            crate::mediafiles::scan::non_video_files(&self.movie.path, true)
        }

        fn scan(&self, movie: &Movie) -> anyhow::Result<Vec<String>> {
            let (movie, files) = (movie.clone(), self.possible_extra_files());
            self.db
                .call_blocking(move |conn| import_existing(conn, &movie, &files, Utc::now()))
        }
    }

    // --- `ExtraServiceFixture`

    #[test]
    fn should_not_pass_file_if_import_disabled() {
        let lib = library("import-disabled");
        lib.release(&["Movie.Title.2022.nfo"]);
        let mut config = settings("nfo,srt");
        config.import_extra_files = false;
        assert!(lib.import_movie(&config, &lib.local()).is_empty());
        assert!(lib.rows().is_empty());
    }

    #[test]
    fn should_not_pass_unwanted_file() {
        let lib = library("unwanted");
        lib.release(&["Movie Title - 2022.sub", "Movie Title - 2022.ass"]);
        // As the original has it, with the import off; then with it on and
        // the extensions not among the ones asked for.
        let mut config = settings("nfo,srt");
        config.import_extra_files = false;
        assert!(lib.import_movie(&config, &lib.local()).is_empty());
        config.import_extra_files = true;
        assert!(lib.import_movie(&config, &lib.local()).is_empty());
        assert!(lib.rows().is_empty());
    }

    #[test]
    fn should_pass_subtitle_file_to_subtitle_service() {
        let lib = library("to-subtitles");
        lib.release(&["Movie.Title.2022.en.srt"]);
        let imported = lib.import_movie(&settings("nfo,srt"), &lib.local());
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].kind, ExtraKind::Subtitle);
        assert_eq!(imported[0].relative_path, "Movie Title - 2022.en.srt");
        assert_eq!(imported[0].movie_file_id, Some(lib.file.id));
    }

    #[test]
    fn should_pass_nfo_file_to_other_service() {
        let lib = library("to-others");
        lib.release(&["Movie.Title.2022.nfo"]);
        let imported = lib.import_movie(&settings("nfo,srt"), &lib.local());
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].kind, ExtraKind::Other);
        assert_eq!(imported[0].relative_path, "Movie Title - 2022.nfo");
    }

    #[test]
    fn should_search_subtitles_when_importing_from_job_folder() {
        let lib = library("job-folder");
        lib.release(&["Subs/Movie.Title.2022.en.srt"]);
        let mut local = lib.local();
        local.folder_movie_info = parse_movie_title("Movie.Title.2022", true);
        assert_eq!(lib.import_movie(&settings("nfo,srt"), &local).len(), 1);
        assert_eq!(lib.paths(), ["Movie Title - 2022.en.srt"]);
    }

    #[test]
    fn should_not_search_subtitles_when_not_importing_from_job_folder() {
        let lib = library("no-job-folder");
        lib.release(&["Subs/Movie.Title.2022.en.srt"]);
        let mut local = lib.local();
        local.folder_movie_info = None;
        assert!(lib.import_movie(&settings("nfo,srt"), &local).is_empty());
        assert!(lib.rows().is_empty());
    }

    // --- `SubtitleServiceFixture`

    #[test]
    fn should_not_import_non_subtitle_file() {
        let lib = library("non-subtitle");
        let files = lib.release(&["Movie.Title.2022.en.nfo"]);
        assert!(lib.import_subtitles(&files).is_empty());
    }

    #[test]
    fn should_import_matching_subtitle_file() {
        let lib = library("matching-subtitle");
        for (file_path, expected_output_path) in [
            ("Movie Title - 2022.srt", "Movie Title - 2022.srt"),
            ("Movie.Title.2022.en.srt", "Movie Title - 2022.en.srt"),
            ("Movie.Title.2022.english.srt", "Movie Title - 2022.en.srt"),
            (
                "Movie Title 2022_en_sdh_forced.srt",
                "Movie Title - 2022.en.sdh.forced.srt",
            ),
            ("Movie_Title_2022 en.srt", "Movie Title - 2022.en.srt"),
            ("Subs/Movie.Title.2022/2_en.srt", "Movie Title - 2022.en.srt"),
            ("sub.srt", "Movie Title - 2022.srt"),
        ] {
            let files = lib.release(&[file_path]);
            assert_eq!(
                lib.import_subtitles(&files),
                [expected_output_path],
                "{file_path}"
            );
            assert!(lib.on_disk(expected_output_path), "{file_path}");
        }
    }

    #[test]
    fn should_import_multiple_subtitle_files_per_language() {
        let lib = library("per-language");
        let files = lib.release(&[
            "Movie.Title.2022.en.srt",
            "Movie.Title.2022.eng.srt",
            "Subs/Movie_Title_2022_en_forced.srt",
            "Subs/Movie.Title.2022/2_fr.srt",
        ]);
        assert_eq!(
            lib.import_subtitles(&files),
            [
                "Movie Title - 2022.1.en.srt",
                "Movie Title - 2022.2.en.srt",
                "Movie Title - 2022.en.forced.srt",
                "Movie Title - 2022.fr.srt",
            ]
        );
    }

    #[test]
    fn should_import_multiple_subtitle_files_per_language_with_tags() {
        let lib = library("per-language-tags");
        let files = lib.release(&[
            "Movie.Title.2022.en.forced.cc.srt",
            "Movie.Title.2022.other.en.forced.cc.srt",
            "Movie.Title.2022.en.forced.sdh.srt",
            "Movie.Title.2022.en.forced.default.srt",
        ]);
        assert_eq!(
            lib.import_subtitles(&files),
            [
                "Movie Title - 2022.1.en.forced.cc.srt",
                "Movie Title - 2022.2.en.forced.cc.srt",
                "Movie Title - 2022.en.forced.sdh.srt",
                "Movie Title - 2022.en.forced.default.srt",
            ]
        );
    }

    #[test]
    fn should_import_unmatching_subtitle_file_if_only_episode() {
        let lib = library("only-movie");
        let files = lib.release(&["Subs/2_en.srt"]);
        let sample_file = lib.release(&["Movie.Title.2022.sample.mkv"]).remove(0);
        let is_sample = |file: &str| {
            if file == sample_file {
                DetectSampleResult::Sample
            } else {
                DetectSampleResult::NotSample
            }
        };
        assert_eq!(
            lib.import_subtitles_with(&files, &is_sample),
            ["Movie Title - 2022.en.srt"]
        );
        // With the sample a video like any other, the subtitle is nobody's.
        assert!(lib.import_subtitles(&files).is_empty());
    }

    // --- `OtherExtraServiceFixture`

    #[test]
    fn should_import_matching_file() {
        let lib = library("matching-other");
        for (file_path, expected_output_path) in [
            ("Movie Title - 2022.nfo", "Movie Title - 2022.nfo"),
            ("Movie.Title.2022.nfo", "Movie Title - 2022.nfo"),
            ("Movie Title 2022.nfo", "Movie Title - 2022.nfo"),
            ("Movie_Title_2022.nfo", "Movie Title - 2022.nfo"),
            ("Movie.Title.2022/thumb.jpg", "Movie Title - 2022.jpg"),
        ] {
            let files = lib.release(&[file_path]);
            assert_eq!(lib.import_others(&files), [expected_output_path], "{file_path}");
            assert!(lib.on_disk(expected_output_path), "{file_path}");
        }
    }

    #[test]
    fn should_not_import_multiple_nfo_files() {
        let lib = library("several-nfo");
        let files = lib.release(&["Movie.Title.2022.nfo", "Movie_Title_2022.nfo"]);
        assert_eq!(lib.import_others(&files).len(), 1);
    }

    // --- `SubtitleFileRepositoryFixture`

    /// Five subtitles of movie 7, three of file 11 and two of file 10.
    fn five_subtitles(conn: &Connection) -> anyhow::Result<()> {
        for (n, movie_file_id) in [11, 10, 11, 10, 11].into_iter().enumerate() {
            let mut row = new_extra(
                ExtraKind::Subtitle,
                7,
                movie_file_id,
                format!("{n}.en.srt"),
                ".srt",
            );
            row.language = 1;
            store::upsert_extra_file(conn, &mut row, Utc::now())?;
        }
        Ok(())
    }

    #[test]
    fn should_delete_files_by_movie_file_id() {
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        let remaining = db
            .call_blocking(|conn| {
                five_subtitles(conn)?;
                store::delete_extra_files_of_movie_file(conn, 11)?;
                store::extra_files_of(conn, 7)
            })
            .unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn should_get_files_by_movie_file_id() {
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        let files = db
            .call_blocking(|conn| {
                five_subtitles(conn)?;
                store::extra_files_of_movie_file(conn, 11)
            })
            .unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.iter().all(|f| f.movie_file_id == Some(11)));
    }

    #[test]
    fn should_get_files_by_movie_id() {
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        let files = db
            .call_blocking(|conn| {
                five_subtitles(conn)?;
                store::extra_files_of(conn, 7)
            })
            .unwrap();
        assert_eq!(files.len(), 5);
        assert!(files.iter().all(|f| f.movie_id == 7));
        assert_eq!(files.iter().map(|f| f.id).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
    }

    // --- the consumers' `FindMetadataFileFixture`s

    #[test]
    fn kometa_find_metadata_file() {
        let folder = "/Test/Movies/Movie.Title.2024";
        let find = |name: &str| find_metadata_file(Consumer::Kometa, &join(folder, name));
        // should_return_null_if_filename_is_not_handled
        assert_eq!(find("file.jpg"), None);
        // should_return_null_if_not_valid_file_for_movie
        assert_eq!(find("movie.title.2024.jpg"), None);
        // should_not_return_metadata_if_image_file_is_a_thumb
        assert_eq!(find("movie.title.2024-thumb.jpg"), None);
        // should_return_movie_image_for_images_in_movie_folder
        assert_eq!(find("poster.jpg"), Some(MOVIE_IMAGE));
        assert_eq!(find("background.jpg"), Some(MOVIE_IMAGE));
    }

    #[test]
    fn roksbox_find_metadata_file() {
        let folder = "/Test/Movies/The.Movie.2011";
        let find = |name: &str| find_metadata_file(Consumer::Roksbox, &join(folder, name));
        // should_return_null_if_filename_is_not_handled
        assert_eq!(find("file.jpg"), None);
        // should_return_metadata_for_movie_if_valid_file_for_movie
        assert_eq!(find("the.movie.2011.xml"), Some(MOVIE_METADATA));
        assert_eq!(find("the.movie.2011.jpg"), Some(MOVIE_IMAGE));
        // should_return_null_if_not_valid_file_for_movie
        assert_eq!(find("the.movie.here.xml"), None);
        assert_eq!(find("the.movie.here.jpg"), None);
        // should_not_return_metadata_if_image_file_is_a_thumb
        assert_eq!(find("the.movie.2011-thumb.jpg"), None);
        // should_return_movie_image_for_folder_jpg_in_movie_folder
        assert_eq!(find("The.Movie.2011.jpg"), Some(MOVIE_IMAGE));
    }

    #[test]
    fn wdtv_find_metadata_file() {
        let folder = "/Test/Movies/The.Movie";
        let find = |name: &str| find_metadata_file(Consumer::Wdtv, &join(folder, name));
        // should_return_null_if_filename_is_not_handled
        assert_eq!(find("file.jpg"), None);
        // should_return_metadata_for_movie_if_valid_file_for_movie
        assert_eq!(find("the.movie.2011.xml"), Some(MOVIE_METADATA));
        assert_eq!(find("the.movie.2011.metathumb"), Some(MOVIE_IMAGE));
        // should_return_null_if_not_valid_file_for_movie
        assert_eq!(find("the.movie.xml"), None);
        assert_eq!(find("the.movie.metathumb"), None);
        // should_return_movie_image_for_folder_jpg_in_movie_folder
        assert_eq!(find("folder.jpg"), Some(MOVIE_IMAGE));
    }

    #[test]
    fn xbmc_find_metadata_file() {
        let lib = library("xbmc-nfo");
        let find = |path: &str| find_metadata_file(Consumer::Xbmc, path);
        // should_return_null_if_filename_is_not_handled
        assert_eq!(find(&join(&lib.movie.path, "file.jpg")), None);
        // should_return_metadata_for_xbmc_nfo
        let kodi = lib.in_folder(&["kodi/the.movie.2017.nfo"]).remove(0);
        fs::write(
            &kodi,
            "<?xml version=\"1.0\"?>\n<movie>\n  <title>The Movie</title>\n</movie>",
        )
        .unwrap();
        assert_eq!(find(&kodi), Some(MOVIE_METADATA));
        // should_return_null_for_scene_nfo
        let scene = lib.in_folder(&["the.movie.2017.nfo"]).remove(0);
        assert_eq!(find(&scene), None);
        // The images Kodi keeps for a movie and for its file.
        assert_eq!(find(&join(&lib.movie.path, "fanart.jpg")), Some(MOVIE_IMAGE));
        assert_eq!(
            find(&join(&lib.movie.path, "The Movie (2017)-poster.jpg")),
            Some(MOVIE_IMAGE)
        );
    }

    // --- the entry points

    #[test]
    fn suffixes_as_the_original_writes_them() {
        let tags = |list: &[&str]| list.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert_eq!(suffix(1, 1, &[], false, None).as_deref(), Some(".en"));
        assert_eq!(suffix(1, 2, &[], true, None).as_deref(), Some(".2.en"));
        assert_eq!(
            suffix(5, 1, &tags(&["forced", "sdh"]), false, None).as_deref(),
            Some(".it.forced.sdh")
        );
        assert_eq!(suffix(0, 1, &[], false, None).as_deref(), Some(""));
        assert_eq!(
            suffix(2, 3, &[], true, Some("Commentary")).as_deref(),
            Some(".Commentary - 3.fr")
        );
        // Radarr's own numbers: Romanian, Portuguese (Brazil), Telugu.
        assert_eq!(suffix(27, 1, &[], false, None).as_deref(), Some(".ro"));
        assert_eq!(suffix(30, 1, &[], false, None).as_deref(), Some(".pt"));
        assert_eq!(suffix(45, 1, &[], false, None).as_deref(), Some(".te"));
        // Flemish has no code, which the original fails on.
        assert_eq!(suffix(19, 1, &[], false, None), None);
    }

    /// What the real Radarr answered `GET /api/v3/extrafile?movieId=10`
    /// with after a scan of Backcountry that imported the video as movie
    /// file 408 and found its English subtitle
    /// (`tests/fixtures/radarr/golden/rescan-extrafile-10.json`): the
    /// subtitle is number 212, the one after the last the library had, and
    /// is no file's, since the movie the scan was handed had none yet.
    #[test]
    fn the_recorded_rescan_of_backcountry() {
        const VIDEO: &str = "Backcountry.2015.1080p.BluRay.x264-GRP.mkv";
        const SUBTITLE: &str = "Backcountry.2015.1080p.BluRay.x264-GRP.en.srt";
        let lib = library_of("backcountry", 10, "Backcountry", 2015, 408, VIDEO);
        lib.in_folder(&[SUBTITLE, "Sample/backcountry-sample.mkv"]);
        lib.db
            .call_blocking(|conn| store::seed_extra_file_sequence(conn, ExtraKind::Subtitle, 211))
            .unwrap();
        // The movie as the scan was handed it, before it imported the video.
        let mut handed = lib.movie.clone();
        handed.movie_file_id = 0;
        let events = [MovieImportEvent::Scanned {
            movie_id: 10,
            possible_extra_files: lib.possible_extra_files(),
        }];
        handle_scanned(&lib.db, &handed, &events, Utc::now());

        let rows = lib.rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        let subtitle = &rows[0];
        assert_eq!(subtitle.kind, ExtraKind::Subtitle);
        assert_eq!(subtitle.id, 212);
        assert_eq!(subtitle.movie_id, 10);
        assert_eq!(subtitle.movie_file_id, Some(0));
        assert_eq!(subtitle.relative_path, SUBTITLE);
        assert_eq!(subtitle.extension, ".srt");
        assert_eq!(subtitle.language, 1);
        assert!(subtitle.language_tags.is_empty());
        assert_eq!(subtitle.title, None);
        assert_eq!(subtitle.copy, 0);
        assert!(subtitle.added.is_some() && subtitle.added == subtitle.last_updated);
    }

    /// A scan remembers the extras of a movie's folder, each kind by its
    /// importer, and forgets the ones that are gone.
    #[test]
    fn a_scan_remembers_the_extras_it_finds() {
        let lib = library("existing");
        lib.in_folder(&[
            "Movie Title - 2022.it.forced.srt",
            "Movie Title - 2022.nfo",
            "Movie Title - IMDb.URL",
            "poster.jpg",
            "fanart.jpg",
            "notes",
        ]);
        fs::write(join(&lib.movie.path, "movie.nfo"), "<movie></movie>").unwrap();
        let imported = lib.scan(&lib.movie).unwrap();
        // Everything but the file without an extension; the poster twice,
        // for the two consumers that take it.
        assert_eq!(imported.len(), 7, "{imported:?}");

        let rows = lib.rows();
        let of = |kind: ExtraKind| -> Vec<(i64, &str, Option<&str>, Option<i64>)> {
            rows.iter()
                .filter(|e| e.kind == kind)
                .map(|e| {
                    (
                        e.id,
                        e.relative_path.as_str(),
                        e.consumer.as_deref(),
                        e.metadata_type,
                    )
                })
                .collect()
        };
        assert_eq!(
            of(ExtraKind::Subtitle),
            [(1, "Movie Title - 2022.it.forced.srt", None, None)]
        );
        assert_eq!(
            of(ExtraKind::Metadata),
            [
                (1, "fanart.jpg", Some("XbmcMetadata"), Some(MOVIE_IMAGE)),
                (2, "movie.nfo", Some("XbmcMetadata"), Some(MOVIE_METADATA)),
                (3, "poster.jpg", Some("XbmcMetadata"), Some(MOVIE_IMAGE)),
                (4, "poster.jpg", Some("KometaMetadata"), Some(MOVIE_IMAGE)),
            ]
        );
        assert_eq!(
            of(ExtraKind::Other),
            [
                (1, "Movie Title - 2022.nfo", None, None),
                (2, "Movie Title - IMDb.URL", None, None),
            ]
        );
        let subtitle = rows.iter().find(|e| e.kind == ExtraKind::Subtitle).unwrap();
        assert_eq!(
            (
                subtitle.language,
                subtitle.language_tags.clone(),
                subtitle.extension.as_str()
            ),
            (5, vec!["forced".to_string()], ".srt")
        );
        assert!(rows.iter().all(|e| e.movie_file_id == Some(lib.file.id)));
        assert!(rows.iter().all(|e| e.added.is_some() && e.last_updated.is_some()));

        // A second scan writes nothing; of the rows it had, it answers with
        // one for each path.
        assert_eq!(lib.scan(&lib.movie).unwrap().len(), 6);
        assert_eq!(lib.rows(), rows);

        // A file removed by hand is forgotten at the next scan; of the two
        // rows of the poster, one at each scan, as the original does.
        fs::remove_file(join(&lib.movie.path, "Movie Title - IMDb.URL")).unwrap();
        fs::remove_file(join(&lib.movie.path, "poster.jpg")).unwrap();
        lib.scan(&lib.movie).unwrap();
        assert_eq!(
            lib.paths(),
            [
                "Movie Title - 2022.it.forced.srt",
                "Movie Title - 2022.nfo",
                "fanart.jpg",
                "movie.nfo",
                "poster.jpg",
            ]
        );
        lib.scan(&lib.movie).unwrap();
        assert_eq!(lib.rows().len(), 4);

        // A new file gets the number after the last its kind ever had.
        lib.in_folder(&["Movie Title - 2022.txt.bak"]);
        lib.scan(&lib.movie).unwrap();
        let other: Vec<i64> = lib
            .rows()
            .iter()
            .filter(|e| e.kind == ExtraKind::Other)
            .map(|e| e.id)
            .collect();
        assert_eq!(other, [1, 3]);
    }

    /// The subtitle of a movie that has no file stops the handler where the
    /// original's stops: the metadata files are remembered, the subtitle
    /// and the other extras are not.
    #[test]
    fn a_subtitle_of_a_movie_without_a_file_ends_the_scan_of_its_extras() {
        let lib = library("no-file");
        lib.in_folder(&[
            "Movie Title - 2022.en.srt",
            "Movie Title - 2022.nfo",
            "fanart.jpg",
        ]);
        let file_id = lib.file.id;
        lib.db
            .call_blocking(move |conn| {
                store::delete_movie_file(conn, file_id)?;
                store::clear_movie_file_id(conn, file_id)
            })
            .unwrap();
        let mut movie = lib.movie.clone();
        movie.movie_file_id = 0;
        assert!(lib.scan(&movie).is_err());
        assert_eq!(lib.paths(), ["fanart.jpg"]);
        // Without the subtitle the others are read.
        fs::remove_file(join(&lib.movie.path, "Movie Title - 2022.en.srt")).unwrap();
        lib.scan(&movie).unwrap();
        assert_eq!(lib.paths(), ["Movie Title - 2022.nfo", "fanart.jpg"]);
        assert!(lib.rows().iter().all(|e| e.movie_file_id == Some(0)));
    }

    /// What came with a download is imported under the movie file's name
    /// when the settings ask for it, and only the extensions they list.
    #[test]
    fn the_extras_of_a_download_come_with_the_movie() {
        let lib = library("import");
        lib.release(&[
            "Movie.Title.2022.en.srt",
            "Movie.Title.2022.it.forced.srt",
            "Movie.Title.2022.nfo",
            "Movie.Title.2022.jpg",
            "Subs/Movie.Title.2022/2_fr.srt",
            "Subs/Another.Movie.2020.en.srt",
        ]);
        let mut local = lib.local();
        local.folder_movie_info = parse_movie_title("Movie.Title.2022", true);
        let imported = lib.import_movie(&settings("srt, .nfo"), &local);
        assert_eq!(imported.len(), 4, "{imported:?}");
        assert_eq!(
            lib.paths(),
            [
                "Movie Title - 2022.en.srt",
                "Movie Title - 2022.fr.srt",
                "Movie Title - 2022.it.forced.srt",
                "Movie Title - 2022.nfo",
            ]
        );
        for path in lib.paths() {
            assert!(lib.on_disk(&path), "{path}");
        }
        assert!(
            Path::new(&join(&lib.release_folder, "Movie.Title.2022.en.srt")).is_file(),
            "the source stays"
        );
        let rows = lib.rows();
        let italian = rows
            .iter()
            .find(|e| e.relative_path.ends_with(".it.forced.srt"))
            .unwrap();
        assert_eq!(
            (italian.kind, italian.language, italian.movie_file_id),
            (ExtraKind::Subtitle, 5, Some(lib.file.id))
        );
        assert_eq!(italian.language_tags, ["forced"]);
        assert_eq!(rows.iter().filter(|e| e.kind == ExtraKind::Other).count(), 1);

        // Moved when the download need not stay.
        let lib = library("import-move");
        lib.release(&["Movie.Title.2022.en.srt"]);
        import_movie(
            &lib.db,
            &settings("srt"),
            &lib.local(),
            &lib.file,
            false,
            Utc::now(),
        )
        .unwrap();
        assert!(lib.on_disk("Movie Title - 2022.en.srt"));
        assert!(!Path::new(&join(&lib.release_folder, "Movie.Title.2022.en.srt")).exists());
    }

    /// A file that would take the movie file's own name is left where it is.
    #[test]
    fn an_extra_is_not_imported_over_the_movie_file() {
        let lib = library("interference");
        let files = lib.release(&["Movie.Title.2022.mkv"]);
        assert!(lib.import_others(&files).is_empty());
        assert_eq!(
            fs::read(join(&lib.movie.path, &lib.file.relative_path)).unwrap(),
            b"video"
        );
    }

    /// A rename takes the subtitles and the other extras of the file
    /// along, numbering the subtitles that would share a name: the original
    /// counts once for the number it gives and once more for each
    /// subtitle, so two unnumbered copies become 1 and 3.
    #[test]
    fn the_extras_follow_a_renamed_movie_file() {
        let lib = library("rename");
        lib.in_folder(&[
            "Movie Title - 2022.en.srt",
            "Movie Title - 2022.2.en.srt",
            "Movie Title - 2022.it.sdh.srt",
            "Movie Title - 2022.nfo",
            "fanart.jpg",
        ]);
        lib.scan(&lib.movie).unwrap();
        let mut renamed = lib.file.clone();
        renamed.relative_path = "Movie Title (2022) [Bluray-1080p].mkv".into();
        fs::rename(
            join(&lib.movie.path, &lib.file.relative_path),
            join(&lib.movie.path, &renamed.relative_path),
        )
        .unwrap();
        let move_extras = |file: &MovieFile| -> usize {
            let (movie, file) = (lib.movie.clone(), file.clone());
            lib.db
                .call_blocking(move |conn| {
                    move_after_rename(conn, &MediaManagementConfig::default(), &movie, &file, Utc::now())
                })
                .unwrap()
                .len()
        };
        assert_eq!(move_extras(&renamed), 4);
        assert_eq!(
            lib.paths(),
            [
                "Movie Title (2022) [Bluray-1080p].1.en.srt",
                "Movie Title (2022) [Bluray-1080p].3.en.srt",
                "Movie Title (2022) [Bluray-1080p].it.sdh.srt",
                "Movie Title (2022) [Bluray-1080p].nfo",
                "fanart.jpg",
            ]
        );
        for path in lib.paths() {
            assert!(lib.on_disk(&path), "{path}");
        }
        assert!(!lib.on_disk("Movie Title - 2022.en.srt"));
        // Nothing is left to move, and another file's extras are not its.
        assert_eq!(move_extras(&renamed), 0);
        assert_eq!(move_extras(&movie_file(99, lib.movie.id, "Other.mkv")), 0);
    }

    /// Deleting the movie file takes its extras from the disk and from the
    /// table; a row tidied away leaves the disk alone.
    #[test]
    fn the_extras_leave_with_a_deleted_movie_file() {
        let lib = library("delete");
        lib.in_folder(&[
            "Movie Title - 2022.en.srt",
            "Movie Title - 2022.nfo",
            "fanart.jpg",
        ]);
        lib.scan(&lib.movie).unwrap();
        assert_eq!(lib.rows().len(), 3);
        // Another file's extra stays.
        let mut foreign = new_extra(ExtraKind::Other, lib.movie.id, 99, "keep.txt".into(), ".txt");
        lib.in_folder(&["keep.txt"]);
        let bin = lib.root.join("bin").to_string_lossy().into_owned();
        let (movie, file, bin2) = (lib.movie.clone(), lib.file.clone(), bin.clone());
        lib.db
            .call_blocking(move |conn| {
                store::upsert_extra_file(conn, &mut foreign, Utc::now())?;
                conn.execute(
                    "INSERT OR REPLACE INTO config (key, value) VALUES ('recyclebin', ?1)",
                    [&bin2],
                )?;
                super::super::deletion::delete_file(conn, &movie, &file, DeleteReason::Manual, Utc::now())
            })
            .unwrap();
        assert_eq!(lib.paths(), ["keep.txt"]);
        assert!(!lib.on_disk("Movie Title - 2022.en.srt"));
        assert!(!lib.on_disk("fanart.jpg"));
        assert!(lib.on_disk("keep.txt"));
        // To the recycling bin, under the movie's folder.
        let folder = file_name(&lib.movie.path).to_string();
        assert!(Path::new(&bin)
            .join(&folder)
            .join("Movie Title - 2022.en.srt")
            .is_file());

        // The cleanup routine removes the rows and nothing else.
        let movie = lib.movie.clone();
        lib.db
            .call_blocking(move |conn| {
                movie_file_deleted(
                    conn,
                    &MediaManagementConfig::default(),
                    &movie,
                    99,
                    DeleteReason::NoLinkedEpisodes,
                )
            })
            .unwrap();
        assert!(lib.rows().is_empty());
        assert!(lib.on_disk("keep.txt"));
    }

    /// Each kind counts on its own, past the highest number it ever had.
    #[test]
    fn ids_continue_within_a_kind() {
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        let ids = db
            .call_blocking(|conn| {
                let mut imported = new_extra(ExtraKind::Subtitle, 1, 0, "a.it.srt".into(), ".srt");
                imported.id = 75;
                store::insert_extra_file(conn, &imported)?;
                store::seed_extra_file_sequence(conn, ExtraKind::Metadata, 5075)?;
                let mut ids = Vec::new();
                for kind in [
                    ExtraKind::Subtitle,
                    ExtraKind::Metadata,
                    ExtraKind::Other,
                    ExtraKind::Subtitle,
                ] {
                    let mut row = new_extra(kind, 1, 0, "b".into(), ".b");
                    store::upsert_extra_file(conn, &mut row, Utc::now())?;
                    ids.push(row.id);
                }
                // A row deleted does not give its number back, and a seed
                // never lowers the counter.
                store::delete_extra_file(conn, ExtraKind::Subtitle, 77)?;
                store::seed_extra_file_sequence(conn, ExtraKind::Subtitle, 10)?;
                ids.push(store::next_extra_file_id(conn, ExtraKind::Subtitle)?);
                Ok(ids)
            })
            .unwrap();
        assert_eq!(ids, [76, 5076, 1, 77, 78]);
    }
}
