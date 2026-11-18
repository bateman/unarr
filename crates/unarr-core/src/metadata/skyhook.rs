//! `SkyHookProxy`: the Servarr team's TVDB proxy, and the daily-series list
//! from the Servarr services that its mapping consults.

use crate::sync::Recover;
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{BoxFuture, MetadataError, MetadataProvider, SeriesInfo, Upstreams};
use crate::model::{Episode, Image, NewItemMonitor, Ratings, Season, Series, SeriesStatus, SeriesType};
use crate::parser::language::lang;

/// `SonarrCloudRequestBuilder.SkyHookTvdb`, the route and language filled
/// in as `GetSeriesInfo` and `SearchForNewSeries` do (`en` always).
const SKYHOOK: &str = "https://skyhook.sonarr.tv/v1/tvdb";
/// `SonarrCloudRequestBuilder.Services`.
const SERVICES: &str = "https://services.sonarr.tv/v1";

/// `DailySeriesService`: the list is kept for an hour.
const DAILY_SERIES_TTL: Duration = Duration::from_secs(3600);

/// Skyhook, through the shared HTTP client, with the original's user agent.
pub struct Skyhook {
    client: reqwest::Client,
    upstreams: Upstreams,
    daily: Mutex<Option<(Instant, HashSet<i64>)>>,
}

impl Skyhook {
    pub fn new(upstreams: Upstreams) -> Skyhook {
        Skyhook::with_client(crate::http::shared().clone(), upstreams)
    }

    pub fn with_client(client: reqwest::Client, upstreams: Upstreams) -> Skyhook {
        Skyhook {
            client,
            upstreams,
            daily: Mutex::new(None),
        }
    }

    async fn get(&self, url: &str) -> Result<(reqwest::StatusCode, Vec<u8>), MetadataError> {
        let response = self
            .client
            .get(self.upstreams.url(url))
            .header(
                reqwest::header::USER_AGENT,
                crate::http::servarr_proxy_user_agent(crate::Personality::Sonarr),
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| MetadataError::Unavailable(transport_message(&e)))?;
        let status = response.status();
        let body = crate::http::read_body(response, crate::http::MAX_RESPONSE_BYTES)
            .await
            .map_err(|e| MetadataError::Unavailable(e.to_string()))?;
        Ok((status, body))
    }

    /// `DailySeriesDataProxy.GetDailySeriesIds`: a failure is logged and
    /// read as no daily series, and the empty answer is cached like a good
    /// one.
    async fn daily_series(&self) -> HashSet<i64> {
        if let Some((at, ids)) = &*self.daily.lock().recover() {
            if at.elapsed() < DAILY_SERIES_TTL {
                return ids.clone();
            }
        }
        let ids = match self.get(&format!("{SERVICES}/dailyseries")).await {
            Ok((status, body)) if status.is_success() => {
                match serde_json::from_slice::<Vec<DailySeriesResource>>(&body) {
                    Ok(list) => list.into_iter().map(|d| d.tvdb_id).collect(),
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to get Daily Series");
                        HashSet::new()
                    }
                }
            }
            Ok((status, _)) => {
                tracing::warn!(%status, "Failed to get Daily Series");
                HashSet::new()
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to get Daily Series");
                HashSet::new()
            }
        };
        *self.daily.lock().recover() = Some((Instant::now(), ids.clone()));
        ids
    }
}

/// A transport error's innermost cause: the outer text names the URL.
fn transport_message(e: &reqwest::Error) -> String {
    let mut cause: &dyn std::error::Error = e;
    while let Some(next) = cause.source() {
        cause = next;
    }
    cause.to_string()
}

