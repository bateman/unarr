//! Plex Media Server for movies, after Radarr's `PlexServer`,
//! `PlexServerService` and `PlexServerProxy`: the movie sections whose
//! location holds the movie's root folder are asked to refresh the movie's
//! folder. The settings, the requests and the path mapping are the series
//! side's, the daily ping to plex.tv included (the sign-in and the server
//! list are not reproduced there, nor here).

use chrono::{DateTime, Utc};

use crate::movies::model::Movie;
use crate::notifications::plex::{
    cached_version, sections_of, update_sections_for, validate_version_for, PlexError, Settings,
};
use crate::notifications::{Notifier, TestOutcome};
use crate::store;

/// The type of the sections `GetMovieSections` keeps.
const SECTION_TYPE: &str = "movie";

/// `PlexServerService.UpdateLibrary(IEnumerable<Movie>, settings)`: the
/// server's version (remembered for two hours), its movie sections, then
/// each movie's folder.
pub async fn update_library(
    notifier: &Notifier,
    settings: &Settings,
    movies: &[Movie],
    now: DateTime<Utc>,
) -> Result<(), PlexError> {
    tracing::debug!("sending update request to Plex server");
    let version = cached_version(notifier, settings, now).await?;
    validate_version_for(&version, "Radarr")?;
    let sections = sections_of(notifier, settings, SECTION_TYPE).await?;
    let root_folders = notifier
        .db()
        .call(|conn| store::catalog::list_root_folders(conn))
        .await
        .unwrap_or_default();
    for movie in movies {
        // `GetRelativePath` trims the separators around what is left of
        // the movie's path under its root folder.
        update_sections_for(
            notifier,
            settings,
            movie.path.trim_end_matches(['/', '\\']),
            &sections,
            &root_folders,
            "Movie",
        )
        .await?;
    }
    tracing::debug!("finished sending update request to Plex server");
    Ok(())
}

/// `PlexServerService.Test`: the movie sections, with the cached version
/// forgotten (`NotificationsPlexValidationNoMovieLibraryFound` when the
/// server has none).
pub async fn test(notifier: &Notifier, settings: &Settings) -> TestOutcome {
    crate::notifications::plex::test_for(
        notifier,
        settings,
        SECTION_TYPE,
        "At least one Movie library is required",
    )
    .await
}
