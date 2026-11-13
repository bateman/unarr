//! `GET /importlist/movie`: the movies the enabled lists named at their
//! last sync, which the UI's Discover page shows
//! (`ImportListMoviesController.GetDiscoverMovies`).
//!
//! What the page asks TMDB for directly — recommendations, trending and
//! popular movies — is not fetched: those flags answer as if they were
//! off, and the lists' own movies are all that comes back.

use axum::extract::State;
use axum::http::Uri;
use axum::Json;
use serde_json::{json, Map, Value};
use unarr_core::movies::model::MovieMetadata;
use unarr_core::movies::naming as movie_naming;
use unarr_core::movies::store as movies;
use unarr_core::store;

use crate::error::ApiResult;
use crate::query::Params;
use crate::state::AppState;

use super::catalog::language_resource;
use super::resources::{images_resource, Covers};

fn set(out: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        out.insert(key.to_string(), value);
    }
}

fn ts(t: Option<chrono::DateTime<chrono::Utc>>) -> Option<Value> {
    t.map(|t| json!(unarr_core::time::format_utc(t)))
}

pub async fn list(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let params = Params::from_uri(&uri);
    for flag in ["includeRecommendations", "includeTrending", "includePopular"] {
        if params.bool(flag, false) {
            tracing::debug!("{flag} is not served: Unarr asks the metadata server for no such list");
        }
    }
    let (rows, lists, excluded, in_library, naming, translations) = state
        .db()
        .call(|conn| {
            let lists: Vec<i64> =
                store::providers::list_providers(conn, unarr_core::model::ProviderKind::ImportList)?
                    .iter()
                    .filter(|p| p.attributes["enabled"].as_bool().unwrap_or(false))
                    .map(|p| p.id)
                    .collect();
            let rows = movies::list_movies_for(conn, &lists)?;
            let excluded: Vec<i64> = movies::list_import_exclusions(conn)?
                .iter()
                .map(|e| e.tmdb_id)
                .collect();
            let in_library = movies::all_movie_tmdb_ids(conn)?;
            let naming = movies::naming(conn)?;
            let language = store::config::config(conn, "movieinfolanguage")?
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let translations = movies::translations_in(conn, language)?;
            Ok((rows, lists, excluded, in_library, naming, translations))
        })
        .await?;
    let _ = lists;
    let mut out: Vec<Value> = Vec::new();
    let mut seen: Vec<i64> = Vec::new();
    for row in &rows {
        let md = &row.metadata;
        match seen.iter().position(|t| *t == md.tmdb_id) {
            Some(at) => {
                // One movie, the lists that named it.
                let names = out[at]["lists"].as_array().cloned().unwrap_or_default();
                let mut ids: Vec<i64> = names.iter().filter_map(Value::as_i64).collect();
                if !ids.contains(&row.import_list_id) {
                    ids.push(row.import_list_id);
                }
                out[at]["lists"] = json!(ids);
            }
            None => {
                seen.push(md.tmdb_id);
                out.push(resource(
                    md,
                    row.import_list_id,
                    excluded.contains(&md.tmdb_id),
                    in_library.contains(&md.tmdb_id),
                    &naming,
                    translations.get(&md.id),
                ));
            }
        }
    }
    Ok(Json(out))
}

fn resource(
    md: &MovieMetadata,
    list_id: i64,
    excluded: bool,
    existing: bool,
    naming: &unarr_core::movies::model::MovieNamingConfig,
    translation: Option<&unarr_core::movies::model::MovieTranslation>,
) -> Value {
    let translated = |pick: fn(&unarr_core::movies::model::MovieTranslation) -> &Option<String>| {
        translation
            .and_then(|t| pick(t).clone())
            .filter(|s| !s.trim().is_empty())
    };
    let mut out = Map::new();
    out.insert(
        "title".into(),
        json!(translated(|t| &t.title).unwrap_or_else(|| md.title.clone())),
    );
    set(&mut out, "sortTitle", md.sort_title.as_ref().map(|s| json!(s)));
    out.insert(
        "originalLanguage".into(),
        language_resource(md.original_language_id),
    );
    out.insert("status".into(), json!(md.status.as_str()));
    set(
        &mut out,
        "overview",
        translated(|t| &t.overview)
            .or_else(|| md.overview.clone())
            .map(|s| json!(s)),
    );
    set(&mut out, "inCinemas", ts(md.in_cinemas));
    set(&mut out, "physicalRelease", ts(md.physical_release));
    set(&mut out, "digitalRelease", ts(md.digital_release));
    out.insert(
        "images".into(),
        json!(images_resource(&md.images, 0, "", Covers::Remote)),
    );
    set(&mut out, "website", md.website.as_ref().map(|s| json!(s)));
    out.insert("year".into(), json!(md.year));
    set(
        &mut out,
        "youTubeTrailerId",
        md.youtube_trailer_id.as_ref().map(|s| json!(s)),
    );
    set(&mut out, "studio", md.studio.as_ref().map(|s| json!(s)));
    out.insert("runtime".into(), json!(md.runtime));
    set(&mut out, "imdbId", md.imdb_id.as_ref().map(|s| json!(s)));
    out.insert("tmdbId".into(), json!(md.tmdb_id));
    out.insert(
        "folder".into(),
        json!(movie_naming::movie_folder(
            &movie_naming::NamingMovie {
                metadata: md,
                translations: &[],
            },
            naming,
        )),
    );
    set(
        &mut out,
        "certification",
        md.certification.as_ref().map(|s| json!(s)),
    );
    out.insert(
        "popularity".into(),
        json!(super::resources::single_precision(md.popularity)),
    );
    out.insert("genres".into(), json!(md.genres));
    out.insert("ratings".into(), md.ratings.clone());
    if let Some(poster) = md
        .images
        .iter()
        .find(|i| i.cover_type.eq_ignore_ascii_case("poster"))
    {
        out.insert("remotePoster".into(), json!(poster.remote_url));
    }
    // `MovieCollection` as the resource carries it: the title and the id
    // the metadata row holds, the rest of the model unset. A movie that
    // belongs to no collection still carries the empty model.
    let mut collection = json!({
        "id": 0,
        "tmdbId": md.collection_tmdb_id,
        "images": [],
        "added": "0001-01-01T00:00:00Z",
        "minimumAvailability": "tba",
        "monitored": false,
        "qualityProfileId": 0,
        "searchOnAdd": false,
        "tags": [],
    });
    if let Some(title) = md.collection_title.as_ref().filter(|t| !t.trim().is_empty()) {
        collection["title"] = json!(title);
    }
    out.insert("collection".into(), collection);
    out.insert("isExcluded".into(), json!(excluded));
    out.insert("isExisting".into(), json!(existing));
    out.insert("isTrending".into(), json!(false));
    out.insert("isPopular".into(), json!(false));
    out.insert("isRecommendation".into(), json!(false));
    out.insert("lists".into(), json!([list_id]));
    Value::Object(out)
}
