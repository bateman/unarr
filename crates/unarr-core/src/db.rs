//! SQLite access. One connection per database, owned by a dedicated thread;
//! callers hand it closures and await the result.
//!
//! The originals keep thousands of rows and a single user, so one serialised
//! connection is plenty and keeps every query off the async runtime.

use std::path::Path;
use std::sync::mpsc;
use std::thread;

use rusqlite::Connection;

use crate::Personality;

/// Sonarr's schema migrations, applied in order. `PRAGMA user_version`
/// records how many have run. The tables every personality has are written
/// once, under `migrations/shared`, and applied with the series side; the
/// sequence is the one `unarr-sonarr.db` files have gone through since
/// phase 1 and must stay so.
const SONARR_MIGRATIONS: &[&str] = &[
    concat!(
        include_str!("../migrations/shared/0001_core.sql"),
        include_str!("../migrations/sonarr/0001_series.sql")
    ),
    include_str!("../migrations/shared/0002_provider_status.sql"),
    include_str!("../migrations/sonarr/0003_download_history.sql"),
    concat!(
        include_str!("../migrations/shared/0004_commands.sql"),
        include_str!("../migrations/sonarr/0004_pending_releases.sql")
    ),
    include_str!("../migrations/sonarr/0005_history_download_id.sql"),
    include_str!("../migrations/shared/0006_sequences.sql"),
    concat!(
        include_str!("../migrations/sonarr/0007_import_list_items.sql"),
        include_str!("../migrations/shared/0007_import_list_status.sql")
    ),
    include_str!("../migrations/sonarr/0008_episode_statistics.sql"),
    include_str!("../migrations/sonarr/0009_extra_files.sql"),
    include_str!("../migrations/shared/0010_update_history.sql"),
];

/// Radarr's: the shared tables as they stood when the movie side was
/// written, with the movie side; then the shared tables added since, in
/// the order `unarr-radarr.db` files go through them.
const RADARR_MIGRATIONS: &[&str] = &[
    concat!(
        include_str!("../migrations/shared/0001_core.sql"),
        include_str!("../migrations/shared/0002_provider_status.sql"),
        include_str!("../migrations/shared/0004_commands.sql"),
        include_str!("../migrations/shared/0006_sequences.sql"),
        include_str!("../migrations/shared/0007_import_list_status.sql"),
        include_str!("../migrations/radarr/0001_movies.sql")
    ),
    include_str!("../migrations/shared/0010_update_history.sql"),
    RADARR_MEDIA_INFO_STREAMS,
];

/// Radarr's migration 243 for the media info of the movie files, also run
/// by the importer on a database older than 243.
pub const RADARR_MEDIA_INFO_STREAMS: &str = include_str!("../migrations/radarr/0002_media_info_streams.sql");

/// The migration set of a personality's database. Lidarr has none yet and
/// gets Sonarr's, as it did before the sets were split: it serves nothing
/// of its own.
fn migrations(personality: Personality) -> &'static [&'static str] {
    match personality {
        Personality::Sonarr | Personality::Lidarr => SONARR_MIGRATIONS,
        Personality::Radarr => RADARR_MIGRATIONS,
    }
}

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

/// Handle to a database thread. Cheap to clone; the thread exits when the
/// last handle is dropped.
#[derive(Clone)]
pub struct Db {
    tx: mpsc::Sender<Job>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Db")
    }
}

