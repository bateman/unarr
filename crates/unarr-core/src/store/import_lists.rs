//! What the import lists returned and when: the originals'
//! `ImportListItems` and the sync half of `ImportListStatus`.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use super::{parse_ts, ts};

/// `ImportListItemInfo`: a series as a list names it. Ids a list does not
/// give are 0 (or `None` for IMDb's), as the originals store them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ListItem {
    pub id: i64,
    pub import_list_id: i64,
    pub title: String,
    pub tvdb_id: i64,
    pub year: i64,
    pub tmdb_id: i64,
    pub imdb_id: Option<String>,
    pub mal_id: i64,
    pub ani_list_id: i64,
}

/// `ImportListStatus.LastInfoSync` and `HasRemovedItemSinceLastClean`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncStatus {
    pub last_info_sync: Option<DateTime<Utc>>,
    pub has_removed_item_since_last_clean: bool,
}

pub fn insert_item(conn: &Connection, item: &ListItem) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO import_list_items (import_list_id, title, tvdb_id, year, tmdb_id, imdb_id,
            mal_id, ani_list_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            item.import_list_id,
            item.title,
            item.tvdb_id,
            item.year,
            item.tmdb_id,
            item.imdb_id,
            item.mal_id,
            item.ani_list_id
        ],
    )?;
    Ok(())
}

fn items_where(conn: &Connection, clause: &str, list: Option<i64>) -> anyhow::Result<Vec<ListItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, import_list_id, title, tvdb_id, year, tmdb_id, imdb_id, mal_id, ani_list_id
         FROM import_list_items {clause} ORDER BY id"
    ))?;
    let map = |r: &rusqlite::Row| {
        Ok(ListItem {
            id: r.get(0)?,
            import_list_id: r.get(1)?,
            title: r.get(2)?,
            tvdb_id: r.get(3)?,
            year: r.get(4)?,
            tmdb_id: r.get(5)?,
            imdb_id: r.get(6)?,
            mal_id: r.get(7)?,
            ani_list_id: r.get(8)?,
        })
    };
    let rows = match list {
        Some(id) => stmt.query_map(params![id], map)?.collect::<Result<_, _>>()?,
        None => stmt.query_map([], map)?.collect::<Result<_, _>>()?,
    };
    Ok(rows)
}

/// Every item of every list.
pub fn all_items(conn: &Connection) -> anyhow::Result<Vec<ListItem>> {
    items_where(conn, "", None)
}

/// `ImportListItemService.SyncSeriesForList`: bring the stored items of a
/// list in line with what it just returned. A stored item is the same as
/// a returned one when a non-zero TVDB id matches, else the IMDb, TMDb,
/// MAL or AniList id, in that order; matched ones are updated, new ones
/// inserted, the rest deleted. Returns how many were deleted.
pub fn sync_items(conn: &Connection, list_id: i64, items: &[ListItem]) -> anyhow::Result<usize> {
    let mut existing = items_where(conn, "WHERE import_list_id = ?1", Some(list_id))?;
    for item in items {
        let found = existing.iter().position(|e| {
            if item.tvdb_id > 0 {
                return e.tvdb_id == item.tvdb_id;
            }
            if let Some(imdb) = item.imdb_id.as_deref().filter(|s| !s.is_empty()) {
                return e.imdb_id.as_deref() == Some(imdb);
            }
            if item.tmdb_id > 0 {
                return e.tmdb_id == item.tmdb_id;
            }
            if item.mal_id > 0 {
                return e.mal_id == item.mal_id;
            }
            item.ani_list_id > 0 && e.ani_list_id == item.ani_list_id
        });
        match found {
            Some(at) => {
                let stored = existing.remove(at);
                conn.execute(
                    "UPDATE import_list_items SET title = ?2, tvdb_id = ?3, year = ?4, tmdb_id = ?5,
                        imdb_id = ?6, mal_id = ?7, ani_list_id = ?8 WHERE id = ?1",
                    params![
                        stored.id,
                        item.title,
                        item.tvdb_id,
                        item.year,
                        item.tmdb_id,
                        item.imdb_id,
                        item.mal_id,
                        item.ani_list_id
                    ],
                )?;
            }
            None => insert_item(
                conn,
                &ListItem {
                    import_list_id: list_id,
                    ..item.clone()
                },
            )?,
        }
    }
    for gone in &existing {
        conn.execute("DELETE FROM import_list_items WHERE id = ?1", params![gone.id])?;
    }
    Ok(existing.len())
}

