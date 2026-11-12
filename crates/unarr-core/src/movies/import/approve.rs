//! `ImportApprovedMovie` with `UpgradeMediaFileService` and
//! `MovieFileMovingService`: the approved decisions become movie files.
//! The file a movie already has goes to the recycling bin, the new one is
//! moved (or linked, or copied) under the name the naming settings build,
//! the row is added, the movie pointed at it, the history written. File
//! work happens on the caller's thread; each database step is one call on
//! the database thread.

use std::path::Path;

use chrono::{DateTime, Utc};

use super::decide::MovieEnvironment;
use super::deletion;
use super::events::{self, MovieEvents, MovieImportEvent, MovieImportFailed, MovieImported};
use super::local::{DeletedMovieFile, LocalMovie, MovieImportDecision, MovieImportResult};
use crate::db::Db;
use crate::downloadclients::DownloadClientItem;
use crate::mediafiles::approve::{best_root_folder_path, naming_formats};
use crate::mediafiles::local::{DeleteReason, ImportMode};
use crate::mediafiles::transfer::{self, TransferError, TransferMode};
use crate::mediafiles::{extension, file_name, format, is_parent_path, join, parent_path, relative_path};
use crate::model::{CustomFormat, QualityModel, RootFolder};
use crate::movies::model::{Movie, MovieFile, MovieHistoryEvent};
use crate::movies::naming::{self, NamingMovie, NamingMovieFile};
use crate::movies::quality::parse_indexer_flags;
use crate::movies::store;

/// What `Import` hands back: the results in the original's order, the
/// events for the relays, and the movies a destination clash asks to
/// rescan (`RescanMovieCommand`).
#[derive(Debug, Default)]
pub struct ImportedMovies {
    pub results: Vec<MovieImportResult>,
    pub events: MovieEvents,
}

/// The exceptions `Import` catches, with their result messages; the
/// renames tell `SameFile` and `DestinationExists` apart too.
#[derive(Debug)]
pub enum Failure {
    RootFolderMissing(String),
    DestinationExists(String),
    SameFile(String),
    RecycleBin,
    Other(String),
}

impl Failure {
    /// What the log says.
    pub fn detail(&self) -> String {
        match self {
            Failure::RootFolderMissing(m)
            | Failure::Other(m)
            | Failure::DestinationExists(m)
            | Failure::SameFile(m) => m.clone(),
            other => other.message().to_string(),
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Failure::RootFolderMissing(_) => "Failed to import movie, Root folder missing.",
            Failure::DestinationExists(_) => "Failed to import movie, Destination already exists.",
            Failure::RecycleBin => "Failed to import movie, unable to move existing file to the Recycle Bin.",
            Failure::Other(_) | Failure::SameFile(_) => "Failed to import movie",
        }
    }
}

impl From<TransferError> for Failure {
    fn from(e: TransferError) -> Failure {
        match e {
            TransferError::DestinationExists(p) => Failure::DestinationExists(p),
            TransferError::SameFile(p) => Failure::SameFile(p),
            TransferError::RootFolderMissing(m) => Failure::RootFolderMissing(m),
            TransferError::RecycleBin { .. } => Failure::RecycleBin,
            other => Failure::Other(other.to_string()),
        }
    }
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Failure {
        Failure::Other(format!("{e:#}"))
    }
}

