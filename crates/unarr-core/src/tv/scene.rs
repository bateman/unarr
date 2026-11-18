//! `SceneMappingService.UpdateMappings` and `XemService`: the alternate
//! titles from the Servarr services and XEM, and XEM's scene numbering of a
//! series' episodes.

use crate::sync::Recover;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::db::Db;
use crate::metadata::augment::{Augment, RemoteSceneMapping, XemMapping};
use crate::model::{Episode, SceneMapping, Series};
use crate::store;

/// The two `ISceneMappingProvider`s, in the order the originals' container
/// hands them over, by the type name their rows carry.
const SERVICES_PROVIDER: &str = "ServicesProvider";
const XEM_SERVICE: &str = "XemService";

/// What the scene services remember between calls: when the mappings were
/// last updated, XEM's list of series.
#[derive(Default)]
pub struct SceneState {
    /// `_updatedAfterStartup` and the `_findByTvdbIdCache` age.
    mappings_updated: Mutex<Option<Instant>>,
    /// `XemService._cache`: the XEM series, and when they were read.
    xem_series: Mutex<Option<(Instant, HashSet<i64>)>>,
}

impl SceneState {
    /// `Handle(SeriesRefreshStartingEvent)` of `SceneMappingService`: a
    /// manual refresh updates the mappings when none happened since the
    /// start, or the last is more than a minute old.
    pub fn mappings_due(&self) -> bool {
        match *self.mappings_updated.lock().recover() {
            None => true,
            Some(at) => at.elapsed() > Duration::from_secs(60),
        }
    }

    /// `Handle(SeriesAddedEvent)`: only when none happened since the start.
    pub fn mappings_never_updated(&self) -> bool {
        self.mappings_updated.lock().recover().is_none()
    }

    fn xem_expired(&self, ttl: Duration) -> bool {
        match &*self.xem_series.lock().recover() {
            None => true,
            Some((at, _)) => at.elapsed() > ttl,
        }
    }
}

/// `UpdateMappings`: each provider's list replaces its rows when it is not
/// empty; a provider that fails or answers nothing leaves its rows.
pub async fn update_mappings(db: &Db, augment: &Augment, state: &SceneState) {
    tracing::info!("Updating Scene mappings");
    *state.mappings_updated.lock().recover() = Some(Instant::now());
    let providers: [(&str, anyhow::Result<Vec<RemoteSceneMapping>>); 2] = [
        (SERVICES_PROVIDER, augment.scene_mappings().await),
        (XEM_SERVICE, augment.xem_names().await),
    ];
    for (kind, fetched) in providers {
        let mappings = match fetched {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, "Failed to Update Scene Mappings");
                continue;
            }
        };
        if mappings.is_empty() {
            tracing::warn!("Received empty list of mapping. will not update");
            continue;
        }
        let rows: Vec<SceneMapping> = mappings
            .into_iter()
            .filter(|m| {
                let valid = m.title.as_deref().is_some_and(|t| !t.trim().is_empty())
                    && m.search_term.as_deref().is_some_and(|t| !t.trim().is_empty());
                if !valid {
                    tracing::warn!("Invalid scene mapping found for: {}, skipping", m.tvdb_id);
                }
                valid
            })
            .map(|m| to_row(m, kind))
            .collect();
        let kind = kind.to_string();
        if let Err(e) = db
            .call(move |conn| {
                let tx = conn.unchecked_transaction()?;
                store::scene::replace_type(&tx, &kind, &rows)?;
                tx.commit()?;
                Ok(())
            })
            .await
        {
            tracing::error!(error = %e, "Failed to Update Scene Mappings");
        }
    }
}

fn to_row(m: RemoteSceneMapping, kind: &str) -> SceneMapping {
    let title = m.title.unwrap_or_default();
    SceneMapping {
        id: 0,
        tvdb_id: m.tvdb_id,
        season_number: m.season_number,
        search_term: m.search_term,
        parse_term: Some(crate::parser::title::clean_series_title(&title)),
        title,
        mapping_type: Some(kind.to_string()),
        scene_season_number: m.scene_season_number,
        filter_regex: m.filter_regex,
        scene_origin: m.scene_origin,
        search_mode: m.search_mode.and_then(|v| match v {
            serde_json::Value::Number(n) => n.as_i64(),
            serde_json::Value::String(s) => match s.to_ascii_lowercase().as_str() {
                "default" => Some(0),
                "searchid" => Some(1),
                "searchtitle" => Some(2),
                _ => None,
            },
            _ => None,
        }),
        comment: m.comment,
    }
}

