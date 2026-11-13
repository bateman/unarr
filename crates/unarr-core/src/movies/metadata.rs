//! Where movie metadata comes from: Radarr's `SkyHookProxy`, which asks the
//! Servarr team's `api.radarr.video` (`docs/DESIGN.md` section 7, option
//! A), behind [`MovieMetadataProvider`]. The trait's output is the
//! original's own model (a `MovieMetadata` with its alternative titles,
//! translations and credits), so that a provider built on other sources
//! stays additive. It sits next to the series trait of `crate::metadata`
//! and shares its [`Upstreams`]; the two differ where the originals do.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Timelike, Utc};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::metadata::{BoxFuture, Upstreams};
use crate::model::Image;
use crate::movies::model::{AlternativeTitle, Credit, MovieMetadata, MovieStatus, MovieTranslation};
use crate::movies::naming::language_by_iso_code;
use crate::movies::parser::{clean_movie_title, normalize_title};

/// `RadarrCloudRequestBuilder.RadarrMetadata`.
const RADARR_METADATA: &str = "https://api.radarr.video/v1";

/// English, the language a movie is given when its own is unknown.
const ENGLISH: i64 = 1;

/// A movie as the provider knows it. Ids are 0: the caller decides whether
/// these become rows or update existing ones.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieInfo {
    pub metadata: MovieMetadata,
    pub alternative_titles: Vec<AlternativeTitle>,
    /// A translation in a language the originals have no row for carries
    /// `language_id` 0 and is dropped when the translations are stored.
    pub translations: Vec<MovieTranslation>,
    pub credits: Vec<Credit>,
}

/// `MovieCollection` as `GetCollectionInfo` maps it, with its parts.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionInfo {
    pub tmdb_id: i64,
    pub title: String,
    pub clean_title: String,
    pub sort_title: String,
    pub overview: Option<String>,
    pub images: Vec<Image>,
    pub movies: Vec<MovieInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MovieMetadataError {
    /// `MovieNotFoundException`: the provider answered 404 for this TMDB
    /// id.
    MovieNotFound(String),
    /// The same for an IMDb id, which the original words differently.
    ImdbNotFound(String),
    /// The provider could not be reached or answered with an error status
    /// (`HttpException`, `WebException`).
    Unavailable(String),
    /// The provider answered something that does not parse.
    Invalid(String),
}

impl std::fmt::Display for MovieMetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MovieMetadataError::MovieNotFound(id) => write!(
                f,
                "Movie with tmdbId {id} was not found, it may have been removed from TMDb."
            ),
            MovieMetadataError::ImdbNotFound(id) => write!(
                f,
                "Movie with IMDBId {id} was not found, it may have been removed from TMDb."
            ),
            MovieMetadataError::Unavailable(m) | MovieMetadataError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for MovieMetadataError {}

/// What the mapping reads besides the answer: the clock a movie's status
/// is computed against, and the country whose certification is kept
/// (`CertificationCountry` of the metadata settings, upper case).
#[derive(Debug, Clone)]
pub struct MapContext {
    pub now: DateTime<Utc>,
    pub certification_country: String,
}

impl MapContext {
    pub fn new(now: DateTime<Utc>, certification_country: &str) -> MapContext {
        MapContext {
            now,
            certification_country: certification_country.to_uppercase(),
        }
    }
}

type Answer<T> = Result<T, MovieMetadataError>;

/// `IProvideMovieInfo` and the provider half of `ISearchForNewMovie`.
pub trait MovieMetadataProvider: Send + Sync {
    /// `GetMovieInfo`: the movie with its credits.
    fn movie_info<'a>(&'a self, tmdb_id: i64, ctx: &'a MapContext) -> BoxFuture<'a, Answer<MovieInfo>>;

