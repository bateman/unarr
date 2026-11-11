//! Replay Radarr's decision-engine test vectors
//! (`tests/vectors/radarr/decisionengine/*.json`, extracted by
//! `scripts/port-vectors.py`) against `unarr_core::movies::decision`. Only
//! the fixtures driven by test cases are data; the others assert on mocks
//! and are ported as unit tests next to the engine. A method this file
//! does not know fails the test.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use serde_json::Value;
use unarr_core::decision::lookup::IndexerInfo;
use unarr_core::decision::megabytes;
use unarr_core::model::{Protocol, QualityDefinition, ReleaseProfile};
use unarr_core::movies::decision::specs::{self, Context, Spec, Subject};
use unarr_core::movies::decision::testing::{movie, profile, quality, release, remote, MemoryMovieLookup};
use unarr_core::movies::decision::{MovieCatalog, RemoteMovie};
use unarr_core::movies::model::Movie;
use unarr_core::movies::quality::MovieQuality;

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/radarr/decisionengine")
}

fn quality_id(member: &str) -> i64 {
    let wanted = member.to_lowercase();
    MovieQuality::all()
        .find(|q| q.name.replace('-', "").to_lowercase() == wanted)
        .map(|q| q.id)
        .unwrap_or_else(|| panic!("unknown quality {member}"))
}

fn accepted(spec: Spec, catalog: &MovieCatalog, remote: &RemoteMovie, movie: &Movie) -> bool {
    let lookup = MemoryMovieLookup::default();
    let profile = catalog
        .base
        .profile(movie.quality_profile_id)
        .expect("the profile");
    let ctx = Context {
        catalog,
        lookup: &lookup,
        now: catalog.base.now,
    };
    let subject = Subject {
        remote,
        movie,
        profile,
        search: None,
    };
    spec(&ctx, &subject).is_none()
}

fn catalog() -> MovieCatalog {
    let mut catalog = MovieCatalog::default();
    catalog.base.now = Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
    catalog.base.profiles = vec![profile(&[], 7, true)];
    catalog
}

