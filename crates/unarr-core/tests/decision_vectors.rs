//! Replay the Sonarr decision engine test vectors
//! (`tests/vectors/decisionengine/*.json`, extracted by
//! `scripts/port-vectors.py`) against `unarr_core::decision`. Each fixture's
//! `SetUp` is rebuilt here with the in-memory lookup, and every test method
//! is mapped to the assertion the original makes; an unknown method fails.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{Duration, Utc};
use serde_json::Value;
use unarr_core::decision::lookup::{Catalog, IndexerInfo, ProperDownloadTypes};
use unarr_core::decision::specs::{self, Context, Subject};
use unarr_core::decision::testing::{default_profile, episode, series, MemoryLookup};
use unarr_core::decision::upgradable::{self, UpgradeRejectReason};
use unarr_core::decision::{megabytes, RemoteEpisode, SearchCriteria, SearchKind};
use unarr_core::indexers::rss::ReleaseInfo;
use unarr_core::model::{Protocol, QualityDefinition, QualityModel, ReleaseProfile};
use unarr_core::parser::title::ParsedEpisodeInfo;
use unarr_core::parser::ParsedQuality;

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/decisionengine")
}

fn quality_id(member: &str) -> i64 {
    match member {
        "Unknown" => 0,
        "SDTV" => 1,
        "DVD" => 2,
        "WEBDL1080p" => 3,
        "HDTV720p" => 4,
        "WEBDL720p" => 5,
        "Bluray720p" => 6,
        "Bluray1080p" => 7,
        "WEBDL480p" => 8,
        "HDTV1080p" => 9,
        "RAWHD" => 10,
        "HDTV2160p" => 16,
        "WEBDL2160p" => 18,
        "Bluray2160p" => 19,
        other => panic!("unknown quality {other}"),
    }
}

fn enum_member(v: &Value) -> &str {
    v["member"].as_str().expect("an enum reference")
}

fn quality(id: i64, version: i64) -> QualityModel {
    QualityModel {
        quality_id: id,
        version,
        real: 0,
        is_repack: false,
    }
}

fn parsed_with(q: QualityModel) -> ParsedEpisodeInfo {
    ParsedEpisodeInfo {
        quality: ParsedQuality {
            quality: q,
            ..ParsedQuality::default()
        },
        ..Default::default()
    }
}

struct Outcome {
    checks: usize,
    failures: Vec<String>,
}

fn check(out: &mut Outcome, ok: bool, what: String) {
    out.checks += 1;
    if !ok {
        out.failures.push(what);
    }
}

/// `AcceptableSizeSpecificationFixture`: SDTV is 2-10 MB per minute, and
/// the release has one, two or six episodes.
fn acceptable_size(method: &str, cases: &[Value], out: &mut Outcome) -> bool {
    let episodes_in_release = match method {
        "single_episode"
        | "should_return_expected_result_for_first_episode_of_season"
        | "should_return_expected_result_for_last_episode_of_season" => 1,
        "multi_episode" => 2,
        "multiset_episode" => 6,
        _ => return false,
    };
    for case in cases {
        let runtime = case[0].as_i64().unwrap();
        let size_mb = case[1].as_i64().unwrap();
        let expected = case[2].as_bool().unwrap();
        let mut catalog = Catalog {
            profiles: vec![default_profile(&[], 0, true)],
            ..Default::default()
        };
        catalog.definitions.push(QualityDefinition {
            id: 1,
            quality_id: 1,
            title: "SDTV".into(),
            min_size: Some(2.0),
            max_size: Some(10.0),
            preferred_size: None,
        });
        let mut s = series(1, "Series", 1);
        s.runtime = runtime;
        s.seasons.push(unarr_core::model::Season {
            season_number: 2,
            monitored: true,
            images: Vec::new(),
        });
        let lookup = MemoryLookup {
            series: vec![s.clone()],
            episodes: (1..=10)
                .map(|n| {
                    let mut e = episode(n, 1, 1, n);
                    e.runtime = 30;
                    e
                })
                .collect(),
            ..Default::default()
        };
        let mut remote = RemoteEpisode::new(ReleaseInfo {
            size: megabytes(size_mb),
            ..Default::default()
        });
        remote.parsed = Some(parsed_with(quality(1, 2)));
        remote.series = Some(s.clone());
        let episode_runtime = if method == "single_episode" { runtime } else { 30 };
        remote.episodes = (1..=episodes_in_release)
            .map(|n| {
                let mut e = episode(n, 1, 1, n);
                e.runtime = if episodes_in_release == 1 {
                    episode_runtime
                } else {
                    runtime
                };
                e
            })
            .collect();
        let ctx = Context {
            catalog: &catalog,
            lookup: &lookup,
            now: Utc::now(),
        };
        let subject = Subject {
            remote: &remote,
            series: &s,
            profile: &catalog.profiles[0],
            search: None,
        };
        let rejection = specs::acceptable_size(&ctx, &subject);
        check(
            out,
            rejection.is_none() == expected,
            format!("{method}({runtime}, {size_mb}): expected accepted={expected}, got {rejection:?}"),
        );
    }
    true
}

