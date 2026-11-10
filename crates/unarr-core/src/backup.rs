//! Backups in the originals' layout: a zip of the database, `config.xml` and
//! an `INFO` note under `<backup folder>/{scheduled,manual,update}/`.
//!
//! Unarr's own zips hold its own database; a zip the original wrote holds
//! the original's. Both restore: the files are staged in
//! `<app data>/restore/` and the next start takes them from there, an
//! original's database through the importer (the originals stage theirs the
//! same way, as `<app>.restore`).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, Utc};

use crate::commands::BackupType;
use crate::db::Db;
use crate::Personality;

/// Folder under the app data a restore waits in for the next start.
pub const RESTORE_DIR: &str = "restore";

/// What a backup needs to know about the installation.
#[derive(Debug, Clone)]
pub struct BackupContext {
    pub personality: Personality,
    /// Directory holding Unarr's database and `config.xml`.
    pub app_data: PathBuf,
    /// Unarr's version, for the file name and the `INFO` note.
    pub version: String,
}

/// One zip in the backup folder.
#[derive(Debug, Clone, PartialEq)]
pub struct BackupFile {
    pub id: i64,
    pub name: String,
    pub kind: BackupType,
    pub size: u64,
    pub time: DateTime<Utc>,
    pub path: PathBuf,
}

impl BackupType {
    pub const ALL: [BackupType; 3] = [BackupType::Scheduled, BackupType::Manual, BackupType::Update];

    /// The folder and the API's name for the type.
    pub fn as_str(self) -> &'static str {
        match self {
            BackupType::Scheduled => "scheduled",
            BackupType::Manual => "manual",
            BackupType::Update => "update",
        }
    }

    /// The enum member's name, as the originals hash it into the id.
    fn member(self) -> &'static str {
        match self {
            BackupType::Scheduled => "Scheduled",
            BackupType::Manual => "Manual",
            BackupType::Update => "Update",
        }
    }
}

/// `HashConverter.GetHashInt31("backup-{type}-{name}")`.
pub fn backup_id(kind: BackupType, name: &str) -> i64 {
    let digest = ring::digest::digest(
        &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        format!("backup-{}-{name}", kind.member()).as_bytes(),
    );
    let d = digest.as_ref();
    i64::from(u32::from_le_bytes([d[0], d[1], d[2], d[3]]) & 0x7fff_ffff)
}

/// `<app>_backup_v<version>_<stamp>.zip` as the originals name theirs, or
/// Unarr's own with `unarr_` in front.
fn is_backup_name(personality: Personality, name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let rest = lower.strip_prefix("unarr_").unwrap_or(&lower);
    let app = personality.app_name().to_ascii_lowercase();
    let Some(rest) = rest
        .strip_prefix(app.as_str())
        .and_then(|r| r.strip_prefix("_backup_"))
        .and_then(|r| r.strip_suffix(".zip"))
    else {
        return false;
    };
    let stamp = match rest.strip_prefix('v') {
        // `v4.0.20.3014_2026.11.09_22.54.12`
        Some(versioned) => match versioned.split_once('_') {
            Some((version, stamp)) if version.chars().all(|c| c.is_ascii_digit() || c == '.') => stamp,
            _ => return false,
        },
        None => rest,
    };
    !stamp.is_empty() && stamp.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '_')
}

/// The `BackupFolder` setting: under the app data unless absolute.
pub fn backup_root(ctx: &BackupContext, backup_folder: Option<&str>) -> PathBuf {
    let folder = backup_folder
        .filter(|f| !f.trim().is_empty())
        .unwrap_or("Backups");
    let path = Path::new(folder);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        ctx.app_data.join(path)
    }
}

/// The backup folder as configured.
pub async fn configured_root(db: &Db, ctx: &BackupContext) -> anyhow::Result<PathBuf> {
    let folder = db
        .call(|conn| crate::store::config::config(conn, "BackupFolder"))
        .await?;
    Ok(backup_root(ctx, folder.as_deref()))
}

/// Every backup under `root`, newest first.
pub fn list(ctx: &BackupContext, root: &Path) -> Vec<BackupFile> {
    let mut found = Vec::new();
    for kind in BackupType::ALL {
        let Ok(entries) = std::fs::read_dir(root.join(kind.as_str())) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() || !is_backup_name(ctx.personality, &name) {
                continue;
            }
            let time = meta
                .modified()
                .map(DateTime::<Utc>::from)
                .unwrap_or(DateTime::UNIX_EPOCH);
            found.push(BackupFile {
                id: backup_id(kind, &name),
                kind,
                size: meta.len(),
                time,
                path: entry.path(),
                name,
            });
        }
    }
    found.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| b.name.cmp(&a.name)));
    found
}