/// `XemService.UpdateXemSeriesIds`: an empty or failed answer keeps the
/// list there was (`ExtendTTL`).
async fn update_xem_series_ids(augment: &Augment, state: &SceneState) {
    let keep = |state: &SceneState| {
        let mut cached = state.xem_series.lock().recover();
        let ids = cached.take().map(|(_, ids)| ids).unwrap_or_default();
        *cached = Some((Instant::now(), ids));
    };
    match augment.xem_series_ids().await {
        Ok(ids) if !ids.is_empty() => {
            *state.xem_series.lock().recover() = Some((Instant::now(), ids.into_iter().collect()));
        }
        Ok(_) => {
            keep(state);
            tracing::warn!("Failed to update Xem series list.");
        }
        Err(e) => {
            keep(state);
            tracing::warn!(error = %e, "Failed to update Xem series list.");
        }
    }
}

/// `XemService.Handle(SeriesRefreshStartingEvent)`.
pub async fn refresh_starting(augment: &Augment, state: &SceneState, manual: bool) {
    if manual && state.xem_expired(Duration::from_secs(60)) {
        update_xem_series_ids(augment, state).await;
    }
}

/// `XemService.Handle(SeriesUpdatedEvent)`: the scene numbering of a series
/// XEM knows (or one that used it before), from XEM's mappings.
pub async fn series_updated(db: &Db, augment: &Augment, state: &SceneState, series: &Series) {
    if state.xem_expired(Duration::from_secs(3 * 3600)) {
        update_xem_series_ids(augment, state).await;
    }
    let known = {
        let cached = state.xem_series.lock().recover();
        match &*cached {
            Some((_, ids)) if !ids.is_empty() => Some(ids.contains(&series.tvdb_id)),
            _ => None,
        }
    };
    let Some(known) = known else {
        tracing::debug!("Scene numbering is not available");
        return;
    };
    if !known && !series.use_scene_numbering {
        tracing::debug!(
            "Scene numbering is not available for {} [{}]",
            series.title,
            series.tvdb_id
        );
        return;
    }
    tracing::debug!("Updating scene numbering mapping for: {}", series.title);
    let mappings = match augment.xem_mappings(series.tvdb_id).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "Error updating scene numbering mappings for {}", series.title);
            return;
        }
    };
    if mappings.is_empty() && !series.use_scene_numbering {
        tracing::debug!("Mappings for: {} are empty, skipping", series.title);
        return;
    }
    let (series_id, title) = (series.id, series.title.clone());
    let result = db
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut episodes = store::episodes::list_episodes(&tx, series_id, None)?;
            apply_xem(&title, &mut episodes, &mappings);
            for e in &episodes {
                store::episodes::update_episode(&tx, e)?;
            }
            store::series::set_use_scene_numbering(&tx, series_id, !mappings.is_empty())?;
            tx.commit()?;
            Ok(())
        })
        .await;
    match result {
        Ok(()) => tracing::debug!("XEM mapping updated for {}", series.title),
        Err(e) => tracing::error!(error = %e, "Error updating scene numbering mappings for {}", series.title),
    }
}

/// `PerformUpdate`'s changes to the episodes.
pub fn apply_xem(title: &str, episodes: &mut [Episode], mappings: &[XemMapping]) {
    for e in episodes.iter_mut() {
        e.scene_absolute_episode_number = None;
        e.scene_season_number = None;
        e.scene_episode_number = None;
        e.unverified_scene_numbering = false;
    }
    for m in mappings {
        let Some(scene) = m.scene else { continue };
        let matching: Vec<usize> = episodes
            .iter()
            .enumerate()
            .filter(|(_, e)| e.season_number == m.tvdb.season && e.episode_number == m.tvdb.episode)
            .map(|(i, _)| i)
            .collect();
        let [i] = matching[..] else {
            tracing::debug!("Information hasn't been added to TheTVDB yet, skipping");
            continue;
        };
        if scene.absolute == 0 && scene.season == 0 && scene.episode == 0 {
            tracing::debug!(
                "Mapping for {title} S{:02}E{:02} is invalid, skipping",
                m.tvdb.season,
                m.tvdb.episode
            );
            continue;
        }
        episodes[i].scene_absolute_episode_number = Some(scene.absolute);
        episodes[i].scene_season_number = Some(scene.season);
        episodes[i].scene_episode_number = Some(scene.episode);
    }
    if episodes
        .iter()
        .any(|e| e.scene_episode_number.is_some() && e.scene_season_number != Some(0))
    {
        extrapolate(episodes, mappings);
    }
}

