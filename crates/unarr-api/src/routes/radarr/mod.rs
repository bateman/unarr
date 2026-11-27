//! Radarr's route table: the handlers every personality shares, and the
//! movie-shaped ones of this module where Sonarr has series and episodes.
//!
//! A write no milestone has brought yet answers 405, a route of a later
//! milestone the originals' empty 404.

pub mod activity;
pub mod catalog;
pub mod catalog_writes;
pub mod config;
pub mod config_writes;
pub mod discover;
pub mod feed;
pub mod manualimport;
pub mod movie_writes;
pub mod movies;
pub mod providers;
pub mod queue;
pub mod release;
pub mod resources;
pub mod system;

use axum::routing::{delete, get, post, put};
use axum::Router;

use crate::routes::{
    backup, catalog as shared_catalog, config as shared_config, downloadclient_writes, importlist_writes,
    indexer_writes, notification_writes, providers as shared_providers, system as shared_system,
};
use crate::state::AppState;

/// `TypedResults.Ok`: what Radarr 7 answers where Radarr 6 said
/// `Accepted` (a bulk update).
fn typed_ok(body: serde_json::Value) -> axum::response::Response {
    use axum::response::IntoResponse;
    (axum::http::StatusCode::OK, axum::Json(body)).into_response()
}

/// Routes under `/api/v3`.
pub fn api_router() -> Router<AppState> {
    Router::new()
        .fallback(crate::routes::unknown_api_route)
        .route("/system/status", get(system::status))
        .route("/system/shutdown", post(shared_system::shutdown))
        .route("/system/restart", post(shared_system::restart))
        .route("/queue", get(queue::list))
        .route("/queue/status", get(queue::status))
        .route("/queue/details", get(queue::details))
        .route("/queue/bulk", delete(queue::remove_many))
        .route("/queue/grab/bulk", post(queue::grab_many))
        .route("/queue/grab/{id}", post(queue::grab))
        .route("/queue/{id}", delete(queue::remove))
        .route("/health", get(shared_system::health))
        .route("/diskspace", get(shared_catalog::disk_space))
        .route("/system/task", get(shared_system::tasks))
        .route("/system/task/{id}", get(shared_system::task))
        .route("/system/backup", get(backup::list))
        .route("/system/backup/{id}", delete(backup::delete))
        .route(
            "/system/backup/restore/upload",
            post(backup::restore_upload).layer(axum::extract::DefaultBodyLimit::max(backup::UPLOAD_LIMIT)),
        )
        .route("/system/backup/restore/{id}", post(backup::restore))
        .route(
            "/command",
            get(crate::routes::command::list).post(crate::routes::command::start),
        )
        .route(
            "/command/{id}",
            get(crate::routes::command::get).delete(crate::routes::command::cancel),
        )
        .route("/log", get(shared_system::log))
        .route("/log/file", get(backup::log_files))
        .route("/log/file/update", get(backup::update_log_files))
        .route("/log/file/{filename}", get(backup::log_file))
        .route("/update", get(shared_system::update))
        .route("/localization", get(providers::localization))
        .route(
            "/localization/language",
            get(shared_system::localization_language),
        )
        .route("/config/host", get(shared_config::host))
        .route(
            "/config/host/{id}",
            get(shared_config::host).put(crate::routes::settings_writes::host),
        )
        .route("/config/ui", get(config::ui))
        .route("/config/ui/{id}", get(config::ui).put(config_writes::ui))
        .route("/config/downloadclient", get(config::download_client))
        .route(
            "/config/downloadclient/{id}",
            get(config::download_client).put(config_writes::download_client),
        )
        .route("/config/importlist", get(config::import_list))
        .route(
            "/config/importlist/{id}",
            get(config::import_list).put(config_writes::import_list),
        )
        .route("/config/naming", get(config::naming))
        .route(
            "/config/naming/{id}",
            get(config::naming).put(config_writes::naming),
        )
        .route("/config/naming/examples", get(config::naming_examples))
        .route("/config/mediamanagement", get(config::media_management))
        .route(
            "/config/mediamanagement/{id}",
            get(config::media_management).put(config_writes::media_management),
        )
        .route("/config/indexer", get(config::indexer))
        .route(
            "/config/indexer/{id}",
            get(config::indexer).put(config_writes::indexer),
        )
        .route("/config/metadata", get(config::metadata))
        .route(
            "/config/metadata/{id}",
            get(config::metadata).put(config_writes::metadata),
        )
        .route(
            "/tag",
            get(shared_catalog::tags).post(crate::routes::settings_writes::tag_create),
        )
        .route("/tag/detail", get(catalog::tag_details))
        .route("/tag/detail/{id}", get(catalog::tag_detail))
        .route(
            "/tag/{id}",
            get(shared_catalog::tag)
                .put(crate::routes::settings_writes::tag_update)
                .delete(crate::routes::settings_writes::tag_delete),
        )
        .route(
            "/rootfolder",
            get(shared_catalog::root_folders).post(crate::routes::settings_writes::root_folder_create),
        )
        .route(
            "/rootfolder/{id}",
            get(shared_catalog::root_folder).delete(crate::routes::settings_writes::root_folder_delete),
        )
        .route("/language", get(catalog::languages))
        .route("/language/{id}", get(catalog::language))
        .route("/qualitydefinition", get(catalog::quality_definitions))
        .route(
            "/qualitydefinition/limits",
            get(catalog::quality_definition_limits),
        )
        .route(
            "/qualitydefinition/update",
            put(catalog_writes::quality_definition_update_many),
        )
        .route(
            "/qualitydefinition/{id}",
            get(catalog::quality_definition).put(catalog_writes::quality_definition_update),
        )
        .route(
            "/qualityprofile",
            get(catalog::quality_profiles).post(catalog_writes::quality_profile_create),
        )
        .route("/qualityprofile/schema", get(catalog::quality_profile_schema))
        .route(
            "/qualityprofile/{id}",
            get(catalog::quality_profile)
                .put(catalog_writes::quality_profile_update)
                .delete(catalog_writes::quality_profile_delete),
        )
        .route(
            "/customformat",
            get(shared_catalog::custom_formats).post(crate::routes::settings_writes::custom_format_create),
        )
        .route(
            "/customformat/bulk",
            put(providers::custom_format_bulk_update)
                .delete(crate::routes::settings_writes::custom_format_bulk_delete),
        )
        .route(
            "/customformat/schema",
            get(shared_providers::custom_format_schema),
        )
        .route(
            "/customformat/{id}",
            get(shared_catalog::custom_format)
                .put(crate::routes::settings_writes::custom_format_update)
                .delete(crate::routes::settings_writes::custom_format_delete),
        )
        .route("/indexerflag", get(catalog::indexer_flags))
        .route(
            "/indexer",
            get(shared_providers::indexers).post(indexer_writes::create),
        )
        .route("/indexer/schema", get(shared_providers::indexer_schema))
        .route("/indexer/test", post(providers::indexer_test))
        .route("/indexer/testall", post(indexer_writes::test_all))
        .route("/indexer/action/{name}", post(indexer_writes::action))
        .route(
            "/indexer/bulk",
            put(providers::indexer_bulk_update).delete(indexer_writes::bulk_delete),
        )
        .route(
            "/indexer/{id}",
            get(shared_providers::indexer)
                .put(indexer_writes::update)
                .delete(indexer_writes::delete),
        )
        .route(
            "/downloadclient",
            get(shared_providers::download_clients).post(downloadclient_writes::create),
        )
        .route(
            "/downloadclient/schema",
            get(shared_providers::download_client_schema),
        )
        .route("/downloadclient/test", post(providers::download_client_test))
        .route("/downloadclient/testall", post(downloadclient_writes::test_all))
        .route(
            "/downloadclient/action/{name}",
            post(downloadclient_writes::action),
        )
        .route(
            "/downloadclient/bulk",
            put(providers::download_client_bulk_update).delete(downloadclient_writes::bulk_delete),
        )
        .route(
            "/downloadclient/{id}",
            get(shared_providers::download_client)
                .put(downloadclient_writes::update)
                .delete(downloadclient_writes::delete),
        )
        .route(
            "/notification",
            get(shared_providers::notifications).post(notification_writes::create),
        )
        .route("/notification/schema", get(shared_providers::notification_schema))
        .route("/notification/test", post(providers::notification_test))
        .route("/notification/testall", post(notification_writes::test_all))
        .route("/notification/action/{name}", post(notification_writes::action))
        // `NotificationController` takes the bulk actions off its routes
        // (`[NonAction]`), and `bulk` is no id (`{id:int?}`).
        .route(
            "/notification/bulk",
            axum::routing::any(crate::routes::unknown_api_route),
        )
        .route(
            "/notification/{id}",
            get(shared_providers::notification)
                .put(notification_writes::update)
                .delete(notification_writes::delete),
        )
        .route(
            "/importlist",
            get(shared_providers::import_lists).post(importlist_writes::create),
        )
        .route("/importlist/schema", get(shared_providers::import_list_schema))
        .route("/importlist/test", post(providers::import_list_test))
        .route("/importlist/testall", post(importlist_writes::test_all))
        .route("/importlist/action/{name}", post(importlist_writes::action))
        .route(
            "/importlist/bulk",
            put(providers::import_list_bulk_update).delete(importlist_writes::bulk_delete),
        )
        .route(
            "/importlist/{id}",
            get(shared_providers::import_list)
                .put(importlist_writes::update)
                .delete(importlist_writes::delete),
        )
        .route("/metadata", get(shared_providers::metadata_list))
        .route("/metadata/schema", get(shared_providers::metadata_schema))
        .route("/metadata/test", post(providers::metadata_test))
        .route("/metadata/testall", post(shared_providers::metadata_test_all))
        .route(
            "/metadata/{id}",
            get(shared_providers::metadata).put(shared_providers::metadata_update),
        )
        .route(
            "/autotagging",
            get(shared_providers::auto_tagging).post(crate::routes::settings_writes::auto_tagging_create),
        )
        .route(
            "/autotagging/{id}",
            get(crate::routes::settings_writes::auto_tagging_one)
                .put(crate::routes::settings_writes::auto_tagging_update)
                .delete(crate::routes::settings_writes::auto_tagging_delete),
        )
        .route("/autotagging/schema", get(shared_providers::auto_tagging_schema))
        .route(
            "/delayprofile",
            get(shared_providers::delay_profiles).post(crate::routes::settings_writes::delay_profile_create),
        )
        .route(
            "/delayprofile/reorder/{id}",
            put(crate::routes::settings_writes::delay_profile_reorder),
        )
        .route(
            "/delayprofile/{id}",
            get(shared_providers::delay_profile)
                .put(crate::routes::settings_writes::delay_profile_update)
                .delete(crate::routes::settings_writes::delay_profile_delete),
        )
        .route(
            "/releaseprofile",
            get(shared_providers::release_profiles)
                .post(crate::routes::settings_writes::release_profile_create),
        )
        .route(
            "/releaseprofile/{id}",
            get(shared_providers::release_profile)
                .put(crate::routes::settings_writes::release_profile_update)
                .delete(crate::routes::settings_writes::release_profile_delete),
        )
        .route(
            "/remotepathmapping",
            get(shared_providers::remote_path_mappings)
                .post(crate::routes::settings_writes::remote_path_mapping_create),
        )
        .route(
            "/remotepathmapping/{id}",
            get(shared_providers::remote_path_mapping)
                .put(providers::remote_path_mapping_update)
                .delete(crate::routes::settings_writes::remote_path_mapping_delete),
        )
        .route(
            "/customfilter",
            get(shared_providers::custom_filters).post(crate::routes::settings_writes::custom_filter_create),
        )
        .route(
            "/customfilter/{id}",
            get(shared_providers::custom_filter)
                .put(crate::routes::settings_writes::custom_filter_update)
                .delete(crate::routes::settings_writes::custom_filter_delete),
        )
        .route("/release", get(release::list).post(release::grab))
        .route("/release/push", post(release::push))
        .route("/movie", get(movies::list).post(movie_writes::add))
        // Routes of the later milestones that would otherwise be read as
        // an id: no route yet, the originals' empty 404.
        .route("/movie/lookup", get(movie_writes::lookup))
        .route("/movie/lookup/tmdb", get(movie_writes::lookup_tmdb))
        .route("/movie/lookup/imdb", get(movie_writes::lookup_imdb))
        // Discover's list, and the movies the page adds from it, which
        // `ImportListMoviesController` adds as the library import does.
        .route(
            "/importlist/movie",
            get(discover::list).post(movie_writes::import_from_lists),
        )
        .route(
            "/movie/editor",
            put(movie_writes::editor_update).delete(movie_writes::editor_delete),
        )
        .route("/movie/import", post(movie_writes::import))
        .route("/movie/{id}/folder", get(movies::folder))
        .route(
            "/movie/{id}",
            get(movies::get)
                .put(movie_writes::update)
                .delete(movie_writes::delete),
        )
        // A movie's cover for clients that hold the API key and no session:
        // the folder is named after the movie's id, as a series' is.
        .route(
            "/mediacover/{movie_id}/{filename}",
            get(crate::routes::series_writes::media_cover_api),
        )
        .route("/moviefile", get(movies::files))
        .route("/moviefile/editor", put(movies::file_editor))
        .route(
            "/moviefile/bulk",
            put(movies::file_bulk_update).delete(movies::file_bulk_delete),
        )
        .route(
            "/moviefile/{id}",
            get(movies::file)
                .put(movies::file_update)
                .delete(movies::file_delete),
        )
        .route("/filesystem", get(crate::routes::series_writes::filesystem))
        .route(
            "/filesystem/type",
            get(crate::routes::series_writes::filesystem_type),
        )
        .route(
            "/filesystem/mediafiles",
            get(crate::routes::series_writes::filesystem_media_files),
        )
        .route(
            "/manualimport",
            get(manualimport::list).post(manualimport::reprocess),
        )
        .route("/parse", get(manualimport::parse))
        .route("/rename", get(movies::rename_preview))
        .route("/extrafile", get(movies::extra_files))
        .route(
            "/collection",
            get(movies::collections).put(catalog_writes::collection_update_many),
        )
        .route(
            "/collection/{id}",
            get(movies::collection).put(catalog_writes::collection_update),
        )
        .route("/credit", get(movies::credits))
        .route("/credit/{id}", get(movies::credit))
        .route("/alttitle", get(movies::alternative_titles))
        .route("/alttitle/{id}", get(movies::alternative_title))
        .route("/history", get(activity::history))
        .route("/history/since", get(activity::history_since))
        .route("/history/movie", get(activity::history_movie))
        .route("/history/failed/{id}", post(activity::history_mark_failed))
        .route("/blocklist", get(activity::blocklist))
        .route("/blocklist/movie", get(activity::blocklist_movie))
        .route(
            "/blocklist/bulk",
            delete(crate::routes::activity::blocklist_bulk_delete),
        )
        .route(
            "/blocklist/{id}",
            delete(crate::routes::activity::blocklist_delete),
        )
        .route("/calendar", get(activity::calendar))
        .route("/wanted/missing", get(activity::missing))
        .route("/wanted/cutoff", get(activity::cutoff))
        .route(
            "/exclusions",
            get(catalog::exclusions).post(catalog_writes::exclusion_create),
        )
        .route("/exclusions/paged", get(catalog::exclusions_paged))
        .route(
            "/exclusions/bulk",
            post(catalog_writes::exclusion_bulk_create).delete(catalog_writes::exclusion_bulk_delete),
        )
        .route(
            "/exclusions/{id}",
            get(catalog::exclusion)
                .put(catalog_writes::exclusion_update)
                .delete(catalog_writes::exclusion_delete),
        )
}
