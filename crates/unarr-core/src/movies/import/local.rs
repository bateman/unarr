//! `LocalMovie`, `ImportDecision`, `ImportRejection`, `ImportResult`: the
//! shapes Radarr's import pipeline passes along.

use crate::downloadclients::DownloadClientItem;
use crate::mediafiles::local::ImportResultType;
use crate::mediafiles::mediainfo::MediaInfo;
use crate::model::{CustomFormat, QualityModel};
use crate::movies::model::{Movie, MovieFile};
use crate::movies::parser::ParsedMovieInfo;
use crate::parser::subtitles::SubtitleTitleInfo;

/// Radarr's `GrabbedReleaseInfo`: what the grab history says about a
/// download.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GrabbedMovieRelease {
    pub title: String,
    pub indexer: Option<String>,
    pub size: i64,
    pub indexer_flags: i64,
    pub movie_ids: Vec<i64>,
}

/// `LocalMovie`: one video file on its way into the library.
#[derive(Debug, Clone, Default)]
pub struct LocalMovie {
    pub path: String,
    pub size: i64,
    pub file_movie_info: Option<ParsedMovieInfo>,
    pub download_client_movie_info: Option<ParsedMovieInfo>,
    pub download_item: Option<DownloadClientItem>,
    pub folder_movie_info: Option<ParsedMovieInfo>,
    pub movie: Option<Movie>,
    pub old_files: Vec<DeletedMovieFile>,
    pub quality: QualityModel,
    pub languages: Vec<i64>,
    pub indexer_flags: i64,
    pub media_info: Option<MediaInfo>,
    pub existing_file: bool,
    pub scene_source: bool,
    pub release_group: Option<String>,
    pub edition: Option<String>,
    pub scene_name: Option<String>,
    pub other_video_files: bool,
    /// The formats of the file under the name the import will give it.
    pub custom_formats: Vec<CustomFormat>,
    pub custom_format_score: i64,
    /// The formats of the file under the name it has now (Radarr 7).
    pub original_file_name_custom_formats: Vec<CustomFormat>,
    pub original_file_name_custom_format_score: i64,
    /// The name `custom_formats` were worked out from.
    pub file_name_used_for_custom_format_calculation: Option<String>,
    pub release: Option<GrabbedMovieRelease>,
    pub script_imported: bool,
    pub file_name_before_rename: Option<String>,
    pub should_import_extras: bool,
    pub possible_extra_files: Vec<String>,
    pub subtitle_info: Option<SubtitleTitleInfo>,
}

impl LocalMovie {
    pub fn movie_ref(&self) -> &Movie {
        self.movie.as_ref().expect("a local movie carries its movie")
    }

    /// `ToMovieFile`: the movie file this local movie would become, its
    /// size and its path in the library left to the import. The relative
    /// path is set for a file already in the movie's folder.
    pub fn to_movie_file(&self, now: chrono::DateTime<chrono::Utc>) -> MovieFile {
        let movie = self.movie_ref();
        MovieFile {
            id: 0,
            movie_id: movie.id,
            relative_path: if crate::mediafiles::is_parent_path(&movie.path, &self.path) {
                crate::mediafiles::relative_path(&movie.path, &self.path).to_string()
            } else {
                String::new()
            },
            original_file_path: Some(self.original_file_path()),
            size: 0,
            date_added: now,
            scene_name: self.scene_name.clone(),
            release_group: self.release_group.clone(),
            edition: self.edition.clone(),
            quality: self.quality,
            languages: self.languages.clone(),
            media_info: self.media_info.as_ref().map(|m| m.model.clone()),
            indexer_flags: self.indexer_flags,
        }
    }

    /// `GetOriginalFilePath` of `LocalMovie`: the file's path relative to
    /// the folder its release is named after, or to its grandparent.
    pub fn original_file_path(&self) -> String {
        use crate::mediafiles::{file_name, parent_path, relative_path};
        let path = self.path.as_str();
        if let Some(folder_info) = &self.folder_movie_info {
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
}

/// `GetAncestorPath`: the nearest folder above `path` named `folder_name`.
fn ancestor_path<'a>(path: &'a str, folder_name: &str) -> Option<&'a str> {
    use crate::mediafiles::{file_name, parent_path};
    let mut current = parent_path(path);
    while let Some(folder) = current {
        if file_name(folder).eq_ignore_ascii_case(folder_name) {
            return Some(folder);
        }
        current = parent_path(folder);
    }
    None
}

