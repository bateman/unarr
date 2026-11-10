//! Auto tagging (`NzbDrone.Core.AutoTagging`): the rules that add tags to,
//! and take them from, a series by what it is, applied whenever a series is
//! refreshed or edited.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::Connection;
use serde_json::Value;

use crate::model::{AutoTagging, Series, SeriesStatus, SeriesType};
use crate::store;

/// `AutoTaggingChanges`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TagChanges {
    pub add: BTreeSet<i64>,
    pub remove: BTreeSet<i64>,
}

/// `SeriesTypes`, as the specification stores it.
fn series_type_code(t: SeriesType) -> i64 {
    match t {
        SeriesType::Standard => 0,
        SeriesType::Daily => 1,
        SeriesType::Anime => 2,
    }
}

/// `SeriesStatusType`.
fn status_code(s: SeriesStatus) -> i64 {
    match s {
        SeriesStatus::Deleted => -1,
        SeriesStatus::Continuing => 0,
        SeriesStatus::Ended => 1,
        SeriesStatus::Upcoming => 2,
    }
}

/// A number field, which a hand-edited rule may hold as text.
fn number(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())
}

/// `IsSatisfiedByWithoutNegate` of each specification, by its type name.
fn satisfied(kind: &str, body: &Value, series: &Series, root_folder_path: &str) -> bool {
    let value = &body["value"];
    match kind {
        "GenreSpecification" => {
            let wanted: Vec<&str> = match value {
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                Value::String(s) => vec![s.as_str()],
                _ => Vec::new(),
            };
            series
                .genres
                .iter()
                .any(|g| wanted.iter().any(|w| w.to_lowercase() == g.to_lowercase()))
        }
        "MonitoredSpecification" => series.monitored,
        "OriginalLanguageSpecification" => number(value) == Some(series.original_language_id),
        "QualityProfileSpecification" => number(value) == Some(series.quality_profile_id),
        "RootFolderSpecification" => value
            .as_str()
            .is_some_and(|v| crate::mediafiles::path_equals(root_folder_path, v)),
        "SeriesTypeSpecification" => number(value) == Some(series_type_code(series.series_type)),
        "StatusSpecification" => number(&body["status"]) == Some(status_code(series.status)),
        "TagSpecification" => number(value).is_some_and(|t| series.tags.contains(&t)),
        "YearSpecification" => match (number(&body["min"]), number(&body["max"])) {
            (Some(min), Some(max)) => series.year >= min && series.year <= max,
            _ => false,
        },
        _ => false,
    }
}

/// `AutoTaggingService.GetTagChanges`: a rule matches when every group of
/// specifications of one type does (`SpecificationMatchesGroup.DidMatch`:
/// no required one failing, and not all of them failing); its tags are
/// then added, or removed when it does not match and says so.
pub fn tag_changes(rules: &[AutoTagging], series: &Series, root_folder_path: &str) -> TagChanges {
    let mut changes = TagChanges::default();
    for rule in rules {
        let mut groups: BTreeMap<&str, Vec<(bool, bool)>> = BTreeMap::new();
        for spec in rule
            .specifications
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let kind = spec["type"].as_str().unwrap_or("");
            let body = &spec["body"];
            let mut matched = satisfied(kind, body, series, root_folder_path);
            if body["negate"].as_bool().unwrap_or(false) {
                matched = !matched;
            }
            groups
                .entry(kind)
                .or_default()
                .push((body["required"].as_bool().unwrap_or(false), matched));
        }
        let all_match = groups.values().all(|group| {
            !(group.iter().any(|(required, matched)| *required && !*matched)
                || group.iter().all(|(_, matched)| !*matched))
        });
        if all_match {
            changes
                .add
                .extend(rule.tags.iter().filter(|t| !series.tags.contains(t)));
        } else if rule.remove_tags_automatically {
            changes.remove.extend(rule.tags.iter());
        }
    }
    changes
}

/// `SeriesService.UpdateTags`: the series' tags after the rules, removals
/// first; whether they changed.
pub fn apply(rules: &[AutoTagging], series: &mut Series, root_folder_path: &str) -> bool {
    if rules.is_empty() {
        return false;
    }
    let changes = tag_changes(rules, series, root_folder_path);
    let before = series.tags.clone();
    series.tags.retain(|t| !changes.remove.contains(t));
    for tag in changes.add {
        if !series.tags.contains(&tag) {
            series.tags.push(tag);
        }
    }
    let changed = series.tags != before;
    if changed {
        tracing::debug!(
            "Updated tags for '{}'. Added: {}, Removed: {}",
            series.title,
            series.tags.iter().filter(|t| !before.contains(t)).count(),
            before.iter().filter(|t| !series.tags.contains(t)).count()
        );
    } else {
        tracing::debug!("Tags not updated for '{}'", series.title);
    }
    changed
}

