//! `RefreshSeriesService`, `RefreshEpisodeService` and `ShouldRefreshSeries`:
//! a series brought in line with what the metadata provider says, its
//! episodes added, updated and removed.
//!
//! The decisions are pure functions over the stored and the remote model, so
//! that the originals' rules can be tested without a provider; the
//! [`refresh_series_info`] wrapper asks the provider and writes the result in
//! one database call. The series is written by columns
//! ([`store::series::update_info`]), never whole: settings the user changed
//! while the refresh ran stay as they are.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

use crate::db::Db;
use crate::metadata::{MetadataError, MetadataProvider, SeriesInfo};
use crate::model::{Episode, NewItemMonitor, Season, Series, SeriesStatus, SeriesType};
use crate::store;

/// What a refresh changed, for the events that follow
/// (`EpisodeInfoRefreshedEvent`, `SeriesUpdatedEvent`).
#[derive(Debug, Clone)]
pub struct Refreshed {
    pub series: Series,
    pub added: Vec<Episode>,
    pub updated: Vec<Episode>,
    pub removed: Vec<Episode>,
    /// Updated anime episodes that got an absolute number they had not
    /// (`AbsoluteEpisodeNumberAdded`), by id.
    pub absolute_added: Vec<i64>,
}

/// Why a refresh of one series stopped.
#[derive(Debug)]
pub enum RefreshError {
    /// The provider does not know the series; it was marked deleted (the
    /// flag says whether this refresh changed its status).
    NotFound {
        series: Box<Series>,
        marked_deleted: bool,
    },
    Other(anyhow::Error),
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefreshError::NotFound { series, .. } => write!(
                f,
                "Series '{}' (tvdbid {}) was not found, it may have been removed from TheTVDB.",
                series.title, series.tvdb_id
            ),
            RefreshError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl From<anyhow::Error> for RefreshError {
    fn from(e: anyhow::Error) -> Self {
        RefreshError::Other(e)
    }
}

/// `RefreshSeriesInfo`: ask the provider, then write the series and its
/// episodes. `series` is the stored series as loaded before the call.
pub async fn refresh_series_info(
    db: &Db,
    provider: &dyn MetadataProvider,
    series_id: i64,
    now: DateTime<Utc>,
) -> Result<Refreshed, RefreshError> {
    let series = db
        .call(move |conn| {
            store::series::get(conn, series_id)?
                .ok_or_else(|| anyhow::anyhow!("Series with ID {series_id} does not exist"))
        })
        .await?;
    tracing::info!("Updating {}", series.title);
    let info = match provider.series_info(series.tvdb_id).await {
        Ok(info) => info,
        Err(MetadataError::SeriesNotFound(_)) => {
            let marked = series.status != SeriesStatus::Deleted;
            if marked {
                db.call(move |conn| store::series::set_status(conn, series_id, SeriesStatus::Deleted))
                    .await?;
                tracing::debug!("Series marked as deleted on tvdb for {}", series.title);
            }
            let mut series = series;
            series.status = SeriesStatus::Deleted;
            return Err(RefreshError::NotFound {
                series: Box::new(series),
                marked_deleted: marked,
            });
        }
        Err(e) => return Err(RefreshError::Other(e.into())),
    };
    let refreshed = db
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            // Read again inside the write: a change made since the provider
            // was asked is kept.
            let mut series = store::series::get(&tx, series_id)?
                .ok_or_else(|| anyhow::anyhow!("Series with ID {series_id} does not exist"))?;
            let SeriesInfo {
                series: remote,
                episodes: remote_episodes,
            } = info;
            apply_info(&mut series, remote, now);
            store::series::update_info(&tx, &series)?;
            let existing = store::episodes::list_episodes(&tx, series_id, None)?;
            let mut changes = merge_episodes(&series, existing, remote_episodes, now);
            store::episodes::delete_episodes(&tx, &changes.removed.iter().map(|e| e.id).collect::<Vec<_>>())?;
            for e in &changes.updated {
                store::episodes::update_episode(&tx, e)?;
            }
            if !changes.added.is_empty() {
                let first = store::sequence::next_ids(&tx, "episodes", changes.added.len())?;
                for (i, e) in changes.added.iter_mut().enumerate() {
                    e.id = first + i as i64;
                    store::episodes::insert_episode(&tx, e)?;
                }
            }
            tx.commit()?;
            Ok(Refreshed {
                series,
                added: changes.added,
                updated: changes.updated,
                removed: changes.removed,
                absolute_added: changes.absolute_added,
            })
        })
        .await?;
    tracing::info!("Finished episode refresh for series: {}.", refreshed.series.title);
    Ok(refreshed)
}

