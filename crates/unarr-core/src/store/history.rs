//! History and blocklist.

use chrono::{DateTime, Utc};
use rusqlite::{named_params, params, Connection, Row};

use super::paging::Paging;
use super::{from_json, parse_enum, parse_ts, parse_ts_required, quality_from_row, to_json, ts};
use crate::model::{BlocklistEntry, HistoryEntry, HistoryEvent, Protocol, ReleaseType};
use crate::time;

pub fn insert(conn: &Connection, h: &HistoryEntry) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO history (id, series_id, episode_id, source_title, date, event_type, quality_id,
            revision_version, revision_real, revision_is_repack, languages, data, download_id)
         VALUES (:id, :series_id, :episode_id, :source_title, :date, :event_type, :quality_id,
            :revision_version, :revision_real, :revision_is_repack, :languages, :data, :download_id)",
        named_params! {
            ":id": h.id, ":series_id": h.series_id, ":episode_id": h.episode_id,
            ":source_title": h.source_title, ":date": ts(Some(h.date)), ":event_type": h.event_type.as_str(),
            ":quality_id": h.quality.quality_id, ":revision_version": h.quality.version,
            ":revision_real": h.quality.real, ":revision_is_repack": h.quality.is_repack,
            ":languages": to_json(&h.languages)?, ":data": to_json(&h.data)?, ":download_id": h.download_id,
        },
    )?;
    Ok(())
}

fn from_row(row: &Row) -> anyhow::Result<HistoryEntry> {
    Ok(HistoryEntry {
        id: row.get("id")?,
        series_id: row.get("series_id")?,
        episode_id: row.get("episode_id")?,
        source_title: row.get("source_title")?,
        date: parse_ts_required(row.get("date")?)?,
        event_type: parse_enum!(HistoryEvent, row.get("event_type")?)?,
        quality: quality_from_row(row)?,
        languages: from_json::<Vec<i64>>(row.get("languages")?)?,
        data: from_json(row.get("data")?)?,
        download_id: row.get("download_id")?,
    })
}

fn query(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> anyhow::Result<Vec<HistoryEntry>> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(from_row(row)?);
    }
    Ok(out)
}

/// The next history id; ids are never reused, as the originals' are not.
pub fn next_id(conn: &Connection) -> anyhow::Result<i64> {
    super::sequence::next_id(conn, "history")
}

/// The first of `count` consecutive history ids reserved at once.
pub fn next_ids(conn: &Connection, count: usize) -> anyhow::Result<i64> {
    super::sequence::next_ids(conn, "history", count)
}

/// `FindByDownloadId`: every row of one download, newest first.
pub fn find_by_download_id(conn: &Connection, download_id: &str) -> anyhow::Result<Vec<HistoryEntry>> {
    query(
        conn,
        "SELECT h.* FROM history h WHERE h.download_id = ?1 ORDER BY h.date DESC, h.id DESC",
        &[&download_id],
    )
}

/// The next free blocklist id.
pub fn next_blocklist_id(conn: &Connection) -> anyhow::Result<i64> {
    super::sequence::next_id(conn, "blocklist")
}

/// Filters of the paged `history` endpoint.
#[derive(Debug, Default, Clone)]
pub struct HistoryFilter {
    pub event_types: Vec<HistoryEvent>,
    pub episode_id: Option<i64>,
    pub download_id: Option<String>,
    pub series_ids: Vec<i64>,
    pub languages: Vec<i64>,
    pub qualities: Vec<i64>,
}

