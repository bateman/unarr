//! Radarr's `HousekeepingService`: the daily clean-up of rows nothing
//! refers to any more, then a `VACUUM`. One housekeeper failing does not
//! stop the others.
//!
//! Ported from Radarr's `NzbDrone.Core/Housekeeping/Housekeepers`, those
//! that have a table here, in the order of their names as the original
//! runs them. The ones with no media in them are the series side's: the
//! users beyond the first, the provider statuses, the status times and the
//! scheduled tasks' last runs in the future. Not
//! ported, as there: the metadata file cleaners, the update's temporary
//! files, the log database, the repair of profiles' format items and of
//! clean titles, the removal of bad covers.

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::housekeeping::{
    additional_users, future_status_times, future_task_runs, orphaned_provider_status,
    stale_pending_releases, Report,
};

type Housekeeper = fn(&Connection, DateTime<Utc>) -> anyhow::Result<usize>;

const HOUSEKEEPERS: [(&str, Housekeeper); 19] = [
    ("CleanupAdditionalUsers", additional_users),
    (
        "CleanupDownloadClientUnavailablePendingReleases",
        stale_pending_releases,
    ),
    ("CleanupDuplicateMovieTranslations", duplicate_translations),
    ("CleanupOrphanedAlternativeTitles", orphaned_alternative_titles),
    ("CleanupOrphanedBlocklist", orphaned_blocklist),
    ("CleanupOrphanedCollections", orphaned_collections),
    ("CleanupOrphanedCredits", orphaned_credits),
    ("CleanupOrphanedExtraFiles", orphaned_extra_files),
    ("CleanupOrphanedHistoryItems", orphaned_history),
    ("CleanupOrphanedImportListMovies", orphaned_import_list_movies),
    ("CleanupOrphanedMovieFiles", orphaned_movie_files),
    ("CleanupOrphanedMovieMetadata", orphaned_movie_metadata),
    ("CleanupOrphanedMovieMovieFileIds", orphaned_movie_file_ids),
    ("CleanupOrphanedMovieTranslations", orphaned_translations),
    ("CleanupOrphanedPendingReleases", orphaned_pending_releases),
    ("CleanupOrphanedProviderStatus", orphaned_provider_status),
    ("CleanupUnusedTags", unused_tags),
    ("FixFutureProviderStatusTimes", future_status_times),
    ("FixFutureRunScheduledTasks", future_task_runs),
];

/// Run every housekeeper, then compact the database.
pub fn run(conn: &Connection, now: DateTime<Utc>) -> Report {
    tracing::info!("Running housecleaning tasks");
    let mut report = Report::new();
    for (name, clean) in HOUSEKEEPERS {
        tracing::debug!("Starting {name}");
        match clean(conn, now) {
            Ok(rows) => {
                tracing::debug!(rows, "Completed {name}");
                report.push((name, rows));
            }
            Err(e) => tracing::error!(error = %e, "Error running housekeeping task: {name}"),
        }
    }
    tracing::debug!("Compressing main database after housekeeping");
    if let Err(e) = conn.execute_batch("VACUUM") {
        tracing::error!(error = %e, "Could not compress the database");
    }
    report
}

/// The later of two translations of one movie into one language.
fn duplicate_translations(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM movie_translations WHERE id IN (
            SELECT max(id) FROM movie_translations
             GROUP BY movie_metadata_id, language_id HAVING count(id) > 1)",
        [],
    )?)
}

fn orphaned_alternative_titles(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM alternative_titles WHERE movie_metadata_id NOT IN (SELECT id FROM movie_metadata)",
        [],
    )?)
}

fn orphaned_blocklist(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM blocklist WHERE movie_id NOT IN (SELECT id FROM movies)",
        [],
    )?)
}

/// Collections none of whose movies is in the library.
fn orphaned_collections(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM collections WHERE tmdb_id NOT IN (
            SELECT mm.collection_tmdb_id FROM movie_metadata mm
              JOIN movies m ON m.movie_metadata_id = mm.id)",
        [],
    )?)
}

fn orphaned_credits(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM credits WHERE movie_metadata_id NOT IN (SELECT id FROM movie_metadata)",
        [],
    )?)
}

/// Subtitles and other extras of a movie or of a movie file that went.
fn orphaned_extra_files(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    let by_movie = conn.execute(
        "DELETE FROM extra_files WHERE movie_id NOT IN (SELECT id FROM movies)",
        [],
    )?;
    let by_file = conn.execute(
        "DELETE FROM extra_files WHERE movie_file_id > 0
            AND movie_file_id NOT IN (SELECT id FROM movie_files)",
        [],
    )?;
    Ok(by_movie + by_file)
}