/// The series fields `RefreshSeriesInfo` takes from the provider, and the
/// seasons as `UpdateSeasons` merges them.
pub fn apply_info(series: &mut Series, info: Series, now: DateTime<Utc>) {
    if series.tvdb_id != info.tvdb_id {
        tracing::warn!(
            "Series '{}' (tvdbid {}) was replaced with '{}' (tvdbid {}), because the original was a duplicate.",
            series.title,
            series.tvdb_id,
            info.title,
            info.tvdb_id
        );
        series.tvdb_id = info.tvdb_id;
    }
    series.seasons = update_seasons(series, &info);
    series.title = info.title;
    series.year = info.year;
    series.title_slug = info.title_slug;
    series.tv_rage_id = info.tv_rage_id;
    series.tv_maze_id = info.tv_maze_id;
    series.tmdb_id = info.tmdb_id;
    series.imdb_id = info.imdb_id;
    series.mal_ids = info.mal_ids;
    series.anilist_ids = info.anilist_ids;
    series.air_time = info.air_time;
    series.overview = info.overview;
    series.original_language_id = info.original_language_id;
    series.status = info.status;
    series.clean_title = info.clean_title;
    series.sort_title = info.sort_title;
    series.last_info_sync = Some(now);
    series.runtime = info.runtime;
    series.images = info.images;
    series.network = info.network;
    series.first_aired = info.first_aired;
    series.last_aired = info.last_aired;
    series.ratings = info.ratings;
    series.actors = info.actors;
    series.genres = info.genres;
    series.certification = info.certification;
    // `new DirectoryInfo(path).FullName`: no trailing separator.
    if series.path.len() > 1 {
        series.path = series.path.trim_end_matches('/').to_string();
    }
}

/// `UpdateSeasons`: the provider's seasons (first of each number), known
/// ones keeping their monitoring, new ones monitored when the series
/// monitors new items, season 0 never.
pub fn update_seasons(series: &Series, info: &Series) -> Vec<Season> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for season in &info.seasons {
        if !seen.insert(season.season_number) {
            continue;
        }
        let mut season = season.clone();
        match series
            .seasons
            .iter()
            .find(|s| s.season_number == season.season_number)
        {
            Some(existing) => season.monitored = existing.monitored,
            None if season.season_number == 0 => {
                tracing::debug!(
                    "Ignoring season 0 for series [{}] {} by default",
                    series.tvdb_id,
                    series.title
                );
                season.monitored = false;
            }
            None => {
                let monitor = series.monitor_new_items == NewItemMonitor::All;
                tracing::debug!(
                    "New season ({}) for series: [{}] {}, setting monitored to {monitor}",
                    season.season_number,
                    series.tvdb_id,
                    series.title
                );
                season.monitored = monitor;
            }
        }
        out.push(season);
    }
    out
}

/// What [`merge_episodes`] decided.
#[derive(Debug, Default)]
pub struct EpisodeChanges {
    /// New rows, id 0, in the order the originals insert them.
    pub added: Vec<Episode>,
    pub updated: Vec<Episode>,
    pub removed: Vec<Episode>,
    pub absolute_added: Vec<i64>,
}

