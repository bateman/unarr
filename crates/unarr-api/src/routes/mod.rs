//! Route tables. `api_router` holds everything under the API prefix (behind
//! the API key); `ui_router` holds what the web UI loads without a key.

pub mod activity;
pub mod catalog;
pub mod config;
pub mod episodes;
pub mod providers;
pub mod series;
pub mod settings;
pub mod signalr;
pub mod system;

use axum::routing::{get, post};
use axum::Router;

use crate::state::AppState;

/// Routes under `/api/v3` (Sonarr, Radarr) or `/api/v1` (Lidarr).
pub fn api_router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .fallback(unknown_api_route)
        .route("/indexerflag", get(catalog::indexer_flags))
        .route("/system/status", get(system::status))
        .route("/health", get(system::health))
        .route("/config/host", get(config::host))
        .route("/config/ui", get(config::ui))
        .route("/config/downloadclient", get(config::download_client))
        .route("/config/importlist", get(config::import_list))
        .route("/series", get(series::list))
        .route("/series/{id}", get(series::get))
        .route("/tag", get(catalog::tags))
        .route("/tag/detail", get(catalog::tag_details))
        .route("/tag/detail/{id}", get(catalog::tag_detail))
        .route("/tag/{id}", get(catalog::tag))
        .route("/rootfolder", get(catalog::root_folders))
        .route("/rootfolder/{id}", get(catalog::root_folder))
        .route("/language", get(catalog::languages))
        .route("/language/{id}", get(catalog::language))
        .route("/qualitydefinition", get(catalog::quality_definitions))
        .route("/qualitydefinition/{id}", get(catalog::quality_definition))
        .route("/qualityprofile", get(catalog::quality_profiles))
        .route("/qualityprofile/{id}", get(catalog::quality_profile))
        .route("/customformat", get(catalog::custom_formats))
        .route("/customformat/{id}", get(catalog::custom_format))
        .route("/diskspace", get(catalog::disk_space))
        .route("/episode", get(episodes::episodes))
        .route("/episode/{id}", get(episodes::episode))
        .route("/episodefile", get(episodes::episode_files))
        .route("/episodefile/{id}", get(episodes::episode_file))
        .route("/calendar", get(episodes::calendar))
        .route("/wanted/missing", get(episodes::missing))
        .route("/wanted/cutoff", get(episodes::cutoff))
        .route("/history", get(activity::history))
        .route("/history/since", get(activity::history_since))
        .route("/history/series", get(activity::history_series))
        .route("/queue", get(activity::queue))
        .route("/queue/status", get(activity::queue_status))
        .route("/queue/details", get(activity::queue_details))
        .route("/blocklist", get(activity::blocklist))
        .route("/config/naming", get(settings::naming))
        .route("/config/mediamanagement", get(settings::media_management))
        .route("/config/indexer", get(settings::indexer_config))
        .route("/config/naming/examples", get(settings::naming_examples))
        .route("/languageprofile", get(settings::language_profiles))
        .route("/languageprofile/{id}", get(settings::language_profile))
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
        .route("/importlistexclusion", get(providers::import_list_exclusions))
        .route("/autotagging", get(providers::auto_tagging))
        .route("/autotagging/schema", get(providers::auto_tagging_schema))
        .route("/customformat/schema", get(providers::custom_format_schema))
        .route("/delayprofile", get(providers::delay_profiles))
        .route("/delayprofile/{id}", get(providers::delay_profile))
        .route("/releaseprofile", get(providers::release_profiles))
        .route("/releaseprofile/{id}", get(providers::release_profile))
        .route("/remotepathmapping", get(providers::remote_path_mappings))
        .route("/remotepathmapping/{id}", get(providers::remote_path_mapping))
        .route("/customfilter", get(providers::custom_filters))
        .route("/customfilter/{id}", get(providers::custom_filter))
        .route("/system/task", get(system::tasks))
        .route("/system/task/{id}", get(system::task))
        .route("/command", get(system::commands))
        .route("/log", get(system::log))
        .route("/update", get(system::update))
        .route("/localization", get(system::localization))
        .route("/localization/language", get(system::localization_language))
}

/// Routes outside the API prefix.
pub fn ui_router(_state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/signalr/messages/negotiate", post(signalr::negotiate))
        .route("/signalr/messages", get(signalr::connect))
}

/// The originals answer an unknown API path with an empty 404.
async fn unknown_api_route() -> axum::http::StatusCode {
    axum::http::StatusCode::NOT_FOUND
}