/// `BuildFilePath(movie, BuildFileName(movie, movieFile, customFormats), extension)`.
/// `current_path` is the file as it is now (the drop folder path for a new
/// download, the library path for a rename), for the original-name tokens.
pub fn build_file_path(
    env: &MovieEnvironment,
    movie: &Movie,
    translations: &[(i64, String)],
    file: &MovieFile,
    current_path: &str,
    extension: &str,
    formats: &[CustomFormat],
) -> Result<String, naming::MovieNamingError> {
    // `GetSceneOrFileName`: the scene name, else the file's own name.
    let scene_or_file_name = file
        .scene_name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| file_name(current_path).to_string());
    let media_info = file
        .media_info
        .as_ref()
        .map(|m| format::naming_media_info(m, Some(&scene_or_file_name)));
    let definition = env.catalog.definition(file.quality.quality_id);
    let naming_file = NamingMovieFile {
        quality: file.quality,
        quality_title: &definition.title,
        relative_path: current_path,
        scene_name: file.scene_name.as_deref(),
        release_group: file.release_group.as_deref(),
        edition: file.edition.as_deref(),
        media_info: media_info.as_ref(),
        three_d: file
            .media_info
            .as_ref()
            .is_some_and(|m| format::int(m, "videoMultiViewCount") > 1),
    };
    let naming_movie = NamingMovie {
        metadata: &movie.metadata,
        translations,
    };
    let name = naming::build_file_name(&naming_movie, &naming_file, &env.naming, &naming_formats(formats))?;
    Ok(join(&movie.path, &format!("{name}{extension}")))
}

/// `EnsureMovieFolder`: the movie's folder and the file's created as
/// needed, once the root folder is known to exist.
fn ensure_movie_folder(
    env: &MovieEnvironment,
    root_folders: &[RootFolder],
    movie: &Movie,
    file_path: &str,
) -> Result<(), Failure> {
    let file_folder = parent_path(file_path).unwrap_or("").to_string();
    let movie_folder = movie.path.as_str();
    let root_folder = best_root_folder_path(root_folders, movie_folder);
    if root_folder.trim().is_empty() {
        return Err(Failure::RootFolderMissing(format!(
            "Root folder was not found, '{movie_folder}' is not a subdirectory of a defined root folder."
        )));
    }
    if !Path::new(&root_folder).is_dir() {
        return Err(Failure::RootFolderMissing(format!(
            "Root folder '{root_folder}' was not found."
        )));
    }
    for folder in [movie_folder, file_folder.as_str()] {
        if !Path::new(folder).is_dir() {
            if let Err(e) = transfer::create_folder(&env.config, Path::new(folder)) {
                tracing::error!(error = %e, "Unable to create directory: {folder}");
            }
        }
    }
    Ok(())
}

/// `TransferFile` of `MovieFileMovingService`: the file to its place in
/// the library, the relative path set, the file's date as the setting
/// asks, the folder's date and the permissions applied.
fn transfer_into_library(
    env: &MovieEnvironment,
    movie: &Movie,
    file: &mut MovieFile,
    source: &str,
    destination: &str,
    mode: TransferMode,
) -> Result<(), Failure> {
    if !Path::new(source).is_file() {
        return Err(Failure::Other(format!(
            "Movie file path does not exist: {source}"
        )));
    }
    if source == destination {
        return Err(Failure::SameFile(source.to_string()));
    }
    file.relative_path = relative_path(&movie.path, destination).to_string();
    transfer::transfer_file(Path::new(source), Path::new(destination), mode)?;
    super::filedate::change_file_date(&env.config, destination, movie);
    transfer::set_last_write_time(Path::new(&movie.path), file.date_added);
    transfer::set_permissions(&env.config, Path::new(destination));
    Ok(())
}

/// `MoveMovieFile(movieFile, movie)`: a library file moved to where the
/// naming settings put it now (the renames). `file.relative_path` is
/// updated; the row is the caller's to write.
pub fn move_movie_file(
    env: &MovieEnvironment,
    root_folders: &[RootFolder],
    movie: &Movie,
    translations: &[(i64, String)],
    file: &mut MovieFile,
) -> Result<(), Failure> {
    let current = join(&movie.path, &file.relative_path);
    let formats = crate::movies::formats::file_formats(&env.catalog.base.formats, file, movie);
    let destination = build_file_path(
        env,
        movie,
        translations,
        file,
        &current,
        extension(&current),
        &formats,
    )
    .map_err(|e| Failure::Other(e.to_string()))?;
    ensure_movie_folder(env, root_folders, movie, &destination)?;
    tracing::debug!("Renaming movie file: {} to {}", current, destination);
    transfer_into_library(env, movie, file, &current, &destination, TransferMode::Move)
}