/// `QualityAllowedByProfileSpecificationFixture`.
fn quality_allowed(method: &str, cases: &[Value], out: &mut Outcome) -> bool {
    let expected = match method {
        "should_allow_if_quality_is_defined_in_profile" => true,
        "should_not_allow_if_quality_is_not_defined_in_profile" => false,
        _ => return false,
    };
    for case in cases {
        let q = quality_id(enum_member(&case[0]));
        let profile = default_profile(&[2, 4, 7], 7, true);
        let catalog = Catalog::default();
        let lookup = MemoryLookup::default();
        let s = series(1, "Series", 1);
        let mut remote = RemoteEpisode::new(ReleaseInfo::default());
        remote.parsed = Some(parsed_with(quality(q, 2)));
        remote.series = Some(s.clone());
        let ctx = Context {
            catalog: &catalog,
            lookup: &lookup,
            now: Utc::now(),
        };
        let subject = Subject {
            remote: &remote,
            series: &s,
            profile: &profile,
            search: None,
        };
        let rejection = specs::quality_allowed_by_profile(&ctx, &subject);
        check(
            out,
            rejection.is_none() == expected,
            format!("{method}({q}): expected accepted={expected}, got {rejection:?}"),
        );
    }
    true
}

/// `RawDiskSpecificationFixture`.
fn raw_disk(method: &str, cases: &[Value], out: &mut Outcome) -> bool {
    let expected = match method {
        "should_return_false_if_matches_disc_format" => false,
        "should_return_true_if_dvdrip" => true,
        _ => return false,
    };
    for case in cases {
        let title = case[0].as_str().unwrap();
        let catalog = Catalog::default();
        let lookup = MemoryLookup::default();
        let s = series(1, "Series", 1);
        let profile = default_profile(&[], 0, true);
        let mut remote = RemoteEpisode::new(ReleaseInfo {
            title: title.into(),
            protocol: Protocol::Torrent,
            ..Default::default()
        });
        remote.parsed = Some(ParsedEpisodeInfo::default());
        remote.series = Some(s.clone());
        let ctx = Context {
            catalog: &catalog,
            lookup: &lookup,
            now: Utc::now(),
        };
        let subject = Subject {
            remote: &remote,
            series: &s,
            profile: &profile,
            search: None,
        };
        let rejection = specs::raw_disk(&ctx, &subject);
        check(
            out,
            rejection.is_none() == expected,
            format!("{method}({title:?}): expected accepted={expected}, got {rejection:?}"),
        );
    }
    true
}

