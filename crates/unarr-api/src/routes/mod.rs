//! Route tables. `api_router` holds everything under the API prefix (behind
//! the API key); `ui_router` holds what the web UI loads without a key.

pub mod activity;
pub mod catalog;
pub mod command;
pub mod config;
pub mod downloadclient_writes;
pub mod episodes;
pub mod indexer_writes;
pub mod mediafiles;
pub mod notification_writes;
pub mod providers;
pub mod queue;
pub mod release;
pub mod series;
pub mod settings;
pub mod signalr;
pub mod system;

use axum::routing::{delete, get, post, put};
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
        .route("/episodefile/editor", put(mediafiles::editor))
        .route(
            "/episodefile/bulk",
            put(mediafiles::bulk_update).delete(mediafiles::bulk_delete),
        )
        .route(
            "/episodefile/{id}",
            get(episodes::episode_file)
                .put(mediafiles::update)
                .delete(mediafiles::delete),
        )
        .route(
            "/manualimport",
            get(mediafiles::manual_import).post(mediafiles::reprocess),
        )
        .route("/rename", get(mediafiles::rename_preview))
        .route("/calendar", get(episodes::calendar))
        .route("/wanted/missing", get(episodes::missing))
        .route("/wanted/cutoff", get(episodes::cutoff))
        .route("/history", get(activity::history))
        .route("/history/since", get(activity::history_since))
        .route("/history/series", get(activity::history_series))
        .route("/queue", get(queue::list))
        .route("/queue/status", get(queue::status))
        .route("/queue/details", get(queue::details))
        .route("/queue/bulk", delete(queue::remove_many))
        .route("/queue/grab/bulk", post(queue::grab_many))
        .route("/queue/grab/{id}", post(queue::grab))
        .route("/queue/{id}", delete(queue::remove))
        .route("/blocklist", get(activity::blocklist))
        .route("/config/naming", get(settings::naming))
        .route("/config/mediamanagement", get(settings::media_management))
        .route("/config/indexer", get(settings::indexer_config))
        .route("/config/naming/examples", get(settings::naming_examples))
        .route("/languageprofile", get(settings::language_profiles))
        .route("/languageprofile/{id}", get(settings::language_profile))
        .route("/indexer", get(providers::indexers).post(indexer_writes::create))
        .route("/indexer/schema", get(providers::indexer_schema))
        .route("/indexer/test", post(indexer_writes::test))
        .route("/indexer/testall", post(indexer_writes::test_all))
        .route("/indexer/action/{name}", post(indexer_writes::action))
        .route(
            "/indexer/bulk",
            put(indexer_writes::bulk_update).delete(indexer_writes::bulk_delete),
        )
        .route(
            "/indexer/{id}",
            get(providers::indexer)
                .put(indexer_writes::update)
                .delete(indexer_writes::delete),
        )
        .route(
            "/downloadclient",
            get(providers::download_clients).post(downloadclient_writes::create),
        )
        .route("/downloadclient/schema", get(providers::download_client_schema))
        .route("/downloadclient/test", post(downloadclient_writes::test))
        .route("/downloadclient/testall", post(downloadclient_writes::test_all))
        .route(
            "/downloadclient/action/{name}",
            post(downloadclient_writes::action),
        )
        .route(
            "/downloadclient/bulk",
            put(downloadclient_writes::bulk_update).delete(downloadclient_writes::bulk_delete),
        )
        .route(
            "/downloadclient/{id}",
            get(providers::download_client)
                .put(downloadclient_writes::update)
                .delete(downloadclient_writes::delete),
        )
        .route(
            "/notification",
            get(providers::notifications).post(notification_writes::create),
        )
        .route("/notification/schema", get(providers::notification_schema))
        .route("/notification/test", post(notification_writes::test))
        .route("/notification/testall", post(notification_writes::test_all))
        .route("/notification/action/{name}", post(notification_writes::action))
        .route(
            "/notification/bulk",
            put(notification_writes::bulk_update).delete(notification_writes::bulk_delete),
        )
        .route(
            "/notification/{id}",
            get(providers::notification)
                .put(notification_writes::update)
                .delete(notification_writes::delete),
        )
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
        .route("/release", get(release::list).post(release::grab))
        .route("/release/push", post(release::push))
        .route("/releaseprofile", get(providers::release_profiles))
        .route("/releaseprofile/{id}", get(providers::release_profile))
        .route("/remotepathmapping", get(providers::remote_path_mappings))
        .route("/remotepathmapping/{id}", get(providers::remote_path_mapping))
        .route("/customfilter", get(providers::custom_filters))
        .route("/customfilter/{id}", get(providers::custom_filter))
        .route("/system/task", get(system::tasks))
        .route("/system/task/{id}", get(system::task))
        .route("/command", get(command::list).post(command::start))
        .route("/command/{id}", get(command::get).delete(command::cancel))
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