pub fn sync_status(conn: &Connection, list_id: i64) -> anyhow::Result<SyncStatus> {
    let found = conn
        .query_row(
            "SELECT last_info_sync, has_removed_item_since_last_clean FROM import_list_status
             WHERE provider_id = ?1",
            params![list_id],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, bool>(1)?)),
        )
        .optional()?;
    Ok(match found {
        Some((last, removed)) => SyncStatus {
            last_info_sync: parse_ts(last)?,
            has_removed_item_since_last_clean: removed,
        },
        None => SyncStatus::default(),
    })
}

pub fn set_sync_status(conn: &Connection, list_id: i64, status: &SyncStatus) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO import_list_status (provider_id, last_info_sync, has_removed_item_since_last_clean)
         VALUES (?1, ?2, ?3)
         ON CONFLICT (provider_id) DO UPDATE SET last_info_sync = ?2,
            has_removed_item_since_last_clean = ?3",
        params![
            list_id,
            ts(status.last_info_sync),
            status.has_removed_item_since_last_clean
        ],
    )?;
    Ok(())
}

/// `UpdateListSyncStatus`: the list was fetched at `now`; `removed` adds
/// to what is owed to the next clean.
pub fn record_sync(conn: &Connection, list_id: i64, now: DateTime<Utc>, removed: bool) -> anyhow::Result<()> {
    let mut status = sync_status(conn, list_id)?;
    status.last_info_sync = Some(now);
    status.has_removed_item_since_last_clean |= removed;
    set_sync_status(conn, list_id, &status)
}

/// `MarkListsAsCleaned`.
pub fn mark_cleaned(conn: &Connection) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE import_list_status SET has_removed_item_since_last_clean = 0",
        [],
    )?;
    Ok(())
}

/// A deleted list takes its items and its status along.
pub fn delete_list(conn: &Connection, list_id: i64) -> anyhow::Result<()> {
    // What a list named is one table in Sonarr (series) and another in
    // Radarr (movies); each database holds only its own.
    for table in ["import_list_items", "import_list_movies"] {
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )?;
        if exists {
            conn.execute(
                &format!("DELETE FROM {table} WHERE import_list_id = ?1"),
                params![list_id],
            )?;
        }
    }
    conn.execute(
        "DELETE FROM import_list_status WHERE provider_id = ?1",
        params![list_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use chrono::TimeZone;

    fn item(title: &str, tvdb: i64, imdb: Option<&str>) -> ListItem {
        ListItem {
            title: title.into(),
            tvdb_id: tvdb,
            imdb_id: imdb.map(str::to_string),
            ..ListItem::default()
        }
    }

    #[test]
    fn a_sync_updates_inserts_and_counts_what_left() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            assert_eq!(
                sync_items(conn, 1, &[item("A", 10, None), item("B", 0, Some("tt1"))])?,
                0
            );
            assert_eq!(sync_items(conn, 2, &[item("Other", 99, None)])?, 0);
            // A stays (renamed), B is matched by its IMDb id, C is new.
            let removed = sync_items(
                conn,
                1,
                &[
                    item("A2", 10, None),
                    item("B", 0, Some("tt1")),
                    item("C", 30, None),
                ],
            )?;
            assert_eq!(removed, 0);
            let titles: Vec<String> = all_items(conn)?
                .into_iter()
                .filter(|i| i.import_list_id == 1)
                .map(|i| i.title)
                .collect();
            assert_eq!(titles, ["A2", "B", "C"]);
            assert_eq!(sync_items(conn, 1, &[item("C", 30, None)])?, 2);
            assert_eq!(all_items(conn)?.len(), 2);

            let now = Utc.with_ymd_and_hms(2026, 11, 9, 12, 0, 0).unwrap();
            assert_eq!(sync_status(conn, 1)?, SyncStatus::default());
            record_sync(conn, 1, now, true)?;
            record_sync(conn, 1, now, false)?;
            assert_eq!(
                sync_status(conn, 1)?,
                SyncStatus {
                    last_info_sync: Some(now),
                    has_removed_item_since_last_clean: true
                }
            );
            mark_cleaned(conn)?;
            assert!(!sync_status(conn, 1)?.has_removed_item_since_last_clean);
            delete_list(conn, 1)?;
            assert_eq!(all_items(conn)?.len(), 1);
            assert_eq!(sync_status(conn, 1)?, SyncStatus::default());
            Ok(())
        })
        .unwrap();
    }
}