/// `ExtrapolateMappings`.
fn extrapolate(episodes: &mut [Episode], mappings: &[XemMapping]) {
    let mappings: Vec<(
        crate::metadata::augment::XemValues,
        crate::metadata::augment::XemValues,
    )> = mappings
        .iter()
        .filter_map(|m| m.scene.map(|s| (s, m.tvdb)))
        .collect();
    if mappings.is_empty() {
        return;
    }
    let mapped_seasons: HashSet<i64> = episodes
        .iter()
        .filter(|e| e.season_number != 0 && e.scene_episode_number.is_some())
        .map(|e| e.season_number)
        .collect();
    let mut scene_episodes: HashMap<i64, HashSet<i64>> = HashMap::new();
    let mut first_tvdb_episode: HashMap<i64, i64> = HashMap::new();
    for (scene, tvdb) in &mappings {
        scene_episodes
            .entry(scene.season)
            .or_default()
            .insert(scene.episode);
        let first = first_tvdb_episode.entry(tvdb.season).or_insert(tvdb.episode);
        *first = (*first).min(tvdb.episode);
    }
    let last_scene_season = mappings.iter().map(|(s, _)| s.season).max().unwrap_or(0);
    let last_tvdb_season = mappings.iter().map(|(_, t)| t.season).max().unwrap_or(0);

    for e in episodes.iter_mut() {
        if e.season_number == 0 || e.scene_episode_number.is_some() {
            continue;
        }
        if mapped_seasons.contains(&e.season_number) {
            if first_tvdb_episode
                .get(&e.season_number)
                .is_some_and(|f| *f <= e.episode_number)
            {
                e.unverified_scene_numbering = true;
                continue;
            }
            if scene_episodes
                .get(&e.season_number)
                .is_some_and(|s| s.contains(&e.episode_number))
            {
                e.unverified_scene_numbering = true;
                continue;
            }
        } else if last_scene_season != last_tvdb_season && e.season_number > last_tvdb_season {
            e.unverified_scene_numbering = true;
        }
    }

    for e in episodes.iter_mut() {
        if e.season_number == 0
            || e.scene_episode_number.is_some()
            || e.season_number < last_tvdb_season
            || !e.unverified_scene_numbering
        {
            continue;
        }
        let season: Vec<_> = mappings
            .iter()
            .filter(|(_, t)| t.season == e.season_number)
            .collect();
        if season.iter().any(|(_, t)| t.episode >= e.episode_number) {
            continue;
        }
        // `OrderBy(...).Last()`: the last of the maximal ones, which is
        // what `max_by_key` returns.
        if let Some((last_scene, last_tvdb)) = season.iter().max_by_key(|(_, t)| t.episode) {
            let in_scene_season = mappings
                .iter()
                .filter(|(s, _)| s.season == last_scene.season)
                .max_by_key(|(s, _)| s.episode);
            if in_scene_season.is_some_and(|(_, t)| t.season == 0) {
                continue;
            }
            let offset = e.episode_number - last_tvdb.episode;
            e.scene_season_number = Some(last_scene.season);
            e.scene_episode_number = Some(last_scene.episode + offset);
            e.scene_absolute_episode_number = Some(last_scene.absolute + offset);
        } else if last_tvdb_season != last_scene_season {
            let offset = e.season_number - last_tvdb_season;
            e.scene_season_number = Some(last_scene_season + offset);
            e.scene_episode_number = Some(e.episode_number);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::augment::XemValues;

    fn ep(season: i64, number: i64) -> Episode {
        let mut e = crate::tv::refresh::blank_episode();
        e.season_number = season;
        e.episode_number = number;
        e
    }

    fn map(tvdb: (i64, i64), scene: (i64, i64)) -> XemMapping {
        XemMapping {
            tvdb: XemValues {
                season: tvdb.0,
                episode: tvdb.1,
                absolute: 0,
            },
            scene: Some(XemValues {
                season: scene.0,
                episode: scene.1,
                absolute: 0,
            }),
        }
    }

    #[test]
    fn xem_maps_and_extrapolates_the_rest_of_a_season() {
        let mut episodes = vec![ep(1, 1), ep(1, 2), ep(1, 3), ep(1, 4)];
        // tvdb 1x03 is scene 2x01; 1x04 is not on XEM yet.
        let mappings = [map((1, 1), (1, 1)), map((1, 2), (1, 2)), map((1, 3), (2, 1))];
        apply_xem("Show", &mut episodes, &mappings);
        let got: Vec<_> = episodes
            .iter()
            .map(|e| {
                (
                    e.scene_season_number,
                    e.scene_episode_number,
                    e.unverified_scene_numbering,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (Some(1), Some(1), false),
                (Some(1), Some(2), false),
                (Some(2), Some(1), false),
                (Some(2), Some(2), true),
            ]
        );
    }
}
