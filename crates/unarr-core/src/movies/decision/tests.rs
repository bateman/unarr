//! The original's fixtures that assert on mocks (`ParsingServiceTests`,
//! `DecisionEngineTests`), ported as tests of the engine on a library held
//! in memory.

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::engine::{aggregate_languages, compare, decide, prioritize, MovieCatalog};
use super::library::{LibraryMovie, MemoryMovieLookup};
use super::mapping::{find_by_title, find_movie, MultipleMovies};
use super::testing::{movie, parsed, profile, quality, release};
use super::{MovieDecision, MovieMatchType, MovieRejectionReason as R, MovieSearch, RemoteMovie};
use crate::decision::ReleaseSource;
use crate::model::{DelayProfile, Protocol};
use crate::movies::model::MovieStatus;
use crate::movies::parser::language::lang;
use crate::movies::parser::{clean_movie_title, ParsedMovieInfo};

const BLURAY1080P: i64 = 7;
const WEBDL1080P: i64 = 3;
const HDTV720P: i64 = 4;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
}

fn catalog() -> MovieCatalog {
    let mut catalog = MovieCatalog::default();
    catalog.base.now = now();
    catalog.base.profiles = vec![profile(&[], BLURAY1080P, true)];
    catalog.base.delay_profiles = vec![DelayProfile {
        id: 1,
        enable_usenet: true,
        enable_torrent: true,
        preferred_protocol: Protocol::Usenet,
        usenet_delay: 0,
        torrent_delay: 0,
        order: i64::from(i32::MAX),
        tags: Vec::new(),
        bypass_if_highest_quality: false,
        bypass_if_above_custom_format_score: false,
        minimum_custom_format_score: 0,
    }];
    catalog
}

/// `MapFixture`'s movie: a title with an umlaut and a number, an
/// alternative title and a translation.
fn goethe() -> LibraryMovie {
    let mut m = movie(1, "Fack Ju Göthe 2", 2015);
    m.metadata.clean_title = Some("fackjugoethe2".into());
    LibraryMovie {
        movie: m,
        alternative_titles: vec![clean_movie_title("Fack Ju Göthe 2: Same same")],
        translations: vec!["translatedtitle".into()],
    }
}

fn info(title: &str, year: i64) -> ParsedMovieInfo {
    let mut info = parsed(quality(BLURAY1080P, 1));
    info.movie_titles = vec![title.to_string()];
    info.year = year;
    info
}

fn search(movie: &LibraryMovie) -> MovieSearch {
    MovieSearch {
        movie: movie.clone(),
        user_invoked: false,
        interactive: false,
    }
}

#[test]
fn a_search_matches_the_titles_of_the_movie_searched_for() {
    let goethe = goethe();
    let lookup = MemoryMovieLookup::default();
    let criteria = search(&goethe);
    let map = |title: &str, year: i64| {
        find_movie(&lookup, &info(title, year), "", 0, Some(&criteria))
            .unwrap()
            .map(|(m, kind)| (m.id, kind))
    };
    let found = Some((1, MovieMatchType::Title));
    assert_eq!(map("Fack Ju Göthe 2", 2015), found);
    // should_match_alternative_title, should_match_translation_title
    assert_eq!(map("Fack Ju Göthe 2: Same same", 2015), found);
    assert_eq!(map("Translated Title", 2015), found);
    // should_match_roman_title, should_match_umlauts
    assert_eq!(map("Fack Ju Göthe II", 2015), found);
    assert_eq!(map("Fack Ju Goethe 2", 2015), found);
    assert_eq!(map("Fack Ju Goethe 2: Same same", 2015), found);
    // A year that is not the movie's, a title that is not: no movie.
    assert_eq!(map("Fack Ju Göthe 2", 1900), None);
    assert_eq!(map("Other Title", 2015), None);
    // No year in the name matches on the title alone.
    assert_eq!(map("Fack Ju Göthe 2", 0), found);
}