/// `RefreshEpisodeInfo` without the writes.
pub fn merge_episodes(
    series: &Series,
    mut existing: Vec<Episode>,
    remote: Vec<Episode>,
    now: DateTime<Utc>,
) -> EpisodeChanges {
    let has_existing = !existing.is_empty();
    // `DistinctBy(SeasonNumber, EpisodeNumber)`: the first one wins.
    let mut seen = std::collections::HashSet::new();
    let mut remote: Vec<Episode> = remote
        .into_iter()
        .filter(|e| seen.insert((e.season_number, e.episode_number)))
        .collect();
    if series.series_type == SeriesType::Anime {
        remote = map_absolute_episode_numbers(remote);
    }
    let ordered = order_episodes(series, remote);
    let mut per_season: HashMap<i64, usize> = HashMap::new();
    for e in &ordered {
        *per_season.entry(e.season_number).or_default() += 1;
    }
    let latest_season = series.seasons.iter().map(|s| s.season_number).max();
    let latest_has_finale = latest_season.is_some_and(|latest| {
        ordered
            .iter()
            .any(|e| e.season_number == latest && e.finale_type.is_some())
    });

    let mut changes = EpisodeChanges::default();
    for episode in &ordered {
        let found = existing.iter().position(|e| {
            e.season_number == episode.season_number && e.episode_number == episode.episode_number
        });
        let (mut target, is_new) = match found {
            Some(i) => {
                let target = existing.remove(i);
                if series.series_type == SeriesType::Anime
                    && target.absolute_episode_number.is_none()
                    && episode.absolute_episode_number.is_some()
                {
                    changes.absolute_added.push(target.id);
                }
                (target, false)
            }
            None => {
                let mut fresh = blank_episode();
                fresh.monitored = monitored_status(episode, &series.seasons);
                (fresh, true)
            }
        };
        target.series_id = series.id;
        target.tvdb_id = episode.tvdb_id;
        target.episode_number = episode.episode_number;
        target.season_number = episode.season_number;
        target.absolute_episode_number = episode.absolute_episode_number;
        target.aired_after_season_number = episode.aired_after_season_number;
        target.aired_before_season_number = episode.aired_before_season_number;
        target.aired_before_episode_number = episode.aired_before_episode_number;
        target.title = Some(episode.title.clone().unwrap_or_else(|| "TBA".to_string()));
        target.overview = episode.overview.clone();
        target.air_date = episode.air_date;
        target.air_date_utc = episode.air_date_utc;
        target.runtime = episode.runtime;
        target.finale_type = episode.finale_type.clone();
        target.ratings = episode.ratings;
        target.images = episode.images.clone();

        // TheTVDB lacks most series finales: the last episode of the last
        // season of an ended series, aired in the last two weeks, is one.
        if series.status == SeriesStatus::Ended
            && target.finale_type.is_none()
            && target.season_number > 0
            && Some(target.season_number) == latest_season
            && target.episode_number > 1
            && per_season.get(&target.season_number).copied() == Some(target.episode_number as usize)
            && target.air_date_utc.is_some_and(|d| d > now - Duration::days(14))
            && !latest_has_finale
        {
            target.finale_type = Some("series".to_string());
        }

        if is_new {
            changes.added.push(target);
        } else {
            changes.updated.push(target);
        }
    }

    unmonitor_readded_episodes(series, &mut changes.added, has_existing, now);

    // `AdjustMultiEpisodeAirTime` and `AdjustDirectToDvdAirDate` over the
    // new episodes first, then the updated, as the originals list them.
    {
        let mut all: Vec<&mut Episode> = changes
            .added
            .iter_mut()
            .chain(changes.updated.iter_mut())
            .collect();
        adjust_multi_episode_air_time(series, &mut all);
        adjust_direct_to_dvd_air_date(series, &mut all);
    }
    changes.removed = existing;
    changes
}

/// `new Episode()`.
pub(crate) fn blank_episode() -> Episode {
    Episode {
        id: 0,
        series_id: 0,
        tvdb_id: 0,
        season_number: 0,
        episode_number: 0,
        title: None,
        overview: None,
        episode_file_id: 0,
        absolute_episode_number: None,
        scene_absolute_episode_number: None,
        scene_season_number: None,
        scene_episode_number: None,
        unverified_scene_numbering: false,
        monitored: false,
        air_date: None,
        air_date_utc: None,
        runtime: 0,
        finale_type: None,
        images: Vec::new(),
        ratings: Default::default(),
        last_search_time: None,
        aired_after_season_number: None,
        aired_before_season_number: None,
        aired_before_episode_number: None,
    }
}

