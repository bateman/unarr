//! Route tables. `api_router` holds everything under the API prefix (behind
//! the API key); `ui_router` holds what the web UI loads without a key.

pub mod activity;
pub mod backup;
pub mod catalog;
pub mod command;
pub mod config;
pub mod downloadclient_writes;
pub mod episodes;
pub mod feed;
pub mod importlist_writes;
pub mod indexer_writes;
pub mod mediafiles;
pub mod notification_writes;
#[cfg(feature = "panic-switch")]
pub mod panic_switch;
pub mod providers;
pub mod queue;
pub mod radarr;
pub mod release;
pub mod series;
pub mod series_writes;
pub mod settings;
pub mod settings_writes;
pub mod signalr;
pub mod system;

use axum::routing::{delete, get, post, put};
use axum::Router;

use unarr_core::Personality;

use crate::state::AppState;

/// Routes under `/api/v3` (Sonarr, Radarr) or `/api/v1` (Lidarr): each
/// personality has its own table. Lidarr has none yet and shows Sonarr's,
/// as every personality did before there were two.
pub fn api_router(state: &AppState) -> Router<AppState> {
    let api = match state.personality() {
        Personality::Sonarr | Personality::Lidarr => sonarr_api_router(),
        Personality::Radarr => radarr::api_router(),
    };
    #[cfg(feature = "panic-switch")]
    let api = api.route("/system/panic", get(panic_switch::api));
    api
}

/// What is opened with the API key outside the API prefix. The calendar feed:
/// `/feed/v3/calendar/Sonarr.ics` or `Radarr.ics`, and the lower-case
/// spelling the originals' case-insensitive routing also answers.
pub fn feed_router(state: &AppState) -> Router<AppState> {
    let name = state.personality().app_name();
    let handler = match state.personality() {
        Personality::Sonarr | Personality::Lidarr => get(feed::calendar),
        Personality::Radarr => get(radarr::feed::calendar),
    };
    // `GET /api`, which the originals answer with the versions they speak.
    Router::new()
        .route("/api", get(system::api_root))
        .route(&format!("/feed/v3/calendar/{name}.ics"), handler.clone())
        .route(&format!("/feed/v3/calendar/{}.ics", name.to_lowercase()), handler)
}