fn filter_sql(f: &HistoryFilter) -> String {
    let mut clauses = vec!["1 = 1".to_string()];
    if !f.event_types.is_empty() {
        let list: Vec<String> = f
            .event_types
            .iter()
            .map(|e| format!("'{}'", e.as_str()))
            .collect();
        clauses.push(format!("h.event_type IN ({})", list.join(",")));
    }
    if let Some(id) = f.episode_id {
        clauses.push(format!("h.episode_id = {id}"));
    }
    if let Some(d) = &f.download_id {
        clauses.push(format!("h.download_id = '{}'", d.replace('\'', "''")));
    }
    if !f.series_ids.is_empty() {
        let list: Vec<String> = f.series_ids.iter().map(|i| i.to_string()).collect();
        clauses.push(format!("h.series_id IN ({})", list.join(",")));
    }
    if !f.qualities.is_empty() {
        let list: Vec<String> = f.qualities.iter().map(|i| i.to_string()).collect();
        clauses.push(format!("h.quality_id IN ({})", list.join(",")));
    }
    if !f.languages.is_empty() {
        // Languages are a JSON array of ids; match any of the requested ones.
        let any: Vec<String> = f
            .languages
            .iter()
            .map(|l| format!("EXISTS (SELECT 1 FROM json_each(h.languages) WHERE value = {l})"))
            .collect();
        clauses.push(format!("({})", any.join(" OR ")));
    }
    clauses.join(" AND ")
}

/// One page of history, joined with series and episodes as the originals do
/// (rows whose series or episode is gone are not listed).
pub fn paged(
    conn: &Connection,
    paging: &Paging,
    filter: &HistoryFilter,
) -> anyhow::Result<(Vec<HistoryEntry>, i64)> {
    let base = format!(
        "FROM history h JOIN series s ON s.id = h.series_id JOIN episodes e ON e.id = h.episode_id WHERE {}",
        filter_sql(filter)
    );
    let total: i64 = conn.query_row(&format!("SELECT count(*) {base}"), [], |r| r.get(0))?;
    let column = match paging.sort_key.as_str() {
        "series.sortTitle" => "s.sort_title",
        _ => "h.date",
    };
    let sql = format!(
        "SELECT h.* {base} ORDER BY {column} {dir}, h.id {dir} LIMIT ?1 OFFSET ?2",
        dir = paging.direction_sql()
    );
    let records = query(conn, &sql, &[&paging.page_size, &paging.offset()])?;
    Ok((records, total))
}

/// One page of the entries since `date`, oldest first, optionally of one
/// event type, in `(date, id)` order after the cursor: a full
/// `history/since` on a large library streams page by page instead of
/// building one response in memory.
pub fn since_page(
    conn: &Connection,
    date: DateTime<Utc>,
    event_type: Option<HistoryEvent>,
    after: Option<(DateTime<Utc>, i64)>,
    limit: i64,
) -> anyhow::Result<Vec<HistoryEntry>> {
    let date = time::format_utc(date);
    let event = event_type.map(|e| e.as_str().to_string());
    let (after_date, after_id) = match after {
        Some((d, id)) => (Some(time::format_utc(d)), id),
        None => (None, 0),
    };
    query(
        conn,
        "SELECT h.* FROM history h JOIN series s ON s.id = h.series_id JOIN episodes e ON e.id = h.episode_id
         WHERE h.date >= ?1 AND (?2 IS NULL OR h.event_type = ?2)
           AND (?3 IS NULL OR h.date > ?3 OR (h.date = ?3 AND h.id > ?4))
         ORDER BY h.date, h.id LIMIT ?5",
        &[&date, &event, &after_date, &after_id, &limit],
    )
}

/// Entries of one series (optionally one season), newest first.
pub fn for_series(
    conn: &Connection,
    series_id: i64,
    season: Option<i64>,
    event_type: Option<HistoryEvent>,
) -> anyhow::Result<Vec<HistoryEntry>> {
    let event = event_type.map(|e| e.as_str().to_string());
    query(
        conn,
        "SELECT h.* FROM history h JOIN episodes e ON e.id = h.episode_id
         WHERE h.series_id = ?1 AND (?2 IS NULL OR e.season_number = ?2) AND (?3 IS NULL OR h.event_type = ?3)
         ORDER BY h.date DESC, h.id DESC",
        &[&series_id, &season, &event],
    )
}