/// `MoveMovieFile(movieFile, localMovie)` or `CopyMovieFile`.
fn move_or_copy(
    env: &MovieEnvironment,
    root_folders: &[RootFolder],
    translations: &[(i64, String)],
    local: &mut LocalMovie,
    file: &mut MovieFile,
    copy_only: bool,
) -> Result<(), Failure> {
    let movie = local.movie_ref().clone();
    let destination = build_file_path(
        env,
        &movie,
        translations,
        file,
        &local.path,
        extension(&local.path),
        &local.custom_formats,
    )
    .map_err(|e| Failure::Other(e.to_string()))?;
    ensure_movie_folder(env, root_folders, &movie, &destination)?;
    let mode = if !copy_only {
        tracing::debug!("Moving movie file: {} to {}", local.path, destination);
        TransferMode::Move
    } else if env.config.copy_using_hardlinks {
        tracing::debug!(
            "Attempting to hardlink movie file: {} to {}",
            local.path,
            destination
        );
        TransferMode::HardLinkOrCopy
    } else {
        tracing::debug!("Copying movie file: {} to {}", local.path, destination);
        TransferMode::Copy
    };
    let source = local.path.clone();
    transfer_into_library(env, &movie, file, &source, &destination, mode)?;
    local.file_name_before_rename = Some(file.relative_path.clone());
    Ok(())
}

/// `UpgradeMovieFile`: the file the movie has is recycled and its row
/// deleted, then the new file is moved or copied in.
#[allow(clippy::too_many_arguments)]
fn upgrade(
    db: &Db,
    env: &MovieEnvironment,
    root_folders: &[RootFolder],
    translations: &[(i64, String)],
    local: &mut LocalMovie,
    file: &mut MovieFile,
    copy_only: bool,
    now: DateTime<Utc>,
    events: &mut MovieEvents,
) -> Result<Vec<DeletedMovieFile>, Failure> {
    tracing::trace!("Upgrading movie file.");
    let movie = local.movie_ref().clone();
    let existing = if movie.movie_file_id > 0 {
        let id = movie.movie_file_id;
        db.call_blocking(move |conn| store::get_movie_file(conn, id))?
    } else {
        None
    };
    let root_folder = parent_path(&movie.path).unwrap_or("").to_string();
    // With a file to replace and the root folder missing, stop: the old
    // file must not be left behind by a half-done import.
    if existing.is_some() && !Path::new(&root_folder).is_dir() {
        return Err(Failure::RootFolderMissing(format!(
            "Root folder '{root_folder}' was not found."
        )));
    }
    let mut old_files = Vec::new();
    if let Some(old) = existing {
        let path = join(&movie.path, &old.relative_path);
        let subfolder = relative_path(&root_folder, parent_path(&path).unwrap_or("")).to_string();
        let mut recycle_bin_path = None;
        if Path::new(&path).is_file() {
            tracing::debug!("Removing existing movie file: {}", path);
            recycle_bin_path = transfer::recycle_file(&env.config, Path::new(&path), &subfolder)?
                .map(|p| p.to_string_lossy().into_owned());
        } else {
            tracing::warn!("Existing movie file missing from disk: {}", path);
        }
        old_files.push(DeletedMovieFile {
            file: old.clone(),
            recycle_bin_path,
        });
        let movie2 = movie.clone();
        let deleted = db.call_blocking(move |conn| {
            deletion::delete_file(conn, &movie2, &old, DeleteReason::Upgrade, now)
        })?;
        deletion::clean_folders(&env.config, &deleted);
        events.push(MovieImportEvent::FileDeleted(Box::new(deleted)));
    }
    local.old_files = old_files.clone();
    move_or_copy(env, root_folders, translations, local, file, copy_only)?;
    Ok(old_files)
}

