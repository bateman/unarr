//! Tags, root folders, quality definitions, quality profiles, custom formats.

use rusqlite::{params, Connection};

use super::{from_json, to_json};
use crate::model::{
    CustomFormat, ProfileFormatItem, ProfileItem, QualityDefinition, QualityProfile, RootFolder, Tag,
};

pub fn insert_tag(conn: &Connection, t: &Tag) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO tags (id, label) VALUES (?1, ?2)",
        params![t.id, t.label],
    )?;
    Ok(())
}

pub fn list_tags(conn: &Connection) -> anyhow::Result<Vec<Tag>> {
    let mut stmt = conn.prepare("SELECT id, label FROM tags ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok(Tag {
            id: r.get(0)?,
            label: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn insert_root_folder(conn: &Connection, r: &RootFolder) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO root_folders (id, path) VALUES (?1, ?2)",
        params![r.id, r.path],
    )?;
    Ok(())
}

pub fn list_root_folders(conn: &Connection) -> anyhow::Result<Vec<RootFolder>> {
    let mut stmt = conn.prepare("SELECT id, path FROM root_folders ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok(RootFolder {
            id: r.get(0)?,
            path: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// `QualityDefinitionService.Execute(ResetQualityDefinitionsCommand)`:
/// every definition's sizes back to the defaults, its title too when
/// asked.
pub fn reset_quality_definitions(conn: &Connection, reset_titles: bool) -> anyhow::Result<()> {
    for default in crate::quality::SONARR_QUALITIES {
        conn.execute(
            "UPDATE quality_definitions SET min_size = ?2, max_size = ?3, preferred_size = ?4,
                title = CASE WHEN ?5 THEN ?6 ELSE title END
             WHERE quality_id = ?1",
            params![
                default.quality.id,
                default.min_size,
                default.max_size,
                default.preferred_size,
                reset_titles,
                default.quality.name
            ],
        )?;
    }
    Ok(())
}

pub fn insert_quality_definition(conn: &Connection, d: &QualityDefinition) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO quality_definitions (id, quality_id, title, min_size, max_size, preferred_size)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            d.id,
            d.quality_id,
            d.title,
            d.min_size,
            d.max_size,
            d.preferred_size
        ],
    )?;
    Ok(())
}

pub fn list_quality_definitions(conn: &Connection) -> anyhow::Result<Vec<QualityDefinition>> {
    let mut stmt = conn.prepare(
        "SELECT id, quality_id, title, min_size, max_size, preferred_size FROM quality_definitions ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(QualityDefinition {
            id: r.get(0)?,
            quality_id: r.get(1)?,
            title: r.get(2)?,
            min_size: r.get(3)?,
            max_size: r.get(4)?,
            preferred_size: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn insert_custom_format(conn: &Connection, c: &CustomFormat) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO custom_formats (id, name, include_when_renaming, specifications) VALUES (?1, ?2, ?3, ?4)",
        params![c.id, c.name, c.include_when_renaming, to_json(&c.specifications)?],
    )?;
    Ok(())
}

pub fn list_custom_formats(conn: &Connection) -> anyhow::Result<Vec<CustomFormat>> {
    let mut stmt = conn
        .prepare("SELECT id, name, include_when_renaming, specifications FROM custom_formats ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(CustomFormat {
            id: r.get(0)?,
            name: r.get(1)?,
            include_when_renaming: r.get(2)?,
            specifications: from_json(r.get(3)?)?,
        });
    }
    Ok(out)
}

pub fn insert_quality_profile(conn: &Connection, p: &QualityProfile) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO quality_profiles (id, name, cutoff, upgrade_allowed, items, format_items, min_format_score,
            cutoff_format_score, min_upgrade_format_score)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            p.id,
            p.name,
            p.cutoff,
            p.upgrade_allowed,
            to_json(&p.items)?,
            to_json(&p.format_items)?,
            p.min_format_score,
            p.cutoff_format_score,
            p.min_upgrade_format_score
        ],
    )?;
    Ok(())
}

pub fn list_quality_profiles(conn: &Connection) -> anyhow::Result<Vec<QualityProfile>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, cutoff, upgrade_allowed, items, format_items, min_format_score, cutoff_format_score,
            min_upgrade_format_score FROM quality_profiles ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(QualityProfile {
            id: r.get(0)?,
            name: r.get(1)?,
            cutoff: r.get(2)?,
            upgrade_allowed: r.get(3)?,
            items: from_json::<Vec<ProfileItem>>(r.get(4)?)?,
            format_items: from_json::<Vec<ProfileFormatItem>>(r.get(5)?)?,
            min_format_score: r.get(6)?,
            cutoff_format_score: r.get(7)?,
            min_upgrade_format_score: r.get(8)?,
        });
    }
    Ok(out)
}

/// A row of one of the catalogue tables, gone; whether there was one.
/// Updates delete and insert again under the same id, in a transaction.
pub fn delete_row(conn: &Connection, table: &str, id: i64) -> anyhow::Result<bool> {
    const TABLES: &[&str] = &[
        "tags",
        "root_folders",
        "quality_definitions",
        "custom_formats",
        "quality_profiles",
        "delay_profiles",
        "release_profiles",
        "remote_path_mappings",
        "custom_filters",
        "import_list_exclusions",
        "auto_tagging",
    ];
    anyhow::ensure!(TABLES.contains(&table), "not a catalogue table: {table}");
    Ok(conn.execute(&format!("DELETE FROM {table} WHERE id = ?1"), params![id])? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn round_trips_the_catalogue() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            insert_tag(conn, &Tag { id: 1, label: "anime".into() })?;
            insert_root_folder(conn, &RootFolder { id: 1, path: "/data/tv".into() })?;
            let def = QualityDefinition {
                id: 1,
                quality_id: 0,
                title: "Unknown".into(),
                min_size: Some(1.0),
                max_size: Some(199.9),
                preferred_size: Some(95.0),
            };
            insert_quality_definition(conn, &def)?;
            let cf = CustomFormat {
                id: 1,
                name: "x265".into(),
                include_when_renaming: false,
                specifications: serde_json::json!([{"type": "ReleaseTitleSpecification", "body": {"value": "x265"}}]),
            };
            insert_custom_format(conn, &cf)?;
            let profile = QualityProfile {
                id: 7,
                name: "Fixture".into(),
                cutoff: 1002,
                upgrade_allowed: true,
                items: vec![
                    ProfileItem { id: None, name: None, quality: Some(0), items: vec![], allowed: false },
                    ProfileItem {
                        id: Some(1000),
                        name: Some("WEB 480p".into()),
                        quality: None,
                        items: vec![ProfileItem { id: None, name: None, quality: Some(12), items: vec![], allowed: true }],
                        allowed: true,
                    },
                ],
                format_items: vec![ProfileFormatItem { format: 1, score: 10 }],
                min_format_score: 0,
                cutoff_format_score: 10,
                min_upgrade_format_score: 1,
            };
            insert_quality_profile(conn, &profile)?;
            assert_eq!(list_tags(conn)?.len(), 1);
            assert_eq!(list_root_folders(conn)?[0].path, "/data/tv");
            assert_eq!(list_quality_definitions(conn)?, vec![def]);
            assert_eq!(list_custom_formats(conn)?, vec![cf]);
            assert_eq!(list_quality_profiles(conn)?, vec![profile]);
            Ok(())
        })
        .unwrap();
    }
}
