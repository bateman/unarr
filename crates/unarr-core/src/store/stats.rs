//! Per-season episode statistics, computed the way the originals do: an
//! aggregate over episodes (counts and airing bounds relative to `now`) joined
//! with an aggregate over files (size, release groups).

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{params, Connection};

use crate::time;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeasonStats {
    pub series_id: i64,
    pub season_number: i64,
    pub total_episode_count: i64,
    /// Aired or downloaded.
    pub available_episode_count: i64,
    /// Monitored and aired, or downloaded.
    pub episode_count: i64,
    pub episode_file_count: i64,
    pub monitored_episode_count: i64,
    pub next_airing: Option<DateTime<Utc>>,
    pub previous_airing: Option<DateTime<Utc>>,
    pub last_aired: Option<NaiveDate>,
    pub size_on_disk: i64,
    /// Distinct, in file order.
    pub release_groups: Vec<String>,
}

impl SeasonStats {
    pub fn percent_of_episodes(&self) -> f64 {
        if self.episode_count == 0 {
            0.0
        } else {
            // Multiply first: the originals compute in decimal, and this keeps
            // the double result identical to theirs for the usual ratios.
            (self.episode_file_count * 100) as f64 / self.episode_count as f64
        }
    }
}

/// Statistics of every season of every series (or of one series), keyed by
/// series id, seasons in number order.
pub fn season_statistics(
    conn: &Connection,
    series_id: Option<i64>,
    now: DateTime<Utc>,
) -> anyhow::Result<HashMap<i64, Vec<SeasonStats>>> {
    let now = time::format_utc(now);
    let mut out: HashMap<i64, Vec<SeasonStats>> = HashMap::new();

    let mut stmt = conn.prepare(
        "SELECT series_id, season_number, COUNT(*),
            SUM(CASE WHEN air_date_utc <= ?1 OR episode_file_id > 0 THEN 1 ELSE 0 END),
            SUM(CASE WHEN (monitored = 1 AND air_date_utc <= ?1) OR episode_file_id > 0 THEN 1 ELSE 0 END),
            SUM(CASE WHEN episode_file_id > 0 THEN 1 ELSE 0 END),
            SUM(CASE WHEN monitored = 1 THEN 1 ELSE 0 END),
            MIN(CASE WHEN air_date_utc < ?1 OR monitored = 0 THEN NULL ELSE air_date_utc END),
            MAX(CASE WHEN air_date_utc >= ?1 OR monitored = 0 THEN NULL ELSE air_date_utc END),
            MAX(air_date)
         FROM episodes
         WHERE ?2 IS NULL OR series_id = ?2
         GROUP BY series_id, season_number
         ORDER BY series_id, season_number",
    )?;
    let mut rows = stmt.query(params![now, series_id])?;
    while let Some(r) = rows.next()? {
        let stats = SeasonStats {
            series_id: r.get(0)?,
            season_number: r.get(1)?,
            total_episode_count: r.get(2)?,
            available_episode_count: r.get(3)?,
            episode_count: r.get(4)?,
            episode_file_count: r.get(5)?,
            monitored_episode_count: r.get(6)?,
            next_airing: r.get::<_, Option<String>>(7)?.and_then(|s| time::parse_utc(&s)),
            previous_airing: r.get::<_, Option<String>>(8)?.and_then(|s| time::parse_utc(&s)),
            last_aired: r.get::<_, Option<String>>(9)?.and_then(|s| time::parse_date(&s)),
            size_on_disk: 0,
            release_groups: Vec::new(),
        };
        out.entry(stats.series_id).or_default().push(stats);
    }

    let mut stmt = conn.prepare(
        "SELECT series_id, season_number, SUM(COALESCE(size, 0)), GROUP_CONCAT(release_group, '|')
         FROM (SELECT * FROM episode_files WHERE ?1 IS NULL OR series_id = ?1 ORDER BY id)
         GROUP BY series_id, season_number",
    )?;
    let mut rows = stmt.query(params![series_id])?;
    while let Some(r) = rows.next()? {
        let (sid, season): (i64, i64) = (r.get(0)?, r.get(1)?);
        let size: i64 = r.get(2)?;
        let groups: Option<String> = r.get(3)?;
        if let Some(season_stats) = out
            .get_mut(&sid)
            .and_then(|v| v.iter_mut().find(|s| s.season_number == season))
        {
            season_stats.size_on_disk = size;
            let mut seen = Vec::new();
            for g in groups.unwrap_or_default().split('|') {
                if !g.is_empty() && !seen.iter().any(|s| s == g) {
                    seen.push(g.to_string());
                }
            }
            // The originals sort each season's groups (culture-aware); the
            // series list is the seasons' lists concatenated and de-duplicated.
            seen.sort_by(|a, b| crate::collation::compare(a, b));
            season_stats.release_groups = seen;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_handles_zero() {
        let s = SeasonStats::default();
        assert_eq!(s.percent_of_episodes(), 0.0);
        let s = SeasonStats {
            episode_count: 13,
            episode_file_count: 3,
            ..SeasonStats::default()
        };
        assert_eq!(s.percent_of_episodes(), 23.076923076923077);
    }
}
