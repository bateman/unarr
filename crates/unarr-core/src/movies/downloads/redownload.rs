//! Radarr's `RedownloadFailedDownloadService`: a failed download's movie
//! searched for again, unless the user or the settings say not to.

use chrono::{DateTime, Utc};

use super::failed::MovieDownloadFailed;
use super::MovieDownloads;
use crate::commands::{CommandBody, CommandPriority, CommandTrigger};
use crate::decision::ReleaseSource;

fn config_bool(conn: &rusqlite::Connection, key: &str, default: bool) -> bool {
    crate::store::config::config(conn, key)
        .ok()
        .flatten()
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(default)
}

/// `Handle(DownloadFailedEvent)`: the command pushed, if any.
pub async fn redownload(
    downloads: &MovieDownloads,
    event: &MovieDownloadFailed,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<CommandBody>> {
    if event.skip_redownload {
        tracing::debug!("skip redownloading requested by user");
        return Ok(None);
    }
    let Some(commands) = downloads.commands() else {
        return Ok(None);
    };
    let (auto, from_interactive) = downloads
        .db()
        .call(|conn| {
            Ok((
                config_bool(conn, "AutoRedownloadFailed", true),
                config_bool(conn, "AutoRedownloadFailedFromInteractiveSearch", true),
            ))
        })
        .await?;
    if !auto {
        tracing::debug!("auto redownloading failed movies is disabled");
        return Ok(None);
    }
    if event.release_source == ReleaseSource::InteractiveSearch && !from_interactive {
        tracing::debug!("auto redownloading failed movies from interactive search is disabled");
        return Ok(None);
    }
    if event.movie_id == 0 {
        return Ok(None);
    }
    tracing::debug!("failed download contains a movie, searching again");
    let body = CommandBody::MoviesSearch {
        movie_ids: vec![event.movie_id],
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