#[test]
fn ids_win_when_the_year_agrees() {
    let goethe = goethe();
    let lookup = MemoryMovieLookup {
        movies: vec![goethe.clone()],
        ..MemoryMovieLookup::default()
    };
    let by = |imdb: &str, tmdb: i64, year: i64, criteria: Option<&MovieSearch>| {
        find_movie(&lookup, &info("Other Title", year), imdb, tmdb, criteria)
            .unwrap()
            .map(|(m, kind)| (m.id, kind))
    };
    let by_id = Some((1, MovieMatchType::Id));
    // The feed's IMDb number, as the mapping receives it.
    assert_eq!(by("1001", 0, 2015, None), by_id);
    assert_eq!(by("0", 1001, 2015, None), by_id);
    // An indexer naming the wrong id: the year disagrees, the title too.
    assert_eq!(by("1001", 0, 1999, None), None);
    assert_eq!(by("0", 1001, 0, None), by_id);
    // In a search the ids of the movie searched for are believed even
    // then: its TMDB id, and (Radarr 7, `MapFixture`) its IMDb id once
    // the feed's number is normalised to the `tt` form.
    let criteria = search(&goethe);
    assert_eq!(by("0", 1001, 1999, Some(&criteria)), by_id);
    assert_eq!(by("1001", 0, 1999, Some(&criteria)), by_id);
    // `should_not_match_search_criteria_movie_without_imdb_id_when_release_has_none`.
    let mut no_imdb = goethe.clone();
    no_imdb.movie.metadata.imdb_id = None;
    let criteria = search(&no_imdb);
    assert_eq!(by("0", 0, 1999, Some(&criteria)), None);
}

#[test]
fn a_title_finds_one_movie_across_the_library() {
    let mut remake = movie(2, "The Thing", 2011);
    remake.metadata.secondary_year = Some(2012);
    let lookup = MemoryMovieLookup {
        movies: vec![
            goethe(),
            LibraryMovie::bare(movie(3, "The Thing", 1982)),
            LibraryMovie::bare(remake),
        ],
        ..MemoryMovieLookup::default()
    };
    let find = |title: &str, year: Option<i64>| {
        find_by_title(&lookup, &[title.to_string()], year).map(|m| m.map(|m| m.id))
    };
    assert_eq!(find("The Thing", Some(1982)), Ok(Some(3)));
    assert_eq!(
        find("The Thing", Some(2012)),
        Ok(Some(2)),
        "the secondary year counts"
    );
    assert_eq!(find("The Thing", Some(1999)), Ok(None));
    // Two movies of that title and no year to tell them apart.
    assert_eq!(find("The Thing", None), Err(MultipleMovies(2)));
    // A numeral in the name finds a movie titled with the roman one; the
    // other way round the original compares a lower-case title with
    // upper-case numerals and finds nothing (a search still matches it,
    // through the titles of the movie searched for).
    assert_eq!(find("Fack Ju Göthe II", Some(2015)), Ok(None));
    let rocky = MemoryMovieLookup {
        movies: vec![LibraryMovie::bare(movie(4, "Rocky II", 1979))],
        ..MemoryMovieLookup::default()
    };
    assert_eq!(
        find_by_title(&rocky, &["Rocky 2".to_string()], Some(1979)).map(|m| m.map(|m| m.id)),
        Ok(Some(4))
    );
    // Alternative titles and translations.
    assert_eq!(find("Fack Ju Goethe 2 Same same", Some(2015)), Ok(Some(1)));
    assert_eq!(find("Translated Title", None), Ok(Some(1)));
    assert_eq!(find("Nothing Like It", None), Ok(None));
}

/// `DownloadDecisionMakerFixture` (Radarr 7): a release that cannot be
/// processed (two movies answer to its title) is an error that still
/// carries what its title parsed to, so the interactive search can show
/// it.
#[test]
fn a_release_that_cannot_be_processed_keeps_what_its_title_parsed_to() {
    let lookup = MemoryMovieLookup {
        movies: vec![
            LibraryMovie::bare(movie(3, "The Thing", 1982)),
            LibraryMovie::bare(movie(4, "The Thing", 1982)),
        ],
        ..MemoryMovieLookup::default()
    };
    let r = release("The.Thing.1982.1080p.BluRay.x264-GRP", now() - Duration::hours(3));
    let d = decide(&catalog(), &lookup, &[r], None, false).remove(0);
    assert_eq!(reasons(&d), vec![R::Error]);
    assert_eq!(d.rejections[0].message, "Unexpected error processing release");
    let parsed = d.remote.parsed.as_ref().expect("the parsed title is kept");
    assert_eq!(parsed.quality.quality.quality_id, BLURAY1080P);
    assert_eq!(d.remote.languages, parsed.languages);
}