/// `DeletedMovieFile`: a file an import replaced, and where the recycle
/// bin put it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedMovieFile {
    pub file: MovieFile,
    pub recycle_bin_path: Option<String>,
}

/// Radarr's `ImportRejectionReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovieRejectionReason {
    Unknown,
    FileLocked,
    UnknownMovie,
    DangerousFile,
    ExecutableFile,
    ArchiveFile,
    MovieFolder,
    InvalidFilePath,
    UnsupportedExtension,
    InvalidMovie,
    UnableToParse,
    Error,
    DecisionError,
    MovieAlreadyImported,
    MinimumFreeSpace,
    NoAudio,
    MovieNotFoundInRelease,
    Sample,
    SampleIndeterminate,
    Unpacking,
    MultiPartMovie,
    NotQualityUpgrade,
    NotRevisionUpgrade,
    NotCustomFormatUpgrade,
    NotCustomFormatUpgradeAfterRename,
}

/// `ImportRejection`: every one the original raises is permanent.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieImportRejection {
    pub reason: MovieRejectionReason,
    pub message: String,
}

impl MovieImportRejection {
    pub fn new(reason: MovieRejectionReason, message: impl Into<String>) -> MovieImportRejection {
        MovieImportRejection {
            reason,
            message: message.into(),
        }
    }
}

/// `Rejection.ToString()`: `[Permanent] message`, as the decision maker
/// logs the rejections of a file.
impl std::fmt::Display for MovieImportRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[Permanent] {}", self.message)
    }
}

/// `ImportDecision`.
#[derive(Debug, Clone)]
pub struct MovieImportDecision {
    /// `None` for the results that have no file behind them (an unknown
    /// movie folder, a mapped movie folder).
    pub local_movie: Option<LocalMovie>,
    pub rejections: Vec<MovieImportRejection>,
}

impl MovieImportDecision {
    pub fn new(local_movie: LocalMovie, rejections: Vec<MovieImportRejection>) -> MovieImportDecision {
        MovieImportDecision {
            local_movie: Some(local_movie),
            rejections,
        }
    }

    pub fn approved(&self) -> bool {
        self.rejections.is_empty()
    }

    pub fn rejected(local_movie: Option<LocalMovie>, rejection: MovieImportRejection) -> MovieImportDecision {
        MovieImportDecision {
            local_movie,
            rejections: vec![rejection],
        }
    }
}

/// `ImportResult`. Unlike the series one it holds no file: the original
/// tells an import from a failure by whether there are errors.
#[derive(Debug, Clone)]
pub struct MovieImportResult {
    pub decision: MovieImportDecision,
    pub errors: Vec<String>,
}

impl MovieImportResult {
    pub fn new(decision: MovieImportDecision, errors: Vec<String>) -> MovieImportResult {
        MovieImportResult { decision, errors }
    }

    /// `Result`: with errors, skipped when the decision approved the file
    /// and rejected when it did not; imported without.
    pub fn result(&self) -> ImportResultType {
        if self.errors.is_empty() {
            ImportResultType::Imported
        } else if self.decision.approved() {
            ImportResultType::Skipped
        } else {
            ImportResultType::Rejected
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_is_imported_only_without_errors() {
        let approved = MovieImportDecision::new(LocalMovie::default(), Vec::new());
        let rejected = MovieImportDecision::rejected(
            None,
            MovieImportRejection::new(MovieRejectionReason::Sample, "Sample"),
        );
        assert!(approved.approved());
        assert!(!rejected.approved());
        assert_eq!(
            MovieImportResult::new(approved.clone(), Vec::new()).result(),
            ImportResultType::Imported
        );
        assert_eq!(
            MovieImportResult::new(approved, vec!["Failed to import movie".into()]).result(),
            ImportResultType::Skipped
        );
        assert_eq!(
            MovieImportResult::new(rejected.clone(), vec!["Sample".into()]).result(),
            ImportResultType::Rejected
        );
        // The original reads a rejected decision without errors as imported.
        assert_eq!(
            MovieImportResult::new(rejected, Vec::new()).result(),
            ImportResultType::Imported
        );
        assert_eq!(
            MovieImportRejection::new(MovieRejectionReason::Sample, "Sample").to_string(),
            "[Permanent] Sample"
        );
    }
}