/// `GetOriginalFilePath`: the file's path relative to the download's
/// folder, to the folder its release is named after, or to its
/// grandparent.
fn original_file_path(item: Option<&DownloadClientItem>, local: &LocalMovie) -> String {
    let path = local.path.as_str();
    if let Some(output) = item
        .and_then(|i| i.output_path.as_deref())
        .filter(|o| !o.is_empty())
    {
        // `OutputPath.Directory`: the parent of what the client reports.
        let output_dir = parent_path(output.trim_end_matches('/'))
            .unwrap_or("")
            .to_string();
        if is_parent_path(&output_dir, path) {
            return relative_path(&output_dir, path).to_string();
        }
    }
    if let Some(folder_info) = &local.folder_movie_info {
        if let Some(folder) = ancestor_path(path, &folder_info.original_title) {
            if let Some(parent) = parent_path(folder) {
                return relative_path(parent, path).to_string();
            }
        }
    }
    if let Some(grandparent) = parent_path(path).and_then(parent_path) {
        return relative_path(grandparent, path).to_string();
    }
    file_name(path).to_string()
}

/// `GetAncestorPath`: the nearest folder above `path` named `folder_name`.
fn ancestor_path<'a>(path: &'a str, folder_name: &str) -> Option<&'a str> {
    let mut current = parent_path(path);
    while let Some(folder) = current {
        if file_name(folder).eq_ignore_ascii_case(folder_name) {
            return Some(folder);
        }
        current = parent_path(folder);
    }
    None
}

/// One approved decision imported.
#[allow(clippy::too_many_arguments)]
fn import_one(
    db: &Db,
    env: &MovieEnvironment,
    root_folders: &[RootFolder],
    local: &mut LocalMovie,
    new_download: bool,
    item: Option<&DownloadClientItem>,
    mode: ImportMode,
    now: DateTime<Utc>,
    events: &mut MovieEvents,
) -> Result<(MovieFile, Vec<DeletedMovieFile>), Failure> {
    let movie = local.movie_ref().clone();
    let mut file = MovieFile {
        id: 0,
        movie_id: movie.id,
        relative_path: String::new(),
        original_file_path: None,
        size: std::fs::metadata(&local.path)
            .map(|m| m.len() as i64)
            .unwrap_or(0),
        date_added: now,
        scene_name: None,
        release_group: local.release_group.clone(),
        edition: local.edition.clone(),
        quality: local.quality,
        languages: local.languages.clone(),
        media_info: local.media_info.as_ref().map(|m| m.model.clone()),
        indexer_flags: 0,
    };
    match item.map(|i| i.download_id.trim()).filter(|d| !d.is_empty()) {
        Some(download_id) => {
            let id = download_id.to_string();
            let flags = db.call_blocking(move |conn| {
                Ok(store::history_by_download_id(conn, &id)?
                    .into_iter()
                    .filter(|h| h.event_type == MovieHistoryEvent::Grabbed)
                    .max_by_key(|h| h.date)
                    .and_then(|h| h.data["indexerFlags"].as_str().and_then(parse_indexer_flags)))
            })?;
            if let Some(flags) = flags {
                file.indexer_flags = flags;
            }
        }
        None => file.indexer_flags = local.indexer_flags,
    }
    let copy_only = match mode {
        ImportMode::Auto => item.is_some_and(|i| !i.can_move_files),
        ImportMode::Move => false,
        ImportMode::Copy => true,
    };
    let metadata_id = movie.metadata.id;
    let translations = db.call_blocking(move |conn| store::translation_titles(conn, metadata_id))?;
    let mut old_files = Vec::new();
    if new_download {
        file.scene_name = local.scene_name.clone();
        file.original_file_path = Some(original_file_path(item, local));
        old_files = upgrade(
            db,
            env,
            root_folders,
            &translations,
            local,
            &mut file,
            copy_only,
            now,
            events,
        )?;
    } else {
        file.relative_path = relative_path(&movie.path, &local.path).to_string();
        // The rows already mapped to this path go.
        let (movie2, relative) = (movie.clone(), file.relative_path.clone());
        let deleted = db.call_blocking(move |conn| {
            let previous = store::movie_files_with_relative_path(conn, movie2.id, &relative)?;
            let mut out = Vec::new();
            for p in previous {
                out.push(deletion::delete_file(
                    conn,
                    &movie2,
                    &p,
                    DeleteReason::ManualOverride,
                    now,
                )?);
            }
            Ok(out)
        })?;
        for d in deleted {
            events.push(MovieImportEvent::FileDeleted(Box::new(d)));
        }
    }
    // `MediaFileService.Add` and its `MovieFileAddedEvent`: the row, and
    // the movie pointed at it.
    let mut added = file.clone();
    let added = db.call_blocking(move |conn| {
        store::add_movie_file(conn, &mut added)?;
        store::set_movie_file_id(conn, added.movie_id, added.id)?;
        Ok(added)
    })?;
    tracing::info!(
        "Assigning file [{}] to movie [{}]",
        added.relative_path,
        movie.metadata.title
    );
    if let Some(m) = local.movie.as_mut() {
        m.movie_file_id = added.id;
    }
    if new_download && (!local.script_imported || local.should_import_extras) {
        if let Err(e) = super::extras::import_movie(db, &env.config, local, &added, copy_only, now) {
            tracing::warn!(error = %format!("{e:#}"), "Failed to import the extra files of {}", local.path);
        }
    }
    Ok((added, old_files))
}