/// `ReleaseRestrictionsSpecificationFixture`, on
/// `Dexter.S08E01.EDITED.WEBRip.x264-KYR`.
fn release_restrictions(method: &str, cases: &[Value], out: &mut Outcome) -> bool {
    for case in cases {
        let (required, ignored, expected): (Vec<String>, Vec<String>, bool) = match method {
            "should_ignore_case_when_matching_required" => (split(case[0].as_str().unwrap()), vec![], true),
            "should_ignore_case_when_matching_ignored" => (vec![], split(case[0].as_str().unwrap()), false),
            "should_match_perl_regex" => (
                split(case[0].as_str().unwrap()),
                vec![],
                case[1].as_bool().unwrap(),
            ),
            _ => return false,
        };
        let catalog = Catalog {
            release_profiles: vec![ReleaseProfile {
                id: 1,
                name: None,
                enabled: true,
                required: required.clone(),
                ignored: ignored.clone(),
                indexer_id: 0,
                tags: vec![],
            }],
            ..Default::default()
        };
        let lookup = MemoryLookup::default();
        let s = series(1, "Dexter", 1);
        let profile = default_profile(&[], 0, true);
        let mut remote = RemoteEpisode::new(ReleaseInfo {
            title: "Dexter.S08E01.EDITED.WEBRip.x264-KYR".into(),
            ..Default::default()
        });
        remote.parsed = Some(ParsedEpisodeInfo::default());
        remote.series = Some(s.clone());
        let ctx = Context {
            catalog: &catalog,
            lookup: &lookup,
            now: Utc::now(),
        };
        let subject = Subject {
            remote: &remote,
            series: &s,
            profile: &profile,
            search: None,
        };
        let rejection = specs::release_restrictions(&ctx, &subject);
        check(
            out,
            rejection.is_none() == expected,
            format!("{method}({required:?}, {ignored:?}): expected accepted={expected}, got {rejection:?}"),
        );
    }
    true
}

fn split(s: &str) -> Vec<String> {
    s.split(',').map(str::to_string).collect()
}

/// `TorrentSeedingSpecificationFixture`: indexer 1 wants 5 seeders.
fn torrent_seeding(method: &str, cases: &[Value], out: &mut Outcome) -> bool {
    let expected = match method {
        "should_return_true_if_seeds_above_or_equal_to_limit" => true,
        "should_return_false_if_seeds_belove_limit" => false,
        _ => return false,
    };
    for case in cases {
        let seeders = case[0].as_i64().unwrap();
        let catalog = Catalog {
            indexers: vec![IndexerInfo {
                id: 1,
                name: "Torrent".into(),
                tags: vec![],
                minimum_seeders: Some(5),
                multi_languages: vec![],
                seed_ratio: None,
                seed_time: None,
                season_pack_seed_time: None,
            }],
            ..Default::default()
        };
        let lookup = MemoryLookup::default();
        let s = series(1, "Series", 1);
        let profile = default_profile(&[], 0, true);
        let mut remote = RemoteEpisode::new(ReleaseInfo {
            title: "Series.Title.S01.720p.BluRay.X264-RlsGrp".into(),
            protocol: Protocol::Torrent,
            indexer_id: 1,
            seeders: Some(seeders),
            ..Default::default()
        });
        remote.parsed = Some(ParsedEpisodeInfo::default());
        remote.series = Some(s.clone());
        let ctx = Context {
            catalog: &catalog,
            lookup: &lookup,
            now: Utc::now(),
        };
        let subject = Subject {
            remote: &remote,
            series: &s,
            profile: &profile,
            search: None,
        };
        let rejection = specs::torrent_seeding(&ctx, &subject);
        check(
            out,
            rejection.is_none() == expected,
            format!("{method}({seeders}): expected accepted={expected}, got {rejection:?}"),
        );
    }
    true
}

/// `SingleEpisodeAgeDownloadDecisionFixture` (`SeasonPackOnlySpecification`):
/// four episodes aired 400, 370, 340 and 310 days ago, searched as a season.
fn season_pack_only(method: &str, cases: &[Value], out: &mut Outcome) -> bool {
    let now = Utc::now();
    let s = series(1, "Series", 1);
    let episodes: Vec<_> = [(1, 400), (2, 370), (3, 340), (4, 310)]
        .iter()
        .map(|(n, age)| {
            let mut e = episode(*n, 1, 1, *n);
            e.air_date_utc = Some(now - Duration::days(*age));
            e
        })
        .collect();
    let search = SearchCriteria {
        series: s.clone(),
        episodes: episodes.clone(),
        kind: SearchKind::Season { season_number: 1 },
        monitored_episodes_only: false,
        user_invoked_search: false,
        interactive_search: false,
    };
    for case in cases {
        let (chosen, max_age, expected, full_season) = match method {
            "single_episode_release" => (
                Some(case[0].as_i64().unwrap()),
                case[1].as_i64().unwrap(),
                case[2].as_bool().unwrap(),
                false,
            ),
            "multi_episode_release" => (None, case[0].as_i64().unwrap(), case[1].as_bool().unwrap(), true),
            _ => return false,
        };
        let catalog = Catalog::default();
        let lookup = MemoryLookup::default();
        let profile = default_profile(&[], 0, true);
        let mut remote = RemoteEpisode::new(ReleaseInfo {
            season_search_maximum_single_episode_age: max_age,
            ..Default::default()
        });
        let mut parsed = parsed_with(quality(1, 2));
        parsed.full_season = full_season;
        remote.parsed = Some(parsed);
        remote.series = Some(s.clone());
        remote.episodes = match chosen {
            Some(n) => episodes
                .iter()
                .filter(|e| e.episode_number == n)
                .cloned()
                .collect(),
            None => episodes.clone(),
        };
        let ctx = Context {
            catalog: &catalog,
            lookup: &lookup,
            now,
        };
        let subject = Subject {
            remote: &remote,
            series: &s,
            profile: &profile,
            search: Some(&search),
        };
        let rejection = specs::season_pack_only(&ctx, &subject);
        check(
            out,
            rejection.is_none() == expected,
            format!("{method}({chosen:?}, {max_age}): expected accepted={expected}, got {rejection:?}"),
        );
    }
    true
}

