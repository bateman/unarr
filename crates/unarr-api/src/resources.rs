//! JSON resources shared by several routes, in the originals' shape.

use chrono::{DateTime, Utc};
use serde::Serialize;
use unarr_core::model::{Image, QualityModel, Series};
use unarr_core::quality::{Language, Quality};
use unarr_core::store::stats::SeasonStats;

use crate::json;

/// `{"id": 1, "name": "English"}`
#[derive(Serialize, Clone)]
pub struct LanguageResource {
    pub id: i64,
    pub name: &'static str,
}

impl From<Language> for LanguageResource {
    fn from(l: Language) -> Self {
        LanguageResource {
            id: l.id,
            name: l.name,
        }
    }
}

pub fn language(id: i64) -> LanguageResource {
    Language::by_id(id).into()
}

pub fn languages(ids: &[i64]) -> Vec<LanguageResource> {
    ids.iter().map(|&id| language(id)).collect()
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RevisionResource {
    pub version: i64,
    pub real: i64,
    pub is_repack: bool,
}

/// `{"quality": {...}, "revision": {...}}`
#[derive(Serialize, Clone)]
pub struct QualityModelResource {
    pub quality: Quality,
    pub revision: RevisionResource,
}

impl From<QualityModel> for QualityModelResource {
    fn from(q: QualityModel) -> Self {
        QualityModelResource {
            quality: Quality::by_id(q.quality_id),
            revision: RevisionResource {
                version: q.version,
                real: q.real,
                is_repack: q.is_repack,
            },
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ImageResource {
    pub cover_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
}

/// Series images with the local `/MediaCover/{id}/{type}.{ext}` URL the UI
/// loads (`ConvertToLocalUrls`), with `?lastWrite=` when the file is in
/// `covers`; unknown kinds keep the URL they have.
pub fn series_images(
    series_id: i64,
    url_base: &str,
    covers: Option<&std::path::Path>,
    images: &[Image],
) -> Vec<ImageResource> {
    let none = std::path::Path::new("/nonexistent-unarr-covers");
    images
        .iter()
        .map(|i| ImageResource {
            cover_type: i.cover_type.clone(),
            url: unarr_core::tv::covers::local_url(
                covers.unwrap_or(none),
                url_base,
                series_id,
                &i.cover_type,
            )
            .or_else(|| i.url.clone()),
            remote_url: i.remote_url.clone(),
        })
        .collect()
}

/// Images as stored, without a local URL (season and episode images).
pub fn plain_images(images: &[Image]) -> Vec<ImageResource> {
    images
        .iter()
        .map(|i| ImageResource {
            cover_type: i.cover_type.clone(),
            url: i.url.clone(),
            remote_url: i.remote_url.clone(),
        })
        .collect()
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RatingsResource {
    pub votes: i64,
    pub value: f64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AlternateTitleResource {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_season_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SeasonStatisticsResource {
    #[serde(with = "json::ts_opt", skip_serializing_if = "Option::is_none")]
    pub next_airing: Option<DateTime<Utc>>,
    #[serde(with = "json::ts_opt", skip_serializing_if = "Option::is_none")]
    pub previous_airing: Option<DateTime<Utc>>,
    pub episode_file_count: i64,
    pub episode_count: i64,
    pub total_episode_count: i64,
    pub size_on_disk: i64,
    pub release_groups: Vec<String>,
    pub percent_of_episodes: f64,
}

impl From<&SeasonStats> for SeasonStatisticsResource {
    fn from(s: &SeasonStats) -> Self {
        SeasonStatisticsResource {
            next_airing: s.next_airing,
            previous_airing: s.previous_airing,
            episode_file_count: s.episode_file_count,
            episode_count: s.episode_count,
            total_episode_count: s.total_episode_count,
            size_on_disk: s.size_on_disk,
            release_groups: s.release_groups.clone(),
            percent_of_episodes: s.percent_of_episodes(),
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SeriesStatisticsResource {
    pub season_count: i64,
    pub episode_file_count: i64,
    pub episode_count: i64,
    pub total_episode_count: i64,
    pub size_on_disk: i64,
    /// Left out when the statistics are the empty ones of a series not in
    /// the library, or with no episode yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_groups: Option<Vec<String>>,
    pub percent_of_episodes: f64,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SeasonResource {
    pub season_number: i64,
    pub monitored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statistics: Option<SeasonStatisticsResource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageResource>>,
}

/// `SeriesResource` of Sonarr v4.0.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SeriesResource {
    /// Left out for a series not in the library (a lookup result).
    #[serde(skip_serializing_if = "is_zero")]
    pub id: i64,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternate_titles: Option<Vec<AlternateTitleResource>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_title: Option<String>,
    pub status: &'static str,
    pub ended: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
    #[serde(with = "json::ts_opt", skip_serializing_if = "Option::is_none")]
    pub next_airing: Option<DateTime<Utc>>,
    #[serde(with = "json::ts_opt", skip_serializing_if = "Option::is_none")]
    pub previous_airing: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub air_time: Option<String>,
    pub images: Vec<ImageResource>,
    pub original_language: LanguageResource,
    pub seasons: Vec<SeasonResource>,
    pub year: i64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub path: String,
    pub quality_profile_id: i64,
    pub season_folder: bool,
    pub monitored: bool,
    pub monitor_new_items: &'static str,
    pub use_scene_numbering: bool,
    pub runtime: i64,
    pub tvdb_id: i64,
    pub tv_rage_id: i64,
    pub tv_maze_id: i64,
    pub tmdb_id: i64,
    #[serde(with = "json::ts_opt", skip_serializing_if = "Option::is_none")]
    pub first_aired: Option<DateTime<Utc>>,
    #[serde(with = "json::ts_opt", skip_serializing_if = "Option::is_none")]
    pub last_aired: Option<DateTime<Utc>>,
    pub series_type: &'static str,
    pub clean_title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
    pub title_slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_folder_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certification: Option<String>,
    pub genres: Vec<String>,
    pub tags: Vec<i64>,
    #[serde(with = "json::ts")]
    pub added: DateTime<Utc>,
    pub ratings: RatingsResource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statistics: Option<SeriesStatisticsResource>,
    /// Legacy field kept by Sonarr v4 for v3 clients.
    pub language_profile_id: i64,
    /// The lookup's folder name for the series.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The lookup's poster, remote.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_poster: Option<String>,
    /// While the series is being added.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add_options: Option<serde_json::Value>,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// Inputs the series resource needs besides the series itself.
pub struct SeriesContext<'a> {
    pub url_base: &'a str,
    /// Where the covers are, for their `lastWrite`.
    pub media_cover_dir: Option<&'a std::path::Path>,
    pub root_folder_path: String,
    pub alternate_titles: Vec<AlternateTitleResource>,
    pub season_stats: Option<&'a [SeasonStats]>,
    pub include_season_images: bool,
}

pub fn series_resource(s: &Series, ctx: SeriesContext<'_>) -> SeriesResource {
    let stats = ctx.season_stats.unwrap_or(&[]);
    let seasons: Vec<SeasonResource> = s
        .seasons
        .iter()
        .map(|season| SeasonResource {
            season_number: season.season_number,
            monitored: season.monitored,
            statistics: stats
                .iter()
                .find(|st| st.season_number == season.season_number)
                .map(SeasonStatisticsResource::from),
            images: ctx.include_season_images.then(|| plain_images(&season.images)),
        })
        .collect();

    let statistics = ctx.season_stats.map(|stats| {
        let episode_count: i64 = stats.iter().map(|x| x.episode_count).sum();
        let episode_file_count: i64 = stats.iter().map(|x| x.episode_file_count).sum();
        let mut release_groups: Vec<String> = Vec::new();
        for g in stats.iter().flat_map(|x| x.release_groups.iter()) {
            if !release_groups.contains(g) {
                release_groups.push(g.clone());
            }
        }
        SeriesStatisticsResource {
            season_count: s.seasons.iter().filter(|x| x.season_number > 0).count() as i64,
            episode_file_count,
            episode_count,
            total_episode_count: stats.iter().map(|x| x.total_episode_count).sum(),
            size_on_disk: stats.iter().map(|x| x.size_on_disk).sum(),
            release_groups: Some(release_groups),
            percent_of_episodes: if episode_count == 0 {
                0.0
            } else {
                (episode_file_count * 100) as f64 / episode_count as f64
            },
        }
    });
    let next_airing = stats.iter().filter_map(|x| x.next_airing).min();
    let previous_airing = stats.iter().filter_map(|x| x.previous_airing).max();
    let last_aired = s.last_aired.or_else(|| {
        stats
            .iter()
            .filter(|x| x.season_number > 0)
            .filter_map(|x| x.last_aired)
            .max()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| dt.and_utc())
    });

    SeriesResource {
        id: s.id,
        title: s.title.clone(),
        alternate_titles: Some(ctx.alternate_titles),
        sort_title: s.sort_title.clone(),
        status: s.status.as_str(),
        ended: s.status == unarr_core::model::SeriesStatus::Ended,
        overview: s.overview.clone(),
        next_airing,
        previous_airing,
        network: s.network.clone(),
        air_time: s.air_time.clone(),
        images: series_images(s.id, ctx.url_base, ctx.media_cover_dir, &s.images),
        original_language: language(s.original_language_id),
        seasons,
        year: s.year,
        path: s.path.clone(),
        quality_profile_id: s.quality_profile_id,
        season_folder: s.season_folder,
        monitored: s.monitored,
        monitor_new_items: s.monitor_new_items.as_str(),
        use_scene_numbering: s.use_scene_numbering,
        runtime: s.runtime,
        tvdb_id: s.tvdb_id,
        tv_rage_id: s.tv_rage_id,
        tv_maze_id: s.tv_maze_id,
        tmdb_id: s.tmdb_id,
        first_aired: s.first_aired,
        last_aired,
        series_type: s.series_type.as_str(),
        clean_title: s.clean_title.clone(),
        imdb_id: s.imdb_id.clone(),
        title_slug: s.title_slug.clone(),
        root_folder_path: Some(ctx.root_folder_path),
        certification: s.certification.clone(),
        genres: s.genres.clone(),
        tags: s.tags.clone(),
        added: s.added,
        ratings: RatingsResource {
            votes: s.ratings.votes,
            value: s.ratings.value,
        },
        statistics,
        language_profile_id: 1,
        folder: None,
        remote_poster: None,
        add_options: s.add_options.clone(),
    }
}

/// The series as embedded in episode, calendar and history responses: the
/// originals' bare `ToResource()`, without statistics, alternate titles, root
/// folder or local cover URLs.
pub fn series_resource_plain(s: &Series) -> SeriesResource {
    let mut r = series_resource(
        s,
        SeriesContext {
            url_base: "",
            media_cover_dir: None,
            root_folder_path: String::new(),
            alternate_titles: Vec::new(),
            season_stats: None,
            include_season_images: false,
        },
    );
    r.alternate_titles = None;
    r.root_folder_path = None;
    r.statistics = None;
    r.next_airing = None;
    r.previous_airing = None;
    r.last_aired = s.last_aired;
    r.images = plain_images(&s.images);
    r
}

/// `{"id": 1, "name": "x265"}`
#[derive(Serialize, Clone)]
pub struct CustomFormatRef {
    pub id: i64,
    pub name: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfoResource {
    pub audio_bitrate: i64,
    pub audio_channels: serde_json::Value,
    pub audio_codec: Option<String>,
    pub audio_languages: Option<String>,
    pub audio_stream_count: i64,
    pub video_bit_depth: i64,
    pub video_bitrate: i64,
    pub video_codec: Option<String>,
    pub video_fps: serde_json::Value,
    pub video_dynamic_range: String,
    pub video_dynamic_range_type: String,
    pub resolution: String,
    pub run_time: String,
    pub scan_type: Option<String>,
    pub subtitles: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeFileResource {
    pub id: i64,
    pub series_id: i64,
    pub season_number: i64,
    pub relative_path: String,
    pub path: String,
    pub size: i64,
    #[serde(with = "json::ts")]
    pub date_added: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_group: Option<String>,
    pub languages: Vec<LanguageResource>,
    pub quality: QualityModelResource,
    pub custom_formats: Vec<CustomFormatRef>,
    pub custom_format_score: i64,
    pub indexer_flags: i64,
    pub release_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_info: Option<MediaInfoResource>,
    pub quality_cutoff_not_met: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeResource {
    pub id: i64,
    pub series_id: i64,
    pub tvdb_id: i64,
    pub episode_file_id: i64,
    pub season_number: i64,
    pub episode_number: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(with = "json::date_opt", skip_serializing_if = "Option::is_none")]
    pub air_date: Option<chrono::NaiveDate>,
    #[serde(with = "json::ts_opt", skip_serializing_if = "Option::is_none")]
    pub air_date_utc: Option<DateTime<Utc>>,
    #[serde(with = "json::ts_opt", skip_serializing_if = "Option::is_none")]
    pub last_search_time: Option<DateTime<Utc>>,
    pub runtime: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finale_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_file: Option<EpisodeFileResource>,
    pub has_file: bool,
    pub monitored: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub absolute_episode_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_absolute_episode_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_episode_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_season_number: Option<i64>,
    pub unverified_scene_numbering: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<SeriesResource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<ImageResource>>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResource {
    pub id: i64,
    pub episode_id: i64,
    pub series_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_title: Option<String>,
    pub languages: Vec<LanguageResource>,
    pub quality: QualityModelResource,
    pub custom_formats: Vec<CustomFormatRef>,
    pub custom_format_score: i64,
    pub quality_cutoff_not_met: bool,
    #[serde(with = "json::ts")]
    pub date: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_id: Option<String>,
    pub event_type: &'static str,
    pub data: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<EpisodeResource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<SeriesResource>,
}

/// `{page, pageSize, sortKey, sortDirection, totalRecords, records}`
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PagingResource<T: Serialize> {
    pub page: i64,
    pub page_size: i64,
    pub sort_key: String,
    pub sort_direction: &'static str,
    pub total_records: i64,
    pub records: Vec<T>,
}

impl<T: Serialize> PagingResource<T> {
    pub fn new(paging: &unarr_core::store::paging::Paging, total: i64, records: Vec<T>) -> Self {
        PagingResource {
            page: paging.page,
            page_size: paging.page_size,
            sort_key: paging.sort_key.clone(),
            sort_direction: if paging.descending {
                "descending"
            } else {
                "ascending"
            },
            total_records: total,
            records,
        }
    }
}

pub use unarr_core::mediafiles::best_root_folder_path;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_longest_parent_root_folder() {
        let roots = vec!["/data".to_string(), "/data/tv".to_string(), "/other".to_string()];
        assert_eq!(best_root_folder_path("/data/tv/Firefly", &roots), "/data/tv");
        assert_eq!(best_root_folder_path("/data/tvshows/X", &roots), "/data");
        assert_eq!(best_root_folder_path("/elsewhere/Show", &roots), "/elsewhere");
        assert_eq!(best_root_folder_path("/Show", &roots), "/");
    }

    #[test]
    fn cover_urls_follow_the_originals() {
        let imgs = vec![
            Image {
                cover_type: "poster".into(),
                remote_url: Some("https://x/p.jpg".into()),
                url: None,
            },
            Image {
                cover_type: "clearlogo".into(),
                remote_url: Some("https://x/l.png".into()),
                url: None,
            },
        ];
        let out = series_images(7, "/sonarr", None, &imgs);
        assert_eq!(out[0].url.as_deref(), Some("/sonarr/MediaCover/7/poster.jpg"));
        assert_eq!(out[1].url.as_deref(), Some("/sonarr/MediaCover/7/clearlogo.png"));
    }
}