#[test]
fn original_stands_for_the_language_of_the_movie() {
    let catalog = catalog();
    let goethe = goethe().movie;
    let aggregate = |languages: &[i64]| {
        let mut info = info("Fack Ju Göthe 2", 2015);
        info.languages = languages.to_vec();
        let mut remote = RemoteMovie::new(release("Fack.Ju.Goethe.2.2015.1080p.BluRay-GRP", now()));
        remote.parsed = Some(info);
        aggregate_languages(&catalog, &remote, &goethe)
    };
    // should_add_original_language (English is the movie's).
    assert_eq!(
        aggregate(&[lang::ORIGINAL, lang::FRENCH]),
        vec![lang::FRENCH, lang::ENGLISH]
    );
    // With the original language already named, Original becomes Unknown.
    assert_eq!(
        aggregate(&[lang::ORIGINAL, lang::FRENCH, lang::ENGLISH]),
        vec![lang::FRENCH, lang::ENGLISH, lang::UNKNOWN]
    );
    // Nothing parsed: the movie's language.
    assert_eq!(aggregate(&[lang::UNKNOWN]), vec![lang::ENGLISH]);
    assert_eq!(aggregate(&[]), vec![lang::ENGLISH]);
}

fn library() -> MemoryMovieLookup {
    MemoryMovieLookup {
        movies: vec![LibraryMovie::bare(movie(1, "The Holdovers", 2023))],
        ..MemoryMovieLookup::default()
    }
}

fn reasons(decision: &MovieDecision) -> Vec<R> {
    decision.rejections.iter().map(|r| r.reason).collect()
}

#[test]
fn a_release_is_decided_for_the_movie_it_names() {
    let (catalog, lookup) = (catalog(), library());
    let good = release(
        "The.Holdovers.2023.1080p.BluRay.x264-GRP",
        now() - Duration::hours(3),
    );
    let unknown = release(
        "Another.Movie.2020.1080p.BluRay.x264-GRP",
        now() - Duration::hours(3),
    );
    let noise = release("abc123", now());
    let decisions = decide(
        &catalog,
        &lookup,
        &[good.clone(), unknown.clone(), noise.clone()],
        None,
        false,
    );
    // Outside a search a name that does not parse is not decided at all.
    assert_eq!(decisions.len(), 2);
    assert!(decisions[0].approved(), "{:?}", decisions[0].rejections);
    let remote = &decisions[0].remote;
    assert_eq!(remote.movie.as_ref().map(|m| m.id), Some(1));
    assert_eq!(remote.match_type, MovieMatchType::Title);
    assert_eq!(remote.release_source, ReleaseSource::Rss);
    assert_eq!(remote.languages, vec![lang::ENGLISH]);
    assert!(remote.download_allowed);
    assert_eq!(reasons(&decisions[1]), vec![R::UnknownMovie]);
    assert_eq!(
        decisions[1].rejections[0].message,
        "Unknown Movie. Unable to match to correct movie using release title."
    );
    let pushed = decide(&catalog, &lookup, &[unknown], None, true);
    assert_eq!(
        pushed[0].rejections[0].message,
        "Unknown Movie. Unable to match to existing movie in Library using release title."
    );
    assert_eq!(pushed[0].remote.release_source, ReleaseSource::ReleasePush);

    // In a search every release has a decision, and one for another movie
    // is the wrong movie.
    let criteria = MovieSearch {
        movie: lookup.movies[0].clone(),
        user_invoked: true,
        interactive: true,
    };
    let decisions = decide(&catalog, &lookup, &[good, noise], Some(&criteria), false);
    assert_eq!(decisions.len(), 2);
    assert!(decisions[0].approved());
    assert!(decisions[0].remote.movie_requested);
    assert_eq!(
        decisions[0].remote.release_source,
        ReleaseSource::InteractiveSearch
    );
    assert_eq!(reasons(&decisions[1]), vec![R::UnableToParse]);
    assert_eq!(decisions[1].rejections[0].message, "Unable to parse release");
}