/// `QualityModelComparer` under a movie's profile, best first.
fn best_quality_first(
    env: &MovieEnvironment,
    movie: &Movie,
    a: &QualityModel,
    b: &QualityModel,
) -> std::cmp::Ordering {
    match env.catalog.base.profile(movie.quality_profile_id) {
        Some(profile) => crate::decision::upgradable::compare_quality(profile, b.quality_id, a.quality_id)
            .then_with(|| crate::decision::upgradable::compare_revision(b, a)),
        None => std::cmp::Ordering::Equal,
    }
}

/// `Import(decisions, newDownload, downloadClientItem, importMode)`.
pub fn import(
    db: &Db,
    env: &MovieEnvironment,
    decisions: Vec<MovieImportDecision>,
    new_download: bool,
    item: Option<&DownloadClientItem>,
    mode: ImportMode,
    now: DateTime<Utc>,
) -> ImportedMovies {
    tracing::debug!("Decisions: {}", decisions.len());
    let mut imported = ImportedMovies::default();
    let root_folders = db
        .call_blocking(|conn| crate::store::catalog::list_root_folders(conn))
        .unwrap_or_default();
    let (approved, rejected): (Vec<_>, Vec<_>) = decisions
        .into_iter()
        .partition(|d| d.approved() && d.local_movie.as_ref().is_some_and(|l| l.movie.is_some()));
    // Grouped by movie in the order met, each group best quality first and
    // then largest first; the whole then largest first, ties as they stand.
    let mut groups: Vec<(i64, Vec<MovieImportDecision>)> = Vec::new();
    for d in approved {
        let id = d.local_movie.as_ref().map(|l| l.movie_ref().id).unwrap_or(0);
        match groups.iter_mut().find(|(g, _)| *g == id) {
            Some((_, group)) => group.push(d),
            None => groups.push((id, vec![d])),
        }
    }
    let mut qualified: Vec<MovieImportDecision> = Vec::new();
    for (_, mut group) in groups {
        group.sort_by(|a, b| {
            let (la, lb) = (a.local_movie.as_ref().unwrap(), b.local_movie.as_ref().unwrap());
            best_quality_first(env, la.movie_ref(), &la.quality, &lb.quality).then(lb.size.cmp(&la.size))
        });
        qualified.extend(group);
    }
    qualified.sort_by(|a, b| {
        let (la, lb) = (a.local_movie.as_ref().unwrap(), b.local_movie.as_ref().unwrap());
        lb.size.cmp(&la.size)
    });
    let mut imported_movies: Vec<i64> = Vec::new();
    for mut decision in qualified {
        let mut local = decision
            .local_movie
            .take()
            .expect("approved decisions carry a file");
        let movie_id = local.movie_ref().id;
        if imported_movies.contains(&movie_id) {
            decision.local_movie = Some(local);
            imported.results.push(MovieImportResult::new(
                decision,
                vec!["Movie has already been imported".into()],
            ));
            continue;
        }
        match import_one(
            db,
            env,
            &root_folders,
            &mut local,
            new_download,
            item,
            mode,
            now,
            &mut imported.events,
        ) {
            Ok((file, old_files)) => {
                imported_movies.push(movie_id);
                let ev = MovieImported {
                    local: local.clone(),
                    file,
                    old_files,
                    new_download,
                    client: item.map(|i| i.client.clone()),
                    download_id: item.map(|i| i.download_id.clone()),
                };
                let ev2 = ev.clone();
                if let Err(e) = db.call_blocking(move |conn| events::record_imported(conn, &ev2, now)) {
                    tracing::error!(error = %e, "failed to record the import in the history");
                }
                imported.events.push(MovieImportEvent::Imported(Box::new(ev)));
                decision.local_movie = Some(local);
                imported
                    .results
                    .push(MovieImportResult::new(decision, Vec::new()));
            }
            Err(failure) => {
                tracing::warn!("Couldn't import movie {}: {}", local.path, failure.detail());
                match &failure {
                    Failure::RootFolderMissing(_) | Failure::RecycleBin => {
                        imported
                            .events
                            .push(MovieImportEvent::ImportFailed(Box::new(MovieImportFailed {
                                local: local.clone(),
                                message: failure.detail(),
                                new_download,
                                client: item.map(|i| i.client.clone()),
                                download_id: item.map(|i| i.download_id.clone()),
                            })));
                    }
                    Failure::DestinationExists(_) => {
                        imported
                            .events
                            .push(MovieImportEvent::RescanRequested { movie_id });
                    }
                    _ => {}
                }
                decision.local_movie = Some(local);
                imported.results.push(MovieImportResult::new(
                    decision,
                    vec![failure.message().to_string()],
                ));
            }
        }
    }
    // The rejected decisions last, each with its rejections' messages.
    for d in rejected {
        let errors = d.rejections.iter().map(|r| r.message.clone()).collect();
        imported.results.push(MovieImportResult::new(d, errors));
    }
    imported
}

