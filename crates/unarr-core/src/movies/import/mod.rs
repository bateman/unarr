//! The decision half of Radarr's import of a completed download or of a
//! folder (`MediaFiles/MovieImport`): each video file is parsed, augmented
//! with what its folder, the download client, the grab history and the
//! media info say, and run through the import specifications. The twin of
//! the series side's `mediafiles::{local, aggregate, specs, decide,
//! sample}`, with which it shares what carries no media notion (the media
//! info reader, the probes, the sample runtime rule, the path helpers).
//!
//! The modules follow the original's classes: `local` holds `LocalMovie`
//! and the decision and result shapes, `aggregate` the
//! `AggregationService` and its aggregators, `specs` the
//! `MovieImport/Specifications`, `sample` is `DetectSample` and `decide`
//! the `ImportDecisionMaker`.

pub mod aggregate;
pub mod approve;
pub mod completed;
pub mod decide;
pub mod deletion;
pub mod downloaded;
pub mod events;
pub mod extras;
pub mod filedate;
pub mod local;
pub mod manual;
pub mod rename;
pub mod sample;
pub mod scan;
pub mod specs;

pub use decide::{get_import_decisions, MovieEnvironment};
pub use local::{
    DeletedMovieFile, GrabbedMovieRelease, LocalMovie, MovieImportDecision, MovieImportRejection,
    MovieImportResult, MovieRejectionReason,
};