fn orphaned_history(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM history WHERE movie_id NOT IN (SELECT id FROM movies)",
        [],
    )?)
}

/// What a list that went, or a metadata row that went, had listed.
fn orphaned_import_list_movies(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    let by_list = conn.execute(
        "DELETE FROM import_list_movies WHERE import_list_id NOT IN (
            SELECT id FROM providers WHERE kind = 'importList')",
        [],
    )?;
    let by_metadata = conn.execute(
        "DELETE FROM import_list_movies WHERE movie_metadata_id NOT IN (SELECT id FROM movie_metadata)",
        [],
    )?;
    Ok(by_list + by_metadata)
}

/// Files no movie points at.
fn orphaned_movie_files(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM movie_files WHERE id NOT IN (SELECT movie_file_id FROM movies)",
        [],
    )?)
}

/// Metadata of no movie, of no listed movie and of no collection.
fn orphaned_movie_metadata(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM movie_metadata
          WHERE id NOT IN (SELECT movie_metadata_id FROM movies)
            AND id NOT IN (SELECT movie_metadata_id FROM import_list_movies)
            AND collection_tmdb_id NOT IN (SELECT tmdb_id FROM collections)",
        [],
    )?)
}

/// Movies that point at a file that is not there.
fn orphaned_movie_file_ids(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "UPDATE movies SET movie_file_id = 0
          WHERE movie_file_id <> 0 AND movie_file_id NOT IN (SELECT id FROM movie_files)",
        [],
    )?)
}

fn orphaned_translations(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM movie_translations WHERE movie_metadata_id NOT IN (SELECT id FROM movie_metadata)",
        [],
    )?)
}

fn orphaned_pending_releases(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM pending_releases WHERE movie_id NOT IN (SELECT id FROM movies)",
        [],
    )?)
}

/// Tags no movie, provider, delay or release profile or auto tagging rule
/// carries, and no rule names in a condition.
fn unused_tags(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM tags WHERE id NOT IN (
            SELECT value FROM movies, json_each(movies.tags)
            UNION SELECT value FROM providers, json_each(providers.tags)
            UNION SELECT value FROM delay_profiles, json_each(delay_profiles.tags)
            UNION SELECT value FROM release_profiles, json_each(release_profiles.tags)
            UNION SELECT value FROM auto_tagging, json_each(auto_tagging.tags)
            UNION SELECT json_extract(spec.value, '$.body.value')
                  FROM auto_tagging, json_each(auto_tagging.specifications) AS spec
                  WHERE json_extract(spec.value, '$.type') = 'TagSpecification'
                    AND json_extract(spec.value, '$.body.value') IS NOT NULL
         )",
        [],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::movies::decision::testing;
    use crate::movies::store;
    use crate::Personality;

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn orphans_go_and_what_is_referred_to_stays() {
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        db.call_blocking(|conn| {
            let mut movie = testing::movie(1, "The Movie", 2018);
            movie.movie_file_id = 7;
            store::insert_metadata(conn, &movie.metadata)?;
            store::insert_movie(conn, &movie)?;
            conn.execute_batch(
                "INSERT INTO movie_files (id, movie_id, relative_path, date_added) VALUES
                    (7, 1, 'kept.mkv', '2026-01-01T00:00:00Z'), (8, 1, 'orphan.mkv', '2026-01-01T00:00:00Z');
                 INSERT INTO tags (id, label) VALUES (1, 'unused');",
            )?;
            let report = run(conn, Utc::now());
            let removed = |name: &str| report.iter().find(|(n, _)| *n == name).map(|(_, rows)| *rows);
            assert_eq!(removed("CleanupOrphanedMovieFiles"), Some(1));
            assert_eq!(removed("CleanupUnusedTags"), Some(1));
            assert_eq!(removed("CleanupOrphanedMovieMetadata"), Some(0));
            assert_eq!(report.len(), HOUSEKEEPERS.len(), "every housekeeper ran");
            assert_eq!(count(conn, "SELECT count(*) FROM movie_files"), 1);
            assert_eq!(count(conn, "SELECT count(*) FROM movies"), 1);
            // A movie pointing at a file that went is detached.
            conn.execute("DELETE FROM movie_files", [])?;
            let report = run(conn, Utc::now());
            assert!(report.contains(&("CleanupOrphanedMovieMovieFileIds", 1)));
            assert_eq!(count(conn, "SELECT movie_file_id FROM movies"), 0);
            Ok(())
        })
        .unwrap();
    }
}