impl Db {
    /// Open (creating if needed) the database of `personality` at `path`,
    /// enable WAL and foreign keys, and bring the schema up to date.
    pub fn open(path: &Path, personality: Personality) -> anyhow::Result<Db> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::start(conn, personality)
    }

    /// A private in-memory database with Sonarr's schema applied. For tests
    /// and for importing without touching disk.
    pub fn open_in_memory() -> anyhow::Result<Db> {
        Self::open_in_memory_for(Personality::Sonarr)
    }

    /// A private in-memory database with the schema of `personality`.
    pub fn open_in_memory_for(personality: Personality) -> anyhow::Result<Db> {
        Self::start(Connection::open_in_memory()?, personality)
    }

    fn start(mut conn: Connection, personality: Personality) -> anyhow::Result<Db> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        refuse_another_personality(&conn, personality)?;
        migrate(&mut conn, personality)?;
        let (tx, rx) = mpsc::channel::<Job>();
        // The thread logs where its opener does: one process serves several
        // personalities, each with a `tracing` dispatcher of its own.
        let log = tracing::dispatcher::get_default(|d| d.clone());
        thread::Builder::new().name("unarr-db".into()).spawn(move || {
            let _log = tracing::dispatcher::set_default(&log);
            for job in rx {
                // A call that panics is caught where it runs (`answer`),
                // which tells its caller. The catch here is not for the
                // call but for what a job does around it: the line
                // `answer` logs, the release of a result no caller waits
                // for any more. None of these is known to panic, and one
                // that did would otherwise end this thread: it owns the
                // only connection, and without it the personality has no
                // database until the process starts again.
                let served = crate::panics::catch_sync(|| job(&mut conn));
                let closed = crate::panics::catch_sync(|| roll_back_what_was_left_open(&conn));
                for panic in [served, closed].into_iter().filter_map(Result::err) {
                    // Not through `tracing`: a log layer that panicked is
                    // one of the ways here.
                    eprintln!("unarr-db: {panic}, outside a database call; the thread goes on");
                }
            }
        })?;
        Ok(Db { tx })
    }

    /// Run `f` on the database thread and await its result.
    pub async fn call<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> anyhow::Result<T> + Send + 'static,
    {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(Box::new(move |conn| {
                let _ = reply_tx.send(answer(f, conn));
            }))
            .map_err(|_| anyhow::anyhow!("database thread is gone"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("database thread dropped the request"))?
    }

    /// Run `f` on the database thread and block the caller until it is done.
    /// For synchronous contexts such as the importer and tests.
    pub fn call_blocking<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> anyhow::Result<T> + Send + 'static,
    {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.tx
            .send(Box::new(move |conn| {
                let _ = reply_tx.send(answer(f, conn));
            }))
            .map_err(|_| anyhow::anyhow!("database thread is gone"))?;
        reply_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("database thread dropped the request"))?
    }
}

/// A `Transaction` rolls back as it is dropped, unwinding included. One
/// begun by hand would still be open after its call, and every later call
/// would join it.
fn roll_back_what_was_left_open(conn: &Connection) {
    if !conn.is_autocommit() {
        tracing::error!("A database call left a transaction open: rolled back");
        if let Err(e) = conn.execute_batch("ROLLBACK") {
            tracing::error!(error = %e, "The open transaction could not be rolled back");
        }
    }
}

/// What a call answers: its result, or the panic it ended with as an
/// error, so that the caller gets an answer either way.
fn answer<T>(
    f: impl FnOnce(&mut Connection) -> anyhow::Result<T>,
    conn: &mut Connection,
) -> anyhow::Result<T> {
    crate::panics::catch_sync(|| f(conn)).unwrap_or_else(|panic| {
        tracing::error!("A database call {panic}");
        Err(anyhow::Error::new(panic))
    })
}

/// Before the migration sets were split, `--personalities radarr` gave
/// `unarr-radarr.db` Sonarr's tables. Such a file holds nothing (nothing
/// was imported into it and it served a clone of Sonarr) and its
/// `user_version` counts another sequence, so it is refused with what to do
/// rather than migrated or silently replaced.
fn refuse_another_personality(conn: &Connection, personality: Personality) -> anyhow::Result<()> {
    let has = |table: &str| -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |r| r.get(0),
        )
    };
    let foreign = match personality {
        Personality::Radarr => has("series")?,
        Personality::Sonarr | Personality::Lidarr => has("movies")?,
    };
    anyhow::ensure!(
        !foreign,
        "{} holds another application's tables (a build older than the Radarr personality gave \
         unarr-radarr.db Sonarr's schema and no data): move the file away and start again, with \
         --import-from to import the original installation",
        personality.db_file()
    );
    Ok(())
}

