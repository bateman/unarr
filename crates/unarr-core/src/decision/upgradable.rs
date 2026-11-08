//! `UpgradableSpecification`: whether a new release improves on what is
//! there (on disk, in history, in the queue), under a quality profile.

use crate::decision::lookup::ProperDownloadTypes;
use crate::model::{CustomFormat, QualityModel, QualityProfile};
use crate::profile;

/// `UpgradeableRejectReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeRejectReason {
    None,
    BetterQuality,
    BetterRevision,
    QualityCutoff,
    CustomFormatScore,
    CustomFormatCutoff,
    MinCustomFormatScore,
    UpgradesNotAllowed,
}

/// `Revision.CompareTo`: real first, then version.
pub fn compare_revision(a: &QualityModel, b: &QualityModel) -> std::cmp::Ordering {
    a.real.cmp(&b.real).then(a.version.cmp(&b.version))
}

/// `QualityModelComparer.Compare(Quality, Quality)`: by position in the profile.
pub fn compare_quality(profile: &QualityProfile, left: i64, right: i64) -> std::cmp::Ordering {
    profile::index_of(profile, left).cmp(&profile::index_of(profile, right))
}

/// `QualityModelComparer.Compare(QualityModel, QualityModel)`.
pub fn compare_model(
    profile: &QualityProfile,
    left: &QualityModel,
    right: &QualityModel,
) -> std::cmp::Ordering {
    compare_quality(profile, left.quality_id, right.quality_id).then_with(|| compare_revision(left, right))
}

fn score(profile: &QualityProfile, formats: &[CustomFormat]) -> i64 {
    let ids: Vec<i64> = formats.iter().map(|f| f.id).collect();
    profile::custom_format_score(profile, &ids)
}

/// `IsUpgradable`.
pub fn is_upgradable(
    propers: ProperDownloadTypes,
    profile: &QualityProfile,
    current: &QualityModel,
    current_formats: &[CustomFormat],
    new: &QualityModel,
    new_formats: &[CustomFormat],
) -> UpgradeRejectReason {
    use std::cmp::Ordering;
    let quality_compare = compare_quality(profile, new.quality_id, current.quality_id);
    if quality_compare == Ordering::Greater && quality_cutoff_not_met(profile, current, Some(new)) {
        return UpgradeRejectReason::None;
    }
    if quality_compare == Ordering::Less {
        return UpgradeRejectReason::BetterQuality;
    }
    let revision_compare = compare_revision(new, current);
    if propers != ProperDownloadTypes::DoNotPrefer && revision_compare == Ordering::Greater {
        return UpgradeRejectReason::None;
    }
    if !profile.upgrade_allowed {
        return UpgradeRejectReason::UpgradesNotAllowed;
    }
    if propers != ProperDownloadTypes::DoNotPrefer && revision_compare == Ordering::Less {
        return UpgradeRejectReason::BetterRevision;
    }
    if quality_compare == Ordering::Greater {
        return UpgradeRejectReason::QualityCutoff;
    }
    let current_score = score(profile, current_formats);
    let new_score = score(profile, new_formats);
    if new_score <= current_score {
        return UpgradeRejectReason::CustomFormatScore;
    }
    if current_score >= profile.cutoff_format_score {
        return UpgradeRejectReason::CustomFormatCutoff;
    }
    if new_score < current_score + profile.min_upgrade_format_score {
        return UpgradeRejectReason::MinCustomFormatScore;
    }
    UpgradeRejectReason::None
}

/// `QualityCutoffNotMet`.
pub fn quality_cutoff_not_met(
    profile: &QualityProfile,
    current: &QualityModel,
    new: Option<&QualityModel>,
) -> bool {
    let cutoff = profile::effective_cutoff(profile);
    if compare_quality(profile, current.quality_id, cutoff) == std::cmp::Ordering::Less {
        return true;
    }
    new.is_some_and(|n| is_revision_upgrade(current, n))
}

fn custom_format_cutoff_not_met(profile: &QualityProfile, current_formats: &[CustomFormat]) -> bool {
    let cutoff = if profile.upgrade_allowed {
        profile.cutoff_format_score
    } else {
        profile.min_format_score
    };
    score(profile, current_formats) < cutoff
}

/// `CutoffNotMet`.
pub fn cutoff_not_met(
    profile: &QualityProfile,
    current: &QualityModel,
    current_formats: &[CustomFormat],
    new: Option<&QualityModel>,
) -> bool {
    quality_cutoff_not_met(profile, current, new) || custom_format_cutoff_not_met(profile, current_formats)
}

/// `IsRevisionUpgrade`: same quality, better revision.
pub fn is_revision_upgrade(current: &QualityModel, new: &QualityModel) -> bool {
    current.quality_id == new.quality_id && compare_revision(new, current) == std::cmp::Ordering::Greater
}