#[cfg(test)]
mod tests {
    //! The file work of an import, over a library and a download folder in
    //! a temporary directory: what the transfer modes leave behind, and
    //! what becomes of the file an upgrade replaces.

    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;

    use super::*;
    use crate::downloadclients::{ClientKind, ItemClientInfo};
    use crate::mediafiles::local::ImportResultType;
    use crate::model::Protocol;
    use crate::movies::decision::testing;
    use crate::movies::model::MovieNamingConfig;
    use crate::movies::parser::quality::{BLURAY1080P, WEBDL1080P};
    use crate::Personality;

    struct Library {
        root: PathBuf,
        db: Db,
        env: MovieEnvironment,
        movie: Movie,
    }

    impl Drop for Library {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn library(test: &str) -> Library {
        let root = std::env::temp_dir().join(format!("unarr-movie-approve-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let movies = root.join("movies");
        std::fs::create_dir_all(&movies).unwrap();
        std::fs::create_dir_all(root.join("downloads")).unwrap();
        let mut movie = testing::movie(1, "The Movie", 2018);
        movie.path = movies.join("The Movie (2018)").to_string_lossy().into_owned();
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        let (movie2, movies2) = (movie.clone(), movies.to_string_lossy().into_owned());
        let mut env = db
            .call_blocking(move |conn| {
                store::insert_metadata(conn, &movie2.metadata)?;
                store::insert_movie(conn, &movie2)?;
                crate::store::catalog::insert_root_folder(conn, &RootFolder { id: 1, path: movies2 })?;
                MovieEnvironment::load(conn, Utc::now())
            })
            .unwrap();
        env.naming = MovieNamingConfig {
            rename_movies: true,
            replace_illegal_characters: true,
            colon_replacement_format: 4,
            standard_movie_format: "{Movie Title} ({Release Year}) [{Quality Full}]".into(),
            movie_folder_format: "{Movie Title} ({Release Year})".into(),
        };
        Library { root, db, env, movie }
    }

    impl Library {
        /// A download's file, and the approved decision to import it.
        fn download(&self, name: &str, quality_id: i64) -> (String, MovieImportDecision) {
            let path = self.root.join("downloads").join(name).join(format!("{name}.mkv"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, name.as_bytes()).unwrap();
            let path = path.to_string_lossy().into_owned();
            let id = self.movie.id;
            let movie = self
                .db
                .call_blocking(move |conn| store::get_movie(conn, id))
                .unwrap()
                .unwrap();
            let local = LocalMovie {
                path: path.clone(),
                size: name.len() as i64,
                movie: Some(movie),
                quality: testing::quality(quality_id, 1),
                languages: vec![1],
                scene_name: Some(name.to_string()),
                ..Default::default()
            };
            (path, MovieImportDecision::new(local, Vec::new()))
        }

        fn item(&self, name: &str, can_move_files: bool) -> DownloadClientItem {
            let mut item = DownloadClientItem::new(
                ItemClientInfo {
                    protocol: Protocol::Torrent,
                    kind: ClientKind::QBittorrent,
                    id: 1,
                    name: "qBittorrent".into(),
                    remove_completed_downloads: true,
                    has_post_import_category: false,
                },
                "HASH1",
                name,
            );
            item.can_move_files = can_move_files;
            item.output_path = Some(
                self.root
                    .join("downloads")
                    .join(name)
                    .to_string_lossy()
                    .into_owned(),
            );
            item
        }

        fn import(&self, decision: MovieImportDecision, item: &DownloadClientItem) -> ImportedMovies {
            import(
                &self.db,
                &self.env,
                vec![decision],
                true,
                Some(item),
                ImportMode::Auto,
                Utc::now(),
            )
        }

        fn files(&self) -> Vec<MovieFile> {
            let id = self.movie.id;
            self.db
                .call_blocking(move |conn| store::movie_files_of(conn, id))
                .unwrap()
        }

        fn history(&self) -> Vec<MovieHistoryEvent> {
            let id = self.movie.id;
            let mut rows = self
                .db
                .call_blocking(move |conn| store::history_of_movie(conn, id, None))
                .unwrap();
            rows.sort_by_key(|h| h.id);
            rows.into_iter().map(|h| h.event_type).collect()
        }
    }

    const NAME: &str = "The.Movie.2018.1080p.WEB-DL.DD5.1.H264-GRP";
    const BETTER: &str = "The.Movie.2018.1080p.BluRay.x264-GRP";

    #[test]
    fn a_download_its_client_keeps_seeding_is_hardlinked_into_the_library() {
        let lib = library("hardlink");
        let (source, decision) = lib.download(NAME, WEBDL1080P);
        let imported = lib.import(decision, &lib.item(NAME, false));
        assert_eq!(imported.results[0].result(), ImportResultType::Imported);
        let files = lib.files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "The Movie (2018) [WEBDL-1080p].mkv");
        assert_eq!(files[0].scene_name.as_deref(), Some(NAME));
        assert_eq!(
            files[0].original_file_path.as_deref(),
            Some(format!("{NAME}/{NAME}.mkv").as_str())
        );
        let destination = join(&lib.movie.path, &files[0].relative_path);
        // The source stays for the client, and both names are one file.
        let (kept, linked) = (
            std::fs::metadata(&source).unwrap(),
            std::fs::metadata(&destination).unwrap(),
        );
        assert_eq!(kept.ino(), linked.ino());
        assert_eq!(linked.nlink(), 2);
        assert_eq!(lib.history(), vec![MovieHistoryEvent::DownloadFolderImported]);
    }

    #[test]
    fn with_hardlinks_off_the_seeding_download_is_copied() {
        let mut lib = library("copy");
        lib.env.config.copy_using_hardlinks = false;
        let (source, decision) = lib.download(NAME, WEBDL1080P);
        lib.import(decision, &lib.item(NAME, false));
        let destination = join(&lib.movie.path, &lib.files()[0].relative_path);
        let (kept, copied) = (
            std::fs::metadata(&source).unwrap(),
            std::fs::metadata(&destination).unwrap(),
        );
        assert_ne!(kept.ino(), copied.ino());
        assert_eq!(copied.nlink(), 1);
    }

    #[test]
    fn a_download_its_client_is_done_with_is_moved() {
        let lib = library("move");
        let (source, decision) = lib.download(NAME, WEBDL1080P);
        lib.import(decision, &lib.item(NAME, true));
        let destination = join(&lib.movie.path, &lib.files()[0].relative_path);
        assert!(!Path::new(&source).exists());
        assert!(Path::new(&destination).is_file());
    }

    #[test]
    fn an_upgrade_deletes_the_file_it_replaces_for_good_without_a_recycling_bin() {
        let lib = library("upgrade");
        let (_, first) = lib.download(NAME, WEBDL1080P);
        lib.import(first, &lib.item(NAME, false));
        let old = join(&lib.movie.path, &lib.files()[0].relative_path);
        let (_, second) = lib.download(BETTER, BLURAY1080P);
        let imported = lib.import(second, &lib.item(BETTER, false));
        assert_eq!(imported.results[0].result(), ImportResultType::Imported);
        assert!(!Path::new(&old).exists());
        let files = lib.files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "The Movie (2018) [Bluray-1080p].mkv");
        // The deletion is written before the import that caused it, and
        // the ids of the files keep counting.
        assert_eq!(files[0].id, 2);
        assert_eq!(
            lib.history(),
            vec![
                MovieHistoryEvent::DownloadFolderImported,
                MovieHistoryEvent::MovieFileDeleted,
                MovieHistoryEvent::DownloadFolderImported,
            ]
        );
        let id = lib.movie.id;
        let movie = lib
            .db
            .call_blocking(move |conn| store::get_movie(conn, id))
            .unwrap()
            .unwrap();
        assert_eq!(movie.movie_file_id, 2);
        assert!(movie.monitored, "an upgrade never unmonitors");
    }

