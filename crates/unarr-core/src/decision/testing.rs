//! An in-memory [`Lookup`] for tests: the series, episodes, files, history,
//! blocklist and scene mappings are plain vectors.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::decision::lookup::Lookup;
use crate::model::{
    BlocklistEntry, Episode, EpisodeFile, HistoryEntry, ProfileItem, QualityProfile, SceneMapping, Series,
};
use crate::parser::clean_series_title;

#[derive(Debug, Clone, Default)]
pub struct MemoryLookup {
    pub series: Vec<Series>,
    pub episodes: Vec<Episode>,
    pub files: Vec<EpisodeFile>,
    pub history: Vec<HistoryEntry>,
    pub blocklist: Vec<BlocklistEntry>,
    pub scene_mappings: Vec<SceneMapping>,
    pub free_space: HashMap<String, i64>,
    pub missing_files: Vec<String>,
    pub oldest_pending_age_minutes: Option<f64>,
}

impl Lookup for MemoryLookup {
    fn series_by_id(&self, id: i64) -> Option<Series> {
        self.series.iter().find(|s| s.id == id).cloned()
    }

    fn series_by_clean_title(&self, clean_title: &str) -> Vec<Series> {
        let wanted = clean_title.to_lowercase();
        self.series
            .iter()
            .filter(|s| s.clean_title == wanted)
            .cloned()
            .collect()
    }

    fn series_by_clean_title_and_year(&self, clean_title: &str, year: i64) -> Vec<Series> {
        let wanted = clean_title.to_lowercase();
        self.series
            .iter()
            .filter(|s| s.clean_title == wanted && s.year == year)
            .cloned()
            .collect()
    }

    fn series_inexact(&self, clean_title: &str) -> Vec<Series> {
        self.series
            .iter()
            .filter(|s| !s.clean_title.is_empty() && clean_title.contains(&s.clean_title))
            .cloned()
            .collect()
    }

    fn series_by_tvdb_id(&self, tvdb_id: i64) -> Option<Series> {
        self.series.iter().find(|s| s.tvdb_id == tvdb_id).cloned()
    }

    fn series_by_tv_rage_id(&self, tv_rage_id: i64) -> Option<Series> {
        self.series.iter().find(|s| s.tv_rage_id == tv_rage_id).cloned()
    }

    fn series_by_imdb_id(&self, imdb_id: &str) -> Option<Series> {
        self.series
            .iter()
            .find(|s| s.imdb_id.as_deref() == Some(imdb_id))
            .cloned()
    }

    fn episodes_by_season(&self, series_id: i64, season: i64) -> Vec<Episode> {
        self.episodes
            .iter()
            .filter(|e| e.series_id == series_id && e.season_number == season)
            .cloned()
            .collect()
    }

    fn episodes_by_scene_season(&self, series_id: i64, scene_season: i64) -> Vec<Episode> {
        self.episodes
            .iter()
            .filter(|e| e.series_id == series_id && e.scene_season_number == Some(scene_season))
            .cloned()
            .collect()
    }

    fn episode_by_number(&self, series_id: i64, season: i64, number: i64) -> Option<Episode> {
        self.episodes
            .iter()
            .find(|e| e.series_id == series_id && e.season_number == season && e.episode_number == number)
            .cloned()
    }

    fn episode_by_absolute(&self, series_id: i64, absolute: i64) -> Option<Episode> {
        self.episodes
            .iter()
            .find(|e| e.series_id == series_id && e.absolute_episode_number == Some(absolute))
            .cloned()
    }

    fn episodes_by_scene_numbering(
        &self,
        series_id: i64,
        scene_season: i64,
        scene_episode: i64,
    ) -> Vec<Episode> {
        self.episodes
            .iter()
            .filter(|e| {
                e.series_id == series_id
                    && e.scene_season_number == Some(scene_season)
                    && e.scene_episode_number == Some(scene_episode)
            })
            .cloned()
            .collect()
    }

    fn episodes_by_scene_absolute(&self, series_id: i64, scene_absolute: i64) -> Vec<Episode> {
        self.episodes
            .iter()
            .filter(|e| e.series_id == series_id && e.scene_absolute_episode_number == Some(scene_absolute))
            .cloned()
            .collect()
    }

    fn episodes_by_air_date(&self, series_id: i64, air_date: &str) -> Vec<Episode> {
        self.episodes
            .iter()
            .filter(|e| {
                e.series_id == series_id
                    && e.air_date
                        .is_some_and(|d| d.format("%Y-%m-%d").to_string() == air_date)
            })
            .cloned()
            .collect()
    }

    fn episodes_by_file(&self, file_id: i64) -> Vec<Episode> {
        self.episodes
            .iter()
            .filter(|e| e.episode_file_id == file_id)
            .cloned()
            .collect()
    }

    fn episode_file(&self, id: i64) -> Option<EpisodeFile> {
        self.files.iter().find(|f| f.id == id).cloned()
    }

