//! Providers (indexers, download clients, notifications, import lists,
//! metadata) and the smaller settings tables.

use chrono::{DateTime, Utc};
use rusqlite::{named_params, params, Connection, Row};

use super::{from_json, parse_enum, parse_ts_precise, to_json, ts_precise};
use crate::model::{
    AutoTagging, CustomFilter, DelayProfile, ImportListExclusion, Protocol, Provider, ProviderKind,
    ReleaseProfile, RemotePathMapping, ScheduledTask, User,
};

pub fn insert_provider(conn: &Connection, p: &Provider) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO providers (kind, id, name, implementation, config_contract, settings, tags, attributes)
         VALUES (:kind, :id, :name, :implementation, :config_contract, :settings, :tags, :attributes)",
        named_params! {
            ":kind": p.kind.as_str(), ":id": p.id, ":name": p.name, ":implementation": p.implementation,
            ":config_contract": p.config_contract, ":settings": to_json(&p.settings)?,
            ":tags": to_json(&p.tags)?, ":attributes": to_json(&p.attributes)?,
        },
    )?;
    // Move the kind's sequence forward, as AUTOINCREMENT would.
    let sequence: i64 = super::config::meta(conn, &sequence_key(p.kind))?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if p.id > sequence {
        set_provider_sequence(conn, p.kind, p.id)?;
    }
    Ok(())
}

/// The `meta` key holding the last id handed out for a provider kind.
fn sequence_key(kind: ProviderKind) -> String {
    format!("sequence.{}", kind.as_str())
}

/// The id the next provider of this kind gets. The originals' tables are
/// `AUTOINCREMENT`, so a deleted id is never reused: the importer records
/// each table's `sqlite_sequence` and inserts move it forward.
pub fn next_provider_id(conn: &Connection, kind: ProviderKind) -> anyhow::Result<i64> {
    let max: Option<i64> = conn.query_row(
        "SELECT max(id) FROM providers WHERE kind = ?1",
        params![kind.as_str()],
        |r| r.get(0),
    )?;
    let sequence: i64 = super::config::meta(conn, &sequence_key(kind))?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Ok(max.unwrap_or(0).max(sequence) + 1)
}

/// Record the last id used for a kind (the importer's `sqlite_sequence`).
pub fn set_provider_sequence(conn: &Connection, kind: ProviderKind, last_id: i64) -> anyhow::Result<()> {
    super::config::set_meta(conn, &sequence_key(kind), &last_id.to_string())
}

/// Replace every column of an existing provider; `false` when there is none.
pub fn update_provider(conn: &Connection, p: &Provider) -> anyhow::Result<bool> {
    let n = conn.execute(
        "UPDATE providers SET name = :name, implementation = :implementation, config_contract = :config_contract,
            settings = :settings, tags = :tags, attributes = :attributes
         WHERE kind = :kind AND id = :id",
        named_params! {
            ":kind": p.kind.as_str(), ":id": p.id, ":name": p.name, ":implementation": p.implementation,
            ":config_contract": p.config_contract, ":settings": to_json(&p.settings)?,
            ":tags": to_json(&p.tags)?, ":attributes": to_json(&p.attributes)?,
        },
    )?;
    Ok(n == 1)
}

/// Remove a provider; `false` when there was none.
pub fn delete_provider(conn: &Connection, kind: ProviderKind, id: i64) -> anyhow::Result<bool> {
    let n = conn.execute(
        "DELETE FROM providers WHERE kind = ?1 AND id = ?2",
        params![kind.as_str(), id],
    )?;
    // `ProviderDeletedEvent`: the status goes with the provider.
    super::provider_status::delete_provider_status(conn, kind, id)?;
    if kind == ProviderKind::ImportList {
        super::import_lists::delete_list(conn, id)?;
    }
    Ok(n == 1)
}

fn provider_from_row(row: &Row) -> anyhow::Result<Provider> {
    Ok(Provider {
        kind: parse_enum!(ProviderKind, row.get("kind")?)?,
        id: row.get("id")?,
        name: row.get("name")?,
        implementation: row.get("implementation")?,
        config_contract: row.get("config_contract")?,
        settings: from_json(row.get("settings")?)?,
        tags: from_json::<Vec<i64>>(row.get("tags")?)?,
        attributes: from_json(row.get("attributes")?)?,
    })
}

