//! What the originals put in a database at start when it is not there: the
//! quality definitions, the six quality profiles of a new installation and
//! the delay profile every installation has
//! (`QualityDefinitionService.InsertMissingDefinitions`,
//! `QualityProfileService.Handle(ApplicationStartedEvent)`, migration 70).
//! An imported database already has them all; one started from nothing
//! could otherwise add no series, for want of a profile to give it.

use rusqlite::{params, Connection};

use crate::model::{ProfileFormatItem, ProfileItem, QualityDefinition, QualityProfile};
use crate::quality::SONARR_QUALITIES;
use crate::store;

/// The group a quality shares its weight with, as the default profiles
/// name it.
fn group_name(weight: i64) -> &'static str {
    match weight {
        3 => "WEB 480p",
        10 => "WEB 720p",
        12 => "WEB 1080p",
        16 => "WEB 2160p",
        _ => "",
    }
}

/// `InsertMissingDefinitions`: a definition for every quality the
/// application knows, none for one it does not. Returns how many were
/// added.
pub fn insert_missing_definitions(conn: &Connection) -> anyhow::Result<usize> {
    let existing = store::catalog::list_quality_definitions(conn)?;
    let mut added = 0;
    let mut by_weight: Vec<&crate::quality::QualityDefault> = SONARR_QUALITIES.iter().collect();
    by_weight.sort_by_key(|d| d.weight);
    for default in by_weight {
        if existing.iter().any(|d| d.quality_id == default.quality.id) {
            continue;
        }
        let id = store::sequence::next_id(conn, "quality_definitions")?;
        store::catalog::insert_quality_definition(
            conn,
            &QualityDefinition {
                id,
                quality_id: default.quality.id,
                title: default.quality.name.to_string(),
                min_size: Some(default.min_size),
                max_size: default.max_size,
                preferred_size: Some(default.preferred_size),
            },
        )?;
        added += 1;
    }
    for unknown in existing
        .iter()
        .filter(|d| !SONARR_QUALITIES.iter().any(|q| q.quality.id == d.quality_id))
    {
        conn.execute(
            "DELETE FROM quality_definitions WHERE id = ?1",
            params![unknown.id],
        )?;
    }
    Ok(added)
}

/// `GetDefaultProfile`: every quality in definition order, those that
/// share a weight as a group, the allowed ones switched on.
pub fn default_profile(name: &str, cutoff: i64, allowed: &[i64], formats: &[i64]) -> QualityProfile {
    let mut weights: Vec<i64> = Vec::new();
    for d in SONARR_QUALITIES {
        if !weights.contains(&d.weight) {
            weights.push(d.weight);
        }
    }
    let mut items = Vec::new();
    let mut group_id = 1000;
    let mut profile_cutoff = cutoff;
    for weight in weights {
        let group: Vec<i64> = SONARR_QUALITIES
            .iter()
            .filter(|d| d.weight == weight)
            .map(|d| d.quality.id)
            .collect();
        if let [quality] = group[..] {
            items.push(ProfileItem {
                id: None,
                name: None,
                quality: Some(quality),
                items: Vec::new(),
                allowed: allowed.contains(&quality),
            });
            continue;
        }
        let group_allowed = group.iter().any(|q| allowed.contains(q));
        items.push(ProfileItem {
            id: Some(group_id),
            name: Some(group_name(weight).to_string()),
            quality: None,
            items: group
                .iter()
                .map(|q| ProfileItem {
                    id: None,
                    name: None,
                    quality: Some(*q),
                    items: Vec::new(),
                    allowed: group_allowed,
                })
                .collect(),
            allowed: group_allowed,
        });
        if group.contains(&cutoff) {
            profile_cutoff = group_id;
        }
        group_id += 1;
    }
    QualityProfile {
        id: 0,
        name: name.to_string(),
        cutoff: profile_cutoff,
        upgrade_allowed: false,
        items,
        format_items: formats
            .iter()
            .map(|format| ProfileFormatItem {
                format: *format,
                score: 0,
            })
            .collect(),
        min_format_score: 0,
        cutoff_format_score: 0,
        min_upgrade_format_score: 1,
    }
}

/// The profiles of a new installation, when there is none: Any, SD,
/// HD-720p, HD-1080p, Ultra-HD, HD - 720p/1080p. Returns how many were
/// added.
pub fn insert_default_profiles(conn: &Connection) -> anyhow::Result<usize> {
    if !store::catalog::list_quality_profiles(conn)?.is_empty() {
        return Ok(0);
    }
    tracing::info!("Setting up default quality profiles");
    // Quality ids: SDTV 1, DVD 2, WEBDL-1080p 3, HDTV-720p 4, WEBDL-720p 5,
    // Bluray-720p 6, Bluray-1080p 7, WEBDL-480p 8, HDTV-1080p 9,
    // WEBRip-480p 12, Bluray-480p 13, WEBRip-720p 14, WEBRip-1080p 15,
    // HDTV-2160p 16, WEBRip-2160p 17, WEBDL-2160p 18, Bluray-2160p 19,
    // Bluray-576p 22.
    let defaults: [(&str, i64, &[i64]); 6] = [
        ("Any", 1, &[1, 12, 8, 2, 13, 22, 4, 9, 14, 5, 15, 3, 6, 7]),
        ("SD", 1, &[1, 12, 8, 2, 13, 22]),
        ("HD-720p", 4, &[4, 14, 5, 6]),
        ("HD-1080p", 9, &[9, 15, 3, 7]),
        ("Ultra-HD", 16, &[16, 17, 18, 19]),
        ("HD - 720p/1080p", 4, &[4, 9, 14, 5, 15, 3, 6, 7]),
    ];
    let formats: Vec<i64> = store::catalog::list_custom_formats(conn)?
        .iter()
        .map(|f| f.id)
        .collect();
    for (name, cutoff, allowed) in defaults {
        let mut profile = default_profile(name, cutoff, allowed, &formats);
        profile.id = store::sequence::next_id(conn, "quality_profiles")?;
        store::catalog::insert_quality_profile(conn, &profile)?;
    }
    Ok(defaults.len())
}

