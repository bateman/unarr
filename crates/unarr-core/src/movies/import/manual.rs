//! Radarr's `ManualImportService`: what the interactive import shows for a
//! folder, a download or a movie, how an item is evaluated again when the
//! user changes it, and the `ManualImport` command that imports the
//! choices.

use std::path::Path;

use chrono::{DateTime, Utc};

use super::aggregate;
use super::approve;
use super::completed;
use super::decide::{self, MovieEnvironment};
use super::downloaded;
use super::events::{self, MovieDownloadCompleted, MovieEvents, MovieImportEvent};
use super::local::{
    LocalMovie, MovieImportDecision, MovieImportRejection, MovieImportResult, MovieRejectionReason,
};
use super::scan::filter_paths;
use super::specs::StoreImportSources;
use crate::db::Db;
use crate::downloads::TrackedDownloadState;
use crate::mediafiles::decide::Probes;
use crate::mediafiles::local::{ImportMode, ImportResultType};
use crate::mediafiles::scan::video_files;
use crate::mediafiles::transfer::directories_under;
use crate::mediafiles::{
    file_name, file_stem, is_parent_path, join, parent_path, path_equals, relative_path,
};
use crate::model::{CustomFormat, QualityModel};
use crate::movies::decision::{mapping, MovieCatalog, MovieLookup, StoreMovieLookup};
use crate::movies::downloads::{MovieDownloads, TrackedMovieDownload};
use crate::movies::model::{Movie, MovieFile};
use crate::movies::parser::language::{lang, parse_languages};
use crate::movies::parser::quality::parse_quality;
use crate::movies::parser::{parse_movie_path, parse_movie_title, parse_release_group, ParsedMovieInfo};
use crate::movies::quality::MovieLanguage;
use crate::movies::store;

/// `ManualImportItem`.
#[derive(Debug, Clone)]
pub struct ManualImportItem {
    pub path: String,
    pub relative_path: String,
    pub folder_name: Option<String>,
    /// `None` for the library's own files, whose `Path` the original
    /// never fills.
    pub name: Option<String>,
    pub size: i64,
    pub movie: Option<Movie>,
    /// How the movie was come by, which its resource shows.
    pub movie_source: MovieSource,
    pub movie_file_id: Option<i64>,
    /// `None` where the original leaves it null: a file nothing could be
    /// read from.
    pub quality: Option<QualityModel>,
    /// `None` where the original leaves the list null.
    pub languages: Option<Vec<i64>>,
    pub release_group: Option<String>,
    pub download_id: Option<String>,
    pub custom_formats: Vec<CustomFormat>,
    pub indexer_flags: i64,
    pub rejections: Vec<MovieImportRejection>,
}

impl ManualImportItem {
    /// The item `ProcessFile` falls back to: the class's own defaults
    /// around the file's path and size.
    fn bare(path: &str, root_folder: &str, download_id: Option<&str>) -> ManualImportItem {
        ManualImportItem {
            path: path.to_string(),
            relative_path: relative_path(root_folder, path).to_string(),
            folder_name: None,
            name: Some(file_stem(path).to_string()),
            size: file_size(path),
            movie: None,
            movie_source: MovieSource::Id,
            movie_file_id: None,
            quality: Some(QualityModel::default()),
            languages: Some(Vec::new()),
            release_group: None,
            download_id: download_id.map(String::from),
            custom_formats: Vec::new(),
            indexer_flags: 0,
            rejections: Vec::new(),
        }
    }

    /// `CalculateCustomFormatScore` under the movie's profile.
    pub fn custom_format_score(&self, catalog: &MovieCatalog) -> i64 {
        self.movie
            .as_ref()
            .and_then(|m| catalog.base.profile(m.quality_profile_id))
            .map(|p| crate::decision::upgradable::format_score(p, &self.custom_formats))
            .unwrap_or(0)
    }
}

/// Where the movie of an item comes from. The original's movie carries
/// its alternative titles when it was read by its id and not when a title
/// found it; the one a tracked download holds is the object the download
/// keeps, with whatever was done to it since.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MovieSource {
    /// `MovieService.GetMovie(id)`.
    #[default]
    Id,
    /// `ParsingService.GetMovie(title)` or `MovieService.FindByTitle`.
    Title,
    /// `TrackedDownload.RemoteMovie.Movie`; `by_id` as
    /// [`TrackedMovieDownload::movie_by_id`] says.
    Download { by_id: bool },
}

/// `ManualImportFile`: one choice of the `ManualImport` command.
#[derive(Debug, Clone, PartialEq)]
pub struct ManualImportFile {
    pub path: String,
    pub folder_name: Option<String>,
    pub movie_id: i64,
    pub quality: QualityModel,
    pub languages: Vec<i64>,
    pub release_group: Option<String>,
    pub indexer_flags: i64,
    pub download_id: Option<String>,
}