    #[test]
    fn an_upgrade_moves_the_file_it_replaces_to_the_recycling_bin_when_one_is_set() {
        let mut lib = library("recycle");
        let bin = lib.root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        lib.env.config.recycle_bin = bin.to_string_lossy().into_owned();
        let (_, first) = lib.download(NAME, WEBDL1080P);
        lib.import(first, &lib.item(NAME, false));
        let old_name = lib.files()[0].relative_path.clone();
        let (_, second) = lib.download(BETTER, BLURAY1080P);
        let imported = lib.import(second, &lib.item(BETTER, false));
        let recycled = bin.join("The Movie (2018)").join(&old_name);
        assert!(recycled.is_file(), "{recycled:?} is not in the bin");
        let deleted = imported
            .events
            .0
            .iter()
            .find_map(|e| match e {
                MovieImportEvent::Imported(ev) => ev.old_files.first().cloned(),
                _ => None,
            })
            .expect("the import names the file it replaced");
        assert_eq!(
            deleted.recycle_bin_path.as_deref(),
            Some(recycled.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn a_missing_root_folder_stops_the_import_and_says_so() {
        let lib = library("rootless");
        let (source, decision) = lib.download(NAME, WEBDL1080P);
        std::fs::remove_dir_all(lib.root.join("movies")).unwrap();
        let imported = lib.import(decision, &lib.item(NAME, true));
        assert_eq!(imported.results[0].result(), ImportResultType::Skipped);
        assert_eq!(
            imported.results[0].errors,
            vec!["Failed to import movie, Root folder missing.".to_string()]
        );
        assert!(Path::new(&source).is_file());
        assert!(imported
            .events
            .0
            .iter()
            .any(|e| matches!(e, MovieImportEvent::ImportFailed(_))));
        assert!(lib.files().is_empty());
    }
}
