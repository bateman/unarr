//! `RedownloadFailedDownloadService`: a failed download searched for
//! again, unless the user or the settings say not to.

use chrono::{DateTime, Utc};

use super::failed::DownloadFailed;
use super::Downloads;
use crate::commands::{CommandBody, CommandPriority, CommandTrigger};
use crate::decision::ReleaseSource;
use crate::store;

fn config_bool(conn: &rusqlite::Connection, key: &str, default: bool) -> bool {
    store::config::config(conn, key)
        .ok()
        .flatten()
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}

/// `Handle(DownloadFailedEvent)`: the command pushed, if any.
pub async fn redownload(
    downloads: &Downloads,
    event: &DownloadFailed,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<CommandBody>> {
    if event.skip_redownload {
        tracing::debug!("skip redownloading requested by user");
        return Ok(None);
    }
    let Some(commands) = downloads.commands() else {
        return Ok(None);
    };
    let (auto, from_interactive, in_season) = {
        let series_id = event.series_id;
        let first = event.episode_ids.first().copied();
        downloads
            .db()
            .call(move |conn| {
                let auto = config_bool(conn, "AutoRedownloadFailed", true);
                let from_interactive = config_bool(conn, "AutoRedownloadFailedFromInteractiveSearch", true);
                let in_season = match first {
                    Some(id) => match store::episodes::get_episode(conn, id)? {
                        Some(e) => Some((
                            e.season_number,
                            store::episodes::list_episodes(conn, series_id, Some(e.season_number))?.len(),
                        )),
                        None => None,
                    },
                    None => None,
                };
                Ok((auto, from_interactive, in_season))
            })
            .await?
    };
    if !auto {
        tracing::debug!("auto redownloading failed episodes is disabled");
        return Ok(None);
    }
    if event.release_source == ReleaseSource::InteractiveSearch && !from_interactive {
        tracing::debug!("auto redownloading failed episodes from interactive search is disabled");
        return Ok(None);
    }
    let body = if event.episode_ids.len() == 1 {
        tracing::debug!("failed download only contains one episode, searching again");
        CommandBody::EpisodeSearch {
            episode_ids: event.episode_ids.clone(),
        }
    } else if let Some((season_number, _)) = in_season.filter(|(_, count)| *count == event.episode_ids.len())
    {
        tracing::debug!("failed download was entire season, searching again");
        CommandBody::SeasonSearch {
            series_id: event.series_id,
            season_number,
        }
    } else {
        tracing::debug!(
            "failed download contains multiple episodes, probably a double episode, searching again"
        );
        CommandBody::EpisodeSearch {
            episode_ids: event.episode_ids.clone(),
        }
    };
    commands
        .push(
            body.clone(),
            CommandPriority::Normal,
            CommandTrigger::Unspecified,
            now,
        )
        .await?;
    Ok(Some(body))
}