/// `IsUpgradeAllowed`.
pub fn is_upgrade_allowed(
    profile: &QualityProfile,
    current: &QualityModel,
    current_formats: &[CustomFormat],
    new: &QualityModel,
    new_formats: &[CustomFormat],
) -> bool {
    let quality_upgrade = compare_model(profile, new, current) == std::cmp::Ordering::Greater;
    let format_upgrade = score(profile, new_formats) > score(profile, current_formats);
    if is_revision_upgrade(current, new) {
        return true;
    }
    if (quality_upgrade || format_upgrade) && profile.upgrade_allowed {
        return true;
    }
    if (quality_upgrade || format_upgrade) && !profile.upgrade_allowed {
        return false;
    }
    true
}

/// The cutoff item the messages name: the profile's cutoff when upgrades are
/// allowed, else the first allowed quality.
pub fn cutoff_item_string(profile: &QualityProfile) -> String {
    let cutoff = profile::effective_cutoff(profile);
    let index = profile::index_of(profile, cutoff);
    profile
        .items
        .get(index)
        .map(crate::decision::profile_item_string)
        .unwrap_or_default()
}

/// The profile's cutoff item as named, whatever `upgradeAllowed` says.
pub fn profile_cutoff_string(profile: &QualityProfile) -> String {
    let index = profile::index_of(profile, profile.cutoff);
    profile
        .items
        .get(index)
        .map(crate::decision::profile_item_string)
        .unwrap_or_default()
}

/// The profile's custom format score of these formats.
pub fn format_score(profile: &QualityProfile, formats: &[CustomFormat]) -> i64 {
    score(profile, formats)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::decision::testing::default_profile;

    fn q(id: i64, version: i64) -> QualityModel {
        QualityModel {
            quality_id: id,
            version,
            real: 0,
            is_repack: false,
        }
    }

    #[test]
    fn upgrade_vectors_from_the_originals() {
        // UpgradeSpecificationFixture.IsUpgradeTestCases, with PreferAndUpgrade.
        let cases: [(i64, i64, i64, i64, UpgradeRejectReason); 7] = [
            (1, 1, 1, 2, UpgradeRejectReason::None),
            (5, 1, 5, 2, UpgradeRejectReason::None),
            (1, 1, 1, 1, UpgradeRejectReason::CustomFormatScore),
            (5, 1, 4, 2, UpgradeRejectReason::BetterQuality),
            (5, 1, 4, 2, UpgradeRejectReason::BetterQuality),
            (5, 1, 5, 1, UpgradeRejectReason::CustomFormatScore),
            (3, 1, 3, 1, UpgradeRejectReason::CustomFormatScore),
        ];
        for (current, cv, new, nv, expected) in cases {
            let profile = default_profile(&[], 0, true);
            assert_eq!(
                is_upgradable(
                    ProperDownloadTypes::PreferAndUpgrade,
                    &profile,
                    &q(current, cv),
                    &[],
                    &q(new, nv),
                    &[]
                ),
                expected,
                "{current} v{cv} -> {new} v{nv}"
            );
        }
        let profile = default_profile(&[], 0, false);
        assert_eq!(
            is_upgradable(
                ProperDownloadTypes::DoNotUpgrade,
                &profile,
                &q(2, 1),
                &[],
                &q(2, 2),
                &[]
            ),
            UpgradeRejectReason::None
        );
        assert_eq!(
            is_upgradable(
                ProperDownloadTypes::DoNotPrefer,
                &profile,
                &q(2, 1),
                &[],
                &q(2, 2),
                &[]
            ),
            UpgradeRejectReason::UpgradesNotAllowed
        );
    }

    #[test]
    fn revisions_and_cutoffs() {
        let profile = default_profile(&[1, 2, 4], 4, true);
        assert!(is_revision_upgrade(&q(1, 1), &q(1, 2)));
        assert!(!is_revision_upgrade(&q(1, 1), &q(2, 2)));
        assert!(quality_cutoff_not_met(&profile, &q(1, 1), None));
        assert!(!quality_cutoff_not_met(&profile, &q(4, 1), None));
        assert!(quality_cutoff_not_met(&profile, &q(4, 1), Some(&q(4, 2))));
        assert_eq!(cutoff_item_string(&profile), "HDTV-720p");
        let no_upgrades = default_profile(&[1, 2, 4], 4, false);
        assert_eq!(cutoff_item_string(&no_upgrades), "SDTV");
        assert!(is_upgrade_allowed(&no_upgrades, &q(1, 1), &[], &q(1, 1), &[]));
        assert!(!is_upgrade_allowed(&no_upgrades, &q(1, 1), &[], &q(2, 1), &[]));
        assert!(is_upgrade_allowed(&profile, &q(1, 1), &[], &q(2, 1), &[]));
    }
}