pub fn list_providers(conn: &Connection, kind: ProviderKind) -> anyhow::Result<Vec<Provider>> {
    let mut stmt = conn.prepare("SELECT * FROM providers WHERE kind = ?1 ORDER BY id")?;
    let mut rows = stmt.query(params![kind.as_str()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(provider_from_row(row)?);
    }
    Ok(out)
}

pub fn get_provider(conn: &Connection, kind: ProviderKind, id: i64) -> anyhow::Result<Option<Provider>> {
    let mut stmt = conn.prepare("SELECT * FROM providers WHERE kind = ?1 AND id = ?2")?;
    let mut rows = stmt.query(params![kind.as_str(), id])?;
    match rows.next()? {
        Some(row) => Ok(Some(provider_from_row(row)?)),
        None => Ok(None),
    }
}

pub fn insert_delay_profile(conn: &Connection, d: &DelayProfile) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO delay_profiles (id, enable_usenet, enable_torrent, preferred_protocol, usenet_delay,
            torrent_delay, sort_order, tags, bypass_if_highest_quality, bypass_if_above_custom_format_score,
            minimum_custom_format_score)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            d.id,
            d.enable_usenet,
            d.enable_torrent,
            d.preferred_protocol.as_str(),
            d.usenet_delay,
            d.torrent_delay,
            d.order,
            to_json(&d.tags)?,
            d.bypass_if_highest_quality,
            d.bypass_if_above_custom_format_score,
            d.minimum_custom_format_score
        ],
    )?;
    Ok(())
}

pub fn list_delay_profiles(conn: &Connection) -> anyhow::Result<Vec<DelayProfile>> {
    let mut stmt = conn.prepare("SELECT * FROM delay_profiles ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(DelayProfile {
            id: row.get("id")?,
            enable_usenet: row.get("enable_usenet")?,
            enable_torrent: row.get("enable_torrent")?,
            preferred_protocol: parse_enum!(Protocol, row.get("preferred_protocol")?)?,
            usenet_delay: row.get("usenet_delay")?,
            torrent_delay: row.get("torrent_delay")?,
            order: row.get("sort_order")?,
            tags: from_json::<Vec<i64>>(row.get("tags")?)?,
            bypass_if_highest_quality: row.get("bypass_if_highest_quality")?,
            bypass_if_above_custom_format_score: row.get("bypass_if_above_custom_format_score")?,
            minimum_custom_format_score: row.get("minimum_custom_format_score")?,
        });
    }
    Ok(out)
}

pub fn insert_release_profile(conn: &Connection, r: &ReleaseProfile) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO release_profiles (id, name, enabled, required, ignored, indexer_id, tags)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            r.id,
            r.name,
            r.enabled,
            to_json(&r.required)?,
            to_json(&r.ignored)?,
            r.indexer_id,
            to_json(&r.tags)?
        ],
    )?;
    Ok(())
}

pub fn list_release_profiles(conn: &Connection) -> anyhow::Result<Vec<ReleaseProfile>> {
    let mut stmt = conn.prepare("SELECT * FROM release_profiles ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(ReleaseProfile {
            id: row.get("id")?,
            name: row.get("name")?,
            enabled: row.get("enabled")?,
            required: from_json::<Vec<String>>(row.get("required")?)?,
            ignored: from_json::<Vec<String>>(row.get("ignored")?)?,
            indexer_id: row.get("indexer_id")?,
            tags: from_json::<Vec<i64>>(row.get("tags")?)?,
        });
    }
    Ok(out)
}

pub fn insert_remote_path_mapping(conn: &Connection, m: &RemotePathMapping) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO remote_path_mappings (id, host, remote_path, local_path) VALUES (?1, ?2, ?3, ?4)",
        params![m.id, m.host, m.remote_path, m.local_path],
    )?;
    Ok(())
}

pub fn list_remote_path_mappings(conn: &Connection) -> anyhow::Result<Vec<RemotePathMapping>> {
    let mut stmt =
        conn.prepare("SELECT id, host, remote_path, local_path FROM remote_path_mappings ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok(RemotePathMapping {
            id: r.get(0)?,
            host: r.get(1)?,
            remote_path: r.get(2)?,
            local_path: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn insert_custom_filter(conn: &Connection, f: &CustomFilter) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO custom_filters (id, type, label, filters) VALUES (?1, ?2, ?3, ?4)",
        params![f.id, f.filter_type, f.label, to_json(&f.filters)?],
    )?;
    Ok(())
}

pub fn list_custom_filters(conn: &Connection) -> anyhow::Result<Vec<CustomFilter>> {
    let mut stmt = conn.prepare("SELECT id, type, label, filters FROM custom_filters ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(CustomFilter {
            id: r.get(0)?,
            filter_type: r.get(1)?,
            label: r.get(2)?,
            filters: from_json(r.get(3)?)?,
        });
    }
    Ok(out)
}

pub fn insert_import_list_exclusion(conn: &Connection, e: &ImportListExclusion) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO import_list_exclusions (id, tvdb_id, title) VALUES (?1, ?2, ?3)",
        params![e.id, e.tvdb_id, e.title],
    )?;
    Ok(())
}