/// What the rules need from the database: the rules and the root folders.
pub struct Rules {
    rules: Vec<AutoTagging>,
    root_folders: Vec<String>,
}

impl Rules {
    pub fn load(conn: &Connection) -> anyhow::Result<Rules> {
        let rules = store::providers::list_auto_tagging(conn)?;
        let root_folders = if rules.is_empty() {
            Vec::new()
        } else {
            store::catalog::list_root_folders(conn)?
                .into_iter()
                .map(|r| r.path)
                .collect()
        };
        Ok(Rules { rules, root_folders })
    }

    /// Apply the rules to a series in memory; whether its tags changed.
    pub fn apply(&self, series: &mut Series) -> bool {
        if self.rules.is_empty() {
            return false;
        }
        let root = crate::mediafiles::best_root_folder_path(&series.path, &self.root_folders);
        apply(&self.rules, series, &root)
    }
}

/// The rules applied to a stored series, its tags written when they
/// changed (the refresh's `UpdateTags`). Returns the series when they did.
pub fn update_stored(conn: &Connection, series_id: i64) -> anyhow::Result<Option<Series>> {
    let rules = Rules::load(conn)?;
    if rules.rules.is_empty() {
        return Ok(None);
    }
    let Some(mut series) = store::series::get(conn, series_id)? else {
        return Ok(None);
    };
    if !rules.apply(&mut series) {
        return Ok(None);
    }
    store::series::set_tags(conn, series.id, &series.tags)?;
    Ok(Some(series))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn series() -> Series {
        let mut s = crate::decision::testing::series(1, "Frieren", 4);
        s.path = "/tv/Frieren".into();
        s.genres = vec!["Animation".into(), "Fantasy".into()];
        s.series_type = SeriesType::Anime;
        s.status = SeriesStatus::Continuing;
        s.year = 2023;
        s.quality_profile_id = 4;
        s.original_language_id = 8;
        s.monitored = true;
        s.tags = vec![7];
        s
    }

    fn rule(tags: &[i64], remove: bool, specs: Value) -> AutoTagging {
        AutoTagging {
            id: 1,
            name: "rule".into(),
            remove_tags_automatically: remove,
            tags: tags.to_vec(),
            specifications: specs,
        }
    }

    fn spec(kind: &str, body: Value) -> Value {
        json!({"type": kind, "body": body})
    }

    #[test]
    fn every_specification_reads_its_field() {
        let s = series();
        let cases = [
            ("GenreSpecification", json!({"value": ["fantasy"]}), true),
            ("GenreSpecification", json!({"value": ["Drama"]}), false),
            ("MonitoredSpecification", json!({}), true),
            ("OriginalLanguageSpecification", json!({"value": 8}), true),
            ("OriginalLanguageSpecification", json!({"value": 1}), false),
            ("QualityProfileSpecification", json!({"value": 4}), true),
            ("RootFolderSpecification", json!({"value": "/tv/"}), true),
            ("RootFolderSpecification", json!({"value": "/anime"}), false),
            ("SeriesTypeSpecification", json!({"value": 2}), true),
            ("SeriesTypeSpecification", json!({"value": 0}), false),
            ("StatusSpecification", json!({"status": 0}), true),
            ("StatusSpecification", json!({"status": 1}), false),
            ("TagSpecification", json!({"value": 7}), true),
            ("TagSpecification", json!({"value": 8}), false),
            ("YearSpecification", json!({"min": 2020, "max": 2023}), true),
            ("YearSpecification", json!({"min": 2024, "max": 2030}), false),
            ("NoSuchSpecification", json!({"value": 1}), false),
        ];
        for (kind, body, expected) in cases {
            assert_eq!(satisfied(kind, &body, &s, "/tv"), expected, "{kind} {body}");
        }
    }

    #[test]
    fn a_matching_rule_adds_the_tags_the_series_lacks() {
        let s = series();
        let rules = [rule(
            &[7, 9],
            false,
            json!([spec("SeriesTypeSpecification", json!({"value": 2}))]),
        )];
        let changes = tag_changes(&rules, &s, "/tv");
        assert_eq!(changes.add, BTreeSet::from([9]));
        assert!(changes.remove.is_empty());
    }

    #[test]
    fn a_failing_rule_removes_only_when_it_says_so() {
        let mut s = series();
        let specs = json!([spec("SeriesTypeSpecification", json!({"value": 0}))]);
        assert!(!apply(&[rule(&[7], false, specs.clone())], &mut s, "/tv"));
        assert_eq!(s.tags, vec![7]);
        assert!(apply(&[rule(&[7], true, specs)], &mut s, "/tv"));
        assert!(s.tags.is_empty());
    }

    #[test]
    fn negate_inverts_and_required_must_hold() {
        let s = series();
        // Two genre conditions: one of a type is enough...
        let either = json!([
            spec("GenreSpecification", json!({"value": ["Drama"]})),
            spec("GenreSpecification", json!({"value": ["Fantasy"]})),
        ]);
        assert_eq!(tag_changes(&[rule(&[9], false, either)], &s, "/tv").add.len(), 1);
        // ...unless the failing one is required.
        let required = json!([
            spec(
                "GenreSpecification",
                json!({"value": ["Drama"], "required": true})
            ),
            spec("GenreSpecification", json!({"value": ["Fantasy"]})),
        ]);
        assert!(tag_changes(&[rule(&[9], false, required)], &s, "/tv")
            .add
            .is_empty());
        // Every type present has to match.
        let both = json!([
            spec("GenreSpecification", json!({"value": ["Fantasy"]})),
            spec("YearSpecification", json!({"min": 1990, "max": 1999})),
        ]);
        assert!(tag_changes(&[rule(&[9], false, both)], &s, "/tv").add.is_empty());
        let negated = json!([spec(
            "YearSpecification",
            json!({"min": 1990, "max": 1999, "negate": true})
        )]);
        assert_eq!(tag_changes(&[rule(&[9], false, negated)], &s, "/tv").add.len(), 1);
    }

    #[test]
    fn a_rule_without_specifications_matches() {
        // `All` over no group is true, as in the originals.
        let s = series();
        assert_eq!(
            tag_changes(&[rule(&[9], false, json!([]))], &s, "/tv").add.len(),
            1
        );
    }

    #[test]
    fn removals_come_before_additions() {
        // One rule takes the tag away, another gives it back: it stays.
        let mut s = series();
        let rules = [
            rule(
                &[7],
                true,
                json!([spec("SeriesTypeSpecification", json!({"value": 0}))]),
            ),
            rule(&[7, 9], false, json!([spec("MonitoredSpecification", json!({}))])),
        ];
        assert!(apply(&rules, &mut s, "/tv"));
        // 7 was on the series, so the matching rule does not list it to add
        // (`GetTagChanges` skips tags the series has) and the removal wins.
        assert_eq!(s.tags, vec![9]);
    }

    #[test]
    fn a_stored_series_gets_its_tags_and_keeps_its_cast() {
        use crate::model::{ProfileItem, QualityProfile, RootFolder, Tag};
        use crate::store::catalog;
        let db = crate::db::Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            for (id, label) in [(1, "drama"), (2, "main")] {
                catalog::insert_tag(
                    conn,
                    &Tag {
                        id,
                        label: label.into(),
                    },
                )?;
            }
            catalog::insert_quality_profile(
                conn,
                &QualityProfile {
                    id: 1,
                    name: "Any".into(),
                    cutoff: 1,
                    upgrade_allowed: false,
                    items: vec![ProfileItem {
                        id: None,
                        name: None,
                        quality: Some(1),
                        items: vec![],
                        allowed: true,
                    }],
                    format_items: vec![],
                    min_format_score: 0,
                    cutoff_format_score: 0,
                    min_upgrade_format_score: 1,
                },
            )?;
            catalog::insert_root_folder(
                conn,
                &RootFolder {
                    id: 1,
                    path: "/data/tv".into(),
                },
            )?;
            let mut stored = crate::store::series::tests::sample(1);
            stored.actors = json!([{"name": "Nathan Fillion", "character": "Mal"}]);
            crate::store::series::insert(conn, &stored)?;

            // No rule: nothing to do.
            assert_eq!(update_stored(conn, 1)?, None);

            store::providers::insert_auto_tagging(
                conn,
                &rule(
                    &[2],
                    false,
                    json!([spec("RootFolderSpecification", json!({"value": "/data/tv"}))]),
                ),
            )?;
            let updated = update_stored(conn, 1)?.expect("the rule matches");
            assert_eq!(updated.tags, vec![1, 2]);
            let read = crate::store::series::get(conn, 1)?.unwrap();
            assert_eq!(read.tags, vec![1, 2]);
            assert_eq!(read.actors, stored.actors);
            // A second pass changes nothing.
            assert_eq!(update_stored(conn, 1)?, None);

            // An edit goes through the rules too: taking the tag away by
            // hand does not last while the rule matches.
            let edit = crate::tv::series::SeriesEdit {
                id: read.id,
                tvdb_id: read.tvdb_id,
                path: read.path.clone(),
                quality_profile_id: read.quality_profile_id,
                season_folder: read.season_folder,
                monitored: read.monitored,
                monitor_new_items: read.monitor_new_items,
                series_type: read.series_type,
                tags: vec![1],
                seasons: read.seasons.clone(),
            };
            let (_, after) = crate::tv::series::update(conn, &edit)?.unwrap();
            assert_eq!(after.tags, vec![1, 2]);
            Ok(())
        })
        .unwrap();
    }
}
