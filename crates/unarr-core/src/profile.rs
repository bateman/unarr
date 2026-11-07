//! Quality profile arithmetic: where a quality sits in a profile and whether
//! a file meets the profile's cutoff. Mirrors the originals' `QualityIndex`
//! and `UpgradableSpecification.QualityCutoffNotMet`.

use crate::model::{ProfileItem, QualityProfile};

/// Position of a quality (or group) in the profile's ordered items. Unknown
/// ids sort first, as in the originals.
pub fn index_of(profile: &QualityProfile, quality_or_group_id: i64) -> usize {
    for (i, item) in profile.items.iter().enumerate() {
        if item.quality == Some(quality_or_group_id) {
            return i;
        }
        if item.id.is_some_and(|id| id > 0 && id == quality_or_group_id) {
            return i;
        }
        if item.items.iter().any(|g| g.quality == Some(quality_or_group_id)) {
            return i;
        }
    }
    0
}

/// Every quality id an item stands for (itself, or its group members).
pub fn qualities_of(item: &ProfileItem) -> Vec<i64> {
    match item.quality {
        Some(q) => vec![q],
        None => item.items.iter().filter_map(|g| g.quality).collect(),
    }
}

/// The first allowed quality, used as cutoff when upgrades are disabled.
pub fn first_allowed_quality(profile: &QualityProfile) -> Option<i64> {
    profile
        .items
        .iter()
        .find(|i| i.allowed)
        .and_then(|i| qualities_of(i).first().copied())
}

/// The id (quality or group) the cutoff resolves to for this profile.
pub fn effective_cutoff(profile: &QualityProfile) -> i64 {
    if profile.upgrade_allowed {
        profile.cutoff
    } else {
        first_allowed_quality(profile).unwrap_or(profile.cutoff)
    }
}

/// Whether a file of `quality_id` is still below the profile's cutoff.
pub fn quality_cutoff_not_met(profile: &QualityProfile, quality_id: i64) -> bool {
    index_of(profile, quality_id) < index_of(profile, effective_cutoff(profile))
}

/// Quality ids strictly below the cutoff: what `wanted/cutoff` looks for.
pub fn qualities_below_cutoff(profile: &QualityProfile) -> Vec<i64> {
    let cutoff_index = index_of(profile, effective_cutoff(profile));
    profile
        .items
        .iter()
        .take(cutoff_index)
        .flat_map(qualities_of)
        .collect()
}

/// Sum of the profile's scores for the given custom format ids.
pub fn custom_format_score(profile: &QualityProfile, format_ids: &[i64]) -> i64 {
    profile
        .format_items
        .iter()
        .filter(|f| format_ids.contains(&f.format))
        .map(|f| f.score)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single(q: i64, allowed: bool) -> ProfileItem {
        ProfileItem {
            id: None,
            name: None,
            quality: Some(q),
            items: vec![],
            allowed,
        }
    }

    fn profile(upgrade_allowed: bool, cutoff: i64) -> QualityProfile {
        QualityProfile {
            id: 1,
            name: "p".into(),
            cutoff,
            upgrade_allowed,
            items: vec![
                single(0, false),
                single(1, true),
                ProfileItem {
                    id: Some(1000),
                    name: Some("WEB 480p".into()),
                    quality: None,
                    items: vec![single(12, true), single(8, true)],
                    allowed: true,
                },
                single(7, true),
            ],
            format_items: vec![],
            min_format_score: 0,
            cutoff_format_score: 0,
            min_upgrade_format_score: 1,
        }
    }

    #[test]
    fn cutoff_logic_matches_the_originals() {
        let p = profile(true, 1000);
        assert_eq!(index_of(&p, 8), 2);
        assert_eq!(index_of(&p, 1000), 2);
        assert_eq!(index_of(&p, 99), 0);
        assert!(quality_cutoff_not_met(&p, 1));
        assert!(!quality_cutoff_not_met(&p, 12));
        assert!(!quality_cutoff_not_met(&p, 7));
        assert_eq!(qualities_below_cutoff(&p), vec![0, 1]);

        // Upgrades off: the first allowed quality is the cutoff, nothing is below it
        // except what sorts before it.
        let p = profile(false, 7);
        assert_eq!(effective_cutoff(&p), 1);
        assert!(!quality_cutoff_not_met(&p, 1));
        assert_eq!(qualities_below_cutoff(&p), vec![0]);
    }
}
