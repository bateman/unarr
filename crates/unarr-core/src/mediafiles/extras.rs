//! The files that go with an episode file: subtitles and whatever else
//! the settings name (`ExtraService`, `SubtitleService`,
//! `OtherExtraService`, `ExistingExtraFileService`, `ExtraFileService`).
//!
//! With "Import Extra Files" on, the files next to a download whose
//! extension is listed come into the library with the episode. Whether or
//! not that is on, the extras a scan finds next to episode files are
//! remembered, so that they are renamed with the episode file and go to
//! the recycling bin with it. Metadata files (nfo, images) written by a
//! metadata consumer are not told apart: Unarr drives no consumer, and
//! they are remembered as other extras when their name parses to an
//! episode.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{named_params, params, Connection, Row};

use super::local::{DeleteReason, LocalEpisode};
use super::transfer::{self, TransferMode};
use crate::db::Db;
use crate::decision::StoreLookup;
use crate::model::{EpisodeFile, Series};
use crate::parser;
use crate::parser::subtitles::{self, SubtitleTitleInfo};
use crate::store::config::MediaManagementConfig;
use crate::store::{self, from_json, parse_ts, to_json, ts};

/// `SubtitleFileExtensions`.
const SUBTITLE_EXTENSIONS: &[&str] = &[
    ".aqt", ".ass", ".idx", ".jss", ".psb", ".rt", ".smi", ".srt", ".ssa", ".sub", ".txt", ".utf", ".utf8",
    ".utf-8", ".vtt",
];

pub(crate) fn is_subtitle_extension(extension: &str) -> bool {
    SUBTITLE_EXTENSIONS
        .iter()
        .any(|e| e.eq_ignore_ascii_case(extension))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Subtitle,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Subtitle => "subtitle",
            Kind::Other => "other",
        }
    }
}

/// `SubtitleFile` / `OtherExtraFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraFile {
    pub id: i64,
    pub kind: Kind,
    pub series_id: i64,
    pub season_number: Option<i64>,
    pub episode_file_id: Option<i64>,
    pub relative_path: String,
    pub extension: String,
    pub added: Option<DateTime<Utc>>,
    /// Subtitles: the originals' language id, 0 when unknown.
    pub language: i64,
    pub language_tags: Vec<String>,
    pub title: Option<String>,
    pub copy: i64,
}

impl ExtraFile {
    fn new(
        kind: Kind,
        series: &Series,
        file: &EpisodeFile,
        relative_path: String,
        extension: &str,
    ) -> ExtraFile {
        ExtraFile {
            id: 0,
            kind,
            series_id: series.id,
            season_number: Some(file.season_number),
            episode_file_id: Some(file.id),
            relative_path,
            extension: extension.to_string(),
            added: None,
            language: 0,
            language_tags: Vec::new(),
            title: None,
            copy: 0,
        }
    }

    /// `AggregateString`: what makes two subtitles of a file copies of
    /// each other.
    fn aggregate(&self) -> String {
        format!(
            "{}{}{}{}",
            self.language,
            self.title.as_deref().unwrap_or(""),
            self.language_tags.join("."),
            self.extension
        )
    }
}

fn from_row(row: &Row) -> anyhow::Result<ExtraFile> {
    Ok(ExtraFile {
        id: row.get("id")?,
        kind: match row.get::<_, String>("kind")?.as_str() {
            "subtitle" => Kind::Subtitle,
            _ => Kind::Other,
        },
        series_id: row.get("series_id")?,
        season_number: row.get("season_number")?,
        episode_file_id: row.get("episode_file_id")?,
        relative_path: row.get("relative_path")?,
        extension: row.get("extension")?,
        added: parse_ts(row.get("added")?)?,
        language: row.get("language")?,
        language_tags: from_json(row.get("language_tags")?)?,
        title: row.get("title")?,
        copy: row.get("copy")?,
    })
}

fn query(conn: &Connection, filter: &str, id: i64) -> anyhow::Result<Vec<ExtraFile>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT * FROM extra_files WHERE {filter} = ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_and_then([id], from_row)?;
    rows.collect()
}

pub fn list_by_series(conn: &Connection, series_id: i64) -> anyhow::Result<Vec<ExtraFile>> {
    query(conn, "series_id", series_id)
}

pub fn list_by_episode_file(conn: &Connection, episode_file_id: i64) -> anyhow::Result<Vec<ExtraFile>> {
    query(conn, "episode_file_id", episode_file_id)
}