/// `UpgradeSpecificationFixture.IsUpgradeTest`, with `PreferAndUpgrade`.
fn upgrade(method: &str, cases: &[Value], out: &mut Outcome) -> bool {
    if method != "IsUpgradeTest" {
        return false;
    }
    for case in cases {
        let current = quality(quality_id(enum_member(&case[0])), case[1].as_i64().unwrap());
        let new = quality(quality_id(enum_member(&case[2])), case[3].as_i64().unwrap());
        let expected = match enum_member(&case[5]) {
            "None" => UpgradeRejectReason::None,
            "BetterQuality" => UpgradeRejectReason::BetterQuality,
            "BetterRevision" => UpgradeRejectReason::BetterRevision,
            "QualityCutoff" => UpgradeRejectReason::QualityCutoff,
            "CustomFormatScore" => UpgradeRejectReason::CustomFormatScore,
            "CustomFormatCutoff" => UpgradeRejectReason::CustomFormatCutoff,
            "MinCustomFormatScore" => UpgradeRejectReason::MinCustomFormatScore,
            "UpgradesNotAllowed" => UpgradeRejectReason::UpgradesNotAllowed,
            other => panic!("unknown reason {other}"),
        };
        let profile = default_profile(&[], 0, true);
        let got = upgradable::is_upgradable(
            ProperDownloadTypes::PreferAndUpgrade,
            &profile,
            &current,
            &[],
            &new,
            &[],
        );
        check(
            out,
            got == expected,
            format!("IsUpgradeTest({current:?} -> {new:?}): expected {expected:?}, got {got:?}"),
        );
    }
    true
}

#[test]
fn sonarr_decision_engine_vectors_pass() {
    let mut out = Outcome {
        checks: 0,
        failures: Vec::new(),
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(vectors_dir())
        .expect("vectors directory")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no vector files");
    for path in files {
        let fixture = path.file_stem().unwrap().to_string_lossy().to_string();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let methods: BTreeMap<String, Value> = serde_json::from_value(doc["methods"].clone()).unwrap();
        for (method, body) in methods {
            let cases = body["cases"].as_array().cloned().unwrap_or_default();
            let known = match fixture.as_str() {
                "AcceptableSizeSpecificationFixture" => acceptable_size(&method, &cases, &mut out),
                "QualityAllowedByProfileSpecificationFixture" => quality_allowed(&method, &cases, &mut out),
                "RawDiskSpecificationFixture" => raw_disk(&method, &cases, &mut out),
                "ReleaseRestrictionsSpecificationFixture" => release_restrictions(&method, &cases, &mut out),
                "TorrentSeedingSpecificationFixture" => torrent_seeding(&method, &cases, &mut out),
                "SingleEpisodeAgeDownloadDecisionFixture" => season_pack_only(&method, &cases, &mut out),
                "UpgradeSpecificationFixture" => upgrade(&method, &cases, &mut out),
                _ => false,
            };
            assert!(
                known,
                "no replay for {fixture}.{method}: map it or list it as skipped"
            );
        }
    }
    assert!(
        out.failures.is_empty(),
        "{} of {} checks failed:\n{}",
        out.failures.len(),
        out.checks,
        out.failures.join("\n")
    );
    eprintln!("{} decision engine checks passed", out.checks);
}
