//! `ReleaseSearchService`: what to ask the indexers for an episode or a
//! season (scene titles, scene numbering, the search kind per series type),
//! the dispatch, and the decisions on what came back.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Utc};

use crate::db::Db;
use crate::decision::{self, Decision, DecisionInput, SearchCriteria, SearchKind};
use crate::indexers::request::{self, SearchMode, SeriesIds};
use crate::indexers::rss::ReleaseInfo;
use crate::indexers::IndexerService;
use crate::model::{Episode, SceneMapping, Series, SeriesType};
use crate::parser::clean_series_title;
use crate::store;

/// `SearchFailedException` and the lookups a search needs.
#[derive(Debug)]
pub enum SearchError {
    EpisodeNotFound(i64),
    SeriesNotFound(i64),
    /// Reported to the caller with the originals' message.
    Failed(String),
    Other(anyhow::Error),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SearchError::EpisodeNotFound(id) => write!(f, "Episode {id} not found"),
            SearchError::SeriesNotFound(id) => write!(f, "Series {id} not found"),
            SearchError::Failed(m) => write!(f, "{m}"),
            SearchError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl From<anyhow::Error> for SearchError {
    fn from(e: anyhow::Error) -> SearchError {
        SearchError::Other(e)
    }
}

/// How the search was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trigger {
    pub user_invoked: bool,
    pub interactive: bool,
}

/// `SceneEpisodeMapping`.
#[derive(Debug, Clone, PartialEq)]
struct EpisodeMapping {
    episode: Episode,
    search_mode: SearchMode,
    scene_titles: Vec<String>,
    season_number: i64,
    episode_number: i64,
    absolute_episode_number: Option<i64>,
}

/// `SceneSeasonMapping`.
#[derive(Debug, Clone, PartialEq)]
struct SeasonMapping {
    episodes: Vec<Episode>,
    episode_mapping: EpisodeMapping,
    search_mode: SearchMode,
    scene_titles: Vec<String>,
    season_number: i64,
}

#[derive(Clone)]
pub struct SearchService {
    db: Db,
    indexers: IndexerService,
    /// The queue the decisions read; empty when no downloads service is
    /// attached (tests of the search alone).
    queue: Option<decision::QueueSource>,
}

impl SearchService {
    pub fn new(db: Db) -> SearchService {
        SearchService {
            indexers: IndexerService::new(db.clone()),
            db,
            queue: None,
        }
    }

    pub fn with_indexers(db: Db, indexers: IndexerService) -> SearchService {
        SearchService {
            db,
            indexers,
            queue: None,
        }
    }

    /// Decide against the queue the source reports.
    pub fn with_queue(mut self, queue: decision::QueueSource) -> SearchService {
        self.queue = Some(queue);
        self
    }

    /// `EpisodeSearch(episodeId, ...)`.
    pub async fn episode_search(
        &self,
        episode_id: i64,
        trigger: Trigger,
        now: DateTime<Utc>,
    ) -> Result<Vec<Decision>, SearchError> {
        let episode = self
            .db
            .call(move |conn| store::episodes::get_episode(conn, episode_id))
            .await?
            .ok_or(SearchError::EpisodeNotFound(episode_id))?;
        self.episode_search_for(episode, trigger, now).await
    }

    /// `EpisodeSearch(episode, ...)`.
    pub async fn episode_search_for(
        &self,
        episode: Episode,
        trigger: Trigger,
        now: DateTime<Utc>,
    ) -> Result<Vec<Decision>, SearchError> {
        let series = self.series(episode.series_id).await?;
        match series.series_type {
            SeriesType::Daily => {
                if episode.air_date.is_none() {
                    tracing::error!("daily episode is missing an air date. Try refreshing the series info.");
                    return Err(SearchError::Failed("Air date is missing".into()));
                }
                self.search_daily(&series, &episode, false, trigger, now).await
            }
            SeriesType::Anime => {
                if episode.season_number == 0
                    && episode.scene_absolute_episode_number.is_none()
                    && episode.absolute_episode_number.is_none()
                {
                    return self
                        .search_special(&series, vec![episode], false, trigger, now)
                        .await;
                }
                self.search_anime(&series, &episode, false, trigger, false, now)
                    .await
            }
            SeriesType::Standard => {
                if episode.season_number == 0 {
                    return self
                        .search_special(&series, vec![episode], false, trigger, now)
                        .await;
                }
                self.search_single(&series, &episode, false, trigger, now).await
            }
        }
    }

