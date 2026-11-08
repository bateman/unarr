//! `SceneMappingService`: which alternate title a parsed series title is,
//! and what it maps to.

use crate::decision::lookup::Lookup;
use crate::model::SceneMapping;
use crate::parser::clean_series_title;

/// `InvalidSceneMappingException`: the title maps to several series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousMapping {
    pub release_title: String,
    pub tvdb_ids: Vec<i64>,
}

impl std::fmt::Display for AmbiguousMapping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Scene mappings for {} are ambiguous (TVDB ids {:?})",
            self.release_title, self.tvdb_ids
        )
    }
}

/// `FindSceneMapping(seriesTitle, releaseTitle, sceneSeasonNumber)`.
pub fn find_scene_mapping(
    lookup: &dyn Lookup,
    series_title: &str,
    release_title: &str,
    scene_season_number: i64,
) -> Result<Option<SceneMapping>, AmbiguousMapping> {
    if series_title.trim().is_empty() {
        return Ok(None);
    }
    let Some(mappings) = find_mappings(lookup, series_title, release_title) else {
        return Ok(None);
    };
    let mappings = filter_by_season(mappings, scene_season_number);
    let mut distinct: Vec<&SceneMapping> = Vec::new();
    for m in &mappings {
        if !distinct.iter().any(|d| d.tvdb_id == m.tvdb_id) {
            distinct.push(m);
        }
    }
    match distinct.len() {
        0 => Ok(None),
        1 => Ok(Some(distinct[0].clone())),
        _ => Err(AmbiguousMapping {
            release_title: release_title.to_string(),
            tvdb_ids: distinct.iter().map(|m| m.tvdb_id).collect(),
        }),
    }
}

/// `FindTvdbId`.
pub fn find_tvdb_id(
    lookup: &dyn Lookup,
    series_title: &str,
    release_title: &str,
    scene_season_number: i64,
) -> Result<Option<i64>, AmbiguousMapping> {
    Ok(find_scene_mapping(lookup, series_title, release_title, scene_season_number)?.map(|m| m.tvdb_id))
}

/// `GetSceneSeasonNumber`.
pub fn scene_season_number(
    lookup: &dyn Lookup,
    series_title: &str,
    release_title: &str,
) -> Result<Option<i64>, AmbiguousMapping> {
    Ok(find_scene_mapping(lookup, series_title, release_title, -1)?.and_then(|m| m.scene_season_number))
}

/// `FindMappings`: the candidates for a title, narrowed to the exact title or
/// the closest one when several alternate titles clean to the same term.
fn find_mappings(lookup: &dyn Lookup, series_title: &str, release_title: &str) -> Option<Vec<SceneMapping>> {
    let candidates = lookup.scene_mappings_by_parse_term(&clean_series_title(series_title));
    if candidates.is_empty() {
        return None;
    }
    let candidates = filter_by_regex(candidates, release_title);
    if candidates.len() <= 1 {
        return Some(candidates);
    }
    let mut exact: Vec<SceneMapping> = candidates
        .iter()
        .filter(|m| m.title == series_title)
        .cloned()
        .collect();
    if !exact.is_empty() {
        exact.sort_by_key(|m| std::cmp::Reverse(m.season_number.unwrap_or(-1)));
        return Some(exact);
    }
    let closest = candidates
        .iter()
        .min_by(|a, b| {
            levenshtein(series_title, &a.title, 10, 1, 10)
                .cmp(&levenshtein(series_title, &b.title, 10, 1, 10))
                .then_with(|| b.season_number.unwrap_or(-1).cmp(&a.season_number.unwrap_or(-1)))
        })
        .cloned()?;
    Some(
        candidates
            .into_iter()
            .filter(|m| m.title == closest.title)
            .collect(),
    )
}