/// The delay profile that applies to every series, when there is none.
pub fn insert_default_delay_profile(conn: &Connection) -> anyhow::Result<bool> {
    let any: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM delay_profiles)", [], |r| r.get(0))?;
    if any {
        return Ok(false);
    }
    let id = store::sequence::next_id(conn, "delay_profiles")?;
    conn.execute(
        "INSERT INTO delay_profiles (id, enable_usenet, enable_torrent, preferred_protocol, usenet_delay,
            torrent_delay, sort_order, tags)
         VALUES (?1, 1, 1, 'usenet', 0, 0, ?2, '[]')",
        params![id, i32::MAX],
    )?;
    Ok(true)
}

/// Everything above, at start.
pub fn ensure(conn: &Connection) -> anyhow::Result<()> {
    tracing::debug!("Setting up default quality config");
    // All of it or none: a start cut short must not leave three profiles
    // of six, which the next start would take for somebody's choice.
    let tx = conn.unchecked_transaction()?;
    insert_missing_definitions(&tx)?;
    insert_default_profiles(&tx)?;
    insert_default_delay_profile(&tx)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn a_database_started_from_nothing_gets_the_defaults_once() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            ensure(conn)?;
            let definitions = store::catalog::list_quality_definitions(conn)?;
            assert_eq!(definitions.len(), SONARR_QUALITIES.len());
            // In weight order: Unknown first, the remux of 2160p last.
            assert_eq!(definitions[0].title, "Unknown");
            assert_eq!(definitions.last().unwrap().title, "Bluray-2160p Remux");
            let profiles = store::catalog::list_quality_profiles(conn)?;
            let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
            assert_eq!(
                names,
                ["Any", "SD", "HD-720p", "HD-1080p", "Ultra-HD", "HD - 720p/1080p"]
            );
            assert_eq!(
                conn.query_row("SELECT count(*), max(sort_order) FROM delay_profiles", [], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })?,
                (1, i32::MAX as i64)
            );

            // A second start adds nothing; a definition of a quality nobody
            // knows goes, a missing one comes back.
            conn.execute("DELETE FROM quality_definitions WHERE quality_id = 7", [])?;
            conn.execute(
                "INSERT INTO quality_definitions (id, quality_id, title) VALUES (900, 999, 'Gone')",
                [],
            )?;
            ensure(conn)?;
            let definitions = store::catalog::list_quality_definitions(conn)?;
            assert_eq!(definitions.len(), SONARR_QUALITIES.len());
            assert!(definitions.iter().any(|d| d.quality_id == 7));
            assert!(definitions.iter().all(|d| d.quality_id != 999));
            assert_eq!(store::catalog::list_quality_profiles(conn)?.len(), 6);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn default_profiles_group_what_shares_a_weight() {
        let hd = default_profile("HD-1080p", 9, &[9, 15, 3, 7], &[4, 5]);
        // HDTV-1080p is the cutoff, on its own.
        assert_eq!(hd.cutoff, 9);
        let web = hd
            .items
            .iter()
            .find(|i| i.name.as_deref() == Some("WEB 1080p"))
            .expect("the WEB 1080p group");
        assert!(web.allowed && web.items.iter().all(|i| i.allowed));
        assert_eq!(
            web.items.iter().filter_map(|i| i.quality).collect::<Vec<_>>(),
            [15, 3]
        );
        assert_eq!(hd.items.iter().filter(|i| i.allowed).count(), 3);
        assert_eq!(hd.format_items.len(), 2);
        assert!(hd.format_items.iter().all(|f| f.score == 0));
        // A cutoff inside a group is the group.
        let web_only = default_profile("x", 3, &[15, 3], &[]);
        assert_eq!(Some(web_only.cutoff), web.id);
        assert_eq!(
            (
                hd.min_format_score,
                hd.cutoff_format_score,
                hd.min_upgrade_format_score
            ),
            (0, 0, 1)
        );
    }

    /// The fixture's "HD-720p" is the original's default, never edited:
    /// the same items in the same order, the same groups, the same cutoff.
    #[test]
    fn the_default_profile_is_the_one_the_original_made() {
        let golden: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/sonarr/golden/qualityprofile.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let original = golden["body"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "HD-720p")
            .expect("the fixture has the profile");
        let ours = default_profile("HD-720p", 4, &[4, 14, 5, 6], &[]);
        assert_eq!(original["cutoff"], ours.cutoff);
        assert_eq!(original["upgradeAllowed"], ours.upgrade_allowed);
        let shape = |item: &serde_json::Value| -> (Option<i64>, Option<String>, Option<i64>, bool) {
            (
                item["id"].as_i64(),
                item["name"].as_str().map(str::to_string),
                item["quality"]["id"].as_i64(),
                item["allowed"].as_bool().unwrap(),
            )
        };
        let own = |item: &ProfileItem| (item.id, item.name.clone(), item.quality, item.allowed);
        let theirs = original["items"].as_array().unwrap();
        assert_eq!(theirs.len(), ours.items.len());
        for (theirs, ours) in theirs.iter().zip(&ours.items) {
            assert_eq!(shape(theirs), own(ours));
            let inner: Vec<_> = theirs["items"].as_array().unwrap().iter().map(shape).collect();
            assert_eq!(inner, ours.items.iter().map(own).collect::<Vec<_>>());
        }
    }
}