    /// `SeasonSearch(seriesId, seasonNumber, missingOnly, monitoredOnly, ...)`.
    pub async fn season_search(
        &self,
        series_id: i64,
        season_number: i64,
        missing_only: bool,
        monitored_only: bool,
        trigger: Trigger,
        now: DateTime<Utc>,
    ) -> Result<Vec<Decision>, SearchError> {
        let mut episodes = self
            .db
            .call(move |conn| store::episodes::list_episodes(conn, series_id, Some(season_number)))
            .await?;
        if missing_only {
            episodes.retain(|e| e.episode_file_id == 0);
        }
        self.season_search_episodes(series_id, season_number, episodes, monitored_only, trigger, now)
            .await
    }

    /// `SeasonSearch(seriesId, seasonNumber, episodes, monitoredOnly, ...)`.
    pub async fn season_search_episodes(
        &self,
        series_id: i64,
        _season_number: i64,
        episodes: Vec<Episode>,
        monitored_only: bool,
        trigger: Trigger,
        now: DateTime<Utc>,
    ) -> Result<Vec<Decision>, SearchError> {
        let series = self.series(series_id).await?;
        match series.series_type {
            SeriesType::Anime => {
                return self
                    .search_anime_season(&series, episodes, monitored_only, trigger, now)
                    .await
            }
            SeriesType::Daily => {
                return self
                    .search_daily_season(&series, episodes, monitored_only, trigger, now)
                    .await
            }
            SeriesType::Standard => {}
        }
        let mappings = self.scene_season_mappings(&series, &episodes).await?;
        let mut decisions = Vec::new();
        for mapping in mappings {
            if mapping.season_number == 0 {
                decisions.extend(
                    self.search_special(&series, mapping.episodes.clone(), monitored_only, trigger, now)
                        .await?,
                );
                continue;
            }
            if mapping.episodes.len() == 1 {
                let criteria = self.criteria_for_season_mapping(
                    &series,
                    &mapping,
                    SearchKind::SingleEpisode {
                        season_number: mapping.season_number,
                        episode_number: mapping.episode_mapping.episode_number,
                    },
                    monitored_only,
                    trigger,
                );
                decisions.extend(
                    self.dispatch(criteria, mapping.search_mode, mapping.scene_titles.clone(), now)
                        .await?,
                );
            } else {
                let criteria = self.criteria_for_season_mapping(
                    &series,
                    &mapping,
                    SearchKind::Season {
                        season_number: mapping.season_number,
                    },
                    monitored_only,
                    trigger,
                );
                decisions.extend(
                    self.dispatch(criteria, mapping.search_mode, mapping.scene_titles.clone(), now)
                        .await?,
                );
            }
        }
        Ok(dedupe(decisions))
    }

    async fn series(&self, id: i64) -> Result<Series, SearchError> {
        self.db
            .call(move |conn| store::series::get(conn, id))
            .await?
            .ok_or(SearchError::SeriesNotFound(id))
    }

    async fn scene_mappings(&self, tvdb_id: i64) -> anyhow::Result<Vec<SceneMapping>> {
        let mut by_id = self
            .db
            .call(move |conn| store::scene::by_tvdb_ids(conn, &[tvdb_id]))
            .await?;
        Ok(by_id.remove(&tvdb_id).unwrap_or_default())
    }

