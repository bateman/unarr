//! Radarr's resources: a movie, its file, and the pieces both are made of.
//! Each function mirrors the original's `ToResource`; a `null` field is
//! left out, as the originals' serialiser leaves it.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use unarr_core::model::{CustomFormat, Image, QualityModel, QualityProfile};
use unarr_core::movies::availability;
use unarr_core::movies::model::{AlternativeTitle, Movie, MovieFile, MovieTranslation};
use unarr_core::movies::quality::MovieQuality;
use unarr_core::movies::{formats as movie_formats, store as movies};
use unarr_core::store;
use unarr_core::time;

use super::catalog::language_resource;
use crate::resources::best_root_folder_path;
use crate::state::AppState;

/// `QualityModel`: the quality with its revision.
pub fn quality_resource(q: &QualityModel) -> Value {
    json!({
        "quality": MovieQuality::by_id(q.quality_id),
        "revision": {"version": q.version, "real": q.real, "isRepack": q.is_repack},
    })
}

pub fn languages_resource(ids: &[i64]) -> Vec<Value> {
    ids.iter().map(|id| language_resource(*id)).collect()
}

pub fn formats_resource(formats: &[CustomFormat]) -> Vec<Value> {
    formats
        .iter()
        .map(|f| json!({"id": f.id, "name": f.name}))
        .collect()
}

/// The first twenty hex digits of the SHA-256 of a cover's remote URL: what
/// Radarr appends to a local cover's address so that a changed cover is
/// fetched again.
fn cover_hash(remote_url: &str) -> String {
    let digest = Sha256::digest(remote_url.as_bytes());
    digest.iter().take(10).map(|b| format!("{b:02x}")).collect()
}

/// How a movie's covers are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Covers {
    /// With the address Unarr serves them at (`MediaCoverService.ConvertToLocalUrls`).
    Local,
    /// As the metadata server names them, with the address a cover
    /// already carries: a movie object the original gave local addresses
    /// to (`ConvertToLocalUrls`) keeps them wherever it is shown.
    Remote,
}

pub fn images_resource(images: &[Image], movie_id: i64, url_base: &str, covers: Covers) -> Vec<Value> {
    images
        .iter()
        .map(|i| {
            let mut out = json!({"coverType": i.cover_type});
            if let (Covers::Remote, Some(url)) = (covers, &i.url) {
                out["url"] = json!(url);
            }
            if let Some(remote) = &i.remote_url {
                out["remoteUrl"] = json!(remote);
                if covers == Covers::Local {
                    let extension = if i.cover_type.eq_ignore_ascii_case("clearlogo") {
                        ".png"
                    } else {
                        ".jpg"
                    };
                    out["url"] = json!(format!(
                        "{url_base}/MediaCover/{movie_id}/{}{extension}?h={}",
                        i.cover_type.to_lowercase(),
                        cover_hash(remote)
                    ));
                }
            }
            out
        })
        .collect()
}

pub fn alternative_title_resource(t: &AlternativeTitle) -> Value {
    json!({
        "id": t.id,
        "movieMetadataId": t.movie_metadata_id,
        "title": t.title,
        "sourceType": t.source_type,
    })
}

/// A number the originals keep in single precision, as they print it: the
/// shortest decimal that reads back to the same `float`.
pub(crate) fn single_precision(value: f64) -> f64 {
    (value as f32).to_string().parse().unwrap_or(value)
}

fn set(out: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(v) = value {
        out.insert(key.to_string(), v);
    }
}

fn ts(t: Option<DateTime<Utc>>) -> Option<Value> {
    t.map(|t| json!(time::format_utc(t)))
}

/// What every movie resource of one answer is built from.
pub struct MovieContext {
    pub url_base: String,
    pub roots: Vec<String>,
    pub profiles: Vec<QualityProfile>,
    pub formats: Vec<CustomFormat>,
    /// Titles and overviews in the language Settings > UI asks for.
    pub translations: HashMap<i64, MovieTranslation>,
    pub now: DateTime<Utc>,
    /// `AvailabilityDelay` of the indexer settings, in days.
    pub availability_delay: i64,
}

