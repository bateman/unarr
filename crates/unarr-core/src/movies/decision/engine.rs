//! Port of Radarr's `DownloadDecisionMaker` and
//! `DownloadDecisionPriorizationService`: every release parsed, mapped to
//! a movie, given its languages, custom formats and score, run through the
//! specifications, and the decisions ranked best first within each movie.

use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::decision::lookup::{Catalog, ProperDownloadTypes};
use crate::decision::{megabytes, ReleaseSource};
use crate::indexers::rss::ReleaseInfo;
use crate::indexers::settings::NewznabSettings;
use crate::model::{CustomFormat, Protocol, ProviderKind, QualityDefinition};
use crate::movies::decision::library::MovieLookup;
use crate::movies::decision::mapping::find_movie;
use crate::movies::decision::specs::{self, Context, Priority, Subject};
use crate::movies::decision::{
    MovieDecision, MovieRejection, MovieRejectionReason as R, MovieSearch, QueuedMovie, RemoteMovie,
};
use crate::movies::model::{Movie, MovieFile, MovieHistoryEntry};
use crate::movies::parser::language::{lang, parse_languages};
use crate::movies::parser::{
    has_multiple_languages, normalize_movie_title, parse_movie_title, quality, ParsedMovieInfo,
};
use crate::movies::quality::RADARR_INDEXER_FLAGS;
use crate::store;

/// What the engine reads once per batch: the shared catalogue of profiles,
/// definitions, formats and settings, and what only Radarr has.
#[derive(Debug, Clone, Default)]
pub struct MovieCatalog {
    pub base: Catalog,
    /// The language each quality profile wants, by profile id.
    pub profile_languages: HashMap<i64, i64>,
    /// `RequiredFlags` of the torrent indexers that set any, by indexer id.
    pub required_flags: HashMap<i64, Vec<i64>>,
    pub allow_hardcoded_subs: bool,
    pub whitelisted_hardcoded_subs: String,
    /// Days after its release date before a movie counts as available.
    pub availability_delay: i64,
    pub prefer_indexer_flags: bool,
    /// `AutoUnmonitorPreviouslyDownloadedMovies`.
    pub auto_unmonitor_previously_downloaded_movies: bool,
    pub queue: Vec<QueuedMovie>,
}

impl MovieCatalog {
    pub fn load(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<MovieCatalog> {
        let text = |key: &str| store::config::config(conn, key);
        let flag = |key: &str| -> anyhow::Result<bool> {
            Ok(text(key)?.is_some_and(|v| v.eq_ignore_ascii_case("true")))
        };
        let required_flags = store::providers::list_providers(conn, ProviderKind::Indexer)?
            .iter()
            .map(|p| (p.id, NewznabSettings::from_value(&p.settings).required_flags))
            .filter(|(_, flags)| !flags.is_empty())
            .collect();
        Ok(MovieCatalog {
            base: Catalog::load(conn, now)?,
            profile_languages: crate::movies::store::profile_languages(conn)?,
            required_flags,
            allow_hardcoded_subs: flag("AllowHardcodedSubs")?,
            whitelisted_hardcoded_subs: text("WhitelistedHardcodedSubs")?.unwrap_or_default(),
            availability_delay: text("AvailabilityDelay")?
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0),
            prefer_indexer_flags: flag("PreferIndexerFlags")?,
            auto_unmonitor_previously_downloaded_movies: flag("AutoUnmonitorPreviouslyDownloadedMovies")?,
            queue: Vec::new(),
        })
    }

    pub fn with_queue(mut self, queue: Vec<QueuedMovie>) -> MovieCatalog {
        self.queue = queue;
        self
    }

    /// The stored definition of a quality. One that is not stored sets no
    /// size: Radarr writes a definition for every quality at start, so an
    /// imported installation has them all.
    pub fn definition(&self, quality_id: i64) -> QualityDefinition {
        self.base
            .definitions
            .iter()
            .find(|d| d.quality_id == quality_id)
            .cloned()
            .unwrap_or(QualityDefinition {
                id: 0,
                quality_id,
                title: crate::movies::quality::MovieQuality::by_id(quality_id)
                    .name
                    .to_string(),
                min_size: None,
                max_size: None,
                preferred_size: None,
            })
    }