    /// `GetMovieByImdbId`: the first movie the provider lists for the id,
    /// `None` when it lists none.
    fn movie_by_imdb_id<'a>(
        &'a self,
        imdb_id: &'a str,
        ctx: &'a MapContext,
    ) -> BoxFuture<'a, Answer<Option<MovieInfo>>>;

    /// `GetCollectionInfo`.
    fn collection_info<'a>(
        &'a self,
        tmdb_id: i64,
        ctx: &'a MapContext,
    ) -> BoxFuture<'a, Answer<CollectionInfo>>;

    /// The provider's search for a title, already in the form
    /// `SearchForNewMovie` sends (`the+holdovers`), and a year or `""`.
    fn search<'a>(
        &'a self,
        q: &'a str,
        year: &'a str,
        ctx: &'a MapContext,
    ) -> BoxFuture<'a, Answer<Vec<MovieInfo>>>;

    /// `GetChangedMovies`: the TMDB ids that changed since a moment.
    fn changed_movies(&self, since: DateTime<Utc>) -> BoxFuture<'_, Answer<HashSet<i64>>>;

    /// The movies of a list the provider resolves itself (`list/imdb/<id>`
    /// for an IMDb watchlist), as `RadarrList2Parser` reads them.
    fn list<'a>(&'a self, route: &'a str, ctx: &'a MapContext) -> BoxFuture<'a, Answer<Vec<MovieInfo>>>;
}

pub type SharedMovieProvider = std::sync::Arc<dyn MovieMetadataProvider>;

/// `api.radarr.video`, through the personality's HTTP client, with the
/// original's user agent.
pub struct RadarrApi {
    client: reqwest::Client,
    upstreams: Upstreams,
}

impl RadarrApi {
    pub fn new(client: reqwest::Client, upstreams: Upstreams) -> RadarrApi {
        RadarrApi { client, upstreams }
    }

    async fn get(&self, url: &str) -> Answer<(reqwest::StatusCode, Vec<u8>)> {
        let response = self
            .client
            .get(self.upstreams.url(url))
            .header(
                reqwest::header::USER_AGENT,
                crate::http::servarr_proxy_user_agent(crate::Personality::Radarr),
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| MovieMetadataError::Unavailable(transport_message(&e)))?;
        let status = response.status();
        let body = crate::http::read_body(response, crate::http::MAX_RESPONSE_BYTES)
            .await
            .map_err(|e| MovieMetadataError::Unavailable(e.to_string()))?;
        Ok((status, body))
    }

    /// A resource by id: 404 is `MovieNotFoundException`, any other error
    /// status the original's `HttpException`.
    async fn resource<T: for<'de> Deserialize<'de>>(&self, url: &str, id: &str) -> Answer<T> {
        let (status, body) = self.get(url).await?;
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(MovieMetadataError::MovieNotFound(id.to_string()));
        }
        if !status.is_success() {
            return Err(http_error(status, url));
        }
        serde_json::from_slice(&body).map_err(|e| MovieMetadataError::Invalid(e.to_string()))
    }
}

/// `HttpStatusCode` as .NET names it: the reason phrase without spaces.
fn status_name(status: reqwest::StatusCode) -> String {
    status
        .canonical_reason()
        .map(|reason| reason.replace(' ', ""))
        .unwrap_or_else(|| status.as_u16().to_string())
}

fn http_error(status: reqwest::StatusCode, url: &str) -> MovieMetadataError {
    MovieMetadataError::Unavailable(format!(
        "HTTP request failed: [{}:{}] [GET] at [{url}]",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    ))
}

/// A transport error's innermost cause: the outer text names the URL.
fn transport_message(e: &reqwest::Error) -> String {
    let mut cause: &dyn std::error::Error = e;
    while let Some(next) = cause.source() {
        cause = next;
    }
    cause.to_string()
}

impl MovieMetadataProvider for RadarrApi {
    fn movie_info<'a>(&'a self, tmdb_id: i64, ctx: &'a MapContext) -> BoxFuture<'a, Answer<MovieInfo>> {
        Box::pin(async move {
            let url = format!("{RADARR_METADATA}/movie/{tmdb_id}");
            let resource: MovieResource = self.resource(&url, &tmdb_id.to_string()).await?;
            Ok(map_movie(resource, ctx, true))
        })
    }