impl MovieContext {
    pub async fn load(state: &AppState) -> anyhow::Result<MovieContext> {
        let url_base = state.settings().url_base.clone();
        let now = state.now();
        state
            .db()
            .call(move |conn| {
                let config = |key: &str, default: i64| -> anyhow::Result<i64> {
                    Ok(store::config::config(conn, key)?
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(default))
                };
                Ok(MovieContext {
                    url_base,
                    roots: store::catalog::list_root_folders(conn)?
                        .into_iter()
                        .map(|r| r.path)
                        .collect(),
                    profiles: store::catalog::list_quality_profiles(conn)?,
                    formats: store::catalog::list_custom_formats(conn)?,
                    translations: movies::translations_in(conn, config("movieinfolanguage", 1)?)?,
                    now,
                    availability_delay: config("availabilitydelay", 0)?,
                })
            })
            .await
    }

    pub fn profile(&self, id: i64) -> Option<&QualityProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }
}

/// `MovieFileResource`. `with_formats` adds the custom formats and their
/// score, which the file shown inside a movie leaves out.
pub fn movie_file_resource(ctx: &MovieContext, file: &MovieFile, movie: &Movie, with_formats: bool) -> Value {
    let mut out = Map::new();
    let profile = ctx.profile(movie.quality_profile_id);
    out.insert("id".into(), json!(file.id));
    out.insert("movieId".into(), json!(file.movie_id));
    out.insert("relativePath".into(), json!(file.relative_path));
    out.insert(
        "path".into(),
        json!(format!(
            "{}/{}",
            movie.path.trim_end_matches('/'),
            file.relative_path
        )),
    );
    out.insert("size".into(), json!(file.size));
    out.insert("dateAdded".into(), json!(time::format_utc(file.date_added)));
    set(&mut out, "sceneName", file.scene_name.as_ref().map(|s| json!(s)));
    set(
        &mut out,
        "releaseGroup",
        file.release_group.as_ref().map(|s| json!(s)),
    );
    set(&mut out, "edition", file.edition.as_ref().map(|s| json!(s)));
    set(
        &mut out,
        "originalFilePath",
        file.original_file_path.as_ref().map(|s| json!(s)),
    );
    out.insert("indexerFlags".into(), json!(file.indexer_flags));
    out.insert("quality".into(), quality_resource(&file.quality));
    out.insert("languages".into(), json!(languages_resource(&file.languages)));
    set(
        &mut out,
        "mediaInfo",
        crate::mediainfo::resource(file.media_info.as_ref(), file.scene_name.as_deref())
            .and_then(|m| serde_json::to_value(m).ok()),
    );
    out.insert(
        "qualityCutoffNotMet".into(),
        json!(
            profile.is_some_and(|p| unarr_core::profile::quality_cutoff_not_met(p, file.quality.quality_id))
        ),
    );
    if with_formats {
        let matched = movie_formats::file_formats(&ctx.formats, file, movie);
        let ids: Vec<i64> = matched.iter().map(|f| f.id).collect();
        out.insert("customFormats".into(), json!(formats_resource(&matched)));
        out.insert(
            "customFormatScore".into(),
            json!(profile
                .map(|p| unarr_core::profile::custom_format_score(p, &ids))
                .unwrap_or(0)),
        );
    }
    Value::Object(out)
}

/// What a movie resource carries beyond the movie itself.
pub struct MovieParts<'a> {
    pub alternative_titles: &'a [AlternativeTitle],
    /// The movie's file, when it has one.
    pub file: Option<&'a MovieFile>,
    pub covers: Covers,
    /// `rootFolderPath`, which only the movie endpoints show.
    pub root_folder: bool,
    /// `hasFile`, `sizeOnDisk` and `statistics`: the movie endpoints, the
    /// calendar and the wanted lists.
    pub statistics: bool,
    /// Whether and how the file itself is shown.
    pub movie_file: FileShown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileShown {
    No,
    /// As the movie endpoints show it.
    Plain,
    /// With its custom formats and their score, as the calendar does.
    WithFormats,
}