/// `BackupService.Backup`: snapshot the database, zip it with `config.xml`
/// and the `INFO` note, then drop the backups of this type older than the
/// retention.
pub async fn create(
    db: &Db,
    ctx: &BackupContext,
    kind: BackupType,
    now: DateTime<Utc>,
) -> anyhow::Result<PathBuf> {
    let (folder, retention) = db
        .call(|conn| {
            Ok((
                crate::store::config::config(conn, "BackupFolder")?,
                crate::store::config::config(conn, "BackupRetention")?,
            ))
        })
        .await?;
    let retention_days = retention.and_then(|v| v.parse::<i64>().ok()).unwrap_or(28);
    let folder = backup_root(ctx, folder.as_deref()).join(kind.as_str());
    std::fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;

    let local = now.with_timezone(&chrono::Local);
    let name = format!(
        "unarr_{}_backup_v{}_{}.zip",
        ctx.personality.app_name().to_ascii_lowercase(),
        ctx.version,
        local.format("%Y.%m.%d_%H.%M.%S")
    );
    let target = folder.join(&name);
    let db_file = ctx.personality.db_file();

    // A consistent copy of the live database, whatever is being written.
    let temp = scratch_dir(&ctx.app_data)?;
    let snapshot = temp.join(db_file);
    let snapshot_arg = snapshot.to_string_lossy().into_owned();
    let vacuumed = db
        .call(move |conn| {
            conn.execute("VACUUM INTO ?1", [&snapshot_arg])?;
            Ok(())
        })
        .await;
    let info = format!("v{}\n{}\n", ctx.version, local.format("%Y-%m-%d %H:%M:%S"));
    let config = ctx.app_data.join("config.xml");
    let written = vacuumed.and_then(|()| {
        let partial = folder.join(format!(".{name}.partial"));
        write_zip(&partial, &snapshot, db_file, &config, &info)
            .and_then(|()| Ok(std::fs::rename(&partial, &target)?))
            .inspect_err(|_| {
                let _ = std::fs::remove_file(&partial);
            })
    });
    let _ = std::fs::remove_dir_all(&temp);
    written.with_context(|| format!("writing {}", target.display()))?;

    // `CleanupOldBackups`.
    if let Some(cutoff) = now.checked_sub_signed(chrono::Duration::days(retention_days)) {
        for old in list(ctx, folder.parent().unwrap_or(&folder)) {
            if old.kind == kind && old.time < cutoff && old.path != target {
                match std::fs::remove_file(&old.path) {
                    Ok(()) => tracing::debug!(path = %old.path.display(), "removed an old backup"),
                    Err(e) => {
                        tracing::warn!(path = %old.path.display(), error = %e, "could not remove an old backup")
                    }
                }
            }
        }
    }
    Ok(target)
}

fn scratch_dir(app_data: &Path) -> anyhow::Result<PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = app_data.join(format!(".backup-tmp-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn write_zip(target: &Path, db: &Path, db_name: &str, config: &Path, info: &str) -> anyhow::Result<()> {
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut zip = zip::ZipWriter::new(std::fs::File::create(target)?);
    if config.is_file() {
        zip.start_file("config.xml", options)?;
        std::io::copy(&mut std::fs::File::open(config)?, &mut zip)?;
    }
    zip.start_file(db_name, options.large_file(true))?;
    std::io::copy(&mut std::fs::File::open(db)?, &mut zip)?;
    zip.start_file("INFO", options)?;
    zip.write_all(info.as_bytes())?;
    zip.finish()?.sync_all()?;
    Ok(())
}

/// Whose database a staged restore holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreSource {
    /// Unarr's own: it replaces the database as it is.
    Unarr,
    /// The original application's: it goes through the importer.
    Original,
}

/// `BackupService.Restore`: take the database and `config.xml` out of the
/// zip into the restore folder, for the next start to apply. Only those two
/// entries are ever read from the archive, by name.
pub fn stage_restore(ctx: &BackupContext, zip_path: &Path) -> anyhow::Result<RestoreSource> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip_path)?)
        .with_context(|| format!("{} is not a zip archive", zip_path.display()))?;
    let (db_name, source) = if archive.by_name(ctx.personality.db_file()).is_ok() {
        (ctx.personality.db_file(), RestoreSource::Unarr)
    } else if archive.by_name(ctx.personality.legacy_db_file()).is_ok() {
        (ctx.personality.legacy_db_file(), RestoreSource::Original)
    } else {
        anyhow::bail!(
            "the backup holds neither {} nor {}",
            ctx.personality.db_file(),
            ctx.personality.legacy_db_file()
        );
    };

    let staging = ctx.app_data.join(format!("{RESTORE_DIR}.partial"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let staged = (|| {
        extract(&mut archive, db_name, &staging.join(db_name))?;
        if archive.by_name("config.xml").is_ok() {
            extract(&mut archive, "config.xml", &staging.join("config.xml"))?;
        }
        check_database(&staging.join(db_name), source)?;
        let dir = ctx.app_data.join(RESTORE_DIR);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(&staging, &dir)?;
        Ok(())
    })();
    if staged.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    staged.map(|()| source)
}

fn extract<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
    target: &Path,
) -> anyhow::Result<()> {
    let mut entry = archive.by_name(name)?;
    let mut out = std::fs::File::create(target)?;
    std::io::copy(&mut entry, &mut out)?;
    out.sync_all()?;
    Ok(())
}

