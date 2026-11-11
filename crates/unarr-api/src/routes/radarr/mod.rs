//! Radarr's route table: the handlers every personality shares, and the
//! movie-shaped ones of this module where Sonarr has series and episodes.
//!
//! A write no milestone has brought yet answers 405, a route of a later
//! milestone the originals' empty 404.

pub mod activity;
pub mod catalog;
pub mod config;
pub mod feed;
pub mod movies;
pub mod providers;
pub mod queue;
pub mod release;
pub mod resources;
pub mod system;

use axum::routing::{delete, get, post, put};
use axum::Router;

use crate::routes::{
    backup, catalog as shared_catalog, config as shared_config, downloadclient_writes, indexer_writes,
    providers as shared_providers, system as shared_system,
};
use crate::state::AppState;

/// Routes under `/api/v3`.
pub fn api_router() -> Router<AppState> {
    Router::new()
        .fallback(crate::routes::unknown_api_route)
        .route("/system/status", get(system::status))
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
        .route(
            "/customformat/schema",
            get(shared_providers::custom_format_schema),
        )
        .route("/customformat/{id}", get(shared_catalog::custom_format))
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
            put(indexer_writes::bulk_update).delete(indexer_writes::bulk_delete),
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
            put(downloadclient_writes::bulk_update).delete(downloadclient_writes::bulk_delete),
        )
        .route(
            "/downloadclient/{id}",
            get(shared_providers::download_client)
                .put(downloadclient_writes::update)
                .delete(downloadclient_writes::delete),
        )
        .route("/notification", get(shared_providers::notifications))
        .route("/notification/schema", get(shared_providers::notification_schema))
        .route("/notification/{id}", get(shared_providers::notification))
        .route("/importlist", get(shared_providers::import_lists))
        .route("/importlist/schema", get(shared_providers::import_list_schema))
        .route("/importlist/{id}", get(shared_providers::import_list))
        .route("/metadata", get(shared_providers::metadata_list))
        .route("/metadata/schema", get(shared_providers::metadata_schema))
        .route("/metadata/{id}", get(shared_providers::metadata))
        .route("/autotagging", get(shared_providers::auto_tagging))
        .route("/autotagging/schema", get(shared_providers::auto_tagging_schema))
        .route("/delayprofile", get(shared_providers::delay_profiles))
        .route(
            "/delayprofile/{id}",
            get(shared_providers::delay_profile).put(crate::routes::settings_writes::delay_profile_update),
        )
        .route("/releaseprofile", get(shared_providers::release_profiles))
        .route("/releaseprofile/{id}", get(shared_providers::release_profile))
        .route("/remotepathmapping", get(shared_providers::remote_path_mappings))
        .route(
            "/remotepathmapping/{id}",
            get(shared_providers::remote_path_mapping),
        )
        .route("/customfilter", get(shared_providers::custom_filters))
        .route("/customfilter/{id}", get(shared_providers::custom_filter))
        .route("/release", get(release::list).post(release::grab))
        .route("/release/push", post(release::push))
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