/// `MovieResource`.
pub fn movie_resource(ctx: &MovieContext, movie: &Movie, parts: &MovieParts) -> Value {
    let md = &movie.metadata;
    let translation = ctx.translations.get(&md.id);
    let translated = |pick: fn(&MovieTranslation) -> &Option<String>| {
        translation
            .and_then(|t| pick(t).clone())
            .filter(|s| !s.trim().is_empty())
    };
    let mut out = Map::new();
    out.insert("id".into(), json!(movie.id));
    out.insert(
        "title".into(),
        json!(translated(|t| &t.title).unwrap_or_else(|| md.title.clone())),
    );
    set(
        &mut out,
        "originalTitle",
        md.original_title.as_ref().map(|s| json!(s)),
    );
    out.insert(
        "originalLanguage".into(),
        language_resource(md.original_language_id),
    );
    out.insert(
        "alternateTitles".into(),
        json!(parts
            .alternative_titles
            .iter()
            .map(alternative_title_resource)
            .collect::<Vec<_>>()),
    );
    set(&mut out, "secondaryYear", md.secondary_year.map(|y| json!(y)));
    out.insert("secondaryYearSourceId".into(), json!(0));
    set(&mut out, "sortTitle", md.sort_title.as_ref().map(|s| json!(s)));
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
    set(&mut out, "releaseDate", ts(availability::release_date(movie)));
    out.insert(
        "images".into(),
        json!(images_resource(&md.images, movie.id, &ctx.url_base, parts.covers)),
    );
    set(&mut out, "website", md.website.as_ref().map(|s| json!(s)));
    out.insert("year".into(), json!(md.year));
    set(
        &mut out,
        "youTubeTrailerId",
        md.youtube_trailer_id.as_ref().map(|s| json!(s)),
    );
    set(&mut out, "studio", md.studio.as_ref().map(|s| json!(s)));
    out.insert("path".into(), json!(movie.path));
    out.insert("qualityProfileId".into(), json!(movie.quality_profile_id));
    out.insert("movieFileId".into(), json!(movie.movie_file_id));
    out.insert("monitored".into(), json!(movie.monitored));
    out.insert(
        "minimumAvailability".into(),
        json!(movie.minimum_availability.as_str()),
    );
    out.insert(
        "isAvailable".into(),
        json!(availability::is_available(movie, ctx.now, ctx.availability_delay)),
    );
    out.insert("folderName".into(), json!(movie.path));
    out.insert("runtime".into(), json!(md.runtime));
    set(&mut out, "cleanTitle", md.clean_title.as_ref().map(|s| json!(s)));
    set(&mut out, "imdbId", md.imdb_id.as_ref().map(|s| json!(s)));
    out.insert("tmdbId".into(), json!(md.tmdb_id));
    out.insert("titleSlug".into(), json!(md.tmdb_id.to_string()));
    set(
        &mut out,
        "certification",
        md.certification.as_ref().map(|s| json!(s)),
    );
    out.insert("genres".into(), json!(md.genres));
    out.insert("keywords".into(), json!(md.keywords));
    out.insert("tags".into(), json!(movie.tags));
    set(&mut out, "added", ts(movie.added));
    set(&mut out, "addOptions", movie.add_options.clone());
    out.insert("ratings".into(), md.ratings.clone());
    // A single-precision number in the originals: its shortest spelling.
    out.insert("popularity".into(), json!(single_precision(md.popularity)));
    set(&mut out, "lastSearchTime", ts(movie.last_search_time));
    if md.collection_tmdb_id > 0 {
        out.insert(
            "collection".into(),
            json!({"title": md.collection_title, "tmdbId": md.collection_tmdb_id}),
        );
    }
    if parts.root_folder {
        out.insert(
            "rootFolderPath".into(),
            json!(best_root_folder_path(&movie.path, &ctx.roots)),
        );
    }
    if parts.statistics {
        let size = parts.file.map(|f| f.size).unwrap_or(0);
        out.insert("hasFile".into(), json!(parts.file.is_some()));
        out.insert("sizeOnDisk".into(), json!(size));
        out.insert(
            "statistics".into(),
            json!({
                "movieFileCount": usize::from(parts.file.is_some()),
                "sizeOnDisk": size,
                "releaseGroups": parts
                    .file
                    .and_then(|f| f.release_group.clone())
                    .filter(|g| !g.is_empty())
                    .into_iter()
                    .collect::<Vec<_>>(),
                "movieFileQualities": parts
                    .file
                    .map(|f| MovieQuality::by_id(f.quality.quality_id))
                    .into_iter()
                    .collect::<Vec<_>>(),
            }),
        );
    }
    if let (Some(file), true) = (parts.file, parts.movie_file != FileShown::No) {
        out.insert(
            "movieFile".into(),
            movie_file_resource(ctx, file, movie, parts.movie_file == FileShown::WithFormats),
        );
    }
    Value::Object(out)
}

/// The movie embedded in a history row, a blocklist entry or a wanted
/// record: without its alternative titles, files and local covers.
pub fn embedded_movie_resource(ctx: &MovieContext, movie: &Movie) -> Value {
    movie_resource(
        ctx,
        movie,
        &MovieParts {
            alternative_titles: &[],
            file: None,
            covers: Covers::Remote,
            root_folder: false,
            statistics: false,
            movie_file: FileShown::No,
        },
    )
}