#[test]
fn rejections_carry_the_originals_words() {
    let mut catalog = catalog();
    catalog.base.profiles = vec![profile(&[WEBDL1080P, BLURAY1080P], BLURAY1080P, true)];
    // The profile wants French; the movie is English.
    catalog.profile_languages.insert(1, lang::FRENCH);
    let mut lookup = library();
    {
        let m = &mut lookup.movies[0].movie;
        m.monitored = false;
        m.minimum_availability = MovieStatus::Released;
        m.metadata.in_cinemas = Some(now() - Duration::days(10));
    }
    let cam = release(
        "The.Holdovers.2023.KORSUB.HDCAM.x264-GRP",
        now() - Duration::hours(3),
    );
    let decisions = decide(&catalog, &lookup, &[cam], None, false);
    let messages: Vec<&str> = decisions[0]
        .rejections
        .iter()
        .map(|r| r.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "Hardcode subs found: KORSUB",
            "French is wanted, but found English",
            "CAM is not wanted in profile",
            "Movie [The Holdovers (2023)][tt0001001, 1001] will only be considered available 0 days after Released",
            "Movie is not monitored",
        ]
    );
    // A search the user asked for skips availability and monitoring.
    let criteria = MovieSearch {
        movie: lookup.movies[0].clone(),
        user_invoked: true,
        interactive: false,
    };
    let cam = release(
        "The.Holdovers.2023.KORSUB.HDCAM.x264-GRP",
        now() - Duration::hours(3),
    );
    let decisions = decide(&catalog, &lookup, &[cam], Some(&criteria), false);
    assert_eq!(
        reasons(&decisions[0]),
        vec![R::HardcodeSubtitles, R::WantedLanguage, R::QualityNotWanted]
    );
    assert_eq!(
        decisions[0].remote.release_source,
        ReleaseSource::UserInvokedSearch
    );
}

/// `PrioritizeDownloadDecisionFixture`: decisions for one movie.
fn decision(title: &str, quality_id: i64, edit: impl FnOnce(&mut RemoteMovie)) -> MovieDecision {
    let m = movie(1, "The Holdovers", 2023);
    let mut r = release(title, now() - Duration::hours(30));
    r.size = 8 * 1024 * 1024 * 1024;
    r.protocol = Protocol::Usenet;
    let mut remote = super::testing::remote(r, &m, quality(quality_id, 1));
    edit(&mut remote);
    MovieDecision {
        remote,
        rejections: Vec::new(),
    }
}

fn titles(decisions: &[MovieDecision]) -> Vec<&str> {
    decisions
        .iter()
        .map(|d| d.remote.release.title.as_str())
        .collect()
}

#[test]
fn the_best_release_of_a_movie_comes_first() {
    let catalog = catalog();
    // should_put_higher_quality_before_lower, should_order_by_custom_format_score.
    let ranked = prioritize(
        &catalog,
        vec![
            decision("hdtv", HDTV720P, |_| {}),
            decision("bluray", BLURAY1080P, |_| {}),
            decision("bluray-scored", BLURAY1080P, |r| r.custom_format_score = 10),
        ],
    );
    assert_eq!(titles(&ranked), ["bluray-scored", "bluray", "hdtv"]);
    // should_put_propers_before_non_propers
    let ranked = prioritize(
        &catalog,
        vec![
            decision("plain", BLURAY1080P, |_| {}),
            decision("proper", BLURAY1080P, |r| {
                r.parsed.as_mut().unwrap().quality.quality.version = 2
            }),
        ],
    );
    assert_eq!(titles(&ranked), ["proper", "plain"]);
    // should_prefer_usenet_over_torrent when usenet is the preferred
    // protocol, then the indexer of lower priority number.
    let ranked = prioritize(
        &catalog,
        vec![
            decision("torrent", BLURAY1080P, |r| r.release.protocol = Protocol::Torrent),
            decision("usenet-low", BLURAY1080P, |r| r.release.indexer_priority = 50),
            decision("usenet", BLURAY1080P, |_| {}),
        ],
    );
    assert_eq!(titles(&ranked), ["usenet", "usenet-low", "torrent"]);
    // should_order_by_youngest for usenet, in the original's steps.
    let aged = |title: &'static str, hours: i64| {
        decision(title, BLURAY1080P, move |r| {
            r.release.publish_date = now() - Duration::hours(hours)
        })
    };
    let ranked = prioritize(
        &catalog,
        vec![
            aged("month", 24 * 40),
            aged("week", 24 * 3),
            aged("minutes", 0),
            aged("day", 20),
        ],
    );
    assert_eq!(titles(&ranked), ["minutes", "day", "week", "month"]);
    // should_order_by_closest_to_preferred_size... without a preferred
    // size, the larger release; and torrents by the order of magnitude of
    // their seeders, then of their peers.
    let torrent = |title: &'static str, seeders: i64, peers: i64| {
        decision(title, BLURAY1080P, move |r| {
            r.release.protocol = Protocol::Torrent;
            r.release.seeders = Some(seeders);
            r.release.peers = Some(peers);
        })
    };
    let ranked = prioritize(
        &catalog,
        vec![
            torrent("ten", 10, 10),
            torrent("thousand", 1000, 10),
            torrent("ten-more-peers", 10, 400),
        ],
    );
    assert_eq!(titles(&ranked), ["thousand", "ten-more-peers", "ten"]);
    let small = decision("small", BLURAY1080P, |r| r.release.size = 1024 * 1024 * 1024);
    let large = decision("large", BLURAY1080P, |_| {});
    assert_eq!(compare(&catalog, &large, &small), std::cmp::Ordering::Greater);
}