    /// `GetSceneSeasonMappings`.
    async fn scene_season_mappings(
        &self,
        series: &Series,
        episodes: &[Episode],
    ) -> anyhow::Result<Vec<SeasonMapping>> {
        let scene_mappings = self.scene_mappings(series.tvdb_id).await?;
        // Grouped by (scene season or season, season), in first-seen order.
        let mut groups: Vec<(i64, Vec<Episode>)> = Vec::new();
        for e in episodes {
            let key = (e.scene_season_number.unwrap_or(e.season_number)) * 100_000 + e.season_number;
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, list)) => list.push(e.clone()),
                None => groups.push((key, vec![e.clone()])),
            }
        }
        let mut out: Vec<SeasonMapping> = Vec::new();
        for (_, group) in groups {
            for episode_mapping in episode_mappings(series, &group[0], &scene_mappings) {
                let candidate = SeasonMapping {
                    episodes: group.clone(),
                    search_mode: episode_mapping.search_mode,
                    scene_titles: episode_mapping.scene_titles.clone(),
                    season_number: episode_mapping.season_number,
                    episode_mapping,
                };
                match out.iter_mut().find(|m| {
                    m.season_number == candidate.season_number && m.search_mode == candidate.search_mode
                }) {
                    Some(existing) => {
                        existing.episodes.extend(candidate.episodes);
                        existing.scene_titles.extend(candidate.scene_titles);
                    }
                    None => out.push(candidate),
                }
            }
        }
        for m in &mut out {
            dedup_episodes(&mut m.episodes);
            dedup_titles(&mut m.scene_titles);
        }
        Ok(out)
    }

    /// `GetSceneEpisodeMappings(series, episode)`.
    async fn scene_episode_mappings(
        &self,
        series: &Series,
        episode: &Episode,
    ) -> anyhow::Result<Vec<EpisodeMapping>> {
        let scene_mappings = self.scene_mappings(series.tvdb_id).await?;
        let mut out: Vec<EpisodeMapping> = Vec::new();
        for mapping in episode_mappings(series, episode, &scene_mappings) {
            match out.iter_mut().find(|m| {
                m.season_number == mapping.season_number
                    && m.episode_number == mapping.episode_number
                    && m.search_mode == mapping.search_mode
            }) {
                Some(existing) => existing.scene_titles.extend(mapping.scene_titles),
                None => out.push(mapping),
            }
        }
        for m in &mut out {
            dedup_titles(&mut m.scene_titles);
        }
        Ok(out)
    }

    async fn search_single(
        &self,
        series: &Series,
        episode: &Episode,
        monitored_only: bool,
        trigger: Trigger,
        now: DateTime<Utc>,
    ) -> Result<Vec<Decision>, SearchError> {
        let mut decisions = Vec::new();
        for mapping in self.scene_episode_mappings(series, episode).await? {
            let criteria = SearchCriteria {
                series: series.clone(),
                episodes: vec![mapping.episode.clone()],
                kind: SearchKind::SingleEpisode {
                    season_number: mapping.season_number,
                    episode_number: mapping.episode_number,
                },
                monitored_episodes_only: monitored_only,
                user_invoked_search: trigger.user_invoked,
                interactive_search: trigger.interactive,
            };
            decisions.extend(
                self.dispatch(criteria, mapping.search_mode, mapping.scene_titles, now)
                    .await?,
            );
        }
        Ok(dedupe(decisions))
    }

    async fn search_daily(
        &self,
        series: &Series,
        episode: &Episode,
        monitored_only: bool,
        trigger: Trigger,
        now: DateTime<Utc>,
    ) -> Result<Vec<Decision>, SearchError> {
        let air_date = episode
            .air_date
            .ok_or_else(|| SearchError::Failed("Air date is missing".into()))?;
        let (criteria, titles) = self
            .criteria_for(
                series,
                vec![episode.clone()],
                SearchKind::DailyEpisode { air_date },
                monitored_only,
                trigger,
            )
            .await?;
        Ok(dedupe(
            self.dispatch(criteria, SearchMode::Default, titles, now).await?,
        ))
    }

    async fn search_anime(
        &self,
        series: &Series,
        episode: &Episode,
        monitored_only: bool,
        trigger: Trigger,
        is_season_search: bool,
        now: DateTime<Utc>,
    ) -> Result<Vec<Decision>, SearchError> {
        let kind = SearchKind::AnimeEpisode {
            season_number: episode.scene_season_number.unwrap_or(episode.season_number),
            episode_number: episode.scene_episode_number.unwrap_or(episode.episode_number),
            absolute_episode_number: episode
                .scene_absolute_episode_number
                .or(episode.absolute_episode_number)
                .unwrap_or(0),
            is_season_search,
        };
        let (criteria, titles) = self
            .criteria_for(series, vec![episode.clone()], kind, monitored_only, trigger)
            .await?;
        Ok(dedupe(
            self.dispatch(criteria, SearchMode::Default, titles, now).await?,
        ))
    }

    async fn search_special(
        &self,
        series: &Series,
        episodes: Vec<Episode>,
        monitored_only: bool,
        trigger: Trigger,
        now: DateTime<Utc>,
    ) -> Result<Vec<Decision>, SearchError> {
        let mut decisions = Vec::new();
        let (criteria, titles) = self
            .criteria_for(
                series,
                episodes.clone(),
                SearchKind::SpecialEpisode,
                monitored_only,
                trigger,
            )
            .await?;
        // "<series> <episode title>" for every titled episode.
        let clean_titles: Vec<String> = {
            let mut out: Vec<String> = Vec::new();
            for t in &titles {
                let c = request::clean_scene_title(t);
                if !out.contains(&c) {
                    out.push(c);
                }
            }
            out
        };
        let mut query_titles: Vec<String> = Vec::new();
        for e in episodes
            .iter()
            .filter(|e| e.title.as_deref().is_some_and(|t| !t.trim().is_empty()))
            .filter(|e| trigger.interactive || !monitored_only || e.monitored)
        {
            let episode_title = request::clean_scene_title(e.title.as_deref().unwrap_or(""));
            for t in &clean_titles {
                let q = format!("{t} {episode_title}");
                if !query_titles.iter().any(|x| x.eq_ignore_ascii_case(&q)) {
                    query_titles.push(q);
                }
            }
        }
        decisions.extend(
            self.dispatch_with_kind(
                criteria,
                SearchMode::Default,
                titles,
                request::SearchKind::Special {
                    episode_query_titles: query_titles,
                },
                now,
            )
            .await?,
        );
        for episode in &episodes {
            if !trigger.interactive && monitored_only && !episode.monitored {
                continue;
            }
            decisions.extend(
                self.search_single(series, episode, monitored_only, trigger, now)
                    .await?,
            );
        }
        Ok(dedupe(decisions))
    }

    async fn search_anime_season(
        &self,
        series: &Series,
        episodes: Vec<Episode>,
        monitored_only: bool,
        trigger: Trigger,
        now: DateTime<Utc>,
    ) -> Result<Vec<Decision>, SearchError> {
        let mut decisions = Vec::new();
        let to_search: Vec<Episode> = episodes
            .iter()
            .filter(|e| trigger.interactive || !monitored_only || e.monitored)
            .filter(|e| e.air_date_utc.is_some_and(|d| d < now))
            .cloned()
            .collect();
        let mappings = self.scene_season_mappings(series, &to_search).await?;
        let mut seasons: Vec<i64> = Vec::new();
        for m in &mappings {
            if !seasons.contains(&m.season_number) {
                seasons.push(m.season_number);
            }
        }
        for season_number in seasons {
            let (criteria, titles) = self
                .criteria_for(
                    series,
                    episodes.clone(),
                    SearchKind::AnimeSeason { season_number },
                    monitored_only,
                    trigger,
                )
                .await?;
            decisions.extend(self.dispatch(criteria, SearchMode::Default, titles, now).await?);
        }
        for episode in &to_search {
            decisions.extend(
                self.search_anime(series, episode, monitored_only, trigger, true, now)
                    .await?,
            );
        }
        Ok(dedupe(decisions))
    }

    async fn search_daily_season(
        &self,
        series: &Series,
        episodes: Vec<Episode>,
        monitored_only: bool,
        trigger: Trigger,
        now: DateTime<Utc>,
    ) -> Result<Vec<Decision>, SearchError> {
        let mut decisions = Vec::new();
        let to_search: Vec<Episode> = episodes
            .iter()
            .filter(|e| trigger.interactive || !monitored_only || e.monitored)
            .filter(|e| e.air_date.is_some())
            .cloned()
            .collect();
        let mut years: Vec<(i64, Vec<Episode>)> = Vec::new();
        for e in to_search {
            let year = e.air_date.unwrap().year() as i64;
            match years.iter_mut().find(|(y, _)| *y == year) {
                Some((_, list)) => list.push(e),
                None => years.push((year, vec![e])),
            }
        }
        for (year, year_episodes) in years {
            if year_episodes.len() > 1 {
                let (criteria, titles) = self
                    .criteria_for(
                        series,
                        year_episodes,
                        SearchKind::DailySeason { year },
                        monitored_only,
                        trigger,
                    )
                    .await?;
                decisions.extend(self.dispatch(criteria, SearchMode::Default, titles, now).await?);
            } else {
                decisions.extend(
                    self.search_daily(series, &year_episodes[0], monitored_only, trigger, now)
                        .await?,
                );
            }
        }
        Ok(dedupe(decisions))
    }

    /// `Get<TSpec>(series, episodes, ...)`: the criteria with the scene names
    /// of the seasons involved, and the series title.
    async fn criteria_for(
        &self,
        series: &Series,
        episodes: Vec<Episode>,
        kind: SearchKind,
        monitored_only: bool,
        trigger: Trigger,
    ) -> anyhow::Result<(SearchCriteria, Vec<String>)> {
        let mappings = self.scene_mappings(series.tvdb_id).await?;
        let seasons: Vec<i64> = episodes.iter().map(|e| e.season_number).collect();
        let scene_seasons: Vec<i64> = episodes
            .iter()
            .map(|e| e.scene_season_number.unwrap_or(e.season_number))
            .collect();
        let mut titles = scene_names(&mappings, &seasons, &scene_seasons);
        if !titles.iter().any(|t| t.eq_ignore_ascii_case(&series.title)) {
            titles.push(series.title.clone());
        }
        Ok((
            SearchCriteria {
                series: series.clone(),
                episodes,
                kind,
                monitored_episodes_only: monitored_only,
                user_invoked_search: trigger.user_invoked,
                interactive_search: trigger.interactive,
            },
            titles,
        ))
    }

    fn criteria_for_season_mapping(
        &self,
        series: &Series,
        mapping: &SeasonMapping,
        kind: SearchKind,
        monitored_only: bool,
        trigger: Trigger,
    ) -> SearchCriteria {
        SearchCriteria {
            series: series.clone(),
            episodes: mapping.episodes.clone(),
            kind,
            monitored_episodes_only: monitored_only,
            user_invoked_search: trigger.user_invoked,
            interactive_search: trigger.interactive,
        }
    }

    async fn dispatch(
        &self,
        criteria: SearchCriteria,
        mode: SearchMode,
        scene_titles: Vec<String>,
        now: DateTime<Utc>,
    ) -> Result<Vec<Decision>, SearchError> {
        let kind = request_kind(&criteria.kind);
        self.dispatch_with_kind(criteria, mode, scene_titles, kind, now)
            .await
    }

    /// `Dispatch`: every indexer enabled for the search whose tags fit the
    /// series, the episodes' last search time, then the decisions.
    async fn dispatch_with_kind(
        &self,
        criteria: SearchCriteria,
        mode: SearchMode,
        scene_titles: Vec<String>,
        kind: request::SearchKind,
        now: DateTime<Utc>,
    ) -> Result<Vec<Decision>, SearchError> {
        let series = &criteria.series;
        let request_criteria = request::SearchCriteria {
            series: SeriesIds {
                tvdb_id: series.tvdb_id,
                imdb_id: series.imdb_id.clone(),
                tv_rage_id: series.tv_rage_id,
                tv_maze_id: series.tv_maze_id,
                tmdb_id: series.tmdb_id,
            },
            scene_titles,
            mode,
            kind,
        };
        tracing::info!(series = series.title, kind = ?criteria.kind, "searching indexers");
        let found = self
            .indexers
            .search(&request_criteria, criteria.interactive_search, &series.tags, now)
            .await?;
        tracing::debug!(
            reports = found.releases.len(),
            indexers = found.indexers_searched,
            "reports found"
        );
        if found.indexers_searched > 0 {
            let ids: Vec<i64> = criteria.episodes.iter().map(|e| e.id).collect();
            self.db
                .call(move |conn| store::episodes::set_last_search_time(conn, &ids, now))
                .await?;
        }
        let releases: Vec<ReleaseInfo> = found.releases;
        let queue = self.queue.as_ref().map(|q| q()).unwrap_or_default();
        let decisions = self
            .db
            .call(move |conn| {
                let catalog = decision::Catalog::load(conn, now)?.with_queue(queue);
                let lookup = decision::StoreLookup::new(conn);
                Ok(decision::decide(
                    &catalog,
                    &lookup,
                    now,
                    releases,
                    DecisionInput::Search(&criteria),
                ))
            })
            .await?;
        Ok(decisions)
    }
}

