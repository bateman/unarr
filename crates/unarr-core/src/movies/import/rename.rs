//! `RenameMovieFileService`: what the naming settings would call each
//! file of a movie, and the files moved there.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use super::approve::{self, Failure};
use super::decide::MovieEnvironment;
use super::events::{self, MovieEvents, MovieFileRenamed, MovieImportEvent};
use crate::db::Db;
use crate::mediafiles::{extension, join, relative_path, transfer};
use crate::movies::model::{Movie, MovieFile};
use crate::movies::store;

/// `RenameMovieFilePreview`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameMoviePreview {
    pub movie_id: i64,
    pub movie_file_id: i64,
    pub existing_path: String,
    pub new_path: String,
}

/// `GetPreviews(movie, files)`: the files whose name would change.
pub fn previews(
    conn: &Connection,
    env: &MovieEnvironment,
    movie: &Movie,
    files: &[MovieFile],
) -> anyhow::Result<Vec<RenameMoviePreview>> {
    let translations = store::translation_titles(conn, movie.metadata.id)?;
    let mut out = Vec::new();
    for file in files {
        let current = join(&movie.path, &file.relative_path);
        let formats = crate::movies::formats::file_formats(&env.catalog.base.formats, file, movie);
        let Ok(new_path) = approve::build_file_path(
            env,
            movie,
            &translations,
            file,
            &current,
            extension(&current),
            &formats,
        ) else {
            continue;
        };
        if current != new_path {
            out.push(RenameMoviePreview {
                movie_id: movie.id,
                movie_file_id: file.id,
                existing_path: file.relative_path.clone(),
                new_path: relative_path(&movie.path, &new_path).to_string(),
            });
        }
    }
    Ok(out)
}

/// `GetRenamePreviews(movieIds)`.
pub fn previews_for_movies(
    conn: &Connection,
    env: &MovieEnvironment,
    movie_ids: &[i64],
) -> anyhow::Result<Vec<RenameMoviePreview>> {
    let mut out = Vec::new();
    let movies = store::movies_by_ids(conn, movie_ids)?;
    // `BasicRepository.Get(ids)`: every id asked for must be there.
    let mut wanted: Vec<i64> = movie_ids.to_vec();
    wanted.sort_unstable();
    wanted.dedup();
    if movies.len() != wanted.len() {
        anyhow::bail!(
            "Expected query to return {} rows but returned {}",
            wanted.len(),
            movies.len()
        );
    }
    for movie in movies {
        let files = store::movie_files_of(conn, movie.id)?;
        out.extend(previews(conn, env, &movie, &files)?);
    }
    Ok(out)
}

/// `RenameFiles(movieFiles, movie)`: each file moved to its new name, its
/// row and the history written; what could not be moved is logged and
/// left. File work: call it off the async runtime.
pub fn rename_files(
    db: &Db,
    env: &MovieEnvironment,
    movie: &Movie,
    files: Vec<MovieFile>,
    now: DateTime<Utc>,
) -> anyhow::Result<MovieEvents> {
    let mut events = MovieEvents::default();
    let root_folders = db.call_blocking(|conn| crate::store::catalog::list_root_folders(conn))?;
    let metadata_id = movie.metadata.id;
    let translations = db.call_blocking(move |conn| store::translation_titles(conn, metadata_id))?;
    let mut renamed = Vec::new();
    for mut file in files {
        let previous_path = join(&movie.path, &file.relative_path);
        tracing::debug!("Renaming movie file: {previous_path}");
        match approve::move_movie_file(env, &root_folders, movie, &translations, &mut file) {
            Ok(()) => {
                let ev = MovieFileRenamed {
                    movie: movie.clone(),
                    file: file.clone(),
                    original_path: previous_path.clone(),
                };
                let (ev2, config) = (ev.clone(), env.config.clone());
                // The file has moved: a row that cannot be written is this
                // file's failure, logged as the original logs any, and the
                // other files still get their turn.
                let stored = db.call_blocking(move |conn| {
                    store::update_movie_file(conn, &ev2.file)?;
                    events::record_renamed(conn, &ev2, now)?;
                    // The subtitles and the other extras follow the file.
                    if let Err(e) = super::extras::move_after_rename(conn, &config, &ev2.movie, &ev2.file, now) {
                        tracing::warn!(error = %e, "failed to move the extra files of {}", ev2.file.relative_path);
                    }
                    Ok(())
                });
                if let Err(e) = stored {
                    tracing::error!("Failed to rename file: {previous_path}: {e:#}");
                    continue;
                }
                tracing::debug!("Renamed movie file: {}", file.relative_path);
                events.push(MovieImportEvent::FileRenamed(Box::new(ev.clone())));
                renamed.push(ev);
            }
            Err(Failure::DestinationExists(path)) => {
                tracing::warn!("File not renamed, there is already a file at the destination: {path}")
            }
            Err(Failure::SameFile(path)) => {
                tracing::debug!("File not renamed, source and destination are the same: {path}")
            }
            Err(e) => tracing::error!("Failed to rename file: {previous_path}: {}", e.detail()),
        }
    }
    if !renamed.is_empty() {
        transfer::remove_empty_subfolders(Path::new(&movie.path));
        events.push(MovieImportEvent::MovieRenamed {
            movie_id: movie.id,
            renamed,
        });
    }
    Ok(events)
}
