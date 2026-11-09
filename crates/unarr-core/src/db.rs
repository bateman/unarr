//! SQLite access. One connection per database, owned by a dedicated thread;
//! callers hand it closures and await the result.
//!
//! The originals keep thousands of rows and a single user, so one serialised
//! connection is plenty and keeps every query off the async runtime.

use std::path::Path;
use std::sync::mpsc;
use std::thread;

use rusqlite::Connection;

/// Schema migrations, applied in order. `PRAGMA user_version` records how many
/// have run.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_initial.sql"),
    include_str!("../migrations/0002_provider_status.sql"),
    include_str!("../migrations/0003_download_history.sql"),
    include_str!("../migrations/0004_commands.sql"),
    include_str!("../migrations/0005_history_download_id.sql"),
    include_str!("../migrations/0006_import.sql"),
];

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
    /// Open (creating if needed) the database at `path`, enable WAL and
    /// foreign keys, and bring the schema up to date.
    pub fn open(path: &Path) -> anyhow::Result<Db> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::start(conn)
    }

    /// A private in-memory database with the schema applied. For tests and
    /// for importing without touching disk.
    pub fn open_in_memory() -> anyhow::Result<Db> {
        Self::start(Connection::open_in_memory()?)
    }

    fn start(mut conn: Connection) -> anyhow::Result<Db> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut conn)?;
        let (tx, rx) = mpsc::channel::<Job>();
        thread::Builder::new().name("unarr-db".into()).spawn(move || {
            for job in rx {
                job(&mut conn);
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
                let _ = reply_tx.send(f(conn));
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
                let _ = reply_tx.send(f(conn));
            }))
            .map_err(|_| anyhow::anyhow!("database thread is gone"))?;
        reply_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("database thread dropped the request"))?
    }
}

/// Apply the migrations not yet recorded in `user_version`.
pub fn migrate(conn: &mut Connection) -> anyhow::Result<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let current = usize::try_from(current).unwrap_or(usize::MAX);
    anyhow::ensure!(
        current <= MIGRATIONS.len(),
        "database schema version {current} is newer than this build ({})",
        MIGRATIONS.len()
    );
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
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
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
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
    fn refuses_a_database_from_the_future() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 999).unwrap();
        assert!(migrate(&mut conn).is_err());
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