/// `ExtraFileService.Upsert`: a new row, or the row written again.
pub fn upsert(conn: &Connection, file: &mut ExtraFile, now: DateTime<Utc>) -> anyhow::Result<()> {
    if file.id == 0 {
        file.id = store::sequence::next_id(conn, "extra_files")?;
        // A row brought over from the original keeps its own date.
        file.added.get_or_insert(now);
    }
    conn.execute(
        "INSERT OR REPLACE INTO extra_files (id, kind, series_id, season_number, episode_file_id,
            relative_path, extension, added, last_updated, language, language_tags, title, copy)
         VALUES (:id, :kind, :series_id, :season_number, :episode_file_id, :relative_path, :extension,
            :added, :last_updated, :language, :language_tags, :title, :copy)",
        named_params! {
            ":id": file.id, ":kind": file.kind.as_str(), ":series_id": file.series_id,
            ":season_number": file.season_number, ":episode_file_id": file.episode_file_id,
            ":relative_path": file.relative_path, ":extension": file.extension,
            ":added": ts(file.added), ":last_updated": ts(Some(now)),
            ":language": file.language, ":language_tags": to_json(&file.language_tags)?,
            ":title": file.title, ":copy": file.copy,
        },
    )?;
    Ok(())
}

fn delete_rows(conn: &Connection, ids: &[i64]) -> anyhow::Result<()> {
    for id in ids {
        conn.execute("DELETE FROM extra_files WHERE id = ?1", params![id])?;
    }
    Ok(())
}

/// `ExtraFileService.Handle(EpisodeFileDeletedEvent)`: the extras of an
/// episode file leave with it, to the recycling bin when there is one.
/// When the file's row is only being tidied away (no episode refers to
/// it), what is on disk stays.
pub fn episode_file_deleted(
    conn: &Connection,
    config: &MediaManagementConfig,
    series: &Series,
    episode_file_id: i64,
    reason: DeleteReason,
) -> anyhow::Result<()> {
    if reason == DeleteReason::NoLinkedEpisodes {
        tracing::debug!(
            "Removing episode file from DB as part of cleanup routine, not deleting extra files from disk."
        );
    } else {
        let root = super::parent_path(&series.path).unwrap_or("");
        for extra in list_by_episode_file(conn, episode_file_id)? {
            let path = super::join(&series.path, &extra.relative_path);
            if !Path::new(&path).is_file() {
                continue;
            }
            let subfolder = super::relative_path(root, super::parent_path(&path).unwrap_or("")).to_string();
            if let Err(e) = transfer::recycle_file(config, Path::new(&path), &subfolder) {
                tracing::warn!(error = %e, "Unable to delete extra file: {path}");
            }
        }
    }
    tracing::debug!("Deleting Extra from database for episode file: {episode_file_id}");
    conn.execute(
        "DELETE FROM extra_files WHERE episode_file_id = ?1",
        params![episode_file_id],
    )?;
    Ok(())
}

/// `IsoLanguages.Get(language).TwoLetterCode`.
fn two_letter_code(language: i64) -> Option<&'static str> {
    const CODES: &[&str] = &[
        "en", "fr", "es", "de", "it", "da", "nl", "ja", "is", "zh", "ru", "pl", "vi", "sv", "no", "fi", "tr",
        "pt", "nl", "el", "ko", "hu", "he", "lt", "cs", "ar", "hi", "bg", "ml", "uk", "sk", "th", "pt", "es",
        "ro", "lv", "fa", "ca", "hr", "sr", "bs", "et", "ta", "id", "mk", "sl",
    ];
    usize::try_from(language - 1)
        .ok()
        .and_then(|i| CODES.get(i))
        .copied()
}

/// `SubtitleService.GetSuffix`: what comes between the episode file's name
/// and the subtitle's extension. `None` when the language has no code to
/// write, which the originals fail on.
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
        out.push_str(two_letter_code(language)?);
    }
    if !tags.is_empty() {
        out.push('.');
        out.push_str(&tags.join("."));
    }
    Some(out)
}

/// Where an extra of `episode_file` belongs: next to it, under its name.
fn target_path(series: &Series, episode_file: &EpisodeFile, suffix: &str, extension: &str) -> String {
    let video = super::join(&series.path, &episode_file.relative_path);
    let folder = super::parent_path(&video).unwrap_or(&series.path);
    super::join(
        folder,
        &format!("{}{suffix}{extension}", super::file_stem(&video)),
    )
}