/// `GetSceneEpisodeMappings(series, episode, sceneMappings)`.
fn episode_mappings(
    series: &Series,
    episode: &Episode,
    scene_mappings: &[SceneMapping],
) -> Vec<EpisodeMapping> {
    let mut out = Vec::new();
    let mut include_global = true;
    let non_negative = |v: Option<i64>| v.filter(|n| *n >= 0);
    for mapping in scene_mappings {
        let ignore_scene_numbering = matches!(
            mapping.scene_origin.as_deref(),
            Some("tvdb") | Some("unknown:tvdb")
        );
        let mapping_scene_season = non_negative(mapping.scene_season_number);
        let mapping_season = non_negative(mapping.season_number);
        let mapped_season = if ignore_scene_numbering {
            episode.season_number
        } else {
            episode.scene_season_number.unwrap_or(episode.season_number)
        };
        let release_season = mapping_scene_season.unwrap_or(mapped_season);
        if mapping_scene_season.is_some() {
            let mapped_alt = mapping_season.or(mapping_scene_season).unwrap_or(mapped_season);
            if mapped_alt != mapped_season {
                continue;
            }
        } else if mapping_season.is_some_and(|s| s != episode.season_number) {
            continue;
        }
        let search_term = mapping.search_term.clone().unwrap_or_default();
        if search_term == series.title
            && mapping
                .filter_regex
                .as_deref()
                .is_none_or(|r| r.trim().is_empty())
        {
            include_global = false;
        }
        let search_mode = match mapping.search_mode {
            Some(m) => search_mode_from(m),
            None => {
                if mapping_scene_season.is_some() && series.clean_title != clean_series_title(&search_term) {
                    SearchMode::SearchTitle
                } else {
                    SearchMode::Default
                }
            }
        };
        if ignore_scene_numbering {
            out.push(EpisodeMapping {
                episode: episode.clone(),
                search_mode,
                scene_titles: vec![search_term],
                season_number: release_season,
                episode_number: episode.episode_number,
                absolute_episode_number: episode.absolute_episode_number,
            });
        } else {
            out.push(EpisodeMapping {
                episode: episode.clone(),
                search_mode,
                scene_titles: vec![search_term],
                season_number: release_season,
                episode_number: episode.scene_episode_number.unwrap_or(episode.episode_number),
                absolute_episode_number: episode
                    .scene_absolute_episode_number
                    .or(episode.absolute_episode_number),
            });
        }
    }
    if include_global {
        out.push(EpisodeMapping {
            episode: episode.clone(),
            search_mode: SearchMode::Default,
            scene_titles: vec![series.title.clone()],
            season_number: episode.scene_season_number.unwrap_or(episode.season_number),
            episode_number: episode.scene_episode_number.unwrap_or(episode.episode_number),
            // The originals read the scene season number here.
            absolute_episode_number: episode.scene_season_number.or(episode.absolute_episode_number),
        });
    }
    out
}