/// Apply the migrations of `personality` not yet recorded in `user_version`.
pub fn migrate(conn: &mut Connection, personality: Personality) -> anyhow::Result<()> {
    let migrations = migrations(personality);
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let current = usize::try_from(current).unwrap_or(usize::MAX);
    anyhow::ensure!(
        current <= migrations.len(),
        "database schema version {current} is newer than this build ({})",
        migrations.len()
    );
    for (i, sql) in migrations.iter().enumerate().skip(current) {
        let version = i + 1;
        tracing::info!(version, "applying schema migration");
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_once_and_are_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, Personality::Sonarr).unwrap();
        migrate(&mut conn, Personality::Sonarr).unwrap();
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        // The sequence existing unarr-sonarr.db files went through.
        assert_eq!(v, 10);
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'series'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
    }

    #[test]
    fn a_call_that_panics_is_an_error_and_the_thread_goes_on() {
        crate::panics::install_hook();
        let db = Db::open_in_memory().unwrap();
        let tags = |db: &Db| -> i64 {
            db.call_blocking(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))?))
                .unwrap()
        };
        let before = tags(&db);

        // Inside a transaction, after a write: the caller is told, and the
        // write is not there.
        let failed = db.call_blocking(|conn| -> anyhow::Result<()> {
            let tx = conn.transaction()?;
            tx.execute("INSERT INTO tags (label) VALUES ('half')", [])?;
            let rows: Vec<i64> = Vec::new();
            let _ = rows[3];
            tx.commit()?;
            Ok(())
        });
        let error = failed.unwrap_err();
        let panic = error.downcast_ref::<crate::panics::Panicked>().unwrap();
        assert!(panic.message.contains("index out of bounds"), "{panic}");
        assert!(panic.location.as_deref().unwrap().contains("db.rs:"), "{panic}");
        assert_eq!(tags(&db), before);

        // A transaction begun by hand and left open is rolled back too.
        let failed = db.call_blocking(|conn| -> anyhow::Result<()> {
            conn.execute_batch("BEGIN; INSERT INTO tags (label) VALUES ('open')")?;
            panic!("with a transaction open");
        });
        assert!(failed.is_err());
        assert_eq!(tags(&db), before);

        // The same from an async caller, and the thread still writes.
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let failed: anyhow::Result<()> = runtime.block_on(db.call(|_| panic!("asked from a task")));
        assert!(failed.unwrap_err().to_string().ends_with("asked from a task"));
        db.call_blocking(|conn| Ok(conn.execute("INSERT INTO tags (label) VALUES ('kept')", [])?))
            .unwrap();
        assert_eq!(tags(&db), before + 1);
    }

    /// A panic in a job outside its call does not end the thread: here
    /// the release of a result whose caller stopped waiting.
    #[test]
    fn a_panic_around_a_call_leaves_the_thread_serving() {
        struct Unsent;
        impl Drop for Unsent {
            fn drop(&mut self) {
                panic!("released without a caller");
            }
        }

        crate::panics::install_hook();
        let db = Db::open_in_memory().unwrap();
        // The thread is held on a first call, and known to be in it,
        // while a second one is sent and its caller leaves: the second
        // call's result is then released on the database thread, never
        // here.
        let (hold, held) = mpsc::channel::<()>();
        let (holding, is_held) = mpsc::channel::<()>();
        let waiting = {
            let db = db.clone();
            thread::spawn(move || {
                db.call_blocking(move |_| {
                    holding.send(())?;
                    Ok(held.recv()?)
                })
            })
        };
        is_held.recv().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            tokio::select! {
                biased;
                _ = db.call(|_| Ok(Unsent)) => unreachable!("the thread is held"),
                _ = std::future::ready(()) => {}
            }
        });
        hold.send(()).unwrap();
        waiting.join().unwrap().unwrap();
        // The second call ran, its result was released on the thread with
        // a panic, and the thread still answers.
        let tags: i64 = db
            .call_blocking(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(tags, 0);
    }

    #[test]
    fn refuses_a_database_from_the_future() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 999).unwrap();
        assert!(migrate(&mut conn, Personality::Sonarr).is_err());
    }

    #[test]
    fn radarr_has_its_own_schema_over_the_shared_tables() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, Personality::Radarr).unwrap();
        migrate(&mut conn, Personality::Radarr).unwrap();
        let has = |table: &str| -> bool {
            conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |r| r.get(0),
            )
            .unwrap()
        };
        for table in [
            "movies",
            "movie_metadata",
            "movie_files",
            "tags",
            "providers",
            "commands",
            "sequences",
            "update_history",
        ] {
            assert!(has(table), "{table}");
        }
        for table in ["series", "episodes", "scene_mappings"] {
            assert!(!has(table), "{table}");
        }
    }

    /// Radarr's migration 243 on a database Unarr wrote before it: the
    /// flat media info becomes one entry per stream, as the original
    /// migrates it (its fixture `mediainfo_multiple_streamsFixture`), and
    /// the movies' metadata is marked stale.
    #[test]
    fn radarr_media_info_moves_to_streams() {
        let mut conn = Connection::open_in_memory().unwrap();
        let before = RADARR_MIGRATIONS
            .iter()
            .position(|m| *m == RADARR_MEDIA_INFO_STREAMS)
            .unwrap();
        for sql in &RADARR_MIGRATIONS[..before] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", before as i64).unwrap();
        conn.execute(
            "INSERT INTO movie_metadata (id, tmdb_id, title, status, last_info_sync)
             VALUES (1, 603, 'The Matrix', 'released', '2026-11-20T10:00:00Z')",
            [],
        )
        .unwrap();
        let rows = [
            (
                1,
                Some(
                    r#"{"schemaRevision":11,"audioFormat":"truehd","audioCodecID":"[0][0][0][0]","audioProfile":"Dolby TrueHD + Dolby Atmos","audioBitrate":224000,"audioChannels":2,"audioChannelPositions":"stereo","audioStreamCount":2,"audioLanguages":["eng","rum"],"subtitles":["ger","eng","rum"],"scanType":"Progressive"}"#,
                ),
            ),
            (2, None),
            (
                3,
                Some(r#"{"audioLanguages":"eng/rum","subtitles":"ger/eng/rum","schemaRevision":13}"#),
            ),
            (4, Some("not json")),
            (5, Some(r#"{"videoFormat":"h264"}"#)),
            (
                6,
                Some(r#"{"audioStreams":[{"language":"eng"}],"subtitleStreams":[]}"#),
            ),
        ];
        for (id, info) in rows {
            conn.execute(
                "INSERT INTO movie_files (id, movie_id, relative_path, date_added, media_info)
                 VALUES (?1, 1, 'm.mkv', '2026-01-01T00:00:00Z', ?2)",
                rusqlite::params![id, info],
            )
            .unwrap();
        }
        migrate(&mut conn, Personality::Radarr).unwrap();
        let info = |id: i64| -> Option<serde_json::Value> {
            conn.query_row("SELECT media_info FROM movie_files WHERE id = ?1", [id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .unwrap()
            .map(|s| serde_json::from_str(&s).unwrap())
        };
        let migrated = info(1).unwrap();
        assert_eq!(
            migrated["audioStreams"],
            serde_json::json!([
                {"language": "eng", "format": "truehd", "codecId": "[0][0][0][0]",
                 "profile": "Dolby TrueHD + Dolby Atmos", "bitrate": 224000, "channels": 2,
                 "channelPositions": "stereo"},
                {"language": "rum"}
            ])
        );
        assert_eq!(
            migrated["subtitleStreams"],
            serde_json::json!([{"language": "ger"}, {"language": "eng"}, {"language": "rum"}])
        );
        for gone in [
            "audioLanguages",
            "audioFormat",
            "audioCodecID",
            "audioProfile",
            "audioBitrate",
            "audioChannels",
            "audioChannelPositions",
            "subtitles",
        ] {
            assert!(migrated.get(gone).is_none(), "{gone}");
        }
        assert_eq!(migrated["audioStreamCount"], 2);
        assert_eq!(migrated["scanType"], "Progressive");
        assert_eq!(info(2), None, "no media info stays none");
        assert_eq!(info(3), None, "lists that are not lists: dropped, probed again");
        assert_eq!(info(4), None, "not JSON: dropped, probed again");
        assert_eq!(
            info(5),
            Some(serde_json::json!({"videoFormat": "h264"})),
            "neither list: left"
        );
        assert_eq!(
            info(6),
            Some(serde_json::json!({"audioStreams": [{"language": "eng"}], "subtitleStreams": []})),
            "already migrated: left"
        );
        let synced: String = conn
            .query_row(
                "SELECT last_info_sync FROM movie_metadata WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(synced, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn the_shared_tables_are_the_same_in_both_schemas() {
        let schema = |personality| {
            let mut conn = Connection::open_in_memory().unwrap();
            migrate(&mut conn, personality).unwrap();
            let mut stmt = conn
                .prepare("SELECT name, sql FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .unwrap();
            let rows: Vec<(String, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            rows
        };
        let sonarr = schema(Personality::Sonarr);
        let radarr = schema(Personality::Radarr);
        // quality_profiles is shared and then altered by Radarr.
        for table in [
            "meta",
            "tags",
            "root_folders",
            "quality_definitions",
            "custom_formats",
            "providers",
            "config",
            "delay_profiles",
            "release_profiles",
            "remote_path_mappings",
            "custom_filters",
            "auto_tagging",
            "users",
            "scheduled_tasks",
            "provider_status",
            "commands",
            "sequences",
            "import_list_status",
        ] {
            let find =
                |rows: &[(String, String)]| rows.iter().find(|(n, _)| n == table).map(|(_, s)| s.clone());
            assert_eq!(find(&sonarr), find(&radarr), "{table}");
            assert!(find(&sonarr).is_some(), "{table}");
        }
    }

    #[test]
    fn a_radarr_database_with_sonarr_tables_is_refused() {
        let dir = std::env::temp_dir().join(format!("unarr-db-clone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unarr-radarr.db");
        let _ = std::fs::remove_file(&path);
        // What `--personalities radarr` left behind before the sets were split.
        drop(Db::open(&path, Personality::Sonarr).unwrap());
        let err = Db::open(&path, Personality::Radarr).unwrap_err().to_string();
        assert!(err.contains("another application's tables"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn runs_closures_on_the_db_thread() {
        let db = Db::open_in_memory().unwrap();
        let n: i64 = db
            .call(|conn| Ok(conn.query_row("SELECT 1 + 1", [], |r| r.get(0))?))
            .await
            .unwrap();
        assert_eq!(n, 2);
        let m: i64 = db
            .call_blocking(|conn| Ok(conn.query_row("SELECT 2 + 2", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(m, 4);
    }
}