/// `ExtraFileManager.ImportFile`: the file brought next to the episode
/// file, moved, or copied (hardlinked when the settings say so) when the
/// source must stay. A file already there is replaced.
fn import_file(
    config: &MediaManagementConfig,
    series: &Series,
    episode_file: &EpisodeFile,
    source: &str,
    read_only: bool,
    suffix: &str,
    extension: &str,
) -> Result<Option<String>, transfer::TransferError> {
    let target = target_path(series, episode_file, suffix, extension);
    // An extra that would take the episode file's own name (a video
    // extension listed among the extras, a sample next to the episode)
    // is left where it is: importing it would delete the episode first.
    if super::path_equals(&target, &super::join(&series.path, &episode_file.relative_path)) {
        tracing::debug!("Extra file {source} not imported, due to naming interference with episode file");
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
    Ok(Some(super::relative_path(&series.path, &target).to_string()))
}

/// Whether an extra next to a download is the episode's: its name starts
/// with the video's, or parses to the same season and episodes.
fn belongs_to(local: &LocalEpisode, source_stem: &str, file: &str) -> bool {
    if super::file_stem(file)
        .to_lowercase()
        .starts_with(&source_stem.to_lowercase())
    {
        return true;
    }
    let Some(info) = parser::parse_path(file) else {
        return false;
    };
    if info.episode_numbers.is_empty() {
        return false;
    }
    local.file_episode_info.as_ref().is_some_and(|own| {
        own.season_number == info.season_number && own.episode_numbers == info.episode_numbers
    })
}

/// `ExtraService.ImportEpisode`: with "Import Extra Files" on, the files
/// next to the imported video whose extension the settings list come with
/// it: subtitles under the episode file's name plus language and tags,
/// anything else under its name. Returns what was imported.
pub fn import_episode(
    db: &Db,
    config: &MediaManagementConfig,
    local: &LocalEpisode,
    episode_file: &EpisodeFile,
    read_only: bool,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<ExtraFile>> {
    if !config.import_extra_files {
        return Ok(Vec::new());
    }
    let Some(series) = &local.series else {
        return Ok(Vec::new());
    };
    // A download that was a folder is searched all the way down.
    let recursive = local.folder_episode_info.is_some();
    let wanted: Vec<String> = config
        .extra_file_extensions
        .split(',')
        .map(|e| e.trim_matches([' ', '.']))
        .filter(|e| !e.is_empty())
        .map(|e| format!(".{e}"))
        .collect();
    let source_folder = super::parent_path(&local.path).unwrap_or("").to_string();
    let source_stem = super::file_stem(&local.path).to_string();
    let files: Vec<String> = super::scan::files_in(&source_folder, recursive)
        .into_iter()
        .filter(|f| wanted.iter().any(|w| w == super::extension(f)))
        .collect();
    let (subtitle_files, other_files): (Vec<String>, Vec<String>) = files
        .into_iter()
        .partition(|f| is_subtitle_extension(super::extension(f)));

    let mut imported = Vec::new();

    // Subtitles.
    let mut matching: Vec<String> = subtitle_files
        .iter()
        .filter(|f| belongs_to(local, &source_stem, f))
        .cloned()
        .collect();
    if matching.is_empty() && !subtitle_files.is_empty() {
        // The only episode in the folder takes whatever subtitles are there.
        let videos = super::scan::video_files(&source_folder, true);
        if videos.len() <= 2 {
            let real: Vec<&String> = videos
                .iter()
                .filter(|v| {
                    super::sample::is_sample_file(series, v, false)
                        != super::sample::DetectSampleResult::Sample
                })
                .collect();
            if real.len() == 1 {
                matching = subtitle_files.clone();
                tracing::warn!("Imported any available subtitle file for episode: {}", local.path);
            }
        }
    }
    let mut subtitles: Vec<(String, ExtraFile)> = matching
        .into_iter()
        .map(|path| {
            let mut extra = ExtraFile::new(
                Kind::Subtitle,
                series,
                episode_file,
                String::new(),
                super::extension(&path),
            );
            extra.language = subtitles::parse_subtitle_language(&path);
            extra.language_tags = subtitles::parse_language_tags(&path);
            (path, extra)
        })
        .collect();
    let mut groups: Vec<String> = Vec::new();
    for (_, extra) in &subtitles {
        let key = extra.aggregate();
        if !groups.contains(&key) {
            groups.push(key);
        }
    }
    for key in groups {
        let count = subtitles.iter().filter(|(_, e)| e.aggregate() == key).count();
        let mut copy = 1;
        for (path, extra) in subtitles.iter_mut().filter(|(_, e)| e.aggregate() == key) {
            let Some(suffix) = suffix(extra.language, copy, &extra.language_tags, count > 1, None) else {
                tracing::warn!("Failed to import subtitle file: {path}");
                continue;
            };
            match import_file(
                config,
                series,
                episode_file,
                path,
                read_only,
                &suffix,
                &extra.extension,
            ) {
                Ok(None) => {}
                Ok(Some(relative)) => {
                    extra.relative_path = relative;
                    transfer::set_permissions(
                        config,
                        Path::new(&super::join(&series.path, &extra.relative_path)),
                    );
                    let mut row = extra.clone();
                    db.call_blocking(move |conn| {
                        upsert(conn, &mut row, now)?;
                        Ok(row)
                    })
                    .map(|row| imported.push(row))?;
                    copy += 1;
                }
                Err(e) => tracing::warn!(error = %e, "Failed to import subtitle file: {path}"),
            }
        }
    }

    // Everything else; of several nfo files the first.
    let mut has_nfo = false;
    for path in other_files {
        if path.to_lowercase().ends_with(".nfo") {
            if has_nfo {
                continue;
            }
            has_nfo = true;
        }
        if !belongs_to(local, &source_stem, &path) {
            continue;
        }
        let extension = super::extension(&path).to_string();
        match import_file(config, series, episode_file, &path, read_only, "", &extension) {
            Ok(None) => {}
            Ok(Some(relative)) => {
                transfer::set_permissions(config, Path::new(&super::join(&series.path, &relative)));
                let mut row = ExtraFile::new(Kind::Other, series, episode_file, relative, &extension);
                db.call_blocking(move |conn| {
                    upsert(conn, &mut row, now)?;
                    Ok(row)
                })
                .map(|row| imported.push(row))?;
            }
            Err(e) => tracing::warn!(error = %e, "Failed to import extra file: {path}"),
        }
    }
    Ok(imported)
}

/// `ExtraFileManager.MoveFile`: the extra under the episode file's present
/// name; `true` when it moved.
fn move_file(series: &Series, episode_file: &EpisodeFile, extra: &mut ExtraFile, suffix: &str) -> bool {
    let existing = super::join(&series.path, &extra.relative_path);
    let target = target_path(series, episode_file, suffix, &extra.extension);
    if super::path_equals(&existing, &target) {
        return false;
    }
    tracing::trace!("Renaming extra file: {existing} to {target}");
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
            extra.relative_path = super::relative_path(&series.path, &target).to_string();
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, "Unable to move file after rename: {existing}");
            false
        }
    }
}