impl MetadataProvider for Skyhook {
    fn series_info(&self, tvdb_id: i64) -> BoxFuture<'_, Result<SeriesInfo, MetadataError>> {
        Box::pin(async move {
            let url = format!("{SKYHOOK}/shows/en/{tvdb_id}");
            let (status, body) = self.get(&url).await?;
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(MetadataError::SeriesNotFound(tvdb_id));
            }
            if !status.is_success() {
                return Err(MetadataError::Unavailable(format!(
                    "HTTP request failed: [{}:{}] [GET] at [{url}]",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("")
                )));
            }
            let show: ShowResource =
                serde_json::from_slice(&body).map_err(|e| MetadataError::Invalid(e.to_string()))?;
            let daily = self.daily_series().await;
            Ok(map_show(show, &daily))
        })
    }

    fn search(&self, term: &str) -> BoxFuture<'_, Result<Vec<Series>, MetadataError>> {
        let term = term.to_lowercase().trim().to_string();
        Box::pin(async move {
            let url = format!(
                "{SKYHOOK}/search/en/?term={}",
                crate::indexers::request::escape_data_string(&term)
            );
            let failed = |why: &str, detail: String| {
                tracing::warn!(%detail, "SkyHook search failed");
                MetadataError::Unavailable(format!("Search for '{term}' failed. {why} SkyHook. {detail}"))
            };
            let (status, body) = self
                .get(&url)
                .await
                .map_err(|e| failed("Unable to communicate with", e.to_string()))?;
            if !status.is_success() {
                return Err(failed(
                    "Unable to communicate with",
                    format!(
                        "HTTP request failed: [{}:{}] [GET] at [{url}]",
                        status.as_u16(),
                        status.canonical_reason().unwrap_or("")
                    ),
                ));
            }
            let shows: Vec<ShowResource> = serde_json::from_slice(&body)
                .map_err(|e| failed("Invalid response received from", e.to_string()))?;
            let daily = if shows.is_empty() {
                HashSet::new()
            } else {
                self.daily_series().await
            };
            Ok(shows.into_iter().map(|s| map_show(s, &daily).series).collect())
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DailySeriesResource {
    tvdb_id: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ShowResource {
    tvdb_id: i64,
    title: String,
    overview: Option<String>,
    slug: Option<String>,
    first_aired: Option<String>,
    last_aired: Option<String>,
    tv_rage_id: Option<i64>,
    tv_maze_id: Option<i64>,
    tmdb_id: Option<i64>,
    mal_ids: Option<Vec<i64>>,
    ani_list_ids: Option<Vec<i64>>,
    status: Option<String>,
    runtime: Option<i64>,
    time_of_day: Option<TimeOfDayResource>,
    network: Option<String>,
    imdb_id: Option<String>,
    original_language: Option<String>,
    actors: Vec<ActorResource>,
    genres: Vec<String>,
    content_rating: Option<String>,
    rating: Option<RatingResource>,
    images: Vec<ImageResource>,
    seasons: Vec<SeasonResource>,
    episodes: Vec<EpisodeResource>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct TimeOfDayResource {
    hours: i64,
    minutes: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ActorResource {
    name: Option<String>,
    character: Option<String>,
    image: Option<String>,
}

/// `RatingResource`: `value` is a decimal, which Skyhook sends as a string.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RatingResource {
    count: i64,
    #[serde(deserialize_with = "decimal")]
    value: f64,
}

fn decimal<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    match Value::deserialize(d)? {
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => s.trim().parse().map_err(serde::de::Error::custom),
        Value::Null => Ok(0.0),
        other => Err(serde::de::Error::custom(format!("not a decimal: {other}"))),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ImageResource {
    cover_type: String,
    url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SeasonResource {
    season_number: i64,
    images: Vec<ImageResource>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct EpisodeResource {
    tvdb_id: i64,
    season_number: i64,
    episode_number: i64,
    absolute_episode_number: Option<i64>,
    aired_after_season_number: Option<i64>,
    aired_before_season_number: Option<i64>,
    aired_before_episode_number: Option<i64>,
    title: Option<String>,
    air_date: Option<String>,
    air_date_utc: Option<DateTime<Utc>>,
    runtime: i64,
    finale_type: Option<String>,
    rating: Option<RatingResource>,
    overview: Option<String>,
    image: Option<String>,
}

/// `new Series()` before `MapSeries` fills it: what a series the provider
/// returns carries where the provider says nothing.
fn blank_series() -> Series {
    Series {
        id: 0,
        tvdb_id: 0,
        tv_rage_id: 0,
        tv_maze_id: 0,
        tmdb_id: 0,
        imdb_id: None,
        title: String::new(),
        title_slug: String::new(),
        clean_title: String::new(),
        sort_title: None,
        status: SeriesStatus::Continuing,
        overview: None,
        air_time: None,
        network: None,
        certification: None,
        images: Vec::new(),
        genres: Vec::new(),
        actors: json!([]),
        path: String::new(),
        monitored: false,
        season_folder: false,
        monitor_new_items: NewItemMonitor::All,
        use_scene_numbering: false,
        runtime: 0,
        series_type: SeriesType::Standard,
        year: 0,
        first_aired: None,
        last_aired: None,
        quality_profile_id: 0,
        original_language_id: lang::ENGLISH,
        ratings: Ratings::default(),
        // DateTime.MinValue
        added: Utc.with_ymd_and_hms(1, 1, 1, 0, 0, 0).unwrap(),
        add_options: None,
        last_info_sync: None,
        last_disk_sync: None,
        mal_ids: None,
        anilist_ids: None,
        seasons: Vec::new(),
        tags: Vec::new(),
    }
}

/// `SeriesTitleNormalizer.Normalize`.
pub fn sort_title(title: &str, tvdb_id: i64) -> String {
    match tvdb_id {
        281588 => "a to z".into(),
        289260 => "ad bible continues".into(),
        328534 => "ap bio".into(),
        77904 => "ateam".into(),
        _ => crate::parser::title::normalize_title(title).to_lowercase(),
    }
}

/// `yyyy-MM-dd` as midnight UTC, as `MapSeries` parses it.
fn day(s: &str) -> Option<DateTime<Utc>> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc())
}

fn map_ratings(rating: Option<RatingResource>) -> Ratings {
    rating.map_or_else(Ratings::default, |r| Ratings {
        votes: r.count,
        value: r.value,
    })
}

/// `MapCoverType`: unknown kinds become `unknown`.
fn map_image(image: ImageResource) -> Image {
    let cover_type = match image.cover_type.to_lowercase().as_str() {
        kind @ ("poster" | "banner" | "fanart" | "clearlogo") => kind.to_string(),
        _ => "unknown".to_string(),
    };
    Image {
        cover_type,
        remote_url: image.url,
        url: None,
    }
}

fn map_show(show: ShowResource, daily: &HashSet<i64>) -> SeriesInfo {
    let mut s = blank_series();
    s.tvdb_id = show.tvdb_id;
    s.tv_rage_id = show.tv_rage_id.unwrap_or(0);
    s.tv_maze_id = show.tv_maze_id.unwrap_or(0);
    s.tmdb_id = show.tmdb_id.unwrap_or(0);
    s.imdb_id = show.imdb_id;
    s.mal_ids = show.mal_ids.map(|ids| json!(ids));
    s.anilist_ids = show.ani_list_ids.map(|ids| json!(ids));
    s.clean_title = crate::parser::title::clean_series_title(&show.title);
    s.sort_title = Some(sort_title(&show.title, show.tvdb_id));
    s.title = show.title;
    s.original_language_id = show
        .original_language
        .as_deref()
        .filter(|l| !l.trim().is_empty())
        .and_then(|l| crate::mediafiles::language_from_iso(&l.to_lowercase()))
        .unwrap_or(lang::ENGLISH);
    if let Some(first) = show.first_aired.as_deref().and_then(day) {
        s.year = chrono::Datelike::year(&first) as i64;
        s.first_aired = Some(first);
    }
    s.last_aired = show.last_aired.as_deref().and_then(day);
    s.overview = show.overview;
    if let Some(runtime) = show.runtime {
        s.runtime = runtime;
    }
    s.network = show.network;
    if let Some(t) = show.time_of_day {
        s.air_time = Some(format!("{:02}:{:02}", t.hours, t.minutes));
    }
    s.title_slug = show.slug.unwrap_or_default();
    s.status = match show.status.as_deref().map(str::to_lowercase).as_deref() {
        Some("ended") => SeriesStatus::Ended,
        Some("upcoming") => SeriesStatus::Upcoming,
        _ => SeriesStatus::Continuing,
    };
    s.ratings = map_ratings(show.rating);
    s.genres = show.genres;
    if let Some(rating) = show.content_rating.filter(|c| !c.trim().is_empty()) {
        s.certification = Some(rating.to_uppercase());
    }
    if daily.contains(&s.tvdb_id) {
        s.series_type = SeriesType::Daily;
    }
    s.actors = Value::Array(
        show.actors
            .into_iter()
            .map(|a| {
                let mut actor = json!({ "name": a.name, "character": a.character });
                actor["images"] = match a.image {
                    Some(url) => json!([{ "coverType": "headshot", "remoteUrl": url }]),
                    None => json!([]),
                };
                actor
            })
            .collect(),
    );
    s.seasons = show
        .seasons
        .into_iter()
        .map(|season| Season {
            season_number: season.season_number,
            monitored: season.season_number > 0,
            images: season.images.into_iter().map(map_image).collect(),
        })
        .collect();
    s.images = show.images.into_iter().map(map_image).collect();
    s.monitored = true;

    let episodes = show.episodes.into_iter().map(map_episode).collect();
    SeriesInfo { series: s, episodes }
}

fn map_episode(e: EpisodeResource) -> Episode {
    Episode {
        id: 0,
        series_id: 0,
        tvdb_id: e.tvdb_id,
        season_number: e.season_number,
        episode_number: e.episode_number,
        title: e.title,
        overview: e.overview,
        episode_file_id: 0,
        absolute_episode_number: e.absolute_episode_number,
        scene_absolute_episode_number: None,
        scene_season_number: None,
        scene_episode_number: None,
        unverified_scene_numbering: false,
        monitored: false,
        air_date: e
            .air_date
            .as_deref()
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()),
        air_date_utc: e.air_date_utc,
        runtime: e.runtime,
        finale_type: e.finale_type,
        // Series fanart is not taken as an episode's screenshot.
        images: e
            .image
            .map(|url| {
                vec![Image {
                    cover_type: "screenshot".into(),
                    remote_url: Some(url),
                    url: None,
                }]
            })
            .unwrap_or_default(),
        ratings: map_ratings(e.rating),
        last_search_time: None,
        aired_after_season_number: e.aired_after_season_number,
        aired_before_season_number: e.aired_before_season_number,
        aired_before_episode_number: e.aired_before_episode_number,
    }
}

/// A recorded show mapped as [`Skyhook::series_info`] maps it, for the
/// tests of the code that consumes series info.
pub fn map_recorded_show(json: &[u8], daily: &HashSet<i64>) -> anyhow::Result<SeriesInfo> {
    Ok(map_show(serde_json::from_slice(json)?, daily))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/metadata")
    }

    fn show(id: i64) -> SeriesInfo {
        let path = fixtures().join(format!("skyhook.sonarr.tv/v1/tvdb/shows/en/{id}.json"));
        map_recorded_show(&std::fs::read(path).unwrap(), &HashSet::from([271728])).unwrap()
    }

    #[test]
    fn maps_a_show_as_the_original() {
        let info = show(448176);
        let s = &info.series;
        assert_eq!(s.title, "The Pitt");
        assert_eq!(s.clean_title, "thepitt");
        assert_eq!(s.sort_title.as_deref(), Some("pitt"));
        assert_eq!(s.title_slug, "the-pitt");
        assert_eq!(s.year, 2025);
        assert_eq!(s.air_time.as_deref(), Some("21:00"));
        assert_eq!(s.tv_maze_id, 75632);
        assert_eq!(s.imdb_id.as_deref(), Some("tt31938062"));
        assert_eq!(s.status, SeriesStatus::Continuing);
        assert_eq!(s.original_language_id, lang::ENGLISH);
        assert!(s.ratings.value > 8.0 && s.ratings.votes > 0);
        assert!(s.monitored);
        assert_eq!(s.series_type, SeriesType::Standard);
        assert_eq!(
            s.images.iter().map(|i| i.cover_type.as_str()).collect::<Vec<_>>(),
            ["banner", "poster", "fanart", "clearlogo"]
        );
        assert!(s.seasons.iter().all(|x| x.monitored == (x.season_number > 0)));
        assert_eq!(s.actors[0]["images"][0]["coverType"], "headshot");
        let e = &info.episodes[0];
        assert_eq!((e.season_number, e.episode_number), (1, 1));
        assert_eq!(e.air_date, NaiveDate::from_ymd_opt(2025, 1, 9));
        assert_eq!(e.images[0].cover_type, "screenshot");
    }

    #[test]
    fn daily_series_and_certification() {
        let s = show(271728).series;
        assert_eq!(s.series_type, SeriesType::Daily);
        let s = show(81189).series;
        assert_eq!(s.certification.as_deref(), Some("TV-MA"));
        assert_eq!(s.status, SeriesStatus::Ended);
    }

    #[test]
    fn precomputed_sort_titles() {
        assert_eq!(sort_title("The A-Team", 77904), "ateam");
        assert_eq!(sort_title("The Office (US)", 1), "office us");
    }
}