pub fn list_import_list_exclusions(conn: &Connection) -> anyhow::Result<Vec<ImportListExclusion>> {
    let mut stmt = conn.prepare("SELECT id, tvdb_id, title FROM import_list_exclusions ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok(ImportListExclusion {
            id: r.get(0)?,
            tvdb_id: r.get(1)?,
            title: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn insert_auto_tagging(conn: &Connection, a: &AutoTagging) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO auto_tagging (id, name, remove_tags_automatically, tags, specifications)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            a.id,
            a.name,
            a.remove_tags_automatically,
            to_json(&a.tags)?,
            to_json(&a.specifications)?
        ],
    )?;
    Ok(())
}

pub fn list_auto_tagging(conn: &Connection) -> anyhow::Result<Vec<AutoTagging>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, remove_tags_automatically, tags, specifications FROM auto_tagging ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(AutoTagging {
            id: r.get(0)?,
            name: r.get(1)?,
            remove_tags_automatically: r.get(2)?,
            tags: from_json::<Vec<i64>>(r.get(3)?)?,
            specifications: from_json(r.get(4)?)?,
        });
    }
    Ok(out)
}

pub fn insert_user(conn: &Connection, u: &User) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO users (id, identifier, username, password, salt, iterations) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![u.id, u.identifier, u.username, u.password, u.salt, u.iterations],
    )?;
    Ok(())
}

/// A user's name and password replaced (`UserService.Update`).
pub fn update_user(conn: &Connection, u: &User) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE users SET username = ?2, password = ?3, salt = ?4, iterations = ?5 WHERE id = ?1",
        params![u.id, u.username, u.password, u.salt, u.iterations],
    )?;
    Ok(())
}

pub fn list_users(conn: &Connection) -> anyhow::Result<Vec<User>> {
    let mut stmt =
        conn.prepare("SELECT id, identifier, username, password, salt, iterations FROM users ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok(User {
            id: r.get(0)?,
            identifier: r.get(1)?,
            username: r.get(2)?,
            password: r.get(3)?,
            salt: r.get(4)?,
            iterations: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Insert a task; an id of zero takes the next free one.
pub fn insert_scheduled_task(conn: &Connection, t: &ScheduledTask) -> anyhow::Result<()> {
    let id = if t.id == 0 {
        let max: Option<i64> = conn.query_row("SELECT max(id) FROM scheduled_tasks", [], |r| r.get(0))?;
        max.unwrap_or(0) + 1
    } else {
        t.id
    };
    conn.execute(
        "INSERT INTO scheduled_tasks (id, type_name, interval_minutes, last_execution, last_start_time)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            id,
            t.type_name,
            t.interval_minutes,
            ts_precise(t.last_execution),
            ts_precise(t.last_start_time)
        ],
    )?;
    Ok(())
}

pub fn delete_scheduled_task(conn: &Connection, id: i64) -> anyhow::Result<()> {
    conn.execute("DELETE FROM scheduled_tasks WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn set_scheduled_task_interval(conn: &Connection, id: i64, interval_minutes: i64) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE scheduled_tasks SET interval_minutes = ?2 WHERE id = ?1",
        params![id, interval_minutes],
    )?;
    Ok(())
}

/// The last run alone, as `FixFutureRunScheduledTasks` sets it.
pub fn set_scheduled_task_last_execution(
    conn: &Connection,
    id: i64,
    last_execution: DateTime<Utc>,
) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE scheduled_tasks SET last_execution = ?2 WHERE id = ?1",
        params![id, ts_precise(Some(last_execution))],
    )?;
    Ok(())
}

/// `SetLastExecutionTime`.
pub fn set_scheduled_task_execution(
    conn: &Connection,
    id: i64,
    last_execution: DateTime<Utc>,
    last_start_time: DateTime<Utc>,
) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE scheduled_tasks SET last_execution = ?2, last_start_time = ?3 WHERE id = ?1",
        params![
            id,
            ts_precise(Some(last_execution)),
            ts_precise(Some(last_start_time))
        ],
    )?;
    Ok(())
}