pub fn insert_blocklist(conn: &Connection, b: &BlocklistEntry) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO blocklist (id, series_id, episode_ids, source_title, quality_id, revision_version,
            revision_real, revision_is_repack, date, published_date, size, protocol, indexer, message,
            torrent_info_hash, languages, indexer_flags, release_type)
         VALUES (:id, :series_id, :episode_ids, :source_title, :quality_id, :revision_version, :revision_real,
            :revision_is_repack, :date, :published_date, :size, :protocol, :indexer, :message,
            :torrent_info_hash, :languages, :indexer_flags, :release_type)",
        named_params! {
            ":id": b.id, ":series_id": b.series_id, ":episode_ids": to_json(&b.episode_ids)?,
            ":source_title": b.source_title, ":quality_id": b.quality.quality_id,
            ":revision_version": b.quality.version, ":revision_real": b.quality.real,
            ":revision_is_repack": b.quality.is_repack, ":date": ts(Some(b.date)),
            ":published_date": ts(b.published_date), ":size": b.size, ":protocol": b.protocol.as_str(),
            ":indexer": b.indexer, ":message": b.message, ":torrent_info_hash": b.torrent_info_hash,
            ":languages": to_json(&b.languages)?, ":indexer_flags": b.indexer_flags,
            ":release_type": b.release_type.as_str(),
        },
    )?;
    Ok(())
}

fn blocklist_from_row(row: &rusqlite::Row) -> anyhow::Result<BlocklistEntry> {
    Ok(BlocklistEntry {
        id: row.get("id")?,
        series_id: row.get("series_id")?,
        episode_ids: from_json::<Vec<i64>>(row.get("episode_ids")?)?,
        source_title: row.get("source_title")?,
        quality: quality_from_row(row)?,
        date: parse_ts_required(row.get("date")?)?,
        published_date: parse_ts(row.get("published_date")?)?,
        size: row.get("size")?,
        protocol: parse_enum!(Protocol, row.get("protocol")?)?,
        indexer: row.get("indexer")?,
        message: row.get("message")?,
        torrent_info_hash: row.get("torrent_info_hash")?,
        languages: from_json::<Vec<i64>>(row.get("languages")?)?,
        indexer_flags: row.get("indexer_flags")?,
        release_type: parse_enum!(ReleaseType, row.get("release_type")?)?,
    })
}

/// One page of the blocklist, joined with series as the originals do. Ties on
/// the sort column come in id order whatever the direction: SQLite's sorter
/// keeps the scan order, and the originals add no secondary key.
pub fn blocklist_paged(
    conn: &Connection,
    paging: &Paging,
    series_ids: &[i64],
    protocols: Option<&[Protocol]>,
) -> anyhow::Result<(Vec<BlocklistEntry>, i64)> {
    let column = match paging.sort_key.as_str() {
        "indexer" => "b.indexer",
        "series.sortTitle" => "s.sort_title",
        "sourceTitle" => "b.source_title",
        _ => "b.date",
    };
    let filter = if series_ids.is_empty() {
        "1 = 1".to_string()
    } else {
        let ids: Vec<String> = series_ids.iter().map(i64::to_string).collect();
        format!("b.series_id IN ({})", ids.join(","))
    };
    // The rows of the protocols asked for; of none, when the filter names
    // only a protocol no row has.
    let filter = match protocols {
        None => filter,
        Some(protocols) => {
            let names: Vec<String> = protocols.iter().map(|p| format!("'{}'", p.as_str())).collect();
            format!("{filter} AND b.protocol IN ({})", names.join(","))
        }
    };
    let base = format!("FROM blocklist b JOIN series s ON s.id = b.series_id WHERE {filter}");
    let total: i64 = conn.query_row(&format!("SELECT count(*) {base}"), [], |r| r.get(0))?;
    let sql = format!(
        "SELECT b.* {base} ORDER BY {column} {} , b.id LIMIT ?1 OFFSET ?2",
        paging.direction_sql()
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![paging.page_size, paging.offset()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(blocklist_from_row(row)?);
    }
    Ok((out, total))
}

/// `HistoryService.FindByEpisodeId`: every entry of an episode, newest first.
pub fn for_episode(conn: &Connection, episode_id: i64) -> anyhow::Result<Vec<HistoryEntry>> {
    query(
        conn,
        "SELECT h.* FROM history h WHERE h.episode_id = ?1 ORDER BY h.date DESC, h.id DESC",
        &[&episode_id],
    )
}

/// `FindDownloadHistory(seriesId, quality)`: the grabs, failures and
/// imports of a series at one quality, for finding the download a manual
/// import belongs to.
pub fn download_history_for(
    conn: &Connection,
    series_id: i64,
    quality: &crate::model::QualityModel,
) -> anyhow::Result<Vec<HistoryEntry>> {
    query(
        conn,
        "SELECT h.* FROM history h WHERE h.series_id = ?1 AND h.quality_id = ?2 AND h.revision_version = ?3
            AND h.revision_real = ?4 AND h.revision_is_repack = ?5
            AND h.event_type IN ('grabbed', 'downloadFailed', 'downloadFolderImported')
         ORDER BY h.date DESC, h.id DESC",
        &[
            &series_id,
            &quality.quality_id,
            &quality.version,
            &quality.real,
            &quality.is_repack,
        ],
    )
}

/// The blocklist entries of a series, oldest first.
/// `ClearBlocklist`: every row.
pub fn clear_blocklist(conn: &Connection) -> anyhow::Result<()> {
    conn.execute("DELETE FROM blocklist", [])?;
    Ok(())
}

/// `BlocklistService.Delete`: the rows with these ids; how many went.
pub fn delete_blocklist(conn: &Connection, ids: &[i64]) -> anyhow::Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut removed = 0;
    for id in ids {
        removed += tx.execute("DELETE FROM blocklist WHERE id = ?1", params![id])?;
    }
    tx.commit()?;
    Ok(removed)
}