#[test]
fn decisions_stay_grouped_by_movie_in_the_order_they_came() {
    let catalog = catalog();
    let other = |title: &'static str, quality_id: i64| {
        let mut d = decision(title, quality_id, |_| {});
        d.remote.movie = Some(movie(2, "Lamb", 2021));
        d
    };
    let mut unknown = decision("unknown", BLURAY1080P, |_| {});
    unknown.remote.movie = None;
    let ranked = prioritize(
        &catalog,
        vec![
            other("lamb-hdtv", HDTV720P),
            unknown,
            decision("holdovers", BLURAY1080P, |_| {}),
            other("lamb-bluray", BLURAY1080P),
        ],
    );
    assert_eq!(
        titles(&ranked),
        ["lamb-bluray", "lamb-hdtv", "holdovers", "unknown"]
    );
}

/// What stands in the way of a release the profile would otherwise take:
/// each rule's rejection, with the setting that brings it about.
mod what_the_library_already_has {
    use super::*;
    use crate::movies::decision::QueuedMovie;
    use crate::movies::model::{MovieBlocklistEntry, MovieFile, MovieHistoryEntry, MovieHistoryEvent};

    const TITLE: &str = "The.Holdovers.2023.1080p.BluRay.x264-GRP";

    fn wanted() -> crate::indexers::rss::ReleaseInfo {
        let mut r = release(TITLE, now() - Duration::hours(3));
        r.protocol = Protocol::Usenet;
        r.indexer = "Indexer".into();
        r.size = 8_000_000_000;
        r
    }

    fn file(quality_id: i64, group: &str) -> MovieFile {
        MovieFile {
            id: 5,
            movie_id: 1,
            relative_path: "The Holdovers (2023).mkv".into(),
            original_file_path: None,
            size: 8_000_000_000,
            date_added: now() - Duration::days(30),
            scene_name: None,
            release_group: Some(group.into()),
            edition: None,
            quality: quality(quality_id, 1),
            languages: vec![lang::ENGLISH],
            media_info: None,
            indexer_flags: 0,
        }
    }

    fn with_file(file: MovieFile) -> MemoryMovieLookup {
        let mut lookup = library();
        lookup.movies[0].movie.movie_file_id = file.id;
        lookup.files = vec![file];
        lookup
    }

    fn blocked(protocol: Protocol, hash: Option<&str>) -> MovieBlocklistEntry {
        MovieBlocklistEntry {
            id: 1,
            movie_id: 1,
            source_title: Some(TITLE.into()),
            quality: quality(BLURAY1080P, 1),
            date: now() - Duration::hours(1),
            published_date: Some(now() - Duration::hours(3)),
            size: Some(8_000_000_000),
            protocol,
            indexer: Some("Indexer".into()),
            message: Some("Manually marked as failed".into()),
            torrent_info_hash: hash.map(String::from),
            languages: vec![lang::ENGLISH],
            indexer_flags: 0,
        }
    }

    fn decide_one(
        catalog: &MovieCatalog,
        lookup: &MemoryMovieLookup,
        r: crate::indexers::rss::ReleaseInfo,
    ) -> MovieDecision {
        decide(catalog, lookup, &[r], None, false).remove(0)
    }

    #[test]
    fn nothing_in_the_way_is_an_approval() {
        let d = decide_one(&catalog(), &library(), wanted());
        assert!(d.approved(), "{:?}", d.rejections);
    }