pub fn list_scheduled_tasks(conn: &Connection) -> anyhow::Result<Vec<ScheduledTask>> {
    let mut stmt = conn.prepare(
        "SELECT id, type_name, interval_minutes, last_execution, last_start_time FROM scheduled_tasks ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(ScheduledTask {
            id: r.get(0)?,
            type_name: r.get(1)?,
            interval_minutes: r.get(2)?,
            last_execution: parse_ts_precise(r.get(3)?)?,
            last_start_time: parse_ts_precise(r.get(4)?)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn round_trips_providers_and_settings_tables() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            let idx = Provider {
                kind: ProviderKind::Indexer,
                id: 1,
                name: "Fixture Newznab".into(),
                implementation: "Newznab".into(),
                config_contract: "NewznabSettings".into(),
                settings: serde_json::json!({"baseUrl": "http://indexer.invalid", "categories": [5030]}),
                tags: vec![],
                attributes: serde_json::json!({"enableRss": true, "priority": 25}),
            };
            insert_provider(conn, &idx)?;
            // Same id, different kind: separate sequences in the originals.
            insert_provider(
                conn,
                &Provider {
                    kind: ProviderKind::DownloadClient,
                    ..idx.clone()
                },
            )?;
            assert_eq!(list_providers(conn, ProviderKind::Indexer)?, vec![idx.clone()]);
            assert_eq!(get_provider(conn, ProviderKind::Indexer, 1)?, Some(idx.clone()));
            assert_eq!(get_provider(conn, ProviderKind::Notification, 1)?, None);
            assert_eq!(next_provider_id(conn, ProviderKind::Indexer)?, 2);
            assert_eq!(next_provider_id(conn, ProviderKind::Notification)?, 1);
            let renamed = Provider {
                name: "Renamed".into(),
                attributes: serde_json::json!({"enableRss": false, "priority": 1}),
                ..idx.clone()
            };
            assert!(update_provider(conn, &renamed)?);
            assert_eq!(get_provider(conn, ProviderKind::Indexer, 1)?, Some(renamed));
            assert!(!update_provider(
                conn,
                &Provider {
                    id: 99,
                    ..idx.clone()
                }
            )?);
            assert!(delete_provider(conn, ProviderKind::Indexer, 1)?);
            assert!(!delete_provider(conn, ProviderKind::Indexer, 1)?);
            assert!(list_providers(conn, ProviderKind::Indexer)?.is_empty());
            // A deleted id is not reused, as AUTOINCREMENT would not.
            assert_eq!(next_provider_id(conn, ProviderKind::Indexer)?, 2);
            set_provider_sequence(conn, ProviderKind::Indexer, 19)?;
            assert_eq!(next_provider_id(conn, ProviderKind::Indexer)?, 20);
            // The other kind with the same id is untouched.
            assert!(get_provider(conn, ProviderKind::DownloadClient, 1)?.is_some());

            let delay = DelayProfile {
                id: 1,
                enable_usenet: true,
                enable_torrent: true,
                preferred_protocol: Protocol::Usenet,
                usenet_delay: 30,
                torrent_delay: 0,
                order: i32::MAX as i64,
                tags: vec![],
                bypass_if_highest_quality: true,
                bypass_if_above_custom_format_score: false,
                minimum_custom_format_score: 0,
            };
            insert_delay_profile(conn, &delay)?;
            assert_eq!(list_delay_profiles(conn)?, vec![delay]);

            let rp = ReleaseProfile {
                id: 1,
                name: Some("No HDR".into()),
                enabled: true,
                required: vec![],
                ignored: vec!["HDR".into(), "DV".into()],
                indexer_id: 0,
                tags: vec![1],
            };
            insert_release_profile(conn, &rp)?;
            assert_eq!(list_release_profiles(conn)?, vec![rp]);

            let m = RemotePathMapping {
                id: 1,
                host: "sab.invalid".into(),
                remote_path: "/downloads/".into(),
                local_path: "/data/downloads/".into(),
            };
            insert_remote_path_mapping(conn, &m)?;
            assert_eq!(list_remote_path_mappings(conn)?, vec![m]);

            let f = CustomFilter {
                id: 1,
                filter_type: "series".into(),
                label: "Anime only".into(),
                filters: serde_json::json!([{"key": "seriesType", "value": ["anime"], "type": "equal"}]),
            };
            insert_custom_filter(conn, &f)?;
            assert_eq!(list_custom_filters(conn)?, vec![f]);

            let t = ScheduledTask {
                id: 1,
                type_name: "NzbDrone.Core.Backup.BackupCommand".into(),
                interval_minutes: 10080,
                last_execution: crate::time::parse_utc("2026-11-06 23:23:13.9033925Z"),
                last_start_time: crate::time::parse_utc("0001-01-01 00:00:00Z"),
            };
            insert_scheduled_task(conn, &t)?;
            assert_eq!(list_scheduled_tasks(conn)?, vec![t]);
            assert!(list_users(conn)?.is_empty());
            assert!(list_import_list_exclusions(conn)?.is_empty());
            assert!(list_auto_tagging(conn)?.is_empty());
            Ok(())
        })
        .unwrap();
    }
}