/// `SearchMode` flags: 0 default, 1 search id, 2 search title, 3 both.
pub fn search_mode_from(value: i64) -> SearchMode {
    match value {
        1 => SearchMode::SearchId,
        2 => SearchMode::SearchTitle,
        3 => SearchMode::Both,
        _ => SearchMode::Default,
    }
}

/// `SceneMappingService.GetSceneNames`: the English search terms of the
/// mappings for these seasons, or unbound mappings not from TVDB.
pub fn scene_names(mappings: &[SceneMapping], seasons: &[i64], scene_seasons: &[i64]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in mappings {
        let season = m.season_number.unwrap_or(-1);
        let scene_season = m.scene_season_number.unwrap_or(-1);
        let applies = seasons.contains(&season)
            || scene_seasons.contains(&scene_season)
            || (season == -1 && scene_season == -1 && m.scene_origin.as_deref() != Some("tvdb"));
        if !applies {
            continue;
        }
        let Some(term) = m.search_term.as_deref() else {
            continue;
        };
        if !term.chars().all(|c| (c as u32) <= 255) {
            continue;
        }
        if !out.iter().any(|t| t.eq_ignore_ascii_case(term)) {
            out.push(term.to_string());
        }
    }
    out
}

/// The request-level kind for a decision-level one.
fn request_kind(kind: &SearchKind) -> request::SearchKind {
    match kind {
        SearchKind::SingleEpisode {
            season_number,
            episode_number,
        } => request::SearchKind::SingleEpisode {
            season: *season_number,
            episode: *episode_number,
        },
        SearchKind::Season { season_number } => request::SearchKind::Season {
            season: *season_number,
        },
        SearchKind::AnimeEpisode {
            season_number,
            episode_number,
            absolute_episode_number,
            ..
        } => request::SearchKind::AnimeEpisode {
            absolute_episode: *absolute_episode_number,
            season: *season_number,
            episode: *episode_number,
        },
        SearchKind::AnimeSeason { season_number } => request::SearchKind::AnimeSeason {
            season: *season_number,
        },
        SearchKind::DailyEpisode { air_date } => request::SearchKind::DailyEpisode { air_date: *air_date },
        SearchKind::DailySeason { year } => request::SearchKind::DailySeason { year: *year },
        SearchKind::SpecialEpisode => request::SearchKind::Special {
            episode_query_titles: Vec::new(),
        },
    }
}

