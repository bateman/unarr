//! `EpisodeMonitoredService`: what the monitor presets of Add Series, the
//! season pass and the import lists mean for a series' episodes and seasons.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Episode, Series, SeriesStatus};

/// `MonitorTypes`, as the API names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MonitorType {
    Unknown,
    All,
    Future,
    Missing,
    Existing,
    FirstSeason,
    LastSeason,
    /// Obsolete, read as `lastSeason`.
    LatestSeason,
    Pilot,
    Recent,
    MonitorSpecials,
    UnmonitorSpecials,
    None,
    Skip,
}

/// `MonitoringOptions` (and the `AddSeriesOptions` it is the base of): the
/// preset, or the legacy flags of the v2 endpoints.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitoringOptions {
    pub monitor: MonitorType,
    pub ignore_episodes_with_files: bool,
    pub ignore_episodes_without_files: bool,
}

impl MonitoringOptions {
    /// From the JSON the API or `add_options` carries; a missing or
    /// unknown `monitor` is `Unknown`, as the originals' enum default.
    pub fn from_json(v: &Value) -> MonitoringOptions {
        let monitor = v
            .get("monitor")
            .and_then(|m| match m {
                Value::String(s) => serde_json::from_value(Value::String(lower_first(s))).ok(),
                Value::Number(n) => n.as_i64().and_then(monitor_from_number),
                _ => None,
            })
            .unwrap_or(MonitorType::Unknown);
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        MonitoringOptions {
            monitor,
            ignore_episodes_with_files: flag("ignoreEpisodesWithFiles"),
            ignore_episodes_without_files: flag("ignoreEpisodesWithoutFiles"),
        }
    }
}

/// The API accepts enum names in any case (`FirstSeason`, `firstSeason`).
fn lower_first(s: &str) -> String {
    let lowered = s.to_ascii_lowercase();
    // The camelCase names, matched case-insensitively.
    for name in [
        "unknown",
        "all",
        "future",
        "missing",
        "existing",
        "firstSeason",
        "lastSeason",
        "latestSeason",
        "pilot",
        "recent",
        "monitorSpecials",
        "unmonitorSpecials",
        "none",
        "skip",
    ] {
        if name.to_ascii_lowercase() == lowered {
            return name.to_string();
        }
    }
    s.to_string()
}

/// `MonitorTypes` by number, in the originals' declaration order.
fn monitor_from_number(n: i64) -> Option<MonitorType> {
    use MonitorType::*;
    [
        Unknown,
        All,
        Future,
        Missing,
        Existing,
        FirstSeason,
        LastSeason,
        LatestSeason,
        Pilot,
        Recent,
        MonitorSpecials,
        UnmonitorSpecials,
        None,
        Skip,
    ]
    .get(n as usize)
    .copied()
}

/// What `SetEpisodeMonitoredStatus` decided: `None` when it leaves the
/// episodes alone (`Skip`).
pub fn apply(
    series: &mut Series,
    episodes: &mut [Episode],
    options: &MonitoringOptions,
    now: DateTime<Utc>,
) -> bool {
    use MonitorType::*;
    if options.monitor == Unknown {
        legacy(series, episodes, options, now);
        return true;
    }
    if options.monitor == Skip {
        return false;
    }
    let first_season = series
        .seasons
        .iter()
        .map(|s| s.season_number)
        .filter(|s| *s > 0)
        .min()
        .unwrap_or(0);
    let last_season = series.seasons.iter().map(|s| s.season_number).max().unwrap_or(0);
    let toggle = |episodes: &mut [Episode], f: &dyn Fn(&Episode) -> bool| {
        for e in episodes.iter_mut() {
            e.monitored = f(e);
        }
    };
    match options.monitor {
        All => toggle(episodes, &|e| e.season_number > 0),
        Future => toggle(episodes, &|e| {
            e.season_number > 0 && e.air_date_utc.is_none_or(|d| d >= now)
        }),
        Missing => toggle(episodes, &|e| e.season_number > 0 && e.episode_file_id == 0),
        Existing => toggle(episodes, &|e| e.season_number > 0 && e.episode_file_id > 0),
        Pilot => toggle(episodes, &|e| {
            e.season_number > 0 && e.season_number == first_season && e.episode_number == 1
        }),
        FirstSeason => toggle(episodes, &|e| {
            e.season_number > 0 && e.season_number == first_season
        }),
        LastSeason | LatestSeason => toggle(episodes, &|e| {
            e.season_number > 0 && e.season_number == last_season
        }),
        Recent => toggle(episodes, &|e| {
            e.season_number > 0
                && match e.air_date_utc {
                    Option::None => true,
                    // `InLastDays(90)`: after now minus 90 days (and before now).
                    Some(d) => (d < now && d >= now - Duration::days(90)) || d > now,
                }
        }),
        MonitorSpecials => {
            for e in episodes.iter_mut().filter(|e| e.season_number == 0) {
                e.monitored = true;
            }
        }
        UnmonitorSpecials => {
            for e in episodes.iter_mut().filter(|e| e.season_number == 0) {
                e.monitored = false;
            }
        }
        None => toggle(episodes, &|_| false),
        Unknown | Skip => unreachable!(),
    }
    let monitored_seasons: std::collections::HashSet<i64> = episodes
        .iter()
        .filter(|e| e.monitored)
        .map(|e| e.season_number)
        .collect();
    let continuing = matches!(series.status, SeriesStatus::Continuing | SeriesStatus::Upcoming);
    for season in series.seasons.iter_mut() {
        let n = season.season_number;
        season.monitored = if n > 0
            && n == last_season
            && (options.monitor == All || (options.monitor == Future && continuing))
        {
            true
        } else if n == first_season && options.monitor == Pilot {
            false
        } else {
            monitored_seasons.contains(&n)
        };
    }
    true
}

