//! Naming configuration, key/value settings and import metadata.

use rusqlite::{params, Connection, OptionalExtension};

use crate::model::NamingConfig;

pub fn set_naming(conn: &Connection, n: &NamingConfig) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO naming_config (id, rename_episodes, replace_illegal_characters,
            colon_replacement_format, custom_colon_replacement_format, multi_episode_style,
            standard_episode_format, daily_episode_format, anime_episode_format, series_folder_format,
            season_folder_format, specials_folder_format)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            n.rename_episodes,
            n.replace_illegal_characters,
            n.colon_replacement_format,
            n.custom_colon_replacement_format,
            n.multi_episode_style,
            n.standard_episode_format,
            n.daily_episode_format,
            n.anime_episode_format,
            n.series_folder_format,
            n.season_folder_format,
            n.specials_folder_format
        ],
    )?;
    Ok(())
}

/// The naming configuration, or Sonarr's defaults when none was stored.
pub fn naming(conn: &Connection) -> anyhow::Result<NamingConfig> {
    let found = conn
        .query_row("SELECT * FROM naming_config WHERE id = 1", [], |r| {
            Ok(NamingConfig {
                rename_episodes: r.get("rename_episodes")?,
                replace_illegal_characters: r.get("replace_illegal_characters")?,
                colon_replacement_format: r.get("colon_replacement_format")?,
                custom_colon_replacement_format: r.get("custom_colon_replacement_format")?,
                multi_episode_style: r.get("multi_episode_style")?,
                standard_episode_format: r.get("standard_episode_format")?,
                daily_episode_format: r.get("daily_episode_format")?,
                anime_episode_format: r.get("anime_episode_format")?,
                series_folder_format: r.get("series_folder_format")?,
                season_folder_format: r.get("season_folder_format")?,
                specials_folder_format: r.get("specials_folder_format")?,
            })
        })
        .optional()?;
    Ok(found.unwrap_or_default())
}

/// Store a setting. Keys are lower-cased, as the originals do.
pub fn set_config(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)",
        params![key.to_ascii_lowercase(), value],
    )?;
    Ok(())
}

pub fn config(conn: &Connection, key: &str) -> anyhow::Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM config WHERE key = ?1",
            params![key.to_ascii_lowercase()],
            |r| r.get(0),
        )
        .optional()?)
}

/// Every setting as (key, value), for building the config resources.
pub fn all_config(conn: &Connection) -> anyhow::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT key, value FROM config ORDER BY key")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(())
}

pub fn meta(conn: &Connection, key: &str) -> anyhow::Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
            r.get(0)
        })
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn naming_defaults_then_overrides() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            assert_eq!(naming(conn)?, NamingConfig::default());
            let custom = NamingConfig {
                rename_episodes: true,
                ..NamingConfig::default()
            };
            set_naming(conn, &custom)?;
            set_naming(conn, &custom)?;
            assert_eq!(naming(conn)?, custom);
            set_config(conn, "EnableCompletedDownloadHandling", "True")?;
            assert_eq!(
                config(conn, "enablecompleteddownloadhandling")?.as_deref(),
                Some("True")
            );
            assert_eq!(config(conn, "missing")?, None);
            assert_eq!(all_config(conn)?.len(), 1);
            set_meta(conn, "source", "sonarr")?;
            assert_eq!(meta(conn, "source")?.as_deref(), Some("sonarr"));
            Ok(())
        })
        .unwrap();
    }
}