/// Run one test method's cases; `None` when the method is unknown.
fn run_method(method: &str, cases: &[Value]) -> Option<Vec<String>> {
    const SDTV: i64 = 1;
    let mut failures = Vec::new();
    let now = catalog().base.now;
    for case in cases {
        let a = case.as_array().cloned().unwrap_or_default();
        let mut check = |ok: bool| {
            if !ok {
                failures.push(format!("{method}{case}"));
            }
        };
        match method {
            // AcceptableSizeSpecificationFixture: SDTV between 2 and 10 MB a minute.
            "single_episode" => {
                let mut catalog = catalog();
                catalog.base.definitions = vec![QualityDefinition {
                    id: 1,
                    quality_id: SDTV,
                    title: "SDTV".into(),
                    min_size: Some(2.0),
                    max_size: Some(10.0),
                    preferred_size: None,
                }];
                let mut m = movie(1, "A Movie", 2020);
                m.metadata.runtime = a[0].as_i64().unwrap();
                let mut r = release("A.Movie.2020.SDTV-GRP", now);
                r.size = megabytes(a[1].as_i64().unwrap());
                let remote = remote(r, &m, quality(SDTV, 2));
                check(accepted(specs::acceptable_size, &catalog, &remote, &m) == a[2].as_bool().unwrap());
            }
            // QualityAllowedByProfileSpecificationFixture: DVD, HDTV-720p and Bluray-1080p allowed.
            "should_allow_if_quality_is_defined_in_profile"
            | "should_not_allow_if_quality_is_not_defined_in_profile" => {
                let mut catalog = catalog();
                catalog.base.profiles = vec![profile(&[2, 4, 7], 7, true)];
                let m = movie(1, "A Movie", 2020);
                let id = quality_id(a[0]["member"].as_str().unwrap());
                let remote = remote(release("A.Movie.2020-GRP", now), &m, quality(id, 2));
                let wanted = method == "should_allow_if_quality_is_defined_in_profile";
                check(accepted(specs::quality_allowed_by_profile, &catalog, &remote, &m) == wanted);
            }
            // RawDiskSpecificationFixture.
            "should_return_false_if_matches_disc_format" | "should_return_true_if_dvdrip" => {
                let m = movie(1, "A Movie", 2020);
                let remote = remote(release(a[0].as_str().unwrap(), now), &m, quality(2, 1));
                let wanted = method == "should_return_true_if_dvdrip";
                check(accepted(specs::raw_disk, &catalog(), &remote, &m) == wanted);
            }
            // ReleaseRestrictionsSpecificationFixture.
            "should_ignore_case_when_matching_required"
            | "should_ignore_case_when_matching_ignored"
            | "should_match_perl_regex" => {
                let terms: Vec<String> = a[0].as_str().unwrap().split(',').map(str::to_string).collect();
                let (required, ignored, wanted) = match method {
                    "should_ignore_case_when_matching_required" => (terms, Vec::new(), true),
                    "should_ignore_case_when_matching_ignored" => (Vec::new(), terms, false),
                    _ => (
                        vec![a[0].as_str().unwrap().to_string()],
                        Vec::new(),
                        a[1].as_bool().unwrap(),
                    ),
                };
                let mut catalog = catalog();
                catalog.base.release_profiles = vec![ReleaseProfile {
                    id: 1,
                    name: None,
                    enabled: true,
                    required,
                    ignored,
                    indexer_id: 0,
                    tags: Vec::new(),
                }];
                let m = movie(1, "A Movie", 2020);
                let remote = remote(
                    release("Dexter.S08E01.EDITED.WEBRip.x264-KYR", now),
                    &m,
                    quality(14, 1),
                );
                check(accepted(specs::release_restrictions, &catalog, &remote, &m) == wanted);
            }
            // TorrentSeedingSpecificationFixture: a torrent indexer asking for five seeders.
            "should_return_true_if_seeds_above_or_equal_to_limit"
            | "should_return_false_if_seeds_belove_limit" => {
                let mut catalog = catalog();
                catalog.base.indexers = vec![IndexerInfo {
                    id: 1,
                    name: "Torznab".into(),
                    tags: Vec::new(),
                    minimum_seeders: Some(5),
                    multi_languages: Vec::new(),
                    seed_ratio: None,
                    seed_time: None,
                    season_pack_seed_time: None,
                }];
                let m = movie(1, "A Movie", 2020);
                let mut r = release("A.Movie.2020.1080p.WEB-DL-GRP", now);
                r.protocol = Protocol::Torrent;
                r.indexer_id = 1;
                r.seeders = a[0].as_i64();
                let remote = remote(r, &m, quality(3, 1));
                let wanted = method == "should_return_true_if_seeds_above_or_equal_to_limit";
                check(accepted(specs::torrent_seeding, &catalog, &remote, &m) == wanted);
            }
            _ => return None,
        }
    }
    Some(failures)
}

#[test]
fn radarr_decision_vectors() {
    let mut failures = Vec::new();
    let mut unknown = Vec::new();
    let mut checked = 0;
    let mut files: Vec<PathBuf> = std::fs::read_dir(vectors_dir())
        .expect("tests/vectors/radarr/decisionengine")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no vectors found");
    for path in files {
        let data: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let methods: BTreeMap<String, Value> = serde_json::from_value(data["methods"].clone()).unwrap();
        for (method, body) in methods {
            let cases = body["cases"].as_array().cloned().unwrap_or_default();
            checked += cases.len();
            match run_method(&method, &cases) {
                Some(f) => failures.extend(f),
                None => unknown.push(method),
            }
        }
    }
    assert!(unknown.is_empty(), "test methods without a replay: {unknown:#?}");
    assert!(
        failures.is_empty(),
        "{} of {checked} vectors fail:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