    /// `QualityProfile.Language`; a profile without one wants any.
    pub fn profile_language(&self, profile_id: i64) -> i64 {
        self.profile_languages
            .get(&profile_id)
            .copied()
            .unwrap_or(lang::ANY)
    }
}

/// The custom formats of a mapped release.
pub fn remote_formats(catalog: &MovieCatalog, remote: &RemoteMovie, size: i64) -> Vec<CustomFormat> {
    match (&remote.parsed, &remote.movie) {
        (Some(parsed), Some(movie)) => crate::movies::formats::remote_formats(
            &catalog.base.formats,
            parsed,
            &remote.languages,
            size,
            remote.release.indexer_flags,
            movie,
        ),
        _ => Vec::new(),
    }
}

pub fn file_formats(catalog: &MovieCatalog, file: &MovieFile, movie: &Movie) -> Vec<CustomFormat> {
    crate::movies::formats::file_formats(&catalog.base.formats, file, movie)
}

pub fn history_formats(
    catalog: &MovieCatalog,
    history: &MovieHistoryEntry,
    movie: &Movie,
) -> Vec<CustomFormat> {
    crate::movies::formats::history_formats(&catalog.base.formats, history, movie)
}

/// `GetReleaseSource`.
fn release_source(pushed: bool, search: Option<&MovieSearch>) -> ReleaseSource {
    match search {
        None if pushed => ReleaseSource::ReleasePush,
        None => ReleaseSource::Rss,
        Some(s) if s.interactive => ReleaseSource::InteractiveSearch,
        Some(s) if s.user_invoked => ReleaseSource::UserInvokedSearch,
        Some(_) => ReleaseSource::Search,
    }
}

/// The number Radarr's feed parsers keep of an IMDb id, as text: what
/// `report.ImdbId.ToString()` hands the mapping (`0` when there is none).
fn imdb_number(release: &ReleaseInfo) -> String {
    release
        .imdb_id
        .as_deref()
        .and_then(|id| id.trim_start_matches("tt").parse::<i64>().ok())
        .unwrap_or(0)
        .to_string()
}

/// `AggregateLanguages`: the languages of a mapped release. Those the
/// indexer gave win; else the parsed ones, without those that only the
/// movie's own title put there; a `MULTi` release takes the indexer's
/// languages; none at all means the movie's original language, which also
/// stands in for `Original`.
pub fn aggregate_languages(catalog: &MovieCatalog, remote: &RemoteMovie, movie: &Movie) -> Vec<i64> {
    let Some(parsed) = &remote.parsed else {
        return remote.languages.clone();
    };
    let release = &remote.release;
    let mut languages = parsed.languages.clone();
    if !release.languages.is_empty() {
        languages = release.languages.clone();
    } else {
        let mut tokens = if parsed.simple_release_title.is_empty() {
            parsed.release_title.clone()
        } else {
            parsed.simple_release_title.clone()
        };
        let normalized_tokens = normalize_movie_title(&tokens);
        let mut to_remove = Vec::new();
        let title_languages = parse_languages(&movie.metadata.title);
        if !title_languages.contains(&lang::UNKNOWN) {
            let normalized_title = normalize_movie_title(&movie.metadata.title);
            // The title is looked for in the normalised name and cut out of
            // the name as it is, at that position, as the original does.
            if let Some(index) = normalized_tokens
                .to_lowercase()
                .find(&normalized_title.to_lowercase())
            {
                let start = normalized_tokens[..index].chars().count();
                let length = normalized_title.chars().count();
                let chars: Vec<char> = tokens.chars().collect();
                if start + length <= chars.len() {
                    tokens = chars[..start].iter().chain(&chars[start + length..]).collect();
                }
                to_remove.extend(title_languages);
            }
        }
        // What is still in the name stays.
        let still_there = parse_languages(&tokens);
        to_remove.retain(|l| !still_there.contains(l));
        languages.retain(|l| !to_remove.contains(l));
    }

    if !release.title.trim().is_empty() {
        let indexer = catalog
            .base
            .indexer(release.indexer_id)
            .filter(|_| release.indexer_id > 0)
            .or_else(|| {
                Some(release.indexer.as_str())
                    .filter(|name| !name.trim().is_empty())
                    .and_then(|name| catalog.base.indexer_by_name(name))
            });
        if let Some(indexer) = indexer {
            if !indexer.multi_languages.is_empty() && has_multiple_languages(&release.title) {
                if languages.is_empty() || languages == [lang::UNKNOWN] {
                    languages = indexer.multi_languages.clone();
                } else {
                    for l in &indexer.multi_languages {
                        if !languages.contains(l) {
                            languages.push(*l);
                        }
                    }
                }
            }
        }
    }

    let original = movie.metadata.original_language_id;
    if languages.is_empty() || languages == [lang::UNKNOWN] {
        languages = vec![original];
    }
    if let Some(position) = languages.iter().position(|l| *l == lang::ORIGINAL) {
        languages.remove(position);
        if languages.contains(&original) {
            languages.push(lang::UNKNOWN);
        } else {
            languages.push(original);
        }
    }
    languages
}