/// `MoveFilesAfterRename`: the extras of the given episode files follow
/// them to their names. Subtitles that would otherwise share a name are
/// numbered. Returns how many moved.
pub fn move_after_rename(
    db: &Db,
    series: &Series,
    episode_files: &[EpisodeFile],
    now: DateTime<Utc>,
) -> anyhow::Result<usize> {
    let series_id = series.id;
    let extras = db.call_blocking(move |conn| list_by_series(conn, series_id))?;
    let mut moved: Vec<ExtraFile> = Vec::new();
    for episode_file in episode_files {
        let own: Vec<&ExtraFile> = extras
            .iter()
            .filter(|e| e.episode_file_id == Some(episode_file.id))
            .collect();
        // Subtitles, by what makes them copies of each other.
        let mut keys: Vec<String> = Vec::new();
        for extra in own.iter().filter(|e| e.kind == Kind::Subtitle) {
            let key = extra.aggregate();
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        for key in keys {
            let mut group: Vec<ExtraFile> = own
                .iter()
                .filter(|e| e.kind == Kind::Subtitle && e.aggregate() == key)
                .map(|e| (*e).clone())
                .collect();
            let multiple = group.len() > 1;
            let mut copy = group[0].copy;
            group.sort_by_key(|e| -e.copy);
            for mut extra in group {
                if multiple && extra.copy == 0 {
                    copy += 1;
                    extra.copy = copy;
                }
                let Some(suffix) = suffix(
                    extra.language,
                    extra.copy,
                    &extra.language_tags,
                    multiple,
                    extra.title.as_deref(),
                ) else {
                    continue;
                };
                if move_file(series, episode_file, &mut extra, &suffix) {
                    moved.push(extra);
                }
            }
        }
        for extra in own.iter().filter(|e| e.kind == Kind::Other) {
            let mut extra = (*extra).clone();
            if move_file(series, episode_file, &mut extra, "") {
                moved.push(extra);
            }
        }
    }
    let count = moved.len();
    if count > 0 {
        db.call_blocking(move |conn| {
            for mut extra in moved {
                upsert(conn, &mut extra, now)?;
            }
            Ok(())
        })?;
    }
    Ok(count)
}

/// `AggregateSubtitleInfo.CleanSubtitleTitleInfo`: what the subtitle's
/// name says, less what it only repeats from the episode file's name.
fn subtitle_info(episode_file: &EpisodeFile, path: &str) -> SubtitleTitleInfo {
    let mut info = subtitles::parse_subtitle_information(path);
    let file_title = super::file_stem(&episode_file.relative_path).to_lowercase();
    let original_title = episode_file
        .original_file_path
        .as_deref()
        .map(|p| super::file_stem(p).to_lowercase())
        .unwrap_or_default();
    if info.title_first {
        if let Some(raw) = info.raw_title.as_deref().map(str::to_lowercase) {
            if file_title.contains(&raw) || original_title.contains(&raw) {
                info = subtitles::parse_basic_subtitle(path);
            }
        }
    }
    info.language_tags
        .retain(|tag| !file_title.contains(&tag.to_lowercase()));
    info
}

/// `ExistingExtraFileService.Handle(SeriesScannedEvent)`: the extras a
/// scan finds in the series' folder are matched to episode files and
/// remembered; rows whose file is gone are dropped. `files_on_disk` are
/// the folder's files that are not videos. Returns how many were added.
pub fn import_existing(
    conn: &Connection,
    series: &Series,
    files_on_disk: &[String],
    now: DateTime<Utc>,
) -> anyhow::Result<usize> {
    tracing::debug!("Looking for existing extra files in {}", series.path);
    let known = list_by_series(conn, series.id)?;
    let on_disk: std::collections::HashSet<&str> = files_on_disk.iter().map(String::as_str).collect();
    let mut known_paths = std::collections::HashSet::new();
    let mut gone = Vec::new();
    for extra in &known {
        let path = super::join(&series.path, &extra.relative_path);
        if on_disk.contains(path.as_str()) {
            known_paths.insert(path);
        } else {
            gone.push(extra.id);
        }
    }
    delete_rows(conn, &gone)?;
    let lookup = StoreLookup::new(conn);
    let mut added = 0;
    for path in files_on_disk.iter().filter(|p| !known_paths.contains(p.as_str())) {
        let extension = super::extension(path).to_string();
        if extension.trim().is_empty() {
            tracing::debug!("No extension for file: {path}");
            continue;
        }
        let episodes = super::aggregate::episodes_of_existing_file(&lookup, series, path);
        let Some(first) = episodes.first() else {
            tracing::debug!("Cannot find related episodes for: {path}");
            continue;
        };
        if episodes
            .iter()
            .any(|e| e.episode_file_id != first.episode_file_id)
        {
            tracing::debug!("Extra file: {path} does not match existing files.");
            continue;
        }
        // An extra of an episode that has no file is nobody's.
        let Some(episode_file) = store::episodes::list_files_by_ids(conn, &[first.episode_file_id])?
            .into_iter()
            .next()
        else {
            continue;
        };
        let relative = super::relative_path(&series.path, path).to_string();
        let mut extra = if is_subtitle_extension(&extension) {
            let info = subtitle_info(&episode_file, path);
            let mut extra = ExtraFile::new(Kind::Subtitle, series, &episode_file, relative, &extension);
            extra.language = info.language;
            extra.language_tags = info.language_tags;
            extra.title = info.title;
            extra.copy = info.copy;
            extra
        } else {
            // A rename gives an other extra the episode file's name and its
            // own extension, nothing between. One that is named otherwise
            // (`-thumb.jpg`, written by a media centre) is left alone:
            // the originals hand those to their metadata consumers, which
            // Unarr does not drive.
            if !super::file_stem(path).eq_ignore_ascii_case(super::file_stem(&episode_file.relative_path)) {
                tracing::debug!("Extra file: {path} is not named after its episode file.");
                continue;
            }
            ExtraFile::new(Kind::Other, series, &episode_file, relative, &extension)
        };
        extra.season_number = Some(first.season_number);
        upsert(conn, &mut extra, now)?;
        added += 1;
    }
    tracing::info!(
        "Found {} possible extra files, imported {added} files.",
        files_on_disk.len()
    );
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixes_as_the_originals_write_them() {
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
        assert_eq!(
            suffix(2, 0, &[], false, Some("Commentary")).as_deref(),
            Some(".Commentary.fr")
        );
        // Portuguese (Brazil) and Flemish write the code of their language.
        assert_eq!(suffix(33, 1, &[], false, None).as_deref(), Some(".pt"));
        assert_eq!(suffix(19, 1, &[], false, None).as_deref(), Some(".nl"));
        // A language the originals have no code for.
        assert_eq!(suffix(47, 1, &[], false, None), None);
    }

    use crate::model::{Episode, QualityModel, ReleaseType};
    use std::fs;

    /// A series folder with one episode file, in a database of its own.
    struct Library {
        root: std::path::PathBuf,
        db: Db,
        series: Series,
        file: EpisodeFile,
    }

    fn library(name: &str) -> Library {
        let root = std::env::temp_dir().join(format!("unarr_extras_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("tv/Show/Season 01")).unwrap();
        let db = Db::open_in_memory().unwrap();
        let mut series = crate::store::series::tests::sample(1);
        series.title = "Show".into();
        series.path = root.join("tv/Show").to_string_lossy().into_owned();
        let file = EpisodeFile {
            id: 10,
            series_id: 1,
            season_number: 1,
            relative_path: "Season 01/Show - S01E01 - Pilot.mkv".into(),
            original_file_path: None,
            size: 1,
            date_added: Utc::now(),
            scene_name: None,
            release_group: None,
            release_hash: None,
            quality: QualityModel {
                quality_id: 7,
                version: 1,
                real: 0,
                is_repack: false,
            },
            languages: vec![1],
            media_info: None,
            indexer_flags: 0,
            release_type: ReleaseType::SingleEpisode,
        };
        fs::write(root.join("tv/Show").join(&file.relative_path), b"video").unwrap();
        let (stored, stored_file) = (series.clone(), file.clone());
        db.call_blocking(move |conn| {
            conn.execute_batch(
                "INSERT INTO quality_profiles (id, name, cutoff, items) VALUES (1, 'Any', 1, '[]');
                 INSERT INTO tags (id, label) VALUES (1, 'drama');",
            )?;
            crate::store::series::insert(conn, &stored)?;
            store::episodes::insert_file(conn, &stored_file)?;
            for number in [1, 2] {
                store::episodes::insert_episode(
                    conn,
                    &Episode {
                        id: number,
                        series_id: 1,
                        tvdb_id: 100 + number,
                        season_number: 1,
                        episode_number: number,
                        title: Some(format!("Episode {number}")),
                        overview: None,
                        episode_file_id: if number == 1 { 10 } else { 0 },
                        absolute_episode_number: None,
                        scene_absolute_episode_number: None,
                        scene_season_number: None,
                        scene_episode_number: None,
                        unverified_scene_numbering: false,
                        monitored: true,
                        air_date: None,
                        air_date_utc: None,
                        runtime: 44,
                        finale_type: None,
                        images: vec![],
                        ratings: crate::model::Ratings::default(),
                        last_search_time: None,
                        aired_after_season_number: None,
                        aired_before_season_number: None,
                        aired_before_episode_number: None,
                    },
                )?;
            }
            Ok(())
        })
        .unwrap();
        Library {
            root,
            db,
            series,
            file,
        }
    }

    impl Library {
        fn rows(&self) -> Vec<ExtraFile> {
            self.db.call_blocking(|conn| list_by_series(conn, 1)).unwrap()
        }

        fn paths(&self) -> Vec<String> {
            let mut paths: Vec<String> = self.rows().into_iter().map(|e| e.relative_path).collect();
            paths.sort();
            paths
        }

        fn on_disk(&self, relative: &str) -> bool {
            Path::new(&self.series.path).join(relative).is_file()
        }
    }

    /// What came with a download is imported under the episode file's name
    /// when the settings ask for it, and only the extensions they list.
    #[test]
    fn an_extra_that_would_take_the_episodes_name_is_left_alone() {
        let lib = library("interference");
        let downloads = lib.root.join("downloads");
        fs::create_dir_all(&downloads).unwrap();
        let sample = downloads.join("Show.S01E01.sample.mkv");
        fs::write(&sample, b"sample").unwrap();
        let config = MediaManagementConfig::default();
        // `mkv` listed among the extra extensions, and a move: the target
        // is the episode file itself.
        let imported = import_file(
            &config,
            &lib.series,
            &lib.file,
            &sample.to_string_lossy(),
            false,
            "",
            ".mkv",
        )
        .unwrap();
        assert_eq!(imported, None);
        assert!(sample.is_file(), "the sample was moved");
        assert_eq!(
            fs::read(lib.root.join("tv/Show").join(&lib.file.relative_path)).unwrap(),
            b"video",
            "the episode file was replaced"
        );
        let _ = fs::remove_dir_all(&lib.root);
    }

    #[test]
    fn the_extras_of_a_download_come_with_the_episode() {
        let lib = library("import");
        let download = lib.root.join("downloads/Show.S01E01.1080p.WEB-GRP");
        fs::create_dir_all(download.join("Subs")).unwrap();
        let video = download.join("Show.S01E01.1080p.WEB-GRP.mkv");
        for (name, body) in [
            ("Show.S01E01.1080p.WEB-GRP.mkv", "v"),
            ("Show.S01E01.1080p.WEB-GRP.en.srt", "english"),
            ("Show.S01E01.1080p.WEB-GRP.it.forced.srt", "italian"),
            ("Show.S01E01.1080p.WEB-GRP.nfo", "nfo"),
            ("Show.S01E01.1080p.WEB-GRP.jpg", "not listed"),
            ("Subs/show.s01e01.fr.srt", "french, by episode number"),
            ("Subs/Show.S01E02.en.srt", "another episode's"),
        ] {
            fs::write(download.join(name), body).unwrap();
        }
        let local = LocalEpisode {
            path: video.to_string_lossy().into_owned(),
            series: Some(lib.series.clone()),
            file_episode_info: parser::parse_path(&video.to_string_lossy()),
            folder_episode_info: parser::parse_title("Show.S01E01.1080p.WEB-GRP"),
            ..Default::default()
        };
        let mut config = MediaManagementConfig {
            extra_file_extensions: "srt, .nfo".into(),
            ..Default::default()
        };
        let now = Utc::now();

        // Off: nothing.
        config.import_extra_files = false;
        assert!(import_episode(&lib.db, &config, &local, &lib.file, true, now)
            .unwrap()
            .is_empty());

        // On, the download to be left as it is: copies.
        config.import_extra_files = true;
        let imported = import_episode(&lib.db, &config, &local, &lib.file, true, now).unwrap();
        assert_eq!(imported.len(), 4, "{imported:?}");
        assert_eq!(
            lib.paths(),
            [
                "Season 01/Show - S01E01 - Pilot.en.srt",
                "Season 01/Show - S01E01 - Pilot.fr.srt",
                "Season 01/Show - S01E01 - Pilot.it.forced.srt",
                "Season 01/Show - S01E01 - Pilot.nfo",
            ]
        );
        for path in lib.paths() {
            assert!(lib.on_disk(&path), "{path}");
        }
        assert!(
            download.join("Show.S01E01.1080p.WEB-GRP.en.srt").is_file(),
            "the source stays"
        );
        let rows = lib.rows();
        let italian = rows
            .iter()
            .find(|e| e.relative_path.ends_with(".it.forced.srt"))
            .unwrap();
        assert_eq!(
            (italian.kind, italian.language, italian.episode_file_id),
            (Kind::Subtitle, 5, Some(10))
        );
        assert_eq!(italian.language_tags, ["forced"]);
        assert_eq!(rows.iter().filter(|e| e.kind == Kind::Other).count(), 1);
        let _ = fs::remove_dir_all(&lib.root);
    }

    /// A scan remembers the extras that sit next to episode files; a
    /// rename takes them along; deleting the episode file takes them away.
    #[test]
    fn extras_on_disk_follow_the_episode_file() {
        let lib = library("existing");
        let folder = Path::new(&lib.series.path).join("Season 01");
        for name in [
            "Show - S01E01 - Pilot.it.srt",
            "Show - S01E01 - Pilot.en.sdh.srt",
            "Show - S01E01 - Pilot.nfo",
            // An episode without a file, and nothing that names an episode.
            "Show - S01E02 - Second.en.srt",
            "notes.txt",
        ] {
            fs::write(folder.join(name), b"x").unwrap();
        }
        let on_disk = || -> Vec<String> {
            super::super::scan::files_in(&lib.series.path, true)
                .into_iter()
                .filter(|f| !f.ends_with(".mkv"))
                .collect()
        };
        let now = Utc::now();
        let (series, files) = (lib.series.clone(), on_disk());
        let added = lib
            .db
            .call_blocking(move |conn| import_existing(conn, &series, &files, now))
            .unwrap();
        assert_eq!(added, 3);
        // A second scan adds nothing.
        let (series, files) = (lib.series.clone(), on_disk());
        assert_eq!(
            lib.db
                .call_blocking(move |conn| import_existing(conn, &series, &files, now))
                .unwrap(),
            0
        );
        let rows = lib.rows();
        let sdh = rows
            .iter()
            .find(|e| e.relative_path.ends_with(".en.sdh.srt"))
            .unwrap();
        assert_eq!(
            (sdh.language, sdh.language_tags.clone()),
            (1, vec!["sdh".to_string()])
        );

        // The episode file renamed: its extras take the new name.
        let mut renamed = lib.file.clone();
        renamed.relative_path = "Season 01/Show (2020) - 1x01 - Pilot.mkv".into();
        fs::rename(
            Path::new(&lib.series.path).join(&lib.file.relative_path),
            Path::new(&lib.series.path).join(&renamed.relative_path),
        )
        .unwrap();
        assert_eq!(
            move_after_rename(&lib.db, &lib.series, std::slice::from_ref(&renamed), now).unwrap(),
            3
        );
        assert_eq!(
            lib.paths(),
            [
                "Season 01/Show (2020) - 1x01 - Pilot.en.sdh.srt",
                "Season 01/Show (2020) - 1x01 - Pilot.it.srt",
                "Season 01/Show (2020) - 1x01 - Pilot.nfo",
            ]
        );
        for path in lib.paths() {
            assert!(lib.on_disk(&path), "{path}");
        }
        assert!(!folder.join("Show - S01E01 - Pilot.it.srt").exists());
        assert_eq!(
            move_after_rename(&lib.db, &lib.series, &[renamed], now).unwrap(),
            0
        );

        // A subtitle removed by hand is forgotten at the next scan.
        fs::remove_file(folder.join("Show (2020) - 1x01 - Pilot.nfo")).unwrap();
        let (series, files) = (lib.series.clone(), on_disk());
        lib.db
            .call_blocking(move |conn| import_existing(conn, &series, &files, now))
            .unwrap();
        assert_eq!(lib.rows().len(), 2);

        // The episode file deleted: its extras leave the disk and the table.
        let series = lib.series.clone();
        lib.db
            .call_blocking(move |conn| {
                episode_file_deleted(
                    conn,
                    &MediaManagementConfig::default(),
                    &series,
                    10,
                    DeleteReason::Manual,
                )
            })
            .unwrap();
        assert!(lib.rows().is_empty());
        assert!(!folder.join("Show (2020) - 1x01 - Pilot.it.srt").exists());
        assert!(
            folder.join("Show - S01E02 - Second.en.srt").exists(),
            "not an extra of that file"
        );
        let _ = fs::remove_dir_all(&lib.root);
    }

    /// The scan itself does the remembering: a subtitle next to an episode
    /// file is in the table once the series has been scanned, on the
    /// ordinary path of a scan and not only when the folder is missing.
    #[test]
    fn a_scan_remembers_the_extras_it_finds() {
        let lib = library("scan");
        let folder = Path::new(&lib.series.path).join("Season 01");
        fs::write(folder.join("Show - S01E01 - Pilot.it.srt"), b"x").unwrap();
        let now = Utc::now();
        let env = lib
            .db
            .call_blocking(move |conn| super::super::decide::Environment::load(conn, now))
            .unwrap();
        super::super::scan::scan(&lib.db, &env, &lib.series, now, &mut |_| {}).unwrap();
        assert_eq!(lib.paths(), ["Season 01/Show - S01E01 - Pilot.it.srt"]);
        // The folder gone: nothing is remembered any more.
        fs::remove_dir_all(&lib.series.path).unwrap();
        fs::write(
            lib.root.join("tv/other.txt"),
            b"so that the root folder is not empty",
        )
        .unwrap();
        super::super::scan::scan(&lib.db, &env, &lib.series, now, &mut |_| {}).unwrap();
        assert!(lib.rows().is_empty());
        let _ = fs::remove_dir_all(&lib.root);
    }
}