/// One history row by id.
pub fn get(conn: &Connection, id: i64) -> anyhow::Result<Option<HistoryEntry>> {
    Ok(query(conn, "SELECT h.* FROM history h WHERE h.id = ?1", &[&id])?.pop())
}

pub fn blocklist_for_series(conn: &Connection, series_id: i64) -> anyhow::Result<Vec<BlocklistEntry>> {
    let mut stmt = conn.prepare("SELECT * FROM blocklist WHERE series_id = ?1 ORDER BY id")?;
    let mut rows = stmt.query(params![series_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(blocklist_from_row(row)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::model::QualityModel;
    use chrono::TimeZone;

    #[test]
    fn round_trips_history_newest_first() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            let mk = |id: i64, secs: u32, event| HistoryEntry {
                id,
                series_id: 1,
                episode_id: 7,
                source_title: Some("Firefly.S01E01".into()),
                date: chrono::Utc.with_ymd_and_hms(2026, 11, 6, 23, 23, secs).unwrap(),
                event_type: event,
                quality: QualityModel {
                    quality_id: 7,
                    version: 1,
                    real: 0,
                    is_repack: false,
                },
                languages: vec![1],
                data: serde_json::json!({"size": "407344", "downloadClient": null}),
                download_id: None,
            };
            insert(conn, &mk(1, 10, HistoryEvent::DownloadFolderImported))?;
            insert(conn, &mk(2, 20, HistoryEvent::EpisodeFileDeleted))?;
            // The public readers join series and episodes (none here); read raw.
            let all = query(conn, "SELECT * FROM history ORDER BY date DESC, id DESC", &[])?;
            assert_eq!(all.iter().map(|h| h.id).collect::<Vec<_>>(), vec![2, 1]);
            assert_eq!(all[0], mk(2, 20, HistoryEvent::EpisodeFileDeleted));
            assert_eq!(for_series(conn, 9, None, None)?.len(), 0);
            let paging = Paging::resolve(None, Some(10), None, None, &["date"], "date");
            let (page, total) = blocklist_paged(conn, &paging, &[], Some(&[Protocol::Torrent]))?;
            assert_eq!((page.len(), total), (0, 0));
            Ok(())
        })
        .unwrap();
    }
}