    fn movie_by_imdb_id<'a>(
        &'a self,
        imdb_id: &'a str,
        ctx: &'a MapContext,
    ) -> BoxFuture<'a, Answer<Option<MovieInfo>>> {
        Box::pin(async move {
            let url = format!("{RADARR_METADATA}/movie/imdb/{imdb_id}");
            let resources: Vec<MovieResource> = self.resource(&url, imdb_id).await.map_err(|e| match e {
                MovieMetadataError::MovieNotFound(id) => MovieMetadataError::ImdbNotFound(id),
                other => other,
            })?;
            Ok(resources.into_iter().next().map(|r| map_movie(r, ctx, false)))
        })
    }

    fn collection_info<'a>(
        &'a self,
        tmdb_id: i64,
        ctx: &'a MapContext,
    ) -> BoxFuture<'a, Answer<CollectionInfo>> {
        Box::pin(async move {
            let url = format!("{RADARR_METADATA}/movie/collection/{tmdb_id}");
            let resource: CollectionResource = self.resource(&url, &tmdb_id.to_string()).await?;
            let title = resource.name.unwrap_or_default();
            Ok(CollectionInfo {
                tmdb_id: resource.tmdb_id,
                clean_title: clean_movie_title(&title),
                sort_title: normalize_title(&title),
                title,
                overview: resource.overview,
                images: resource
                    .images
                    .unwrap_or_default()
                    .into_iter()
                    .map(map_image)
                    .collect(),
                movies: resource
                    .parts
                    .unwrap_or_default()
                    .into_iter()
                    .map(|part| map_movie(part, ctx, false))
                    .collect(),
            })
        })
    }

    fn search<'a>(
        &'a self,
        q: &'a str,
        year: &'a str,
        ctx: &'a MapContext,
    ) -> BoxFuture<'a, Answer<Vec<MovieInfo>>> {
        Box::pin(async move {
            // The term already carries `+` for its spaces, which the
            // original's builder leaves as they are.
            let url = format!(
                "{RADARR_METADATA}/search?q={}&year={}",
                crate::indexers::request::escape_data_string(q).replace("%2B", "+"),
                crate::indexers::request::escape_data_string(year)
            );
            let (status, body) = self.get(&url).await?;
            if !status.is_success() {
                return Err(http_error(status, &url));
            }
            let resources: Vec<MovieResource> =
                serde_json::from_slice(&body).map_err(|e| MovieMetadataError::Invalid(e.to_string()))?;
            Ok(resources.into_iter().map(|r| map_movie(r, ctx, false)).collect())
        })
    }

    fn changed_movies(&self, since: DateTime<Utc>) -> BoxFuture<'_, Answer<HashSet<i64>>> {
        Box::pin(async move {
            // Rounded down to the hour, a quarter of an hour early, so that
            // the provider's cache serves every installation the same.
            let adjusted = since - Duration::minutes(15);
            let start = adjusted
                .with_minute(0)
                .and_then(|t| t.with_second(0))
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(adjusted)
                .format("%Y-%m-%dT%H:%M:%S");
            let url = format!(
                "{RADARR_METADATA}/movie/changed?since={}",
                crate::indexers::request::escape_data_string(&start.to_string())
            );
            let (_, body) = self.get(&url).await?;
            let ids: Vec<i64> =
                serde_json::from_slice(&body).map_err(|e| MovieMetadataError::Invalid(e.to_string()))?;
            Ok(ids.into_iter().collect())
        })
    }

    fn list<'a>(&'a self, route: &'a str, ctx: &'a MapContext) -> BoxFuture<'a, Answer<Vec<MovieInfo>>> {
        Box::pin(async move {
            let url = format!("{RADARR_METADATA}/{route}");
            let (status, body) = self.get(&url).await?;
            if !status.is_success() {
                // `RadarrList2Parser.PreProcess`: a list request suppresses
                // the HTTP error, and the parser names the status itself.
                return Err(MovieMetadataError::Unavailable(format!(
                    "Radarr API call resulted in an unexpected StatusCode [{}]",
                    status_name(status)
                )));
            }
            let resources: Vec<MovieResource> =
                serde_json::from_slice(&body).map_err(|e| MovieMetadataError::Invalid(e.to_string()))?;
            Ok(resources.into_iter().map(|r| map_movie(r, ctx, false)).collect())
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MovieResource {
    #[serde(default)]
    tmdb_id: i64,
    imdb_id: Option<String>,
    overview: Option<String>,
    title: Option<String>,
    original_title: Option<String>,
    /// Deprecated in the original, read when `MovieRatings` has no TMDB
    /// rating.
    #[serde(default)]
    ratings: Option<Vec<RatingItem>>,
    movie_ratings: Option<RatingResource>,
    runtime: Option<i64>,
    #[serde(default)]
    images: Option<Vec<ImageResource>>,
    genres: Option<Vec<String>>,
    keywords: Option<Vec<String>>,
    #[serde(default)]
    year: i64,
    premier: Option<DateTime<Utc>>,
    in_cinema: Option<DateTime<Utc>>,
    physical_release: Option<DateTime<Utc>>,
    digital_release: Option<DateTime<Utc>>,
    #[serde(default)]
    alternative_titles: Option<Vec<AlternativeTitleResource>>,
    #[serde(default)]
    translations: Option<Vec<TranslationResource>>,
    credits: Option<CreditsResource>,
    studio: Option<String>,
    youtube_trailer_id: Option<String>,
    #[serde(default)]
    certifications: Option<Vec<CertificationResource>>,
    collection: Option<CollectionResource>,
    original_language: Option<String>,
    homepage: Option<String>,
    recommendations: Option<Vec<RecommendationResource>>,
    popularity: Option<f32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RatingItem {
    #[serde(default)]
    count: i64,
    #[serde(default)]
    value: Value,
    r#type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RatingResource {
    tmdb: Option<RatingItem>,
    imdb: Option<RatingItem>,
    metacritic: Option<RatingItem>,
    rotten_tomatoes: Option<RatingItem>,
    trakt: Option<RatingItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ImageResource {
    cover_type: Option<String>,
    url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AlternativeTitleResource {
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TranslationResource {
    title: Option<String>,
    overview: Option<String>,
    language: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CreditsResource {
    #[serde(default)]
    cast: Vec<CreditResource>,
    #[serde(default)]
    crew: Vec<CreditResource>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CreditResource {
    name: Option<String>,
    #[serde(default)]
    order: i64,
    character: Option<String>,
    job: Option<String>,
    department: Option<String>,
    #[serde(default)]
    tmdb_id: i64,
    credit_id: Option<String>,
    #[serde(default)]
    images: Option<Vec<ImageResource>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CertificationResource {
    country: Option<String>,
    certification: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CollectionResource {
    #[serde(default)]
    tmdb_id: i64,
    name: Option<String>,
    overview: Option<String>,
    #[serde(default)]
    images: Option<Vec<ImageResource>>,
    #[serde(default)]
    parts: Option<Vec<MovieResource>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RecommendationResource {
    #[serde(default)]
    tmdb_id: i64,
}

/// `MapCoverType`, in the spelling the images are stored and shown in.
fn map_image(image: ImageResource) -> Image {
    let cover_type = match image.cover_type.as_deref().unwrap_or("").to_lowercase().as_str() {
        "poster" => "poster",
        "headshot" => "headshot",
        "fanart" => "fanart",
        "clearlogo" => "clearlogo",
        _ => "unknown",
    };
    Image {
        cover_type: cover_type.into(),
        remote_url: image.url,
        url: None,
    }
}

/// A `RatingChild`: `votes`, `value`, `type` in that order, the type in
/// the enumeration's spelling.
fn map_rating(item: &RatingItem) -> Value {
    let kind = match item.r#type.as_deref().unwrap_or("").to_lowercase().as_str() {
        "critic" => "critic",
        _ => "user",
    };
    json!({ "votes": item.count, "value": item.value, "type": kind })
}

/// `MapRatings`, and the fallback to the deprecated list for TMDB's.
fn map_ratings(resource: &MovieResource) -> Value {
    let mut ratings = Map::new();
    if let Some(all) = &resource.movie_ratings {
        for (name, item) in [
            ("imdb", &all.imdb),
            ("tmdb", &all.tmdb),
            ("metacritic", &all.metacritic),
            ("rottenTomatoes", &all.rotten_tomatoes),
            ("trakt", &all.trakt),
        ] {
            if let Some(item) = item {
                ratings.insert(name.into(), map_rating(item));
            }
        }
    }
    if !ratings.contains_key("tmdb") {
        if let Some(first) = resource.ratings.as_ref().and_then(|all| all.first()) {
            ratings.insert(
                "tmdb".into(),
                json!({ "votes": first.count, "value": first.value, "type": "user" }),
            );
        }
    }
    Value::Object(ratings)
}

/// `MovieTitleNormalizer.Normalize`: the title to sort by, with the few
/// titles the normaliser would reduce to nothing spelled out.
pub fn sort_title(title: &str, tmdb_id: i64) -> String {
    match tmdb_id {
        387354 | 1212922 => "a to z".into(),
        888700 => "a to z the first alphabet".into(),
        101273 => "a to zeppelin the story of led zeppelin".into(),
        _ => normalize_title(title).to_lowercase(),
    }
}

/// How far a movie's release has come at `now`, as `MapMovie` computes it.
pub fn status_at(
    in_cinemas: Option<DateTime<Utc>>,
    physical_release: Option<DateTime<Utc>>,
    digital_release: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> MovieStatus {
    let mut status = MovieStatus::Announced;
    if let Some(in_cinemas) = in_cinemas.filter(|at| now > *at) {
        status = MovieStatus::InCinemas;
        if physical_release.is_none() && digital_release.is_none() && now > in_cinemas + Duration::days(90) {
            status = MovieStatus::Released;
        }
    }
    if physical_release.is_some_and(|at| now >= at) || digital_release.is_some_and(|at| now >= at) {
        status = MovieStatus::Released;
    }
    status
}

/// `MapMovie`, with `MapCast` and `MapCrew` when the credits are wanted.
fn map_movie(resource: MovieResource, ctx: &MapContext, with_credits: bool) -> MovieInfo {
    let ratings = map_ratings(&resource);
    let title = resource.title.unwrap_or_default();
    let original_title = resource.original_title;
    let alternative_titles = resource
        .alternative_titles
        .unwrap_or_default()
        .into_iter()
        .map(|t| {
            let title = t.title.unwrap_or_default();
            AlternativeTitle {
                id: 0,
                movie_metadata_id: 0,
                clean_title: Some(clean_movie_title(&title)),
                title,
                source_type: "tmdb".into(),
            }
        })
        .collect();
    let translations = resource
        .translations
        .unwrap_or_default()
        .into_iter()
        .map(|t| MovieTranslation {
            id: 0,
            movie_metadata_id: 0,
            language_id: t
                .language
                .as_deref()
                .and_then(|code| language_by_iso_code(&code.to_lowercase()))
                .unwrap_or(0),
            clean_title: t.title.as_deref().map(clean_movie_title),
            title: t.title,
            overview: t.overview,
        })
        .collect();
    let mut credits = Vec::new();
    if with_credits {
        if let Some(all) = resource.credits {
            for (kind, list) in [("cast", all.cast), ("crew", all.crew)] {
                credits.extend(list.into_iter().map(|c| Credit {
                    id: 0,
                    movie_metadata_id: 0,
                    credit_tmdb_id: c.credit_id,
                    person_tmdb_id: c.tmdb_id,
                    name: c.name,
                    images: c.images.unwrap_or_default().into_iter().map(map_image).collect(),
                    character: if kind == "cast" { c.character } else { None },
                    order: c.order,
                    job: if kind == "crew" { c.job } else { None },
                    department: if kind == "crew" { c.department } else { None },
                    credit_type: kind.into(),
                }));
            }
        }
    }
    // The premier's year, when it is not the TMDB year, is a second year a
    // release may carry.
    let secondary_year = resource
        .premier
        .map(|p| i64::from(chrono::Datelike::year(&p)))
        .filter(|year| *year != resource.year);
    let certification = resource
        .certifications
        .unwrap_or_default()
        .into_iter()
        .find(|c| c.country.as_deref() == Some(ctx.certification_country.as_str()))
        .and_then(|c| c.certification);
    let (collection_tmdb_id, collection_title) = match resource.collection {
        Some(collection) => (collection.tmdb_id, collection.name),
        None => (0, None),
    };
    let metadata = MovieMetadata {
        id: 0,
        tmdb_id: resource.tmdb_id,
        imdb_id: resource.imdb_id,
        sort_title: Some(sort_title(&title, resource.tmdb_id)),
        clean_title: Some(clean_movie_title(&title)),
        clean_original_title: original_title.as_deref().map(clean_movie_title),
        original_title,
        original_language_id: resource
            .original_language
            .as_deref()
            .and_then(|code| language_by_iso_code(&code.to_lowercase()))
            .unwrap_or(ENGLISH),
        status: status_at(
            resource.in_cinema,
            resource.physical_release,
            resource.digital_release,
            ctx.now,
        ),
        overview: resource.overview,
        images: resource
            .images
            .unwrap_or_default()
            .into_iter()
            .map(map_image)
            .collect(),
        genres: resource.genres.unwrap_or_default(),
        keywords: resource.keywords.unwrap_or_default(),
        ratings,
        recommendations: resource
            .recommendations
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.tmdb_id)
            .collect(),
        runtime: resource.runtime.unwrap_or(0),
        in_cinemas: resource.in_cinema,
        physical_release: resource.physical_release,
        digital_release: resource.digital_release,
        year: resource.year,
        secondary_year,
        certification,
        youtube_trailer_id: resource.youtube_trailer_id,
        studio: resource.studio,
        website: resource.homepage,
        // A single-precision number in the original, kept as it prints.
        popularity: f64::from(resource.popularity.unwrap_or(0.0)),
        collection_tmdb_id,
        collection_title,
        last_info_sync: None,
        title,
    };
    MovieInfo {
        metadata,
        alternative_titles,
        translations,
        credits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/metadata/api.radarr.video/v1")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn ctx(now: &str) -> MapContext {
        MapContext::new(now.parse().unwrap(), "us")
    }

    #[test]
    fn maps_a_recorded_movie_as_the_original_stores_it() {
        let resource: MovieResource = serde_json::from_slice(&fixture("movie/840430.json")).unwrap();
        let info = map_movie(resource, &ctx("2026-11-13T00:00:00Z"), true);
        let m = &info.metadata;
        assert_eq!(m.tmdb_id, 840430);
        assert_eq!(m.imdb_id.as_deref(), Some("tt14849194"));
        assert_eq!(m.title, "The Holdovers");
        assert_eq!(m.clean_title.as_deref(), Some("theholdovers"));
        assert_eq!(m.sort_title.as_deref(), Some("holdovers"));
        assert_eq!(m.original_language_id, 1);
        assert_eq!(m.status, MovieStatus::Released);
        assert_eq!(m.year, 2023);
        // The premier and the TMDB year agree.
        assert_eq!(m.secondary_year, None);
        assert_eq!(m.certification.as_deref(), Some("R"));
        assert_eq!(m.studio.as_deref(), Some("Miramax"));
        assert_eq!(m.collection_tmdb_id, 0);
        assert_eq!(m.images[0].cover_type, "poster");
        assert_eq!(m.images[1].cover_type, "fanart");
        assert_eq!(m.ratings["imdb"]["type"], "user");
        assert!(m.ratings["tmdb"]["votes"].as_i64().unwrap() > 0);
        assert_eq!(m.recommendations.len(), 10);
        assert!(info
            .alternative_titles
            .iter()
            .any(|t| t.title == "Barton Academy"));
        assert!(info.alternative_titles.iter().all(|t| t.source_type == "tmdb"));
        // `en-US` is English, the first translation.
        assert_eq!(info.translations[0].language_id, 1);
        assert!(info.credits.iter().any(|c| c.credit_type == "cast"
            && c.name.as_deref() == Some("Paul Giamatti")
            && c.character.as_deref() == Some("Paul Hunham")));
        assert!(info.credits.iter().any(|c| c.credit_type == "crew"
            && c.job.as_deref() == Some("Director")
            && c.character.is_none()));
    }

    #[test]
    fn status_follows_the_clock() {
        let at = |s: &str| Some(s.parse::<DateTime<Utc>>().unwrap());
        let now = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        let cinema = at("2026-01-01T00:00:00Z");
        assert_eq!(
            status_at(None, None, None, now("2026-06-01T00:00:00Z")),
            MovieStatus::Announced
        );
        assert_eq!(
            status_at(cinema, None, None, now("2025-12-31T00:00:00Z")),
            MovieStatus::Announced
        );
        assert_eq!(
            status_at(cinema, None, None, now("2026-02-01T00:00:00Z")),
            MovieStatus::InCinemas
        );
        // Ninety days in cinemas with no other date count as released.
        assert_eq!(
            status_at(cinema, None, None, now("2026-04-02T00:00:00Z")),
            MovieStatus::Released
        );
        // With a later date known, the cinemas alone never release it.
        let digital = at("2026-09-01T00:00:00Z");
        assert_eq!(
            status_at(cinema, None, digital, now("2026-06-01T00:00:00Z")),
            MovieStatus::InCinemas
        );
        assert_eq!(
            status_at(cinema, None, digital, now("2026-09-01T00:00:00Z")),
            MovieStatus::Released
        );
        assert_eq!(
            status_at(None, digital, None, now("2026-09-02T00:00:00Z")),
            MovieStatus::Released
        );
    }

    #[test]
    fn precomputed_sort_titles_are_the_originals() {
        assert_eq!(sort_title("A to Z", 387354), "a to z");
        assert_eq!(sort_title("The Holdovers", 840430), "holdovers");
    }
}
