//! Scene mappings (alternate titles).

use std::collections::HashMap;

use rusqlite::{named_params, params, Connection, Row};

use crate::model::SceneMapping;

pub fn insert(conn: &Connection, m: &SceneMapping) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO scene_mappings (id, tvdb_id, season_number, search_term, parse_term, title, type,
            scene_season_number, filter_regex, scene_origin, search_mode, comment)
         VALUES (:id, :tvdb_id, :season_number, :search_term, :parse_term, :title, :type,
            :scene_season_number, :filter_regex, :scene_origin, :search_mode, :comment)",
        named_params! {
            ":id": m.id, ":tvdb_id": m.tvdb_id, ":season_number": m.season_number,
            ":search_term": m.search_term, ":parse_term": m.parse_term, ":title": m.title,
            ":type": m.mapping_type, ":scene_season_number": m.scene_season_number,
            ":filter_regex": m.filter_regex, ":scene_origin": m.scene_origin,
            ":search_mode": m.search_mode, ":comment": m.comment,
        },
    )?;
    Ok(())
}

fn from_row(row: &Row) -> rusqlite::Result<SceneMapping> {
    Ok(SceneMapping {
        id: row.get("id")?,
        tvdb_id: row.get("tvdb_id")?,
        season_number: row.get("season_number")?,
        search_term: row.get("search_term")?,
        parse_term: row.get("parse_term")?,
        title: row.get("title")?,
        mapping_type: row.get("type")?,
        scene_season_number: row.get("scene_season_number")?,
        filter_regex: row.get("filter_regex")?,
        scene_origin: row.get("scene_origin")?,
        search_mode: row.get("search_mode")?,
        comment: row.get("comment")?,
    })
}

/// Mappings for the given series, grouped by TVDB id.
pub fn by_tvdb_ids(conn: &Connection, tvdb_ids: &[i64]) -> anyhow::Result<HashMap<i64, Vec<SceneMapping>>> {
    let mut out: HashMap<i64, Vec<SceneMapping>> = HashMap::new();
    if tvdb_ids.is_empty() {
        return Ok(out);
    }
    let json = serde_json::to_string(tvdb_ids)?;
    let mut stmt = conn.prepare(
        "SELECT * FROM scene_mappings WHERE tvdb_id IN (SELECT value FROM json_each(?1)) ORDER BY id",
    )?;
    let rows = stmt.query_map(params![json], from_row)?;
    for m in rows {
        let m = m?;
        out.entry(m.tvdb_id).or_default().push(m);
    }
    Ok(out)
}

/// `SceneMappingService.FindMappings` candidates: the mappings whose parse
/// term (the cleaned title) is the given one.
pub fn by_parse_term(conn: &Connection, parse_term: &str) -> anyhow::Result<Vec<SceneMapping>> {
    let mut stmt = conn.prepare("SELECT * FROM scene_mappings WHERE parse_term = ?1 ORDER BY id")?;
    let rows = stmt.query_map(params![parse_term], from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}
