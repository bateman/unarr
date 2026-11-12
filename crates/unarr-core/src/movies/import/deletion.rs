//! `MediaFileService.Delete` with the handlers of `MovieFileDeletedEvent`,
//! and `MediaFileDeletionService.DeleteMovieFile`: a movie file's row
//! removed, its movie detached, the history written.

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use super::events::{self, MovieFileDeleted};
use crate::mediafiles::local::DeleteReason;
use crate::movies::model::{Movie, MovieFile};
use crate::movies::store;

/// `AutoUnmonitorPreviouslyDownloadedMovies`.
fn auto_unmonitor(conn: &Connection) -> anyhow::Result<bool> {
    Ok(
        crate::store::config::config(conn, "AutoUnmonitorPreviouslyDownloadedMovies")?
            .is_some_and(|v| v.eq_ignore_ascii_case("true")),
    )
}

/// `MediaFileService.Delete(movieFile, reason)` and what its event's
/// handlers do: the row goes, the movie no longer points at it and, unless
/// the file went for an upgrade, is no longer monitored when the setting
/// says so; the history gets its `MovieFileDeleted` row.
pub fn delete_file(
    conn: &Connection,
    movie: &Movie,
    file: &MovieFile,
    reason: DeleteReason,
    now: DateTime<Utc>,
) -> anyhow::Result<MovieFileDeleted> {
    store::delete_movie_file(conn, file.id)?;
    tracing::debug!("Detaching movie {} from file.", movie.id);
    store::clear_movie_file_id(conn, file.id)?;
    if reason != DeleteReason::Upgrade && auto_unmonitor(conn)? {
        store::set_movie_monitored(conn, movie.id, false)?;
    }
    let config = crate::store::config::MediaManagementConfig::load(conn)?;
    super::extras::movie_file_deleted(conn, &config, movie, file.id, reason)?;
    let ev = MovieFileDeleted {
        file: file.clone(),
        movie: movie.clone(),
        reason,
    };
    events::record_deleted(conn, &ev, now)?;
    Ok(ev)
}

/// `MediaFileDeletionService.Handle(MovieFileDeletedEvent)`: with Delete
/// Empty Folders on, the folders the file leaves empty go, up to the
/// movie's own.
pub fn clean_folders(config: &crate::store::config::MediaManagementConfig, ev: &MovieFileDeleted) {
    use crate::mediafiles::{is_parent_path, parent_path, transfer};
    use std::path::Path;
    if !config.delete_empty_folders {
        return;
    }
    let movie_path = ev.movie.path.as_str();
    let path = ev.path();
    let mut folder = parent_path(&path).map(str::to_string);
    while let Some(f) = folder.clone() {
        if !is_parent_path(movie_path, &f) {
            break;
        }
        if Path::new(&f).is_dir() {
            transfer::remove_empty_subfolders(Path::new(&f));
        }
        folder = parent_path(&f).map(str::to_string);
    }
    transfer::remove_empty_subfolders(Path::new(movie_path));
    if transfer::folder_empty(Path::new(movie_path)) {
        if let Err(e) = std::fs::remove_dir_all(movie_path) {
            tracing::debug!(error = %e, "unable to delete the empty movie folder {movie_path}");
        }
    }
}

/// What `DeleteMovieFile` refuses with, and the status the API answers.
#[derive(Debug, thiserror::Error)]
pub enum DeleteError {
    #[error("Movie's root folder ({0}) doesn't exist.")]
    RootFolderMissing(String),
    #[error("Movie's root folder ({0}) is empty. Rescan will not update movies as a failsafe.")]
    RootFolderEmpty(String),
    #[error("Unable to delete movie file")]
    Unable,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl DeleteError {
    pub fn status(&self) -> u16 {
        match self {
            DeleteError::RootFolderMissing(_) | DeleteError::RootFolderEmpty(_) => 409,
            DeleteError::Unable | DeleteError::Other(_) => 500,
        }
    }
}

/// `MediaFileDeletionService.DeleteMovieFile(movie, movieFile)`: the file
/// to the recycling bin (or gone for good without one), then its row, as
/// a manual deletion. The row goes even when the file was already gone.
/// File work: call it off the async runtime.
pub fn delete_movie_file(
    db: &crate::db::Db,
    config: &crate::store::config::MediaManagementConfig,
    movie: &Movie,
    file: &MovieFile,
    now: DateTime<Utc>,
) -> Result<MovieFileDeleted, DeleteError> {
    use crate::mediafiles::{join, parent_path, relative_path, transfer};
    use std::path::Path;
    let full_path = join(&movie.path, &file.relative_path);
    let root_folder = parent_path(&movie.path).unwrap_or("").to_string();
    if !Path::new(&root_folder).is_dir() {
        tracing::warn!("Movie's root folder ({root_folder}) doesn't exist.");
        return Err(DeleteError::RootFolderMissing(root_folder));
    }
    if transfer::directories_under(Path::new(&root_folder)).is_empty() {
        tracing::warn!(
            "Movie's root folder ({root_folder}) is empty. Rescan will not update movies as a failsafe."
        );
        return Err(DeleteError::RootFolderEmpty(root_folder));
    }
    if Path::new(&movie.path).is_dir() && Path::new(&full_path).is_file() {
        tracing::info!("Deleting movie file: {full_path}");
        let subfolder = relative_path(&root_folder, parent_path(&full_path).unwrap_or("")).to_string();
        if let Err(e) = transfer::recycle_file(config, Path::new(&full_path), &subfolder) {
            tracing::error!(error = %e, "Unable to delete movie file");
            return Err(DeleteError::Unable);
        }
    }
    let (movie2, file2) = (movie.clone(), file.clone());
    let ev = db.call_blocking(move |conn| delete_file(conn, &movie2, &file2, DeleteReason::Manual, now))?;
    clean_folders(config, &ev);
    Ok(ev)
}
