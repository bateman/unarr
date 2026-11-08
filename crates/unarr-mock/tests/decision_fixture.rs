//! The decision engine on the Sonarr fixture library: the mock indexer's
//! feed is parsed, mapped to the fixture's series and episodes, and judged
//! as an RSS sync and as an interactive search would.

use chrono::{TimeZone, Utc};
use unarr_core::db::Db;
use unarr_core::decision::{self, DecisionInput, RejectionReason, SearchCriteria, SearchKind};
use unarr_core::indexers::rss::{parse_feed, ReleaseInfo};
use unarr_core::indexers::IndexerKind;
use unarr_core::store;

fn fixture_db() -> Db {
    let dir = unarr_mock::fixtures_dir().join("sonarr");
    let db = Db::open_in_memory().unwrap();
    unarr_import::sonarr::import(&dir.join("sonarr.db"), &db).expect("import fixture");
    db
}

fn feed() -> Vec<ReleaseInfo> {
    let xml = std::fs::read_to_string(unarr_mock::fixtures_dir().join("indexers/newznab/rss.xml")).unwrap();
    let mut releases = parse_feed(&xml, IndexerKind::Newznab, "http://127.0.0.1:18989/newznab/api").unwrap();
    for r in &mut releases {
        r.indexer_id = 20;
        r.indexer = "Mock Newznab".into();
    }
    releases
}

fn by_title<'a>(decisions: &'a [decision::Decision], prefix: &str) -> &'a decision::Decision {
    decisions
        .iter()
        .find(|d| d.remote.release.title.starts_with(prefix))
        .unwrap_or_else(|| panic!("no decision for {prefix}"))
}

#[test]
fn rss_decisions_map_the_fixture_library() {
    let db = fixture_db();
    // A day after the newest item, so nothing is "too young".
    let now = Utc.with_ymd_and_hms(2025, 4, 13, 12, 0, 0).unwrap();
    let decisions = db
        .call_blocking(move |conn| {
            let catalog = decision::Catalog::load(conn, now)?;
            let lookup = decision::StoreLookup::new(conn);
            Ok(decision::decide(
                &catalog,
                &lookup,
                now,
                feed(),
                DecisionInput::Rss,
            ))
        })
        .unwrap();
    assert_eq!(decisions.len(), 9);

    let pitt = by_title(&decisions, "The.Pitt.S01E01.1080p.AMZN");
    let series = pitt.remote.series.as_ref().expect("The Pitt is in the library");
    assert_eq!(series.id, 1);
    assert_eq!(pitt.remote.episodes.len(), 1);
    assert_eq!(
        (
            pitt.remote.episodes[0].season_number,
            pitt.remote.episodes[0].episode_number
        ),
        (1, 1)
    );
    assert_eq!(pitt.remote.release_source, decision::ReleaseSource::Rss);
    assert_eq!(pitt.remote.languages, vec![1], "English, the series language");
    assert!(
        pitt.remote.custom_formats.iter().any(|f| f.name == "AMZN"),
        "AMZN format matched: {:?}",
        pitt.remote.custom_formats
    );
    // The fixture leaves the first episodes unmonitored.
    assert!(
        pitt.rejections
            .iter()
            .any(|r| r.reason == RejectionReason::EpisodeNotMonitored),
        "{:?}",
        pitt.rejections
    );

    let pack = by_title(&decisions, "The.Pitt.S01.1080p");
    assert_eq!(pack.remote.episodes.len(), 15, "the whole first season");
    assert!(pack.remote.parsed.as_ref().unwrap().full_season);

    let daredevil = by_title(&decisions, "Daredevil.Born.Again");
    assert_eq!(daredevil.remote.series.as_ref().map(|s| s.id), Some(2));
    assert_eq!(
        daredevil.remote.episodes.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![46]
    );

    let unknown = by_title(&decisions, "Some.Other.Show");
    assert!(unknown.remote.series.is_none());
    assert_eq!(unknown.rejections.len(), 1);
    assert_eq!(unknown.rejections[0].reason, RejectionReason::UnknownSeries);
    assert_eq!(unknown.rejections[0].message, "Unknown Series");

    let italian = by_title(&decisions, "The.Pitt.S01E01.iTALiAN");
    assert_eq!(italian.remote.languages, vec![5], "Italian");
}

#[test]
fn search_decisions_check_the_request() {
    let db = fixture_db();
    let now = Utc.with_ymd_and_hms(2025, 4, 13, 12, 0, 0).unwrap();
    let decisions = db
        .call_blocking(move |conn| {
            let catalog = decision::Catalog::load(conn, now)?;
            let lookup = decision::StoreLookup::new(conn);
            let series = store::series::get(conn, 1)?.unwrap();
            let episode = store::episodes::find_episode(conn, 1, 1, 1)?.unwrap();
            let search = SearchCriteria {
                series,
                episodes: vec![episode],
                kind: SearchKind::SingleEpisode {
                    season_number: 1,
                    episode_number: 1,
                },
                monitored_episodes_only: false,
                user_invoked_search: true,
                interactive_search: true,
            };
            let decisions = decision::decide(&catalog, &lookup, now, feed(), DecisionInput::Search(&search));
            Ok(decision::prioritize(&catalog, now, decisions))
        })
        .unwrap();

    let messages = |prefix: &str| -> Vec<String> {
        by_title(&decisions, prefix)
            .rejections
            .iter()
            .map(|r| r.message.clone())
            .collect()
    };
    // Every rejection of the first priority is reported, as the originals
    // list them; the fixture's "HD-1080p - ITA" profile also scores
    // non-Italian releases below its minimum.
    let has = |prefix: &str, message: &str| {
        let found = messages(prefix);
        assert!(found.iter().any(|m| m == message), "{prefix}: {found:?}");
    };
    has("The.Pitt.S01E02", "Wrong episode");
    has("The.Pitt.S01E02", "Episode wasn't requested: 1x2");
    has("The.Pitt.S01.1080p", "Full season pack");
    has("Daredevil.Born.Again", "Wrong series");
    has(
        "The.Pitt.S01E01.1080p.AMZN",
        "Custom Formats AMZN, Language not Italian, WEB Tier 01 have score -10000 below Series profile minimum 0",
    );
    assert_eq!(messages("Some.Other.Show"), vec!["Unknown Series"]);
    assert!(
        messages("The.Pitt.S01E01.iTALiAN")
            .iter()
            .all(|m| !m.starts_with("Custom Formats")),
        "{:?}",
        messages("The.Pitt.S01E01.iTALiAN")
    );
    let wanted = by_title(&decisions, "The.Pitt.S01E01.1080p.AMZN");
    assert!(wanted.remote.episode_requested);
    assert_eq!(
        wanted.remote.release_source,
        decision::ReleaseSource::InteractiveSearch
    );
    assert!(
        !wanted
            .rejections
            .iter()
            .any(|r| r.reason == RejectionReason::EpisodeNotMonitored),
        "monitoring is not checked for an interactive search: {:?}",
        wanted.rejections
    );

    // Prioritised: within The Pitt, the 2160p release leads when its
    // quality is allowed, otherwise the best allowed 1080p one; the unknown
    // series trails.
    assert!(decisions.last().unwrap().remote.series.is_none());
    let pitt: Vec<&str> = decisions
        .iter()
        .filter(|d| d.remote.series.as_ref().is_some_and(|s| s.id == 1))
        .map(|d| d.remote.release.title.as_str())
        .collect();
    assert_eq!(pitt.len(), 7);
    assert!(
        pitt[0].contains("1080p") || pitt[0].contains("2160p"),
        "best first: {pitt:?}"
    );
}