/// `GetRssDecision` and `GetSearchDecision`: one decision per release that
/// parses, or, in a search, per release at all.
pub fn decide(
    catalog: &MovieCatalog,
    lookup: &dyn MovieLookup,
    releases: &[ReleaseInfo],
    search: Option<&MovieSearch>,
    pushed: bool,
) -> Vec<MovieDecision> {
    let source = release_source(pushed, search);
    releases
        .iter()
        .filter_map(|release| decide_one(catalog, lookup, release, search, pushed, source))
        .collect()
}

fn decide_one(
    catalog: &MovieCatalog,
    lookup: &dyn MovieLookup,
    release: &ReleaseInfo,
    search: Option<&MovieSearch>,
    pushed: bool,
    source: ReleaseSource,
) -> Option<MovieDecision> {
    let mut remote = RemoteMovie::new(release.clone());
    remote.release_source = source;
    let parsed = parse_movie_title(&release.title, false);
    let titled = parsed
        .as_ref()
        .is_some_and(|p| !p.primary_movie_title().trim().is_empty());
    let mut decision = None;

    if let (true, Some(parsed)) = (titled, parsed.as_ref()) {
        let found = find_movie(lookup, parsed, &imdb_number(release), release.tmdb_id, search);
        remote.parsed = Some(parsed.clone());
        remote.languages = parsed.languages.clone();
        match found {
            Err(error) => {
                // More than one movie answers to the title: the original
                // throws, and the release comes out as an error.
                tracing::error!("Couldn't process release {}: {error}", release.title);
                let mut failed = RemoteMovie::new(release.clone());
                failed.release_source = source;
                return Some(MovieDecision {
                    remote: failed,
                    rejections: vec![MovieRejection::permanent(
                        R::Error,
                        "Unexpected error processing release",
                    )],
                });
            }
            Ok(None) => {
                if search.is_some() {
                    remote.movie_requested = false;
                }
                decision = Some(MovieDecision {
                    remote: remote.clone(),
                    rejections: vec![MovieRejection::permanent(
                        R::UnknownMovie,
                        if pushed {
                            "Unknown Movie. Unable to match to existing movie in Library using release title."
                        } else {
                            "Unknown Movie. Unable to match to correct movie using release title."
                        },
                    )],
                });
            }
            Ok(Some((movie, match_type))) => {
                remote.match_type = match_type;
                if let Some(search) = search {
                    remote.movie_requested = movie.id == search.movie.movie.id;
                }
                remote.languages = aggregate_languages(catalog, &remote, &movie);
                remote.movie = Some(movie.clone());
                remote.custom_formats = remote_formats(catalog, &remote, release.size);
                let profile = catalog.base.profile(movie.quality_profile_id);
                remote.custom_format_score = profile
                    .map(|p| crate::decision::upgradable::format_score(p, &remote.custom_formats))
                    .unwrap_or(0);
                remote.download_allowed = true;
                let rejections = match profile {
                    Some(profile) => run_specifications(catalog, lookup, &remote, &movie, profile, search),
                    None => vec![MovieRejection::permanent(
                        R::DecisionError,
                        "Movie has no quality profile",
                    )],
                };
                decision = Some(MovieDecision {
                    remote: remote.clone(),
                    rejections,
                });
            }
        }
    }

    if search.is_some() && !titled {
        let parsed = parsed.unwrap_or_else(|| ParsedMovieInfo {
            movie_titles: Vec::new(),
            original_title: String::new(),
            release_title: String::new(),
            simple_release_title: String::new(),
            quality: quality::parse_quality(&release.title),
            languages: parse_languages(&release.title),
            release_group: None,
            release_hash: String::new(),
            edition: String::new(),
            year: 0,
            imdb_id: String::new(),
            tmdb_id: 0,
            hardcoded_subs: None,
        });
        let mut unparsed = RemoteMovie::new(release.clone());
        unparsed.release_source = source;
        unparsed.languages = parsed.languages.clone();
        unparsed.parsed = Some(parsed);
        decision = Some(MovieDecision {
            remote: unparsed,
            rejections: vec![MovieRejection::permanent(
                R::UnableToParse,
                "Unable to parse release",
            )],
        });
    }
    decision
}