fn sonarr_api_router() -> Router<AppState> {
    Router::new()
        .fallback(unknown_api_route)
        .route("/indexerflag", get(catalog::indexer_flags))
        .route("/system/status", get(system::status))
        .route("/health", get(system::health))
        .route("/config/host", get(config::host))
        .route("/config/host/{id}", put(settings_writes::host))
        .route("/config/ui", get(config::ui))
        .route("/config/ui/{id}", put(settings_writes::ui))
        .route("/config/downloadclient", get(config::download_client))
        .route(
            "/config/downloadclient/{id}",
            put(settings_writes::download_client),
        )
        .route("/config/importlist", get(config::import_list))
        .route("/config/importlist/{id}", put(settings_writes::import_list))
        .route("/series", get(series::list).post(series_writes::add))
        .route("/series/lookup", get(series_writes::lookup))
        .route("/series/import", post(series_writes::import))
        .route(
            "/mediacover/{series_id}/{filename}",
            get(series_writes::media_cover_api),
        )
        .route(
            "/series/editor",
            put(series_writes::editor_update).delete(series_writes::editor_delete),
        )
        .route(
            "/series/{id}",
            get(series::get)
                .put(series_writes::update)
                .delete(series_writes::delete),
        )
        .route("/series/{id}/folder", get(series_writes::folder))
        .route("/seasonpass", post(series_writes::season_pass))
        .route("/parse", get(series_writes::parse))
        .route("/filesystem", get(series_writes::filesystem))
        .route("/filesystem/type", get(series_writes::filesystem_type))
        .route(
            "/filesystem/mediafiles",
            get(series_writes::filesystem_media_files),
        )
        .route("/tag", get(catalog::tags).post(settings_writes::tag_create))
        .route("/tag/detail", get(catalog::tag_details))
        .route("/tag/detail/{id}", get(catalog::tag_detail))
        .route(
            "/tag/{id}",
            get(catalog::tag)
                .put(settings_writes::tag_update)
                .delete(settings_writes::tag_delete),
        )
        .route(
            "/rootfolder",
            get(catalog::root_folders).post(settings_writes::root_folder_create),
        )
        .route(
            "/rootfolder/{id}",
            get(catalog::root_folder).delete(settings_writes::root_folder_delete),
        )
        .route("/language", get(catalog::languages))
        .route("/language/{id}", get(catalog::language))
        .route("/qualitydefinition", get(catalog::quality_definitions))
        .route(
            "/qualitydefinition/update",
            put(settings_writes::quality_definition_update_many),
        )
        .route(
            "/qualitydefinition/{id}",
            get(catalog::quality_definition).put(settings_writes::quality_definition_update),
        )
        .route(
            "/qualityprofile",
            get(catalog::quality_profiles).post(settings_writes::quality_profile_create),
        )
        .route(
            "/qualityprofile/schema",
            get(settings_writes::quality_profile_schema),
        )
        .route(
            "/qualityprofile/{id}",
            get(catalog::quality_profile)
                .put(settings_writes::quality_profile_update)
                .delete(settings_writes::quality_profile_delete),
        )
        .route(
            "/customformat",
            get(catalog::custom_formats).post(settings_writes::custom_format_create),
        )
        .route(
            "/customformat/bulk",
            put(settings_writes::custom_format_bulk_update)
                .delete(settings_writes::custom_format_bulk_delete),
        )
        .route(
            "/customformat/{id}",
            get(catalog::custom_format)
                .put(settings_writes::custom_format_update)
                .delete(settings_writes::custom_format_delete),
        )
        .route("/diskspace", get(catalog::disk_space))
        .route("/episode", get(episodes::episodes))
        .route("/episode/monitor", put(series_writes::episodes_monitor))
        .route(
            "/episode/{id}",
            get(episodes::episode).put(series_writes::episode_update),
        )
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
        .route("/blocklist/bulk", delete(activity::blocklist_bulk_delete))
        .route("/blocklist/{id}", delete(activity::blocklist_delete))
        .route("/history/failed/{id}", post(activity::history_mark_failed))
        .route("/config/naming", get(settings::naming))
        .route("/config/naming/{id}", put(settings_writes::naming))
        .route("/config/mediamanagement", get(settings::media_management))
        .route(
            "/config/mediamanagement/{id}",
            put(settings_writes::media_management),
        )
        .route("/config/indexer", get(settings::indexer_config))
        .route("/config/indexer/{id}", put(settings_writes::indexer))
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
        .route(
            "/importlist",
            get(providers::import_lists).post(importlist_writes::create),
        )
        .route("/importlist/schema", get(providers::import_list_schema))
        .route("/importlist/test", post(importlist_writes::test))
        .route("/importlist/testall", post(importlist_writes::test_all))
        .route("/importlist/action/{name}", post(importlist_writes::action))
        .route(
            "/importlist/bulk",
            put(importlist_writes::bulk_update).delete(importlist_writes::bulk_delete),
        )
        .route(
            "/importlist/{id}",
            get(providers::import_list)
                .put(importlist_writes::update)
                .delete(importlist_writes::delete),
        )
        .route("/metadata", get(providers::metadata_list))
        .route("/metadata/schema", get(providers::metadata_schema))
        .route(
            "/metadata/{id}",
            get(providers::metadata).put(providers::metadata_update),
        )
        .route(
            "/importlistexclusion",
            get(providers::import_list_exclusions).post(settings_writes::exclusion_create),
        )
        .route(
            "/importlistexclusion/paged",
            get(providers::import_list_exclusions_paged),
        )
        .route(
            "/importlistexclusion/bulk",
            delete(settings_writes::exclusion_bulk_delete),
        )
        .route(
            "/importlistexclusion/{id}",
            put(settings_writes::exclusion_update).delete(settings_writes::exclusion_delete),
        )
        .route(
            "/autotagging",
            get(providers::auto_tagging).post(settings_writes::auto_tagging_create),
        )
        .route("/autotagging/schema", get(providers::auto_tagging_schema))
        .route(
            "/autotagging/{id}",
            get(settings_writes::auto_tagging_one)
                .put(settings_writes::auto_tagging_update)
                .delete(settings_writes::auto_tagging_delete),
        )
        .route("/customformat/schema", get(providers::custom_format_schema))
        .route(
            "/delayprofile",
            get(providers::delay_profiles).post(settings_writes::delay_profile_create),
        )
        .route(
            "/delayprofile/reorder/{id}",
            put(settings_writes::delay_profile_reorder),
        )
        .route(
            "/delayprofile/{id}",
            get(providers::delay_profile)
                .put(settings_writes::delay_profile_update)
                .delete(settings_writes::delay_profile_delete),
        )
        .route("/release", get(release::list).post(release::grab))
        .route("/release/push", post(release::push))
        .route(
            "/releaseprofile",
            get(providers::release_profiles).post(settings_writes::release_profile_create),
        )
        .route(
            "/releaseprofile/{id}",
            get(providers::release_profile)
                .put(settings_writes::release_profile_update)
                .delete(settings_writes::release_profile_delete),
        )
        .route(
            "/remotepathmapping",
            get(providers::remote_path_mappings).post(settings_writes::remote_path_mapping_create),
        )
        .route(
            "/remotepathmapping/{id}",
            get(providers::remote_path_mapping)
                .put(settings_writes::remote_path_mapping_update)
                .delete(settings_writes::remote_path_mapping_delete),
        )
        .route(
            "/customfilter",
            get(providers::custom_filters).post(settings_writes::custom_filter_create),
        )
        .route(
            "/customfilter/{id}",
            get(providers::custom_filter)
                .put(settings_writes::custom_filter_update)
                .delete(settings_writes::custom_filter_delete),
        )
        .route("/system/shutdown", post(system::shutdown))
        .route("/system/restart", post(system::restart))
        .route("/system/task", get(system::tasks))
        .route("/system/task/{id}", get(system::task))
        .route("/command", get(command::list).post(command::start))
        .route("/command/{id}", get(command::get).delete(command::cancel))
        .route("/log", get(system::log))
        .route("/log/file", get(backup::log_files))
        .route("/log/file/update", get(backup::update_log_files))
        .route("/log/file/{filename}", get(backup::log_file))
        .route("/system/backup", get(backup::list))
        .route("/system/backup/{id}", delete(backup::delete))
        .route(
            "/system/backup/restore/upload",
            post(backup::restore_upload).layer(axum::extract::DefaultBodyLimit::max(backup::UPLOAD_LIMIT)),
        )
        .route("/system/backup/restore/{id}", post(backup::restore))
        .route("/update", get(system::update))
        .route("/localization", get(system::localization))
        .route("/localization/language", get(system::localization_language))
}

/// Routes outside the API prefix. SignalR is the same hub protocol for
/// every personality.
pub fn ui_router(_state: &AppState) -> Router<AppState> {
    let ui = Router::new()
        .route("/signalr/messages/negotiate", post(signalr::negotiate))
        .route("/signalr/messages", get(signalr::connect));
    #[cfg(feature = "panic-switch")]
    let ui = ui.route("/panic", get(panic_switch::page));
    ui
}

/// The originals answer an unknown API path with an empty 404, and any
/// other method on it with 405 `Allow: GET`: their catch-all route takes
/// `GET` alone (recorded on Sonarr 4.0.20 and Radarr 6.4.4, under `/api/v3`
/// and under `/api/v1` alike).
pub(crate) async fn unknown_api_route(method: axum::http::Method) -> axum::response::Response {
    use axum::response::IntoResponse;
    if method == axum::http::Method::GET {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }
    (
        axum::http::StatusCode::METHOD_NOT_ALLOWED,
        [(axum::http::header::ALLOW, "GET")],
    )
        .into_response()
}