    fn scene_mappings_by_parse_term(&self, parse_term: &str) -> Vec<SceneMapping> {
        self.scene_mappings
            .iter()
            .filter(|m| {
                m.parse_term
                    .clone()
                    .unwrap_or_else(|| clean_series_title(&m.title))
                    == parse_term
            })
            .cloned()
            .collect()
    }

    fn history_for_episode(&self, episode_id: i64) -> Vec<HistoryEntry> {
        let mut out: Vec<HistoryEntry> = self
            .history
            .iter()
            .filter(|h| h.episode_id == episode_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| b.id.cmp(&a.id)));
        out
    }

    fn blocklist_for_series(&self, series_id: i64) -> Vec<BlocklistEntry> {
        self.blocklist
            .iter()
            .filter(|b| b.series_id == series_id)
            .cloned()
            .collect()
    }

    fn oldest_pending_release_age_minutes(
        &self,
        _series_id: i64,
        _episode_ids: &[i64],
        _now: DateTime<Utc>,
    ) -> Option<f64> {
        self.oldest_pending_age_minutes
    }

    fn free_space(&self, path: &str) -> Option<i64> {
        self.free_space.get(path).copied()
    }

    fn file_exists(&self, path: &str) -> bool {
        !self.missing_files.iter().any(|m| m == path)
    }
}

/// A series with the fields the engine reads set to plain values.
pub fn series(id: i64, title: &str, quality_profile_id: i64) -> Series {
    use crate::model::{NewItemMonitor, Ratings, SeriesStatus, SeriesType};
    Series {
        id,
        tvdb_id: 100_000 + id,
        tv_rage_id: 0,
        tv_maze_id: 0,
        tmdb_id: 0,
        imdb_id: None,
        title: title.into(),
        title_slug: clean_series_title(title),
        clean_title: clean_series_title(title),
        sort_title: None,
        status: SeriesStatus::Continuing,
        overview: None,
        air_time: None,
        network: None,
        certification: None,
        images: Vec::new(),
        genres: Vec::new(),
        actors: serde_json::Value::Array(Vec::new()),
        path: format!("/tv/{title}"),
        monitored: true,
        season_folder: true,
        monitor_new_items: NewItemMonitor::All,
        use_scene_numbering: false,
        runtime: 45,
        series_type: SeriesType::Standard,
        year: 2020,
        first_aired: None,
        last_aired: None,
        quality_profile_id,
        original_language_id: 1,
        ratings: Ratings::default(),
        added: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        add_options: None,
        last_info_sync: None,
        last_disk_sync: None,
        mal_ids: None,
        anilist_ids: None,
        seasons: vec![crate::model::Season {
            season_number: 1,
            monitored: true,
            images: Vec::new(),
        }],
        tags: Vec::new(),
    }
}

/// An episode with the fields the engine reads set to plain values.
pub fn episode(id: i64, series_id: i64, season: i64, number: i64) -> Episode {
    Episode {
        id,
        series_id,
        tvdb_id: 1_000_000 + id,
        season_number: season,
        episode_number: number,
        title: Some(format!("Episode {number}")),
        overview: None,
        episode_file_id: 0,
        absolute_episode_number: None,
        scene_absolute_episode_number: None,
        scene_season_number: None,
        scene_episode_number: None,
        unverified_scene_numbering: false,
        monitored: true,
        air_date: Some(chrono::NaiveDate::from_ymd_opt(2020, 1, 1).unwrap()),
        air_date_utc: Some(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH),
        runtime: 0,
        finale_type: None,
        images: Vec::new(),
        ratings: crate::model::Ratings::default(),
        last_search_time: None,
        aired_after_season_number: None,
        aired_before_season_number: None,
        aired_before_episode_number: None,
    }
}

/// `QualityFixture.GetDefaultQualities(allowed...)`: the default
/// qualities in order, the allowed ones moved to the end.
pub fn default_profile(allowed: &[i64], cutoff: i64, upgrade_allowed: bool) -> QualityProfile {
    let qualities = [0, 1, 8, 2, 4, 9, 16, 10, 5, 3, 18, 6, 7, 19];
    let allowed: Vec<i64> = if allowed.is_empty() {
        qualities.to_vec()
    } else {
        allowed.to_vec()
    };
    let mut ordered: Vec<i64> = qualities
        .iter()
        .copied()
        .filter(|q| !allowed.contains(q))
        .collect();
    ordered.extend(allowed.iter().copied());
    QualityProfile {
        id: 1,
        name: "Test".into(),
        cutoff,
        upgrade_allowed,
        items: ordered
            .into_iter()
            .map(|q| ProfileItem {
                id: None,
                name: None,
                quality: Some(q),
                items: vec![],
                allowed: allowed.contains(&q),
            })
            .collect(),
        format_items: vec![],
        min_format_score: 0,
        cutoff_format_score: 0,
        min_upgrade_format_score: 0,
    }
}