/// What `ReprocessItem` was asked.
#[derive(Debug, Clone)]
pub struct ReprocessRequest {
    pub path: String,
    pub download_id: Option<String>,
    pub movie_id: i64,
    pub release_group: Option<String>,
    /// `None` when the request carries no quality.
    pub quality: Option<QualityModel>,
    /// `None` when the request carries no languages.
    pub languages: Option<Vec<i64>>,
    pub indexer_flags: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum ManualImportError {
    /// `ModelNotFoundException`.
    #[error("Movie with ID {0} does not exist")]
    MovieNotFound(i64),
    /// `AugmentingFailedException`, which `ReprocessItem` and `Execute`
    /// let escape.
    #[error("{0}")]
    Augmenting(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

fn file_size(path: &str) -> i64 {
    std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0)
}

fn blank(s: Option<&str>) -> bool {
    s.is_none_or(|s| s.trim().is_empty())
}

/// `SceneSource(movie, folder)`: anything outside the movie's folder.
fn scene_source(movie: &Movie, folder: &str) -> bool {
    !(path_equals(&movie.path, folder) || is_parent_path(&movie.path, folder))
}

/// `GetTrackedDownload(downloadId)`.
fn tracked_for(downloads: &MovieDownloads, download_id: Option<&str>) -> Option<TrackedMovieDownload> {
    download_id
        .filter(|d| !d.trim().is_empty())
        .and_then(|d| downloads.tracked(d))
}

/// `ImportItem.OutputPath` of a tracked download.
fn output_path(tracked: &TrackedMovieDownload) -> String {
    tracked
        .import_item
        .as_ref()
        .or(Some(&tracked.item))
        .and_then(|i| i.output_path.clone())
        .unwrap_or_default()
}

fn get_movie(db: &Db, id: i64) -> Result<Movie, ManualImportError> {
    db.call_blocking(move |conn| store::get_movie(conn, id))?
        .ok_or(ManualImportError::MovieNotFound(id))
}

/// `ParsingService.GetMovie(title)`: by the title and year the name
/// parses to, or by the name as it is when it parses to nothing.
fn movie_for_name(lookup: &dyn MovieLookup, title: &str) -> Result<Option<Movie>, mapping::MultipleMovies> {
    match parse_movie_title(title, false) {
        None => mapping::find_by_title(lookup, &[title.to_string()], None),
        Some(parsed) => {
            let year = (parsed.year > 1800).then_some(parsed.year);
            mapping::find_by_title(lookup, &parsed.movie_titles, year)
        }
    }
}

/// `ParseCustomFormat(LocalMovie)` for an item. A file nothing was read
/// from has no languages at all, which no language condition, negated or
/// not, is satisfied by.
fn item_formats(catalog: &MovieCatalog, local: &LocalMovie, read: bool) -> Vec<CustomFormat> {
    let mut formats = decide::local_formats(catalog, local);
    if !read {
        formats.retain(|f| !has_language_condition(f));
    }
    formats
}

fn has_language_condition(format: &CustomFormat) -> bool {
    format.specifications.as_array().is_some_and(|specs| {
        specs
            .iter()
            .any(|s| s["type"].as_str() == Some("LanguageSpecification"))
    })
}

/// `MapItem(decision, rootFolder, downloadId, folderName)`.
fn map_decision(
    env: &MovieEnvironment,
    decision: MovieImportDecision,
    root_folder: &str,
    download_id: Option<&str>,
    folder_name: Option<&str>,
) -> ManualImportItem {
    let local = decision
        .local_movie
        .expect("a manual import decision carries its file");
    // Where the decision maker gave up before reading anything, the
    // original's local movie has neither quality nor languages.
    let read = !decision.rejections.first().is_some_and(|r| {
        matches!(
            r.reason,
            MovieRejectionReason::UnableToParse | MovieRejectionReason::Error
        )
    });
    let mut item = ManualImportItem::bare(&local.path, root_folder, download_id);
    item.folder_name = folder_name.map(String::from);
    item.quality = read.then_some(local.quality);
    item.languages = read.then(|| local.languages.clone());
    item.release_group = local.release_group.clone();
    item.rejections = decision.rejections;
    item.indexer_flags = local.indexer_flags;
    if local.movie.is_some() {
        item.custom_formats = item_formats(&env.catalog, &local, read);
        item.movie = local.movie;
    }
    item
}

/// `MapItem(movieFile, movie, folderName)`.
fn map_file(env: &MovieEnvironment, file: &MovieFile, movie: &Movie, folder_name: &str) -> ManualImportItem {
    let path = join(&movie.path, &file.relative_path);
    let mut item = ManualImportItem::bare(&path, &movie.path, None);
    item.relative_path = file.relative_path.clone();
    item.name = None;
    item.folder_name = Some(folder_name.to_string());
    item.movie = Some(movie.clone());
    item.release_group = file.release_group.clone();
    item.quality = Some(file.quality);
    item.languages = Some(file.languages.clone());
    item.indexer_flags = file.indexer_flags;
    item.movie_file_id = Some(file.id);
    item.custom_formats = crate::movies::decision::engine::file_formats(&env.catalog, file, movie);
    item
}

/// `GetMediaFiles(movieId)`: the movie's files, then the video files in
/// its folder the library does not know.
pub fn get_media_files_for_movie(
    db: &Db,
    env: &MovieEnvironment,
    movie_id: i64,
) -> Result<Vec<ManualImportItem>, ManualImportError> {
    let movie = get_movie(db, movie_id)?;
    let files = db.call_blocking(move |conn| store::movie_files_of(conn, movie_id))?;
    let folder_name = file_name(&movie.path).to_string();
    let mut items: Vec<ManualImportItem> = files
        .iter()
        .map(|f| map_file(env, f, &movie, &folder_name))
        .collect();
    let media = filter_paths(&movie.path, &video_files(&movie.path, true), true);
    for file in decide::filter_existing(&media, &files, &movie) {
        let mut item = ManualImportItem::bare(&file, &movie.path, None);
        item.folder_name = Some(folder_name.clone());
        item.movie = Some(movie.clone());
        item.release_group = Some(String::new());
        item.languages = Some(vec![lang::UNKNOWN]);
        items.push(item);
    }
    Ok(items)
}

/// `GetMediaFiles(path, downloadId, movieId, filterExistingFiles)`.
pub fn get_media_files(
    db: &Db,
    env: &MovieEnvironment,
    downloads: &MovieDownloads,
    path: Option<&str>,
    download_id: Option<&str>,
    movie_id: Option<i64>,
    filter_existing: bool,
) -> Result<Vec<ManualImportItem>, ManualImportError> {
    let mut path = path.unwrap_or("").to_string();
    let download_id = download_id.filter(|d| !d.trim().is_empty());
    if let Some(id) = download_id {
        let Some(tracked) = downloads.tracked(id) else {
            return Ok(Vec::new());
        };
        path = output_path(&tracked);
    }
    if !Path::new(&path).is_dir() {
        if !Path::new(&path).is_file() {
            return Ok(Vec::new());
        }
        let root = parent_path(&path).unwrap_or("").to_string();
        return Ok(vec![process_file(
            db,
            env,
            downloads,
            &root,
            &root,
            &path,
            download_id,
            None,
        )]);
    }
    process_folder(
        db,
        env,
        downloads,
        &path,
        &path,
        download_id,
        movie_id,
        filter_existing,
    )
}

/// `ProcessFolder(rootFolder, baseFolder, downloadId, movieId, filterExistingFiles)`.
#[allow(clippy::too_many_arguments)]
fn process_folder(
    db: &Db,
    env: &MovieEnvironment,
    downloads: &MovieDownloads,
    root_folder: &str,
    base_folder: &str,
    download_id: Option<&str>,
    movie_id: Option<i64>,
    filter_existing: bool,
) -> Result<Vec<ManualImportItem>, ManualImportError> {
    let folder_name = file_name(base_folder).to_string();
    let mut source = if movie_id.is_some() {
        MovieSource::Id
    } else {
        MovieSource::Title
    };
    let mut movie: Option<Movie> = match movie_id {
        Some(id) => Some(get_movie(db, id)?),
        None => {
            let name = folder_name.clone();
            db.call_blocking(move |conn| {
                let lookup = StoreMovieLookup { conn };
                Ok(match movie_for_name(&lookup, &name) {
                    Ok(movie) => movie,
                    Err(e) => {
                        tracing::warn!("Unable to match movie by title: {e}");
                        None
                    }
                })
            })?
        }
    };
    let tracked = tracked_for(downloads, download_id);
    let item = tracked.as_ref().map(|t| t.item.clone());
    if movie.is_none() {
        if let Some(t) = &tracked {
            movie = t.remote.as_ref().and_then(|r| r.movie.clone());
            source = MovieSource::Download { by_id: t.movie_by_id };
        }
    }
    let Some(movie) = movie else {
        // Filtered against the root folder, so that what a subfolder to
        // ignore holds is ignored too.
        let files = filter_paths(root_folder, &video_files(base_folder, false), true);
        if files.len() > 100 {
            tracing::warn!(
                "Unable to determine movie from folder name and found more than 100 files. Skipping parsing"
            );
            return Ok(process_download_directory(env, root_folder, &files));
        }
        let subfolders: Vec<String> = directories_under(Path::new(base_folder))
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let mut subfolders = filter_paths(root_folder, &subfolders, true);
        subfolders.sort();
        let mut out: Vec<ManualImportItem> = files
            .iter()
            .map(|f| process_file(db, env, downloads, root_folder, base_folder, f, download_id, None))
            .collect();
        for sub in subfolders {
            out.extend(process_folder(
                db,
                env,
                downloads,
                root_folder,
                &sub,
                download_id,
                None,
                filter_existing,
            )?);
        }
        return Ok(out);
    };
    let folder_info = parse_movie_title(&folder_name, false);
    let files = filter_paths(root_folder, &video_files(base_folder, true), true);
    let probes = Probes::probe_all_for(&files, crate::Personality::Radarr);
    let scene = scene_source(&movie, base_folder);
    let tracked_flags = tracked
        .as_ref()
        .and_then(|t| t.remote.as_ref())
        .map(|r| r.release.indexer_flags);
    let (env2, movie2) = (env.clone(), movie.clone());
    let decisions = db.call_blocking(move |conn| {
        decide::get_import_decisions(
            conn,
            &env2,
            &probes,
            &files,
            &movie2,
            item.as_ref(),
            folder_info.as_ref(),
            scene,
            filter_existing,
            tracked_flags,
        )
    })?;
    Ok(decisions
        .into_iter()
        .map(|d| {
            let mut mapped = map_decision(env, d, root_folder, download_id, Some(&folder_name));
            mapped.movie_source = source;
            mapped
        })
        .collect())
}

/// `ProcessFile(rootFolder, baseFolder, file, downloadId, movie)`. Whatever
/// goes wrong leaves the bare item, as the original catches everything.
#[allow(clippy::too_many_arguments)]
fn process_file(
    db: &Db,
    env: &MovieEnvironment,
    downloads: &MovieDownloads,
    root_folder: &str,
    base_folder: &str,
    file: &str,
    download_id: Option<&str>,
    movie: Option<Movie>,
) -> ManualImportItem {
    match try_process_file(
        db,
        env,
        downloads,
        root_folder,
        base_folder,
        file,
        download_id,
        movie,
    ) {
        Ok(Some(item)) => item,
        Ok(None) => ManualImportItem::bare(file, root_folder, download_id),
        Err(e) => {
            tracing::warn!("Failed to process file: {file}: {e:#}");
            ManualImportItem::bare(file, root_folder, download_id)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn try_process_file(
    db: &Db,
    env: &MovieEnvironment,
    downloads: &MovieDownloads,
    root_folder: &str,
    base_folder: &str,
    file: &str,
    download_id: Option<&str>,
    movie: Option<Movie>,
) -> anyhow::Result<Option<ManualImportItem>> {
    let tracked = tracked_for(downloads, download_id);
    let relative = relative_path(base_folder, file).to_string();
    let mut source = MovieSource::Id;
    let mut movie = movie;
    if movie.is_none() {
        source = MovieSource::Title;
        // By the first folder of the relative path, then by all of it, then
        // by what the path parses to. Several movies under one title end
        // the attempt.
        let rel = relative.clone();
        movie = db
            .call_blocking(move |conn| {
                let lookup = StoreMovieLookup { conn };
                let first = rel.split(['\\', '/']).next().unwrap_or("");
                Ok(match movie_for_name(&lookup, first) {
                    Ok(Some(movie)) => Ok(Some(movie)),
                    Ok(None) => movie_for_name(&lookup, &rel),
                    Err(e) => Err(e),
                })
            })?
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    if movie.is_none() {
        if let Some(t) = &tracked {
            movie = t.remote.as_ref().and_then(|r| r.movie.clone());
            if movie.is_some() {
                source = MovieSource::Download { by_id: t.movie_by_id };
            }
        }
    }
    if movie.is_none() {
        if let Some(parsed) = parse_movie_path(&relative) {
            movie = db
                .call_blocking(move |conn| {
                    let lookup = StoreMovieLookup { conn };
                    let title = parsed.primary_movie_title().to_string();
                    Ok(mapping::find_by_title(&lookup, &[title], Some(parsed.year)))
                })?
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }
    let Some(movie) = movie else {
        let local = LocalMovie {
            path: file.to_string(),
            release_group: parse_release_group(file),
            quality: parse_quality(file).quality,
            languages: parse_languages(file),
            size: file_size(file),
            ..Default::default()
        };
        let decision = MovieImportDecision::rejected(
            Some(local),
            MovieImportRejection::new(MovieRejectionReason::UnknownMovie, "Unknown Movie"),
        );
        return Ok(Some(map_decision(env, decision, root_folder, download_id, None)));
    };
    let probes = Probes::probe_all_for(
        std::slice::from_ref(&file.to_string()),
        crate::Personality::Radarr,
    );
    let scene = scene_source(&movie, base_folder);
    let item = tracked.as_ref().map(|t| t.item.clone());
    let tracked_flags = tracked
        .as_ref()
        .and_then(|t| t.remote.as_ref())
        .map(|r| r.release.indexer_flags);
    let (env2, file2) = (env.clone(), file.to_string());
    let decisions = db.call_blocking(move |conn| {
        decide::get_import_decisions(
            conn,
            &env2,
            &probes,
            &[file2],
            &movie,
            item.as_ref(),
            None,
            scene,
            false,
            tracked_flags,
        )
    })?;
    Ok(decisions.into_iter().next().map(|d| {
        let mut mapped = map_decision(env, d, root_folder, download_id, None);
        mapped.movie_source = source;
        mapped
    }))
}

/// `ProcessDownloadDirectory`: too many files to parse; each as it is.
fn process_download_directory(
    env: &MovieEnvironment,
    root_folder: &str,
    files: &[String],
) -> Vec<ManualImportItem> {
    files
        .iter()
        .map(|f| {
            let local = LocalMovie {
                path: f.clone(),
                quality: QualityModel::default(),
                languages: vec![lang::UNKNOWN],
                release_group: parse_release_group(f),
                size: file_size(f),
                ..Default::default()
            };
            map_decision(
                env,
                MovieImportDecision::new(local, Vec::new()),
                root_folder,
                None,
                None,
            )
        })
        .collect()
}

/// At most one language, and that one `Unknown`.
fn unknown_only(languages: &[i64]) -> bool {
    languages.len() <= 1 && languages.first().copied().unwrap_or(lang::UNKNOWN) == lang::UNKNOWN
}

/// `ReprocessItem(path, downloadId, movieId, releaseGroup, quality, languages, indexerFlags)`:
/// the item evaluated again with the user's choices. A file whose name,
/// folder and download say nothing fails the augmenting, which the
/// original lets escape.
pub fn reprocess_item(
    db: &Db,
    env: &MovieEnvironment,
    downloads: &MovieDownloads,
    req: &ReprocessRequest,
) -> Result<ManualImportItem, ManualImportError> {
    let root_folder = parent_path(&req.path).unwrap_or("").to_string();
    let movie = get_movie(db, req.movie_id)?;
    let mut language_parse = parse_languages(&req.path);
    if unknown_only(&language_parse) {
        language_parse = vec![movie.metadata.original_language_id];
        tracing::debug!(
            "Language couldn't be parsed from release, fallback to movie original language: {}",
            MovieLanguage::name_of(movie.metadata.original_language_id)
        );
    }
    let item = tracked_for(downloads, req.download_id.as_deref()).map(|t| t.item);
    let release_group = if blank(req.release_group.as_deref()) {
        parse_release_group(&req.path)
    } else {
        req.release_group.clone()
    };
    let quality = match req.quality {
        Some(q) if q.quality_id != 0 => q,
        _ => parse_quality(&req.path).quality,
    };
    // A request without languages keeps none, as the original's null
    // conditional leaves the list null.
    let languages = match &req.languages {
        Some(given) if unknown_only(given) => language_parse,
        Some(given) => given.clone(),
        None => Vec::new(),
    };
    let existing_file = is_parent_path(&movie.path, &req.path);
    let mut local = LocalMovie {
        movie: Some(movie.clone()),
        file_movie_info: parse_movie_path(&req.path),
        download_client_movie_info: item.as_ref().and_then(|i| parse_movie_title(&i.title, false)),
        download_item: item.clone(),
        path: req.path.clone(),
        scene_source: scene_source(&movie, &root_folder),
        existing_file,
        size: file_size(&req.path),
        release_group: release_group.clone(),
        languages: languages.clone(),
        quality,
        indexer_flags: req.indexer_flags,
        ..Default::default()
    };
    decide::score_formats(&env.catalog, &mut local);
    let media_info = (!existing_file || env.config.enable_media_info)
        .then(|| {
            Probes::probe_all_for(std::slice::from_ref(&req.path), crate::Personality::Radarr).get(&req.path)
        })
        .flatten();
    let env2 = env.clone();
    let indexer_flags = req.indexer_flags;
    let decision = db.call_blocking(move |conn| {
        if let Err(e) = aggregate::augment(conn, &mut local, item.as_ref(), media_info) {
            return Ok(Err(e.0));
        }
        // The user's choices stand.
        local.movie = Some(movie);
        local.release_group = release_group;
        local.quality = quality;
        local.languages = languages;
        local.indexer_flags = indexer_flags;
        let lookup = StoreMovieLookup { conn };
        let sources = StoreImportSources { conn };
        Ok(Ok(decide::specs_only(
            &lookup,
            &sources,
            &env2,
            local,
            item.as_ref(),
        )))
    })?;
    let decision = decision.map_err(ManualImportError::Augmenting)?;
    let mut item = map_decision(env, decision, &root_folder, req.download_id.as_deref(), None);
    if req.languages.is_none() {
        // The original's list stays null, with what that means for the
        // language conditions.
        item.languages = None;
        item.custom_formats.retain(|f| !has_language_condition(f));
    }
    Ok(item)
}

/// What `Execute(ManualImportCommand)` leaves for the runtime: the events,
/// and the tracked downloads it marked imported.
#[derive(Debug, Default)]
pub struct ManualImportOutcome {
    pub events: MovieEvents,
    pub completed: Vec<TrackedMovieDownload>,
}

/// `Execute(ManualImportCommand)`.
pub fn execute(
    db: &Db,
    env: &MovieEnvironment,
    downloads: &MovieDownloads,
    files: &[ManualImportFile],
    mode: ImportMode,
    now: DateTime<Utc>,
    progress: &mut dyn FnMut(String),
) -> Result<ManualImportOutcome, ManualImportError> {
    // The mode as the original's enum prints it.
    let mode_name = match mode {
        ImportMode::Auto => "Auto",
        ImportMode::Move => "Move",
        ImportMode::Copy => "Copy",
    };
    progress(format!(
        "Manually importing {} files using mode {mode_name}",
        files.len()
    ));
    let mut outcome = ManualImportOutcome::default();
    let mut imported = 0usize;
    // `ManuallyImportedFile`: the result of each file of a tracked download.
    let mut tracked_results: Vec<(TrackedMovieDownload, MovieImportResult)> = Vec::new();
    for (i, file) in files.iter().enumerate() {
        progress(format!("Processing file {} of {}", i + 1, files.len()));
        let movie = get_movie(db, file.movie_id)?;
        let existing_file = is_parent_path(&movie.path, &file.path);
        let tracked = tracked_for(downloads, file.download_id.as_deref());
        // A name that parses to nothing still counts as parsed here.
        let file_info = parse_movie_path(&file.path);
        let unparsed = file_info.is_none();
        let mut local = LocalMovie {
            existing_file,
            file_movie_info: Some(file_info.unwrap_or_else(empty_parsed_info)),
            path: file.path.clone(),
            release_group: file.release_group.clone(),
            quality: file.quality,
            languages: file.languages.clone(),
            indexer_flags: file.indexer_flags,
            movie: Some(movie.clone()),
            size: 0,
            ..Default::default()
        };
        if let Some(t) = &tracked {
            local.download_client_movie_info = t.remote.as_ref().and_then(|r| r.parsed.clone());
            local.download_item = Some(t.item.clone());
        }
        if !blank(file.folder_name.as_deref()) {
            local.folder_movie_info = parse_movie_title(file.folder_name.as_deref().unwrap_or(""), false);
            local.scene_source = !existing_file;
        }
        let media_info = (!existing_file || env.config.enable_media_info)
            .then(|| {
                Probes::probe_all_for(std::slice::from_ref(&file.path), crate::Personality::Radarr)
                    .get(&file.path)
            })
            .flatten();
        let item = tracked.as_ref().map(|t| t.item.clone());
        let (file2, catalog, item2) = (file.clone(), env.catalog.clone(), item.clone());
        let local = db.call_blocking(move |conn| {
            if let Err(e) = aggregate::augment(conn, &mut local, item2.as_ref(), media_info) {
                return Ok(Err(e.0));
            }
            // The empty info that stands in for an unparsed name has no
            // edition, where one parsed has an empty one.
            if unparsed && blank(local.edition.as_deref()) {
                local.edition = None;
            }
            // The user's choices stand.
            local.movie = Some(movie);
            local.release_group = file2.release_group;
            local.quality = file2.quality;
            local.indexer_flags = file2.indexer_flags;
            if !file2.languages.is_empty()
                && file2.languages.iter().all(|l| MovieLanguage::by_id(*l).is_some())
            {
                local.languages = file2.languages;
            }
            decide::score_formats(&catalog, &mut local);
            Ok(Ok(local))
        })?;
        let local = local.map_err(ManualImportError::Augmenting)?;
        let decision = MovieImportDecision::new(local, Vec::new());
        match tracked {
            None => {
                let result = approve::import(db, env, vec![decision], !existing_file, None, mode, now);
                outcome.events.extend(result.events);
                imported += result.results.len();
            }
            Some(t) => {
                let result = approve::import(db, env, vec![decision], true, item.as_ref(), mode, now);
                outcome.events.extend(result.events);
                if let Some(r) = result.results.into_iter().next() {
                    imported += 1;
                    tracked_results.push((t, r));
                }
            }
        }
    }
    progress(format!("Manually imported {imported} files"));
    // `importedTrackedDownload.GroupBy(DownloadId)`: the folder of a
    // download goes when only samples remain in it, and a download a file
    // was imported from is done.
    let mut seen: Vec<String> = Vec::new();
    for (t, first) in &tracked_results {
        let id = t.item.download_id.clone();
        if seen.contains(&id) {
            continue;
        }
        seen.push(id.clone());
        let mut tracked = t.clone();
        let Some(import_movie) = first.decision.local_movie.as_ref().and_then(|l| l.movie.clone()) else {
            continue;
        };
        let output = output_path(&tracked);
        if Path::new(&output).is_dir()
            && downloaded::should_delete_folder(&output, &import_movie)
            && tracked.item.can_move_files
        {
            if let Err(e) = std::fs::remove_dir_all(&output) {
                tracing::debug!(error = %e, "unable to remove the download folder {output}");
            }
        }
        let any_imported = tracked_results
            .iter()
            .filter(|(x, _)| x.item.download_id == id)
            .any(|(_, r)| r.result() == ImportResultType::Imported);
        if any_imported {
            tracked.state = TrackedDownloadState::Imported;
            let grabbed = completed::grabbed_history(db, tracked.download_id());
            let ev = MovieDownloadCompleted {
                tracked: tracked.clone(),
                movie_id: import_movie.id,
                release: (!grabbed.is_empty()).then(|| aggregate::grabbed_release(&grabbed)),
            };
            let ev2 = ev.clone();
            if let Err(e) = db.call_blocking(move |conn| events::record_download_completed(conn, &ev2, now)) {
                tracing::error!(error = %e, "failed to record the completed download");
            }
            outcome
                .events
                .push(MovieImportEvent::DownloadCompleted(Box::new(ev)));
            outcome.completed.push(tracked);
        }
    }
    Ok(outcome)
}

/// `new ParsedMovieInfo()`.
fn empty_parsed_info() -> ParsedMovieInfo {
    ParsedMovieInfo {
        movie_titles: Vec::new(),
        original_title: String::new(),
        release_title: String::new(),
        simple_release_title: String::new(),
        quality: Default::default(),
        languages: Vec::new(),
        release_group: None,
        release_hash: String::new(),
        edition: String::new(),
        year: 0,
        imdb_id: String::new(),
        tmdb_id: 0,
        hardcoded_subs: None,
    }
}

#[cfg(test)]
mod tests {
    //! The original has no fixture for its `ManualImportService`. What the
    //! recorded answers do not show is checked here over a library and a
    //! download folder in a temporary directory: folders and files no
    //! download is tracked for, the choices of a re-evaluation, and the
    //! command's import of files that are no download's.

    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::downloadclients::DownloadClientService;
    use crate::model::RootFolder;
    use crate::movies::decision::testing;
    use crate::movies::model::MovieNamingConfig;
    use crate::movies::parser::quality::{BLURAY1080P, DVD, WEBDL1080P};
    use crate::Personality;

    struct Library {
        root: PathBuf,
        db: Db,
        env: MovieEnvironment,
        downloads: MovieDownloads,
        movie: Movie,
    }

    impl Drop for Library {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn library(test: &str) -> Library {
        let root = std::env::temp_dir().join(format!("unarr-movie-manual-{}-{test}", std::process::id()));
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
        env.config.skip_free_space_check_when_importing = true;
        env.naming = MovieNamingConfig {
            rename_movies: true,
            replace_illegal_characters: true,
            colon_replacement_format: 4,
            standard_movie_format: "{Movie Title} ({Release Year}) [{Quality Full}]".into(),
            movie_folder_format: "{Movie Title} ({Release Year})".into(),
        };
        let downloads = MovieDownloads::new(db.clone(), DownloadClientService::new(db.clone()));
        Library {
            root,
            db,
            env,
            downloads,
            movie,
        }
    }

    impl Library {
        /// A file under the download folder.
        fn file(&self, relative: &str) -> String {
            let path = self.root.join("downloads").join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"video").unwrap();
            path.to_string_lossy().into_owned()
        }

        fn folder(&self, name: &str) -> String {
            self.root
                .join("downloads")
                .join(name)
                .to_string_lossy()
                .into_owned()
        }

        fn media_files(&self, path: &str, movie_id: Option<i64>) -> Vec<ManualImportItem> {
            get_media_files(
                &self.db,
                &self.env,
                &self.downloads,
                Some(path),
                None,
                movie_id,
                true,
            )
            .unwrap()
        }

        fn files(&self) -> Vec<MovieFile> {
            let id = self.movie.id;
            self.db
                .call_blocking(move |conn| store::movie_files_of(conn, id))
                .unwrap()
        }

        fn execute(&self, files: &[ManualImportFile]) -> (Vec<String>, ManualImportOutcome) {
            let mut messages = Vec::new();
            let outcome = execute(
                &self.db,
                &self.env,
                &self.downloads,
                files,
                ImportMode::Auto,
                Utc::now(),
                &mut |m| messages.push(m),
            )
            .unwrap();
            (messages, outcome)
        }
    }

    fn choice(path: &str, quality_id: i64) -> ManualImportFile {
        ManualImportFile {
            path: path.to_string(),
            folder_name: None,
            movie_id: 1,
            quality: testing::quality(quality_id, 1),
            languages: vec![lang::ENGLISH],
            release_group: Some("GRP".into()),
            indexer_flags: 0,
            download_id: None,
        }
    }

    const NAME: &str = "The.Movie.2018.1080p.WEB-DL.DD5.1.H264-GRP";

    #[test]
    fn nothing_for_a_path_that_is_not_there_or_a_download_not_tracked() {
        let lib = library("nothing");
        assert!(lib.media_files(&lib.folder("nothing-here"), None).is_empty());
        let items = get_media_files(
            &lib.db,
            &lib.env,
            &lib.downloads,
            Some(&lib.folder("")),
            Some("NOTTRACKED"),
            None,
            true,
        )
        .unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn a_folder_of_no_known_movie_lists_its_files_and_its_subfolders_as_unknown() {
        let lib = library("unknown");
        let top = lib.file("unsorted/Another.Film.2019.720p.HDTV.x264-SECOND.mkv");
        let nested = lib.file("unsorted/more/Yet.Another.Film.2020.DVDRip.XviD-THIRD.avi");
        // Hidden folders are not looked into.
        lib.file("unsorted/.hidden/Hidden.Film.2020.DVDRip.XviD-THIRD.avi");
        let items = lib.media_files(&lib.folder("unsorted"), None);
        assert_eq!(
            items.iter().map(|i| i.path.clone()).collect::<Vec<_>>(),
            vec![top, nested]
        );
        let first = &items[0];
        assert_eq!(first.relative_path, "Another.Film.2019.720p.HDTV.x264-SECOND.mkv");
        assert_eq!(
            first.name.as_deref(),
            Some("Another.Film.2019.720p.HDTV.x264-SECOND")
        );
        assert_eq!(first.folder_name, None);
        assert_eq!(first.size, 5);
        assert!(first.movie.is_none());
        assert_eq!(first.release_group.as_deref(), Some("SECOND"));
        assert_eq!(first.languages, Some(vec![lang::UNKNOWN]));
        assert!(first.quality.is_some());
        assert!(first.custom_formats.is_empty());
        assert_eq!(first.download_id, None);
        assert_eq!(
            first
                .rejections
                .iter()
                .map(|r| (r.reason, r.message.as_str()))
                .collect::<Vec<_>>(),
            vec![(MovieRejectionReason::UnknownMovie, "Unknown Movie")]
        );
        // Relative to the folder that was asked for, not to its own.
        assert_eq!(
            items[1].relative_path,
            "more/Yet.Another.Film.2020.DVDRip.XviD-THIRD.avi"
        );
        assert_eq!(items[1].quality.map(|q| q.quality_id), Some(DVD));
    }

    #[test]
    fn a_folder_named_after_a_movie_is_decided_for_that_movie() {
        let lib = library("named");
        let file = lib.file(&format!("{NAME}/abc.mkv"));
        let items = lib.media_files(&lib.folder(NAME), None);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.path, file);
        assert_eq!(item.folder_name.as_deref(), Some(NAME));
        assert_eq!(item.relative_path, "abc.mkv");
        assert_eq!(item.movie.as_ref().map(|m| m.id), Some(1));
        assert_eq!(item.movie_source, MovieSource::Title);
        // The folder names what the file does not.
        assert_eq!(item.quality.map(|q| q.quality_id), Some(WEBDL1080P));
        assert_eq!(item.release_group.as_deref(), Some("GRP"));
        assert_eq!(item.languages, Some(vec![lang::ENGLISH]));
        // Nothing could measure the five bytes.
        assert_eq!(
            item.rejections.iter().map(|r| r.reason).collect::<Vec<_>>(),
            vec![MovieRejectionReason::SampleIndeterminate]
        );
    }

    #[test]
    fn the_movie_asked_for_wins_over_what_the_folder_says() {
        let lib = library("asked");
        lib.file("unsorted/Another.Film.2019.720p.HDTV.x264-SECOND.mkv");
        let items = lib.media_files(&lib.folder("unsorted"), Some(1));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].movie.as_ref().map(|m| m.id), Some(1));
        assert_eq!(items[0].movie_source, MovieSource::Id);
        assert_eq!(items[0].folder_name.as_deref(), Some("unsorted"));
        // A movie that is not there is the original's `ModelNotFoundException`.
        let missing = get_media_files(
            &lib.db,
            &lib.env,
            &lib.downloads,
            Some(&lib.folder("unsorted")),
            None,
            Some(9),
            true,
        );
        assert!(matches!(missing, Err(ManualImportError::MovieNotFound(9))));
    }

    #[test]
    fn a_single_file_is_processed_against_its_own_folder() {
        let lib = library("single");
        let file = lib.file(&format!("drop/{NAME}.mkv"));
        let items = lib.media_files(&file, None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].relative_path, format!("{NAME}.mkv"));
        assert_eq!(items[0].folder_name, None);
        assert_eq!(items[0].movie.as_ref().map(|m| m.id), Some(1));
        assert_eq!(items[0].movie_source, MovieSource::Title);
        // A name that says nothing, in a folder that says nothing.
        let nameless = lib.file("drop2/abc.mkv");
        let items = lib.media_files(&nameless, None);
        assert_eq!(items.len(), 1);
        assert!(items[0].movie.is_none());
        assert_eq!(items[0].rejections[0].message, "Unknown Movie");
    }

    #[test]
    fn more_than_a_hundred_files_of_no_known_movie_are_listed_unparsed() {
        let lib = library("hundred");
        for n in 0..101 {
            lib.file(&format!(
                "bulk/Another.Film.{}.720p.HDTV.x264-SECOND.mkv",
                1900 + n
            ));
        }
        let items = lib.media_files(&lib.folder("bulk"), None);
        assert_eq!(items.len(), 101);
        for item in &items {
            assert!(item.movie.is_none());
            assert!(item.rejections.is_empty());
            assert_eq!(item.quality, Some(QualityModel::default()));
            assert_eq!(item.languages, Some(vec![lang::UNKNOWN]));
            assert_eq!(item.release_group.as_deref(), Some("SECOND"));
            assert_eq!(item.download_id, None);
        }
    }

    #[test]
    fn the_files_of_a_movie_are_its_own_and_the_ones_its_folder_holds_unmapped() {
        let lib = library("movie");
        let folder = PathBuf::from(&lib.movie.path);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("The Movie (2018) [Bluray-1080p].mkv"), b"library").unwrap();
        std::fs::write(folder.join("stray.mkv"), b"stray!").unwrap();
        let movie_id = lib.movie.id;
        lib.db
            .call_blocking(move |conn| {
                let mut file = MovieFile {
                    id: 0,
                    movie_id,
                    relative_path: "The Movie (2018) [Bluray-1080p].mkv".into(),
                    original_file_path: None,
                    size: 7,
                    date_added: Utc::now(),
                    scene_name: None,
                    release_group: Some("GRP".into()),
                    edition: None,
                    quality: testing::quality(BLURAY1080P, 1),
                    languages: vec![lang::ENGLISH],
                    media_info: None,
                    indexer_flags: 4,
                };
                store::add_movie_file(conn, &mut file)?;
                store::set_movie_file_id(conn, movie_id, file.id)
            })
            .unwrap();
        let items = get_media_files_for_movie(&lib.db, &lib.env, 1).unwrap();
        assert_eq!(items.len(), 2);
        let (held, stray) = (&items[0], &items[1]);
        assert_eq!(held.movie_file_id, lib.files().first().map(|f| f.id));
        assert_eq!(held.name, None, "the original reads a path it never fills");
        assert_eq!(held.relative_path, "The Movie (2018) [Bluray-1080p].mkv");
        assert_eq!(held.folder_name.as_deref(), Some("The Movie (2018)"));
        assert_eq!(held.size, 7);
        assert_eq!(held.quality.map(|q| q.quality_id), Some(BLURAY1080P));
        assert_eq!(held.release_group.as_deref(), Some("GRP"));
        assert_eq!(held.indexer_flags, 4);
        assert!(held.rejections.is_empty());
        assert_eq!(stray.movie_file_id, None);
        assert_eq!(stray.name.as_deref(), Some("stray"));
        assert_eq!(stray.relative_path, "stray.mkv");
        assert_eq!(stray.size, 6);
        assert_eq!(stray.quality, Some(QualityModel::default()));
        assert_eq!(stray.languages, Some(vec![lang::UNKNOWN]));
        assert_eq!(stray.release_group.as_deref(), Some(""));
        assert_eq!(stray.movie.as_ref().map(|m| m.id), Some(1));
        assert!(matches!(
            get_media_files_for_movie(&lib.db, &lib.env, 9),
            Err(ManualImportError::MovieNotFound(9))
        ));
    }

    fn request(path: &str) -> ReprocessRequest {
        ReprocessRequest {
            path: path.to_string(),
            download_id: None,
            movie_id: 1,
            release_group: None,
            quality: None,
            languages: None,
            indexer_flags: 0,
        }
    }

    #[test]
    fn reprocessing_fails_for_a_movie_that_is_not_there_and_a_file_nothing_parses() {
        let lib = library("reprocess-fails");
        let file = lib.file("c8949b89/c8949b89.mkv");
        let mut req = request(&file);
        req.movie_id = 0;
        let error = reprocess_item(&lib.db, &lib.env, &lib.downloads, &req).unwrap_err();
        assert_eq!(error.to_string(), "Movie with ID 0 does not exist");
        // Neither the name, nor a folder, nor a download says anything: the
        // augmenting gives up, and the original lets that escape.
        let error = reprocess_item(&lib.db, &lib.env, &lib.downloads, &request(&file)).unwrap_err();
        assert!(matches!(error, ManualImportError::Augmenting(_)));
        assert_eq!(
            error.to_string(),
            format!("Unable to parse movie info from path: {file}")
        );
    }

    #[test]
    fn reprocessing_keeps_what_the_user_chose_and_parses_the_rest() {
        let lib = library("reprocess");
        let file = lib.file(&format!("drop/{NAME}.mkv"));
        // Nothing chosen: the name speaks, but for the languages, which a
        // request without any leaves out.
        let item = reprocess_item(&lib.db, &lib.env, &lib.downloads, &request(&file)).unwrap();
        assert_eq!(item.languages, None);
        assert_eq!(item.movie.as_ref().map(|m| m.id), Some(1));
        assert_eq!(item.movie_source, MovieSource::Id);
        assert_eq!(item.quality.map(|q| q.quality_id), Some(WEBDL1080P));
        assert_eq!(item.release_group.as_deref(), Some("GRP"));
        assert_eq!(item.relative_path, format!("{NAME}.mkv"));
        assert_eq!(item.folder_name, None);
        // An unknown language asks for the name's, which falls back to the
        // movie's original one.
        let mut req = request(&file);
        req.languages = Some(vec![lang::UNKNOWN]);
        let item = reprocess_item(&lib.db, &lib.env, &lib.downloads, &req).unwrap();
        assert_eq!(item.languages, Some(vec![lang::ENGLISH]));
        // The choices stand, whatever the name says.
        let mut req = request(&file);
        req.quality = Some(testing::quality(BLURAY1080P, 2));
        req.languages = Some(vec![lang::ITALIAN, lang::ENGLISH]);
        req.release_group = Some("MINE".into());
        req.indexer_flags = 8;
        let item = reprocess_item(&lib.db, &lib.env, &lib.downloads, &req).unwrap();
        assert_eq!(item.quality, Some(testing::quality(BLURAY1080P, 2)));
        assert_eq!(item.languages, Some(vec![lang::ITALIAN, lang::ENGLISH]));
        assert_eq!(item.release_group.as_deref(), Some("MINE"));
        assert_eq!(item.indexer_flags, 8);
        // A blank group is none chosen.
        let mut req = request(&file);
        req.release_group = Some("  ".into());
        let item = reprocess_item(&lib.db, &lib.env, &lib.downloads, &req).unwrap();
        assert_eq!(item.release_group.as_deref(), Some("GRP"));
    }

    #[test]
    fn the_command_imports_the_files_chosen_with_the_choices_made() {
        let lib = library("execute");
        let file = lib.file(&format!("drop/{NAME}.mkv"));
        let mut chosen = choice(&file, BLURAY1080P);
        chosen.languages = vec![lang::ITALIAN];
        chosen.release_group = Some("MINE".into());
        chosen.indexer_flags = 8;
        let (messages, outcome) = lib.execute(&[chosen]);
        assert_eq!(
            messages,
            vec![
                "Manually importing 1 files using mode Auto".to_string(),
                "Processing file 1 of 1".to_string(),
                "Manually imported 1 files".to_string(),
            ]
        );
        assert!(outcome.completed.is_empty(), "no download behind the file");
        let imported: Vec<_> = outcome
            .events
            .0
            .iter()
            .filter_map(|e| match e {
                MovieImportEvent::Imported(i) => Some(i),
                _ => None,
            })
            .collect();
        assert_eq!(imported.len(), 1);
        assert!(imported[0].new_download, "a file from outside the movie's folder");
        let files = lib.files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].quality.quality_id, BLURAY1080P);
        assert_eq!(files[0].languages, vec![lang::ITALIAN]);
        assert_eq!(files[0].release_group.as_deref(), Some("MINE"));
        assert_eq!(files[0].indexer_flags, 8);
        assert_eq!(files[0].relative_path, "The Movie (2018) [Bluray-1080p].mkv");
        // No folder name was given: not a scene source, no scene name.
        assert_eq!(files[0].scene_name, None);
        assert!(!std::path::Path::new(&file).exists(), "moved, no client seeds it");
    }

    #[test]
    fn the_command_imports_a_file_whose_name_says_nothing() {
        let lib = library("execute-nameless");
        let file = lib.file("c8949b89/c8949b89.mkv");
        let mut chosen = choice(&file, WEBDL1080P);
        chosen.folder_name = Some("c8949b89".into());
        // Languages the application does not know leave the augmented ones.
        chosen.languages = vec![lang::ENGLISH, 9999];
        let (messages, _) = lib.execute(&[chosen]);
        assert_eq!(
            messages.last().map(String::as_str),
            Some("Manually imported 1 files")
        );
        let files = lib.files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].quality.quality_id, WEBDL1080P);
        assert_eq!(files[0].edition, None, "the empty parsed info has no edition");
        assert_eq!(
            files[0].languages,
            vec![lang::ENGLISH],
            "the movie's original language"
        );
        assert_eq!(
            files[0].original_file_path.as_deref(),
            Some("c8949b89/c8949b89.mkv")
        );
    }

    #[test]
    fn the_command_fails_for_a_movie_that_is_not_there() {
        let lib = library("execute-missing");
        let file = lib.file(&format!("drop/{NAME}.mkv"));
        let mut chosen = choice(&file, WEBDL1080P);
        chosen.movie_id = 9;
        let mut messages = Vec::new();
        let error = execute(
            &lib.db,
            &lib.env,
            &lib.downloads,
            &[chosen],
            ImportMode::Copy,
            Utc::now(),
            &mut |m| messages.push(m),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "Movie with ID 9 does not exist");
        assert_eq!(messages[0], "Manually importing 1 files using mode Copy");
        assert!(lib.files().is_empty());
    }

    #[test]
    fn a_file_nothing_was_read_from_matches_no_language_condition() {
        let mut catalog = MovieCatalog::default();
        let format = |id: i64, name: &str, spec: serde_json::Value| CustomFormat {
            id,
            name: name.into(),
            include_when_renaming: false,
            specifications: json!([spec]),
        };
        catalog.base.formats = vec![
            format(
                1,
                "Language not Italian",
                json!({"type": "LanguageSpecification", "body": {"value": lang::ITALIAN, "negate": true, "required": false}}),
            ),
            format(
                2,
                "Group",
                json!({"type": "ReleaseTitleSpecification", "body": {"value": "GRP", "negate": false, "required": false}}),
            ),
        ].into();
        let local = LocalMovie {
            path: format!("/downloads/{NAME}.mkv"),
            movie: Some(testing::movie(1, "The Movie", 2018)),
            ..Default::default()
        };
        let names = |read: bool| -> Vec<String> {
            item_formats(&catalog, &local, read)
                .into_iter()
                .map(|f| f.name)
                .collect()
        };
        // No languages at all satisfy a negated condition; none read do not.
        assert_eq!(names(true), vec!["Group", "Language not Italian"]);
        assert_eq!(names(false), vec!["Group"]);
    }

    #[test]
    fn the_id_of_an_item_hashes_its_path() {
        // `HashConverter.GetHashInt31(model.Path)`, as the recording shows it.
        assert_eq!(
            crate::downloads::queue::hash_int31(
                "/downloads/movies/c8949b89db6f20a1e049aa8170a7a05c/c8949b89db6f20a1e049aa8170a7a05c.mkv"
            ),
            1955259751
        );
        assert_eq!(
            crate::downloads::queue::hash_int31(
                "/movies/Backcountry (2015)/Backcountry (2015) {tmdb-228973} - [Bluray-1080p][AAC 1.0][x264]-GRP.mkv"
            ),
            1037969214
        );
    }
}