    #[test]
    fn a_release_in_the_queue_that_meets_the_cutoff() {
        let m = movie(1, "The Holdovers", 2023);
        let queued = |failed_pending| QueuedMovie {
            remote: super::super::testing::remote(wanted(), &m, quality(BLURAY1080P, 1)),
            size: 8_000_000_000,
            failed_pending,
        };
        let mut c = catalog();
        c.queue = vec![queued(false)];
        let d = decide_one(&c, &library(), wanted());
        assert_eq!(reasons(&d), vec![R::QueueCutoffMet]);
        assert_eq!(
            d.rejections[0].message,
            "Quality for release in queue already meets cutoff: Bluray-1080p v1"
        );
        // One that failed and waits to be dealt with is not in the way.
        c.queue = vec![queued(true)];
        assert!(decide_one(&c, &library(), wanted()).approved());
        // A lesser one in the queue leaves room for the better.
        c.base.profiles = vec![profile(&[WEBDL1080P, BLURAY1080P], BLURAY1080P, true)];
        let mut lesser = queued(false);
        lesser.remote.parsed.as_mut().unwrap().quality.quality = quality(WEBDL1080P, 1);
        c.queue = vec![lesser];
        let d = decide_one(&c, &library(), wanted());
        assert!(d.approved(), "{:?}", d.rejections);
    }

    #[test]
    fn a_blocklisted_release() {
        let mut lookup = library();
        lookup.blocklist = vec![blocked(Protocol::Usenet, None)];
        let d = decide_one(&catalog(), &lookup, wanted());
        assert_eq!(reasons(&d), vec![R::Blocklisted]);
        assert_eq!(d.rejections[0].message, "Release is blocklisted");
        // Another release of the movie is not.
        let other = release(
            "The.Holdovers.2023.1080p.BluRay.x264-OTHER",
            now() - Duration::hours(3),
        );
        assert!(decide_one(&catalog(), &lookup, other).approved());

        // A torrent that names its hash is known by that hash alone: a row
        // without it does not block it, a row with it does.
        let mut torrent = wanted();
        torrent.protocol = Protocol::Torrent;
        torrent.info_hash = Some("ABCDEF0123456789ABCDEF0123456789ABCDEF01".into());
        torrent.seeders = Some(10);
        lookup.blocklist = vec![blocked(Protocol::Torrent, None)];
        assert!(decide_one(&catalog(), &lookup, torrent.clone()).approved());
        lookup.blocklist = vec![blocked(
            Protocol::Torrent,
            Some("abcdef0123456789abcdef0123456789abcdef01"),
        )];
        assert_eq!(
            reasons(&decide_one(&catalog(), &lookup, torrent)),
            vec![R::Blocklisted]
        );
    }

    #[test]
    fn a_disk_without_room() {
        let mut lookup = library();
        lookup.free_space = Some(1_000_000_000);
        let d = decide_one(&catalog(), &lookup, wanted());
        assert_eq!(reasons(&d), vec![R::MinimumFreeSpace]);
        assert_eq!(
            d.rejections[0].message,
            "Importing after download will exceed available disk space"
        );
        // Room for the release, not for the margin the settings ask for.
        lookup.free_space = Some(8_050_000_000);
        let d = decide_one(&catalog(), &lookup, wanted());
        assert_eq!(reasons(&d), vec![R::MinimumFreeSpace]);
        assert!(d.rejections[0].message.starts_with("Not enough free space ("));
        let mut c = catalog();
        c.base.config.skip_free_space_check_when_importing = true;
        assert!(decide_one(&c, &lookup, wanted()).approved());
    }