/// A file that is not the database it claims to be must not reach the next
/// start, which would have nothing to open.
fn check_database(path: &Path, source: RestoreSource) -> anyhow::Result<()> {
    let table = match source {
        RestoreSource::Unarr => "meta",
        RestoreSource::Original => "VersionInfo",
    };
    let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let found: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )
        .context("the database in the backup cannot be read")?;
    anyhow::ensure!(found == 1, "the database in the backup has no {table} table");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dir: &Path) -> BackupContext {
        BackupContext {
            personality: Personality::Sonarr,
            app_data: dir.to_path_buf(),
            version: "0.0.1".into(),
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("unarr_backup_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The id the real Sonarr gave this file.
    #[test]
    fn id_is_the_originals_hash() {
        assert_eq!(
            backup_id(
                BackupType::Manual,
                "sonarr_backup_v4.0.20.3014_2026.11.09_22.54.12.zip"
            ),
            695_069_839
        );
    }

    #[test]
    fn names_of_both_applications_are_recognised() {
        let p = Personality::Sonarr;
        assert!(is_backup_name(
            p,
            "sonarr_backup_v4.0.20.3014_2026.11.09_22.54.12.zip"
        ));
        assert!(is_backup_name(p, "sonarr_backup_2026.11.09_22.54.12.zip"));
        assert!(is_backup_name(
            p,
            "unarr_sonarr_backup_v0.0.1_2026.11.09_22.54.12.zip"
        ));
        assert!(!is_backup_name(p, "radarr_backup_v5.0_2026.11.09_22.54.12.zip"));
        assert!(!is_backup_name(p, "sonarr_backup_v4_notes.zip"));
        assert!(!is_backup_name(
            p,
            "sonarr_backup_v4.0.20.3014_2026.11.09_22.54.12.zip.partial"
        ));
    }

    #[tokio::test]
    async fn backup_round_trips_through_the_restore_folder() {
        let dir = temp("roundtrip");
        std::fs::write(dir.join("config.xml"), "<Config><Port>8989</Port></Config>").unwrap();
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| crate::store::config::set_meta(conn, "probe", "kept"))
            .unwrap();
        let ctx = ctx(&dir);
        let now = Utc::now();

        let zip = create(&db, &ctx, BackupType::Manual, now).await.unwrap();
        assert!(zip.starts_with(dir.join("Backups/manual")), "{}", zip.display());
        let listed = list(&ctx, &dir.join("Backups"));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].kind, BackupType::Manual);
        assert_eq!(listed[0].id, backup_id(BackupType::Manual, &listed[0].name));
        // Nothing of the snapshot is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".backup-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        assert_eq!(stage_restore(&ctx, &zip).unwrap(), RestoreSource::Unarr);
        let restored = dir.join(RESTORE_DIR);
        assert!(restored.join("config.xml").is_file());
        let conn = rusqlite::Connection::open(restored.join("unarr-sonarr.db")).unwrap();
        assert_eq!(
            crate::store::config::meta(&conn, "probe").unwrap().as_deref(),
            Some("kept")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn old_backups_of_the_same_type_are_removed() {
        let dir = temp("retention");
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| crate::store::config::set_config(conn, "BackupRetention", "7"))
            .unwrap();
        let ctx = ctx(&dir);
        let now = Utc::now();
        let stale = |kind: &str| {
            let folder = dir.join("Backups").join(kind);
            std::fs::create_dir_all(&folder).unwrap();
            let path = folder.join("sonarr_backup_v4.0.20.3014_2020.01.01_00.00.00.zip");
            let file = std::fs::File::create(&path).unwrap();
            file.set_modified((now - chrono::Duration::days(8)).into())
                .unwrap();
            path
        };
        let (scheduled, manual) = (stale("scheduled"), stale("manual"));

        create(&db, &ctx, BackupType::Scheduled, now).await.unwrap();
        assert!(!scheduled.exists(), "past the retention");
        assert!(manual.exists(), "another type is left alone");
        assert_eq!(list(&ctx, &dir.join("Backups")).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_zip_without_a_database_is_refused() {
        let dir = temp("refused");
        let ctx = ctx(&dir);
        let path = dir.join("sonarr_backup_v4.0.20.3014_2026.11.09_22.54.12.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("../../evil.db", options).unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();
        assert!(stage_restore(&ctx, &path).is_err());

        // A file by the right name that is no database.
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file("sonarr.db", options).unwrap();
        zip.write_all(b"not a database").unwrap();
        zip.finish().unwrap();
        assert!(stage_restore(&ctx, &path).is_err());
        assert!(!dir.join(RESTORE_DIR).exists());
        assert!(!dir.join("evil.db").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