/// `LegacySetEpisodeMonitoredStatus`.
fn legacy(series: &mut Series, episodes: &mut [Episode], options: &MonitoringOptions, now: DateTime<Utc>) {
    for e in episodes.iter_mut().filter(|e| e.episode_file_id > 0) {
        e.monitored = !options.ignore_episodes_with_files;
    }
    for e in episodes
        .iter_mut()
        .filter(|e| e.episode_file_id == 0 && e.air_date_utc.is_some_and(|d| d < now))
    {
        e.monitored = !options.ignore_episodes_without_files;
    }
    let last_season = series.seasons.iter().map(|s| s.season_number).max().unwrap_or(0);
    for season in series.seasons.iter_mut() {
        if !season.monitored {
            for e in episodes
                .iter_mut()
                .filter(|e| e.season_number == season.season_number)
            {
                e.monitored = false;
            }
        }
        if season.season_number < last_season
            && episodes
                .iter()
                .filter(|e| e.season_number == season.season_number)
                .all(|e| !e.monitored)
        {
            season.monitored = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Season;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 9, 12, 0, 0).unwrap()
    }

    fn setup() -> (Series, Vec<Episode>) {
        let mut s = crate::store::series::tests::sample(1);
        s.seasons = (0..=2)
            .map(|n| Season {
                season_number: n,
                monitored: true,
                images: vec![],
            })
            .collect();
        s.status = SeriesStatus::Continuing;
        let mut episodes = Vec::new();
        for (season, number, days) in [(0, 1, -400), (1, 1, -400), (1, 2, -30), (2, 1, 3), (2, 2, 10)] {
            let mut e = crate::tv::refresh::blank_episode();
            e.season_number = season;
            e.episode_number = number;
            e.air_date_utc = Some(now() + Duration::days(days));
            episodes.push(e);
        }
        episodes[1].episode_file_id = 9;
        (s, episodes)
    }

    fn monitored(episodes: &[Episode]) -> Vec<(i64, i64)> {
        episodes
            .iter()
            .filter(|e| e.monitored)
            .map(|e| (e.season_number, e.episode_number))
            .collect()
    }

    fn opts(monitor: MonitorType) -> MonitoringOptions {
        MonitoringOptions {
            monitor,
            ignore_episodes_with_files: false,
            ignore_episodes_without_files: false,
        }
    }

    #[test]
    fn presets_follow_the_original() {
        let cases = [
            (MonitorType::All, vec![(1, 1), (1, 2), (2, 1), (2, 2)]),
            (MonitorType::Future, vec![(2, 1), (2, 2)]),
            (MonitorType::Missing, vec![(1, 2), (2, 1), (2, 2)]),
            (MonitorType::Existing, vec![(1, 1)]),
            (MonitorType::Pilot, vec![(1, 1)]),
            (MonitorType::FirstSeason, vec![(1, 1), (1, 2)]),
            (MonitorType::LastSeason, vec![(2, 1), (2, 2)]),
            (MonitorType::Recent, vec![(1, 2), (2, 1), (2, 2)]),
            (MonitorType::None, vec![]),
        ];
        for (monitor, expected) in cases {
            let (mut s, mut e) = setup();
            apply(&mut s, &mut e, &opts(monitor), now());
            assert_eq!(monitored(&e), expected, "{monitor:?}");
        }
    }

    #[test]
    fn seasons_follow_their_episodes() {
        let (mut s, mut e) = setup();
        apply(&mut s, &mut e, &opts(MonitorType::Pilot), now());
        assert_eq!(
            s.seasons.iter().map(|x| x.monitored).collect::<Vec<_>>(),
            [false, false, false]
        );
        let (mut s, mut e) = setup();
        apply(&mut s, &mut e, &opts(MonitorType::Existing), now());
        assert_eq!(
            s.seasons.iter().map(|x| x.monitored).collect::<Vec<_>>(),
            [false, true, false]
        );
        let (mut s, mut e) = setup();
        apply(&mut s, &mut e, &opts(MonitorType::None), now());
        assert!(s.seasons.iter().all(|x| !x.monitored));
    }

    #[test]
    fn options_parse_names_in_any_case() {
        let o = MonitoringOptions::from_json(&serde_json::json!({"monitor": "FirstSeason"}));
        assert_eq!(o.monitor, MonitorType::FirstSeason);
        let o = MonitoringOptions::from_json(&serde_json::json!({}));
        assert_eq!(o.monitor, MonitorType::Unknown);
    }
}