    /// `DeletedMovieFileSpecificationFixture` (Radarr 7): with deleted
    /// files unmonitoring their movie, a file gone from disk holds an RSS
    /// release back, as if the movie were unmonitored; a file still there,
    /// the setting off or a search let it through.
    #[test]
    fn a_file_deleted_from_disk_and_not_yet_scanned() {
        let dir = std::env::temp_dir().join(format!("unarr_deleted_file_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut c = catalog();
        c.base.profiles = vec![profile(&[WEBDL1080P, BLURAY1080P], BLURAY1080P, true)];
        let mut lookup = with_file(file(WEBDL1080P, "GRP"));
        lookup.movies[0].movie.path = dir.to_string_lossy().into_owned();
        assert!(decide_one(&c, &lookup, wanted()).approved(), "the setting off");
        c.auto_unmonitor_previously_downloaded_movies = true;
        let d = decide_one(&c, &lookup, wanted());
        assert_eq!(reasons(&d), vec![R::MovieNotMonitored]);
        assert_eq!(d.rejections[0].message, "Movie is not monitored");
        assert_eq!(d.rejections[0].kind, crate::decision::RejectionKind::Temporary);
        std::fs::write(dir.join("The Holdovers (2023).mkv"), b"x").unwrap();
        assert!(decide_one(&c, &lookup, wanted()).approved(), "the file is there");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_on_disk_that_is_as_good() {
        let lookup = with_file(file(BLURAY1080P, "GRP"));
        let d = decide_one(&catalog(), &lookup, wanted());
        assert_eq!(reasons(&d), vec![R::DiskCutoffMet]);
        // Below the cutoff the better release is an upgrade...
        let mut c = catalog();
        c.base.profiles = vec![profile(&[WEBDL1080P, BLURAY1080P], BLURAY1080P, true)];
        let lookup = with_file(file(WEBDL1080P, "GRP"));
        let d = decide_one(&c, &lookup, wanted());
        assert!(d.approved(), "{:?}", d.rejections);
        // ...unless the profile takes none.
        c.base.profiles = vec![profile(&[WEBDL1080P, BLURAY1080P], BLURAY1080P, false)];
        let d = decide_one(&c, &lookup, wanted());
        assert!(
            reasons(&d).contains(&R::QualityUpgradesDisabled),
            "{:?}",
            d.rejections
        );
    }

    #[test]
    fn a_grab_a_few_hours_ago() {
        let grab = |hours: i64, event: MovieHistoryEvent| MovieHistoryEntry {
            id: 1,
            movie_id: 1,
            source_title: Some(TITLE.into()),
            date: now() - Duration::hours(hours),
            event_type: event,
            quality: quality(BLURAY1080P, 1),
            languages: vec![lang::ENGLISH],
            data: serde_json::json!({}),
            download_id: Some("id".into()),
        };
        let mut lookup = library();
        lookup.history = vec![grab(2, MovieHistoryEvent::Grabbed)];
        let d = decide_one(&catalog(), &lookup, wanted());
        assert_eq!(reasons(&d), vec![R::HistoryRecentCutoffMet]);
        assert_eq!(
            d.rejections[0].message,
            "Recent grab event in history already meets cutoff: Bluray-1080p v1"
        );
        // Half a day later the grab no longer counts, nor does a history
        // whose last word is a failure, nor any history in a search.
        lookup.history = vec![grab(13, MovieHistoryEvent::Grabbed)];
        assert!(decide_one(&catalog(), &lookup, wanted()).approved());
        lookup.history = vec![grab(1, MovieHistoryEvent::DownloadFailed)];
        assert!(decide_one(&catalog(), &lookup, wanted()).approved());
        lookup.history = vec![grab(2, MovieHistoryEvent::Grabbed)];
        let criteria = MovieSearch {
            movie: lookup.movies[0].clone(),
            user_invoked: false,
            interactive: false,
        };
        let searched = decide(&catalog(), &lookup, &[wanted()], Some(&criteria), false).remove(0);
        assert!(searched.approved(), "{:?}", searched.rejections);
    }

    #[test]
    fn a_repack_of_another_group() {
        let mut c = catalog();
        c.base.profiles = vec![profile(&[BLURAY1080P], BLURAY1080P, true)];
        let repack = |group: &str| {
            let mut r = wanted();
            r.title = format!("The.Holdovers.2023.REPACK.1080p.BluRay.x264-{group}");
            r.guid = format!("guid-{group}");
            r
        };
        let lookup = with_file(file(BLURAY1080P, "GRP"));
        let d = decide_one(&c, &lookup, repack("OTHER"));
        assert!(
            reasons(&d).contains(&R::RepackReleaseGroupDoesNotMatch),
            "{:?}",
            d.rejections
        );
        let same = decide_one(&c, &lookup, repack("GRP"));
        assert!(
            !reasons(&same).contains(&R::RepackReleaseGroupDoesNotMatch),
            "{:?}",
            same.rejections
        );
        // A file whose group is not known cannot be matched.
        let lookup = with_file(file(BLURAY1080P, ""));
        let d = decide_one(&c, &lookup, repack("GRP"));
        assert!(
            reasons(&d).contains(&R::RepackUnknownReleaseGroup),
            "{:?}",
            d.rejections
        );
    }

    #[test]
    fn a_proper_of_a_file_that_is_not_new() {
        use crate::decision::lookup::ProperDownloadTypes;
        let proper = || {
            let mut r = wanted();
            r.title = "The.Holdovers.2023.PROPER.1080p.BluRay.x264-GRP".into();
            r.guid = "guid-proper".into();
            r
        };
        let aged = |days: i64| {
            let mut f = file(BLURAY1080P, "GRP");
            f.date_added = now() - Duration::days(days);
            with_file(f)
        };
        // A file imported a month ago is not replaced by its proper...
        let d = decide_one(&catalog(), &aged(30), proper());
        assert!(reasons(&d).contains(&R::ProperForOldFile), "{:?}", d.rejections);
        assert!(d.rejections.iter().any(|r| r.message == "Proper for old file"));
        // ...one imported this week is.
        let d = decide_one(&catalog(), &aged(2), proper());
        assert!(!reasons(&d).contains(&R::ProperForOldFile), "{:?}", d.rejections);
        // With propers not to be upgraded to, none is, old file or new.
        let mut c = catalog();
        c.base.config.download_propers_and_repacks = ProperDownloadTypes::DoNotUpgrade;
        let d = decide_one(&c, &aged(2), proper());
        assert!(reasons(&d).contains(&R::PropersDisabled), "{:?}", d.rejections);
        assert!(d
            .rejections
            .iter()
            .any(|r| r.message == "Proper downloading is disabled"));
        // A release that is no revision upgrade is not the rule's business,
        // nor is any release in a search.
        let d = decide_one(&c, &aged(30), wanted());
        assert!(!reasons(&d).contains(&R::PropersDisabled) && !reasons(&d).contains(&R::ProperForOldFile));
        let lookup = aged(30);
        let criteria = MovieSearch {
            movie: lookup.movies[0].clone(),
            user_invoked: false,
            interactive: false,
        };
        let searched = decide(&catalog(), &lookup, &[proper()], Some(&criteria), false).remove(0);
        assert!(
            !reasons(&searched).contains(&R::ProperForOldFile),
            "{:?}",
            searched.rejections
        );
    }

    #[test]
    fn a_release_already_grabbed_and_imported_as_something_else() {
        const HASH: &str = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
        let row = |id: i64, hours: i64, event: MovieHistoryEvent, quality_id: i64| MovieHistoryEntry {
            id,
            movie_id: 1,
            source_title: Some(TITLE.into()),
            date: now() - Duration::hours(hours),
            event_type: event,
            quality: quality(quality_id, 1),
            languages: vec![lang::ENGLISH],
            data: serde_json::json!({}),
            download_id: Some(HASH.into()),
        };
        // Grabbed as a Bluray, imported as the WEB-DL it turned out to be.
        let imported_as = |quality_id: i64| {
            let mut lookup = with_file(file(WEBDL1080P, "GRP"));
            lookup.history = vec![
                row(2, 20, MovieHistoryEvent::DownloadFolderImported, quality_id),
                row(1, 24, MovieHistoryEvent::Grabbed, BLURAY1080P),
            ];
            lookup
        };
        let mut c = catalog();
        c.base.profiles = vec![profile(&[WEBDL1080P, BLURAY1080P], BLURAY1080P, true)];
        let lookup = imported_as(WEBDL1080P);
        let d = decide_one(&c, &lookup, wanted());
        assert_eq!(reasons(&d), vec![R::AlreadyImportedSameName]);
        assert_eq!(
            d.rejections[0].message,
            "Has same release name as a grabbed and imported release"
        );
        // A torrent is known by its hash before its name.
        let mut torrent = wanted();
        torrent.title = "The.Holdovers.2023.1080p.BluRay.x264-OTHER".into();
        torrent.protocol = Protocol::Torrent;
        torrent.info_hash = Some(HASH.to_lowercase());
        torrent.seeders = Some(10);
        let d = decide_one(&c, &lookup, torrent);
        assert_eq!(reasons(&d), vec![R::AlreadyImportedSameHash]);
        assert_eq!(
            d.rejections[0].message,
            "Has same torrent hash as a grabbed and imported release"
        );
        // Another release of the movie is an upgrade like any other.
        let other = release(
            "The.Holdovers.2023.1080p.BluRay.x264-OTHER",
            now() - Duration::hours(3),
        );
        let d = decide_one(&c, &lookup, other);
        assert!(d.approved(), "{:?}", d.rejections);
        // Imported as what was grabbed, the rule has nothing to say (the
        // file on disk does).
        let d = decide_one(&c, &imported_as(BLURAY1080P), wanted());
        assert!(
            !reasons(&d).contains(&R::AlreadyImportedSameName),
            "{:?}",
            d.rejections
        );
        // Nor with completed download handling off.
        c.base.config.enable_completed_download_handling = false;
        let d = decide_one(&c, &lookup, wanted());
        assert!(
            !reasons(&d).contains(&R::AlreadyImportedSameName),
            "{:?}",
            d.rejections
        );
    }
}