/// Mappings with a `filterRegex` apply only when the release title matches
/// it; without a match the plain ones apply.
fn filter_by_regex(candidates: Vec<SceneMapping>, release_title: &str) -> Vec<SceneMapping> {
    let (filtered, normal): (Vec<SceneMapping>, Vec<SceneMapping>) = candidates
        .into_iter()
        .partition(|m| m.filter_regex.as_deref().is_some_and(|r| !r.trim().is_empty()));
    if release_title.trim().is_empty() {
        return normal;
    }
    // The originals match the simplified title; the release title is the
    // closest the parser exposes.
    let matching: Vec<SceneMapping> = filtered
        .into_iter()
        .filter(|m| {
            fancy_regex::Regex::new(m.filter_regex.as_deref().unwrap_or(""))
                .ok()
                .and_then(|re| re.is_match(release_title).ok())
                .unwrap_or(false)
        })
        .collect();
    if !matching.is_empty() {
        return matching;
    }
    normal
}

/// Mappings bound to a scene season apply to that season and later ones; for
/// a season the newest applicable one of each title wins.
fn filter_by_season(candidates: Vec<SceneMapping>, scene_season_number: i64) -> Vec<SceneMapping> {
    let (filtered, normal): (Vec<SceneMapping>, Vec<SceneMapping>) = candidates
        .into_iter()
        .partition(|m| m.scene_season_number.unwrap_or(-1) != -1 && m.season_number.unwrap_or(-1) != -1);
    if scene_season_number == -1 {
        return normal;
    }
    if filtered.is_empty() {
        return normal;
    }
    let mut applicable: Vec<SceneMapping> = filtered
        .into_iter()
        .filter(|m| m.scene_season_number.unwrap_or(-1) <= scene_season_number)
        .collect();
    applicable.sort_by(|a, b| {
        b.scene_season_number
            .cmp(&a.scene_season_number)
            .then_with(|| b.season_number.cmp(&a.season_number))
    });
    let mut out: Vec<SceneMapping> = Vec::new();
    for m in applicable {
        if !out.iter().any(|o| o.title == m.title) {
            out.push(m);
        }
    }
    out
}

/// Weighted Levenshtein distance, as the originals' extension computes it.
pub fn levenshtein(
    text: &str,
    other: &str,
    cost_insert: usize,
    cost_delete: usize,
    cost_substitute: usize,
) -> usize {
    let a: Vec<char> = text.chars().collect();
    let b: Vec<char> = other.chars().collect();
    if a.is_empty() {
        return b.len() * cost_insert;
    }
    if b.is_empty() {
        return a.len() * cost_delete;
    }
    let mut prev: Vec<usize> = (0..=b.len()).map(|j| j * cost_insert).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i * cost_delete;
        for j in 1..=b.len() {
            let substitute = prev[j - 1] + if a[i - 1] == b[j - 1] { 0 } else { cost_substitute };
            cur[j] = substitute
                .min(prev[j] + cost_delete)
                .min(cur[j - 1] + cost_insert);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_weigh_insertions_and_substitutions() {
        assert_eq!(levenshtein("abc", "abc", 10, 1, 10), 0);
        assert_eq!(levenshtein("abc", "abcd", 10, 1, 10), 10);
        assert_eq!(levenshtein("abcd", "abc", 10, 1, 10), 1);
        assert_eq!(levenshtein("abc", "abd", 10, 1, 10), 10);
    }

    fn mapping(tvdb_id: i64, title: &str, season: Option<i64>, scene_season: Option<i64>) -> SceneMapping {
        SceneMapping {
            id: tvdb_id,
            tvdb_id,
            season_number: season,
            search_term: Some(title.into()),
            parse_term: Some(clean_series_title(title)),
            title: title.into(),
            mapping_type: Some("XemService".into()),
            scene_season_number: scene_season,
            filter_regex: None,
            scene_origin: None,
            search_mode: None,
            comment: None,
        }
    }

    #[test]
    fn season_bound_mappings_pick_the_newest_applicable() {
        let m = filter_by_season(
            vec![
                mapping(1, "Show", Some(2), Some(1)),
                mapping(1, "Show", Some(4), Some(3)),
                mapping(2, "Show", None, None),
            ],
            3,
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].season_number, Some(4));
        let plain = filter_by_season(
            vec![
                mapping(1, "Show", Some(2), Some(1)),
                mapping(2, "Show", None, None),
            ],
            -1,
        );
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0].tvdb_id, 2);
    }
}