/// `GetMonitoredStatus`.
fn monitored_status(episode: &Episode, seasons: &[Season]) -> bool {
    if episode.episode_number == 0 && episode.season_number != 1 {
        return false;
    }
    seasons
        .iter()
        .find(|s| s.season_number == episode.season_number)
        .is_none_or(|s| s.monitored)
}

/// `UnmonitorReaddedEpisodes`.
fn unmonitor_readded_episodes(
    series: &Series,
    added: &mut [Episode],
    has_existing: bool,
    now: DateTime<Utc>,
) {
    if series.add_options.is_some() {
        return;
    }
    let threshold = now - Duration::days(14);
    let old = added
        .iter()
        .filter(|e| e.air_date_utc.is_some_and(|d| d < threshold))
        .count();
    if old == 0 {
        return;
    }
    if has_existing {
        tracing::warn!(
            "Show {} ({}) had {old} old episodes appear, please check monitored status.",
            series.tvdb_id,
            series.title
        );
    } else {
        let threshold = now - Duration::days(1);
        for e in added.iter_mut() {
            if e.air_date_utc.is_some_and(|d| d < threshold) {
                e.monitored = false;
            }
        }
        tracing::warn!(
            "Show {} ({}) had {old} old episodes appear, unmonitored aired episodes to prevent unexpected downloads.",
            series.tvdb_id,
            series.title
        );
    }
}

/// `AdjustMultiEpisodeAirTime`: episodes of a season that air the same day
/// are spread by the series' runtime, unless more than three do (outside
/// specials).
fn adjust_multi_episode_air_time(series: &Series, all: &mut [&mut Episode]) {
    type Day = (i64, Option<chrono::NaiveDate>);
    let mut groups: Vec<(Day, Vec<usize>)> = Vec::new();
    for (i, e) in all.iter().enumerate() {
        if e.air_date_utc.is_none() {
            continue;
        }
        let key = (e.season_number, e.air_date);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(i),
            None => groups.push((key, vec![i])),
        }
    }
    for ((season, _), mut members) in groups.into_iter().filter(|(_, m)| m.len() > 1) {
        if season != 0 && members.len() > 3 {
            tracing::debug!(
                "Not adjusting multi-episode air times for series {} season {season} since more than 3 episodes 'aired' on the same day",
                series.title
            );
            continue;
        }
        // A stable sort by season then episode number, as `OrderBy`.
        members.sort_by_key(|&i| (all[i].season_number, all[i].episode_number));
        for (count, i) in members.into_iter().enumerate() {
            if let Some(d) = all[i].air_date_utc {
                all[i].air_date_utc = Some(d + Duration::minutes(series.runtime * count as i64));
            }
        }
    }
}

/// `AdjustDirectToDvdAirDate`.
fn adjust_direct_to_dvd_air_date(series: &Series, all: &mut [&mut Episode]) {
    if series.status == SeriesStatus::Ended && all.iter().all(|e| e.air_date_utc.is_none()) {
        if let Some(first) = series.first_aired {
            for e in all.iter_mut() {
                e.air_date_utc = Some(first);
                e.air_date = Some(first.date_naive());
            }
        }
    }
}

/// `MapAbsoluteEpisodeNumbers`: one episode per absolute number (the latest
/// season's), then those without one.
fn map_absolute_episode_numbers(remote: Vec<Episode>) -> Vec<Episode> {
    let (mut with_abs, without): (Vec<Episode>, Vec<Episode>) = remote
        .into_iter()
        .partition(|e| e.absolute_episode_number.is_some());
    // `OrderByDescending(SeasonNumber)` is stable.
    with_abs.sort_by_key(|e| std::cmp::Reverse(e.season_number));
    let mut seen = std::collections::HashSet::new();
    with_abs.retain(|e| seen.insert(e.absolute_episode_number));
    with_abs.into_iter().chain(without).collect()
}