/// `GetDecisionForReport`: the specifications by priority; a level that
/// rejects ends it.
fn run_specifications(
    catalog: &MovieCatalog,
    lookup: &dyn MovieLookup,
    remote: &RemoteMovie,
    movie: &Movie,
    profile: &crate::model::QualityProfile,
    search: Option<&MovieSearch>,
) -> Vec<MovieRejection> {
    let ctx = Context {
        catalog,
        lookup,
        now: catalog.base.now,
    };
    let subject = Subject {
        remote,
        movie,
        profile,
        search,
    };
    for priority in [Priority::Default, Priority::Disk] {
        let rejections: Vec<MovieRejection> = specs::SPECIFICATIONS
            .iter()
            .filter(|(_, p, _)| *p == priority)
            .filter_map(|(_, _, spec)| spec(&ctx, &subject))
            .collect();
        if !rejections.is_empty() {
            return rejections;
        }
    }
    Vec::new()
}

/// `PrioritizeDecisionsForMovies`: within each movie, best first; decisions
/// without a movie keep their order at the end.
pub fn prioritize(catalog: &MovieCatalog, decisions: Vec<MovieDecision>) -> Vec<MovieDecision> {
    let (with_movie, without): (Vec<MovieDecision>, Vec<MovieDecision>) =
        decisions.into_iter().partition(|d| d.remote.movie.is_some());
    let id = |d: &MovieDecision| d.remote.movie.as_ref().map(|m| m.id).unwrap_or(0);
    // Group by movie id in order of first appearance, as `GroupBy` does.
    let mut order: Vec<i64> = Vec::new();
    for d in &with_movie {
        if !order.contains(&id(d)) {
            order.push(id(d));
        }
    }
    let mut out = Vec::new();
    for movie_id in order {
        let mut group: Vec<MovieDecision> =
            with_movie.iter().filter(|d| id(d) == movie_id).cloned().collect();
        // OrderByDescending is stable: ties keep their order.
        group.sort_by(|a, b| compare(catalog, b, a));
        out.extend(group);
    }
    out.extend(without);
    out
}

type Comparer = fn(&MovieCatalog, &MovieDecision, &MovieDecision) -> Ordering;

