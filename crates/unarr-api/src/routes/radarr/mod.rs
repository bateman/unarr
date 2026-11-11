//! Radarr's route table: the handlers every personality shares, and the
//! movie-shaped ones of this module where Sonarr has series and episodes.
//!
//! Milestone 4a is read-only: a write answers 405, a route of a later
//! milestone the originals' empty 404.

pub mod activity;
pub mod catalog;
pub mod config;
pub mod feed;
pub mod movies;
pub mod resources;
pub mod system;

use axum::routing::get;
use axum::Router;

use crate::routes::{
    backup, catalog as shared_catalog, config as shared_config, providers, system as shared_system,
};
use crate::state::AppState;

/// Routes under `/api/v3`.
pub fn api_router() -> Router<AppState> {
    Router::new()
        .fallback(crate::routes::unknown_api_route)
        .route("/system/status", get(system::status))
        .route("/queue", get(system::queue))
        .route("/queue/status", get(system::queue_status))
        .route("/queue/details", get(system::queue_details))
        .route("/health", get(shared_system::health))
        .route("/diskspace", get(shared_catalog::disk_space))
        .route("/system/task", get(shared_system::tasks))
        .route("/system/task/{id}", get(shared_system::task))
        .route("/system/backup", get(backup::list))
        .route("/command", get(system::commands))
        .route("/log", get(shared_system::log))
        .route("/log/file", get(backup::log_files))
        .route("/log/file/update", get(backup::update_log_files))
        .route("/log/file/{filename}", get(backup::log_file))
        .route("/update", get(shared_system::update))
        .route("/localization", get(shared_system::localization))
        .route(
            "/localization/language",
            get(shared_system::localization_language),
        )
        .route("/config/host", get(shared_config::host))
        .route("/config/ui", get(config::ui))
        .route("/config/downloadclient", get(config::download_client))
        .route("/config/importlist", get(config::import_list))
        .route("/config/naming", get(config::naming))
        .route("/config/mediamanagement", get(config::media_management))
        .route("/config/indexer", get(config::indexer))
        .route("/config/metadata", get(config::metadata))
        .route("/tag", get(shared_catalog::tags))
        .route("/tag/detail", get(catalog::tag_details))
        .route("/tag/detail/{id}", get(catalog::tag_detail))
        .route("/tag/{id}", get(shared_catalog::tag))
        .route("/rootfolder", get(shared_catalog::root_folders))
        .route("/rootfolder/{id}", get(shared_catalog::root_folder))
        .route("/language", get(catalog::languages))
        .route("/language/{id}", get(catalog::language))
        .route("/qualitydefinition", get(catalog::quality_definitions))
        .route(
            "/qualitydefinition/limits",
            get(catalog::quality_definition_limits),
        )
        .route("/qualitydefinition/{id}", get(catalog::quality_definition))
        .route("/qualityprofile", get(catalog::quality_profiles))
        .route("/qualityprofile/schema", get(catalog::quality_profile_schema))
        .route("/qualityprofile/{id}", get(catalog::quality_profile))
        .route("/customformat", get(shared_catalog::custom_formats))
        .route("/customformat/schema", get(providers::custom_format_schema))
        .route("/customformat/{id}", get(shared_catalog::custom_format))
        .route("/indexerflag", get(catalog::indexer_flags))
        .route("/indexer", get(providers::indexers))
        .route("/indexer/schema", get(providers::indexer_schema))
        .route("/indexer/{id}", get(providers::indexer))
        .route("/downloadclient", get(providers::download_clients))
        .route("/downloadclient/schema", get(providers::download_client_schema))
        .route("/downloadclient/{id}", get(providers::download_client))
        .route("/notification", get(providers::notifications))
        .route("/notification/schema", get(providers::notification_schema))
        .route("/notification/{id}", get(providers::notification))
        .route("/importlist", get(providers::import_lists))
        .route("/importlist/schema", get(providers::import_list_schema))
        .route("/importlist/{id}", get(providers::import_list))
        .route("/metadata", get(providers::metadata_list))
        .route("/metadata/schema", get(providers::metadata_schema))
        .route("/metadata/{id}", get(providers::metadata))
        .route("/autotagging", get(providers::auto_tagging))
        .route("/autotagging/schema", get(providers::auto_tagging_schema))
        .route("/delayprofile", get(providers::delay_profiles))
        .route("/delayprofile/{id}", get(providers::delay_profile))
        .route("/releaseprofile", get(providers::release_profiles))
        .route("/releaseprofile/{id}", get(providers::release_profile))
        .route("/remotepathmapping", get(providers::remote_path_mappings))
        .route("/remotepathmapping/{id}", get(providers::remote_path_mapping))
        .route("/customfilter", get(providers::custom_filters))
        .route("/customfilter/{id}", get(providers::custom_filter))
        .route("/movie", get(movies::list))
        // Routes of the later milestones that would otherwise be read as
        // an id: no route yet, the originals' empty 404.
        .route("/movie/lookup", get(crate::routes::unknown_api_route))
        .route("/importlist/movie", get(crate::routes::unknown_api_route))
        .route("/movie/{id}", get(movies::get))
        // A movie's cover for clients that hold the API key and no session:
        // the folder is named after the movie's id, as a series' is.
        .route(
            "/mediacover/{movie_id}/{filename}",
            get(crate::routes::series_writes::media_cover_api),
        )
        .route("/moviefile", get(movies::files))
        .route("/moviefile/{id}", get(movies::file))
        .route("/extrafile", get(movies::extra_files))
        .route("/collection", get(movies::collections))
        .route("/collection/{id}", get(movies::collection))
        .route("/credit", get(movies::credits))
        .route("/credit/{id}", get(movies::credit))
        .route("/alttitle", get(movies::alternative_titles))
        .route("/alttitle/{id}", get(movies::alternative_title))
        .route("/history", get(activity::history))
        .route("/history/since", get(activity::history_since))
        .route("/history/movie", get(activity::history_movie))
        .route("/blocklist", get(activity::blocklist))
        .route("/blocklist/movie", get(activity::blocklist_movie))
        .route("/calendar", get(activity::calendar))
        .route("/wanted/missing", get(activity::missing))
        .route("/wanted/cutoff", get(activity::cutoff))
        .route("/exclusions", get(catalog::exclusions))
        .route("/exclusions/paged", get(catalog::exclusions_paged))
        .route("/exclusions/{id}", get(catalog::exclusion))
}