/// `DeDupeDecisions`: one decision per guid, the one with the fewest
/// rejections and then the higher indexer priority.
pub fn dedupe(decisions: Vec<Decision>) -> Vec<Decision> {
    let mut order: Vec<String> = Vec::new();
    let mut best: HashMap<String, Decision> = HashMap::new();
    for d in decisions {
        let guid = d.remote.release.guid.clone();
        match best.get(&guid) {
            None => {
                order.push(guid.clone());
                best.insert(guid, d);
            }
            Some(current) => {
                let better = d.rejections.len() < current.rejections.len()
                    || (d.rejections.len() == current.rejections.len()
                        && d.remote.release.indexer_priority < current.remote.release.indexer_priority);
                if better {
                    best.insert(guid, d);
                }
            }
        }
    }
    order.into_iter().filter_map(|g| best.remove(&g)).collect()
}

fn dedup_episodes(episodes: &mut Vec<Episode>) {
    let mut seen = Vec::new();
    episodes.retain(|e| {
        if seen.contains(&e.id) {
            false
        } else {
            seen.push(e.id);
            true
        }
    });
}

fn dedup_titles(titles: &mut Vec<String>) {
    let mut seen: Vec<String> = Vec::new();
    titles.retain(|t| {
        let lower = t.to_lowercase();
        if seen.contains(&lower) {
            false
        } else {
            seen.push(lower);
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::testing::{episode, series};

    fn mapping(
        term: &str,
        season: Option<i64>,
        scene_season: Option<i64>,
        origin: Option<&str>,
    ) -> SceneMapping {
        SceneMapping {
            id: 1,
            tvdb_id: 100_001,
            season_number: season,
            search_term: Some(term.into()),
            parse_term: Some(clean_series_title(term)),
            title: term.into(),
            mapping_type: Some("XemService".into()),
            scene_season_number: scene_season,
            filter_regex: None,
            scene_origin: origin.map(str::to_string),
            search_mode: None,
            comment: None,
        }
    }

    #[test]
    fn episode_mappings_follow_the_originals() {
        let s = series(1, "Show", 1);
        let mut e = episode(1, 1, 2, 3);
        e.scene_season_number = Some(5);
        e.scene_episode_number = Some(7);
        // No mappings: the implied one with scene numbers.
        let m = episode_mappings(&s, &e, &[]);
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].season_number, m[0].episode_number), (5, 7));
        assert_eq!(m[0].scene_titles, ["Show"]);
        // A TVDB-origin mapping uses TVDB numbers; one bound to another
        // season is skipped; an explicit mapping by the series' own name
        // replaces the implied one.
        let maps = [
            mapping("Show Alt", None, None, Some("tvdb")),
            mapping("Other Season", Some(9), None, None),
            mapping("Show", None, None, None),
        ];
        let m = episode_mappings(&s, &e, &maps);
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].season_number, m[0].episode_number), (2, 3));
        assert_eq!(m[0].scene_titles, ["Show Alt"]);
        assert_eq!((m[1].season_number, m[1].episode_number), (5, 7));
        assert_eq!(m[1].scene_titles, ["Show"]);
        // A release-season mapping that applies renumbers the release and
        // searches by title when its name differs.
        let maps = [mapping("Show Part Two", Some(5), Some(1), None)];
        let m = episode_mappings(&s, &e, &maps);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].season_number, 1);
        assert_eq!(m[0].search_mode, SearchMode::SearchTitle);
    }

    #[test]
    fn scene_names_pick_the_seasons_and_english_terms() {
        let maps = [
            mapping("Show", None, None, None),
            mapping("Show S2", Some(2), None, None),
            mapping("Show Scene 3", None, Some(3), None),
            mapping("Shōw Unicode", None, None, None),
            mapping("From TVDB", None, None, Some("tvdb")),
        ];
        assert_eq!(scene_names(&maps, &[1], &[1]), ["Show"]);
        assert_eq!(
            scene_names(&maps, &[2], &[3]),
            ["Show", "Show S2", "Show Scene 3"]
        );
    }

    #[test]
    fn dedupe_keeps_the_least_rejected() {
        let mk = |guid: &str, rejections: usize, priority: i64| {
            let mut remote = crate::decision::RemoteEpisode::new(ReleaseInfo {
                guid: guid.into(),
                indexer_priority: priority,
                ..Default::default()
            });
            remote.languages = vec![];
            Decision {
                remote,
                rejections: (0..rejections)
                    .map(|_| {
                        crate::decision::Rejection::permanent(crate::decision::RejectionReason::Unknown, "x")
                    })
                    .collect(),
            }
        };
        let out = dedupe(vec![
            mk("a", 2, 25),
            mk("b", 0, 25),
            mk("a", 1, 50),
            mk("a", 1, 10),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].remote.release.guid, "a");
        assert_eq!(out[0].rejections.len(), 1);
        assert_eq!(out[0].remote.release.indexer_priority, 10);
        assert_eq!(out[1].remote.release.guid, "b");
    }
}