/// `OrderEpisodes`.
fn order_episodes(series: &Series, mut episodes: Vec<Episode>) -> Vec<Episode> {
    if series.series_type == SeriesType::Anime {
        let (mut with_abs, mut without): (Vec<Episode>, Vec<Episode>) = episodes
            .into_iter()
            .partition(|e| e.absolute_episode_number.is_some());
        with_abs.sort_by_key(|e| e.absolute_episode_number);
        without.sort_by_key(|e| (e.season_number, e.episode_number));
        with_abs.extend(without);
        return with_abs;
    }
    episodes.sort_by_key(|e| (e.season_number, e.episode_number));
    episodes
}

/// `ShouldRefreshSeries.ShouldRefresh`, for the scheduled refresh.
pub fn should_refresh(series: &Series, episodes: &[Episode], now: DateTime<Utc>) -> bool {
    // `LastInfoSync < now - 30 days`: a series never synced compares as
    // false in the originals (a null nullable), and falls through.
    if series
        .last_info_sync
        .is_some_and(|d| d < now - Duration::days(30))
    {
        tracing::trace!(
            "Series {} last updated more than 30 days ago, should refresh.",
            series.title
        );
        return true;
    }
    let aired_tba = episodes.iter().any(|e| {
        e.season_number > 0 && e.air_date_utc.is_some_and(|d| d < now) && e.title.as_deref() == Some("TBA")
    });
    if aired_tba {
        tracing::trace!(
            "Series {} with at least one aired episode with TBA title, should refresh.",
            series.title
        );
        return true;
    }
    if series
        .last_info_sync
        .is_some_and(|d| d >= now - Duration::hours(6))
    {
        tracing::trace!(
            "Series {} last updated less than 6 hours ago, should not be refreshed.",
            series.title
        );
        return false;
    }
    if series.status != SeriesStatus::Ended {
        tracing::trace!("Series {} is not ended, should refresh.", series.title);
        return true;
    }
    // `MaxBy(AirDateUtc)`: nulls sort first, so an aired one wins if any.
    let last = episodes.iter().filter_map(|e| e.air_date_utc).max();
    if last.is_some_and(|d| d > now - Duration::days(30)) {
        tracing::trace!(
            "Last episode in {} aired less than 30 days ago, should refresh.",
            series.title
        );
        return true;
    }
    tracing::trace!("Series {} ended long ago, should not be refreshed.", series.title);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Ratings;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 9, 12, 0, 0).unwrap()
    }

    fn series() -> Series {
        let mut s = crate::store::series::tests::sample(7);
        s.seasons = vec![
            Season {
                season_number: 0,
                monitored: false,
                images: vec![],
            },
            Season {
                season_number: 1,
                monitored: true,
                images: vec![],
            },
        ];
        s.status = SeriesStatus::Continuing;
        s.runtime = 30;
        s
    }

    fn ep(season: i64, number: i64, aired: Option<DateTime<Utc>>) -> Episode {
        let mut e = blank_episode();
        e.season_number = season;
        e.episode_number = number;
        e.tvdb_id = season * 100 + number;
        e.title = Some(format!("E{number}"));
        e.air_date_utc = aired;
        e.air_date = aired.map(|d| d.date_naive());
        e.ratings = Ratings::default();
        e
    }

    #[test]
    fn new_seasons_follow_monitor_new_items() {
        let mut s = series();
        let mut info = series();
        info.seasons.push(Season {
            season_number: 2,
            monitored: true,
            images: vec![],
        });
        info.seasons[1].monitored = false;
        let seasons = update_seasons(&s, &info);
        assert_eq!(
            seasons
                .iter()
                .map(|x| (x.season_number, x.monitored))
                .collect::<Vec<_>>(),
            [(0, false), (1, true), (2, true)]
        );
        s.monitor_new_items = NewItemMonitor::None;
        assert!(!update_seasons(&s, &info)[2].monitored);
    }

    #[test]
    fn episodes_are_added_updated_and_removed() {
        let s = series();
        let mut old = ep(1, 1, Some(now() - Duration::days(30)));
        old.id = 10;
        old.monitored = false;
        old.episode_file_id = 99;
        let mut gone = ep(1, 9, None);
        gone.id = 11;
        let remote = vec![
            ep(1, 2, Some(now() + Duration::days(7))),
            ep(1, 1, Some(now() - Duration::days(30))),
            ep(1, 1, None), // a duplicate, dropped
            ep(0, 1, None),
        ];
        let changes = merge_episodes(&s, vec![old, gone], remote, now());
        assert_eq!(changes.updated.len(), 1);
        assert_eq!(changes.updated[0].id, 10);
        assert!(
            !changes.updated[0].monitored,
            "an existing episode keeps its monitoring"
        );
        assert_eq!(changes.updated[0].episode_file_id, 99);
        assert_eq!(
            changes
                .added
                .iter()
                .map(|e| (e.season_number, e.episode_number))
                .collect::<Vec<_>>(),
            [(0, 1), (1, 2)],
            "new episodes in season then episode order"
        );
        assert!(!changes.added[0].monitored, "season 0 is not monitored");
        assert!(changes.added[1].monitored);
        assert_eq!(changes.removed.iter().map(|e| e.id).collect::<Vec<_>>(), [11]);
    }

    #[test]
    fn a_first_refresh_unmonitors_episodes_aired_long_ago() {
        let s = series();
        let remote = vec![
            ep(1, 1, Some(now() - Duration::days(400))),
            ep(1, 2, Some(now() - Duration::hours(2))),
        ];
        let changes = merge_episodes(&s, vec![], remote, now());
        assert!(!changes.added[0].monitored);
        assert!(changes.added[1].monitored, "aired within the day");
        let mut added = s.clone();
        added.add_options = Some(serde_json::json!({}));
        let changes = merge_episodes(
            &added,
            vec![],
            vec![ep(1, 1, Some(now() - Duration::days(400)))],
            now(),
        );
        assert!(changes.added[0].monitored, "a series being added keeps them");
    }

    #[test]
    fn same_day_episodes_are_spread_by_the_runtime() {
        let s = series();
        let day = Utc.with_ymd_and_hms(2026, 1, 1, 20, 0, 0).unwrap();
        let changes = merge_episodes(&s, vec![], vec![ep(1, 2, Some(day)), ep(1, 1, Some(day))], now());
        let times: Vec<_> = changes.added.iter().map(|e| e.air_date_utc.unwrap()).collect();
        assert_eq!(times, [day, day + Duration::minutes(30)]);
    }

    #[test]
    fn an_ended_series_gets_its_finale() {
        let mut s = series();
        s.status = SeriesStatus::Ended;
        let remote = vec![
            ep(1, 1, Some(now() - Duration::days(9))),
            ep(1, 2, Some(now() - Duration::days(2))),
        ];
        let changes = merge_episodes(&s, vec![], remote, now());
        assert_eq!(changes.added[1].finale_type.as_deref(), Some("series"));
        assert_eq!(changes.added[0].finale_type, None);
    }

    #[test]
    fn anime_keeps_one_episode_per_absolute_number() {
        let mut s = series();
        s.series_type = SeriesType::Anime;
        let mut a = ep(1, 1, None);
        a.absolute_episode_number = Some(1);
        let mut b = ep(2, 1, None);
        b.absolute_episode_number = Some(1);
        let changes = merge_episodes(&s, vec![], vec![a, b], now());
        assert_eq!(changes.added.len(), 1);
        assert_eq!(changes.added[0].season_number, 2);
    }

    #[test]
    fn should_refresh_follows_the_original() {
        let mut s = series();
        s.last_info_sync = Some(now() - Duration::hours(1));
        assert!(!should_refresh(&s, &[], now()));
        assert!(should_refresh(
            &s,
            &[ep(1, 1, Some(now() - Duration::days(1)))].map(|mut e| {
                e.title = Some("TBA".into());
                e
            }),
            now()
        ));
        s.last_info_sync = Some(now() - Duration::days(40));
        assert!(should_refresh(&s, &[], now()));
        s.last_info_sync = Some(now() - Duration::days(2));
        assert!(should_refresh(&s, &[], now()), "continuing");
        s.status = SeriesStatus::Ended;
        assert!(!should_refresh(
            &s,
            &[ep(1, 1, Some(now() - Duration::days(90)))],
            now()
        ));
        assert!(should_refresh(
            &s,
            &[ep(1, 1, Some(now() - Duration::days(9)))],
            now()
        ));
    }
}