/// `DownloadDecisionComparer.Compare`: greater is better.
pub fn compare(catalog: &MovieCatalog, x: &MovieDecision, y: &MovieDecision) -> Ordering {
    let comparers: [Comparer; 8] = [
        compare_quality,
        compare_custom_format_score,
        compare_protocol,
        compare_indexer_priority,
        compare_indexer_flags,
        compare_peers_if_torrent,
        compare_age_if_usenet,
        compare_size,
    ];
    comparers
        .iter()
        .map(|c| c(catalog, x, y))
        .find(|result| *result != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}

fn movie_of(d: &MovieDecision) -> &Movie {
    d.remote
        .movie
        .as_ref()
        .expect("a prioritised decision has a movie")
}

fn quality_of(d: &MovieDecision) -> crate::model::QualityModel {
    d.remote
        .parsed
        .as_ref()
        .map(|p| p.quality.quality)
        .unwrap_or(crate::model::QualityModel {
            quality_id: 0,
            version: 1,
            real: 0,
            is_repack: false,
        })
}

fn compare_quality(catalog: &MovieCatalog, x: &MovieDecision, y: &MovieDecision) -> Ordering {
    let index = |d: &MovieDecision| {
        catalog
            .base
            .profile(movie_of(d).quality_profile_id)
            .map(|p| crate::profile::index_of(p, quality_of(d).quality_id))
            .unwrap_or(0)
    };
    let by_index = index(x).cmp(&index(y));
    if catalog.base.config.download_propers_and_repacks == ProperDownloadTypes::DoNotPrefer {
        return by_index;
    }
    by_index.then_with(|| crate::decision::upgradable::compare_revision(&quality_of(x), &quality_of(y)))
}

fn compare_custom_format_score(_c: &MovieCatalog, x: &MovieDecision, y: &MovieDecision) -> Ordering {
    x.remote.custom_format_score.cmp(&y.remote.custom_format_score)
}

fn compare_protocol(catalog: &MovieCatalog, x: &MovieDecision, y: &MovieDecision) -> Ordering {
    let preferred = |d: &MovieDecision| {
        catalog
            .base
            .delay_profile_for(&movie_of(d).tags)
            .is_some_and(|p| p.preferred_protocol == d.remote.release.protocol)
    };
    preferred(x).cmp(&preferred(y))
}

fn compare_indexer_priority(_c: &MovieCatalog, x: &MovieDecision, y: &MovieDecision) -> Ordering {
    y.remote
        .release
        .indexer_priority
        .cmp(&x.remote.release.indexer_priority)
}

/// `ScoreFlags`: two for a flag that means a better deal or a vetted
/// release, one for half leech.
fn score_flags(flags: i64) -> i64 {
    RADARR_INDEXER_FLAGS
        .iter()
        .filter(|(bit, _)| flags & bit == *bit)
        .map(|(_, name)| match *name {
            "G_DoubleUpload" | "G_Freeleech" | "PTP_Approved" | "PTP_Golden" | "G_Internal" => 2,
            "G_Halfleech" => 1,
            _ => 0,
        })
        .sum()
}

fn compare_indexer_flags(catalog: &MovieCatalog, x: &MovieDecision, y: &MovieDecision) -> Ordering {
    if !catalog.prefer_indexer_flags {
        return Ordering::Equal;
    }
    score_flags(x.remote.release.indexer_flags).cmp(&score_flags(y.remote.release.indexer_flags))
}

fn log_bucket(n: Option<i64>) -> i64 {
    match n {
        Some(n) if n > 0 => (n as f64).log10().round() as i64,
        _ => 0,
    }
}

fn compare_peers_if_torrent(_c: &MovieCatalog, x: &MovieDecision, y: &MovieDecision) -> Ordering {
    let (rx, ry) = (&x.remote.release, &y.remote.release);
    if rx.protocol != Protocol::Torrent || ry.protocol != Protocol::Torrent {
        return Ordering::Equal;
    }
    log_bucket(rx.seeders)
        .cmp(&log_bucket(ry.seeders))
        .then_with(|| log_bucket(rx.peers).cmp(&log_bucket(ry.peers)))
}

fn compare_age_if_usenet(catalog: &MovieCatalog, x: &MovieDecision, y: &MovieDecision) -> Ordering {
    let (rx, ry) = (&x.remote.release, &y.remote.release);
    if rx.protocol != Protocol::Usenet || ry.protocol != Protocol::Usenet {
        return Ordering::Equal;
    }
    let bucket = |release: &ReleaseInfo| -> f64 {
        let age = catalog.base.now - release.publish_date;
        let hours = age.num_milliseconds() as f64 / 3_600_000.0;
        let days = age.num_days();
        if hours < 1.0 {
            1000.0
        } else if hours <= 24.0 {
            100.0
        } else if days <= 7 {
            10.0
        } else {
            -(days as f64).log10().round()
        }
    };
    bucket(rx).partial_cmp(&bucket(ry)).unwrap_or(Ordering::Equal)
}

/// `long.Round(level)`: to the nearest multiple of `level`.
fn round_to(value: i64, level: i64) -> i64 {
    ((value as f64 / level as f64).round() as i64) * level
}

fn compare_size(catalog: &MovieCatalog, x: &MovieDecision, y: &MovieDecision) -> Ordering {
    let key = |d: &MovieDecision| {
        let runtime = movie_of(d).metadata.runtime;
        let preferred = catalog.definition(quality_of(d).quality_id).preferred_size;
        let two_hundred = megabytes(200);
        match preferred {
            Some(preferred) if runtime > 0 => {
                let preferred_size = (runtime as f64 * preferred * 1024.0 * 1024.0) as i64;
                -round_to(d.remote.release.size - preferred_size, two_hundred).abs()
            }
            _ => round_to(d.remote.release.size, two_hundred),
        }
    };
    key(x).cmp(&key(y))
}
