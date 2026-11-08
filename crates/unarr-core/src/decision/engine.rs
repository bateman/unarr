//! `DownloadDecisionMaker` and `DownloadDecisionPriorizationService`.

use std::cmp::Ordering;

use chrono::{DateTime, Utc};

use crate::decision::lookup::{Catalog, Lookup, ProperDownloadTypes};
use crate::decision::specs::{self, Context, Priority, Subject, SPECIFICATIONS};
use crate::decision::{
    formats, mapping, scene, Decision, Rejection, RejectionReason, ReleaseSource, RemoteEpisode,
    SearchCriteria, SeedConfiguration,
};
use crate::indexers::rss::ReleaseInfo;
use crate::model::{Protocol, SeriesType};
use crate::parser;
use crate::profile;

/// What the decisions are made for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecisionInput<'a> {
    Rss,
    /// `POST /release/push`.
    Push,
    Search(&'a SearchCriteria),
}

/// `GetRssDecision` / `GetSearchDecision`: one decision per release, in
/// the order the releases came.
pub fn decide(
    catalog: &Catalog,
    lookup: &dyn Lookup,
    now: DateTime<Utc>,
    releases: Vec<ReleaseInfo>,
    input: DecisionInput<'_>,
) -> Vec<Decision> {
    let search = match input {
        DecisionInput::Search(s) => Some(s),
        _ => None,
    };
    let ctx = Context { catalog, lookup, now };
    if releases.is_empty() {
        tracing::info!("No results found");
    } else {
        tracing::info!(count = releases.len(), "Processing releases");
    }
    let total = releases.len();
    let mut decisions = Vec::with_capacity(total);
    for (i, report) in releases.into_iter().enumerate() {
        tracing::trace!(n = i + 1, total, "processing release");
        tracing::debug!(
            title = report.title,
            indexer = report.indexer,
            "processing release"
        );
        let mut decision = decide_one(&ctx, report, search);
        decision.remote.release_source = match input {
            DecisionInput::Push => ReleaseSource::ReleasePush,
            DecisionInput::Rss => ReleaseSource::Rss,
            DecisionInput::Search(s) => {
                if s.interactive_search {
                    ReleaseSource::InteractiveSearch
                } else if s.user_invoked_search {
                    ReleaseSource::UserInvokedSearch
                } else {
                    ReleaseSource::Search
                }
            }
        };
        if decision.rejections.is_empty() {
            tracing::debug!(title = decision.remote.release.title, "release accepted");
        } else {
            let reasons: Vec<String> = decision.rejections.iter().map(|r| r.to_string()).collect();
            tracing::debug!(
                title = decision.remote.release.title,
                reasons = reasons.join(", "),
                "release rejected"
            );
        }
        decisions.push(decision);
    }
    decisions
}

fn decide_one(ctx: &Context<'_>, report: ReleaseInfo, search: Option<&SearchCriteria>) -> Decision {
    match try_decide(ctx, report.clone(), search) {
        Ok(Some(decision)) => decision,
        Ok(None) => {
            // Unparsable and no search: the originals yield nothing, but a
            // caller wants one decision per release, so it is an unparsed one.
            let mut remote = RemoteEpisode::new(report);
            remote.languages = Vec::new();
            Decision {
                remote,
                rejections: vec![Rejection::permanent(
                    RejectionReason::UnableToParse,
                    "Unable to parse release",
                )],
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "couldn't process release");
            Decision {
                remote: RemoteEpisode::new(report),
                rejections: vec![Rejection::permanent(
                    RejectionReason::Error,
                    "Unexpected error processing release",
                )],
            }
        }
    }
}

fn try_decide(
    ctx: &Context<'_>,
    report: ReleaseInfo,
    search: Option<&SearchCriteria>,
) -> Result<Option<Decision>, mapping::MappingError> {
    let today = ctx.now.date_naive();
    let mut parsed = parser::parse_title_at(&report.title, today);
    if parsed.as_ref().is_none_or(|p| p.is_possible_special_episode()) {
        let special = mapping::parse_special_episode_title(
            ctx.lookup,
            parsed.as_ref(),
            &report.title,
            report.tvdb_id,
            report.tv_rage_id,
            report.imdb_id.as_deref(),
            search,
        )?;
        if special.is_some() {
            parsed = special;
        }
    }
    let mut decision: Option<Decision> = None;
    if let Some(p) = parsed.as_ref().filter(|p| !p.series_title.trim().is_empty()) {
        let mut remote = mapping::map(ctx.lookup, report.clone(), p.clone(), search)?;
        if remote.series.is_none() {
            let matching =
                scene::find_tvdb_id(ctx.lookup, &p.series_title, &p.release_title, p.season_number)?;
            let rejection = match matching {
                Some(tvdb_id) => Rejection::permanent(
                    RejectionReason::MatchesAnotherSeries,
                    format!(
                        "{} matches an alias for series with TVDB ID: {tvdb_id}",
                        p.series_title
                    ),
                ),
                None => Rejection::permanent(RejectionReason::UnknownSeries, "Unknown Series"),
            };
            decision = Some(Decision {
                remote,
                rejections: vec![rejection],
            });
        } else if remote.episodes.is_empty() {
            decision = Some(Decision {
                remote,
                rejections: vec![Rejection::permanent(
                    RejectionReason::UnknownEpisode,
                    "Unable to identify correct episode(s) using release name and scene mappings",
                )],
            });
        } else {
            augment(ctx, &mut remote);
            remote.custom_formats = formats::release_formats(ctx.catalog, &remote, remote.release.size);
            let profile = remote
                .series
                .as_ref()
                .and_then(|s| ctx.catalog.profile(s.quality_profile_id));
            let ids: Vec<i64> = remote.custom_formats.iter().map(|f| f.id).collect();
            remote.custom_format_score = profile
                .map(|p| profile::custom_format_score(p, &ids))
                .unwrap_or(0);
            tracing::trace!(
                score = remote.custom_format_score,
                formats = formats::names(&remote.custom_formats),
                title = report.title,
                "custom format score calculated"
            );
            remote.download_allowed = !remote.episodes.is_empty();
            decision = Some(decision_for(ctx, remote, search));
        }
    }
    if let Some(search) = search {
        if decision.is_none() {
            let parsed = parsed.unwrap_or_else(|| parser::title::ParsedEpisodeInfo {
                languages: parser::language::parse_languages(&report.title),
                quality: parser::quality::parse_quality(&report.title),
                ..Default::default()
            });
            if parsed.series_title.trim().is_empty() {
                let mut remote = RemoteEpisode::new(report);
                remote.languages = parsed.languages.clone();
                remote.parsed = Some(parsed);
                let _ = search;
                return Ok(Some(Decision {
                    remote,
                    rejections: vec![Rejection::permanent(
                        RejectionReason::UnableToParse,
                        "Unable to parse release",
                    )],
                }));
            }
        }
    }
    Ok(decision)
}

/// `RemoteEpisodeAggregationService.Augment`: the languages, as
/// `AggregateLanguages` settles them, and the seed configuration.
fn augment(ctx: &Context<'_>, remote: &mut RemoteEpisode) {
    aggregate_languages(ctx, remote);
    remote.seed_configuration = seed_configuration(ctx, remote);
}

/// `RemoteEpisodeAggregationService.Augment` plus the custom formats: what
/// a release found in a download client (rather than on an indexer) gets
/// filled in before it is shown in the queue.
pub fn augment_tracked(
    catalog: &Catalog,
    lookup: &dyn Lookup,
    now: DateTime<Utc>,
    remote: &mut RemoteEpisode,
    size: i64,
) {
    let ctx = Context { catalog, lookup, now };
    augment(&ctx, remote);
    remote.custom_formats = formats::release_formats(catalog, remote, size);
    remote.custom_format_score = remote
        .series
        .as_ref()
        .and_then(|s| catalog.profile(s.quality_profile_id))
        .map(|p| crate::decision::upgradable::format_score(p, &remote.custom_formats))
        .unwrap_or(0);
}

fn aggregate_languages(ctx: &Context<'_>, remote: &mut RemoteEpisode) {
    use crate::parser::language::lang;
    let parsed = remote.parsed_or_default().into_owned();
    let mut languages = parsed.languages.clone();
    let Some(series) = remote.series.clone() else {
        remote.languages = if !remote.release.languages.is_empty() {
            remote.release.languages.clone()
        } else {
            languages
        };
        return;
    };
    let release_tokens = if parsed.release_tokens.is_empty() {
        parsed.release_title.clone()
    } else {
        parsed.release_tokens.clone()
    };
    if !remote.release.languages.is_empty() {
        languages = remote.release.languages.clone();
    } else {
        let normalized_release_tokens = parser::normalize_episode_title(&release_tokens);
        let mut tokens = release_tokens.clone();
        let mut to_remove: Vec<i64> = Vec::new();
        for episode in &remote.episodes {
            let title = episode.title.clone().unwrap_or_default();
            let episode_languages = parser::language::parse_languages(&title);
            if episode_languages.contains(&lang::UNKNOWN) {
                continue;
            }
            let normalized_title = parser::normalize_episode_title(&title);
            if let Some(index) = normalized_release_tokens
                .to_lowercase()
                .find(&normalized_title.to_lowercase())
            {
                if index + normalized_title.len() <= tokens.len() && tokens.is_char_boundary(index) {
                    let end = (index + normalized_title.len()).min(tokens.len());
                    if tokens.is_char_boundary(end) {
                        tokens.replace_range(index..end, "");
                    }
                }
                to_remove.extend(episode_languages);
            }
        }
        let still_there = parser::language::parse_languages(&tokens);
        to_remove.retain(|l| !still_there.contains(l));
        languages.retain(|l| !to_remove.contains(l));
    }
    if !remote.release.title.trim().is_empty() {
        let indexer = ctx
            .catalog
            .indexer(remote.release.indexer_id)
            .or_else(|| ctx.catalog.indexer_by_name(&remote.release.indexer));
        if let Some(indexer) = indexer {
            if !indexer.multi_languages.is_empty()
                && parser::language::has_multiple_languages(&remote.release.title)
            {
                if languages.is_empty() || (languages.len() == 1 && languages[0] == lang::UNKNOWN) {
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
    if languages.is_empty() || (languages.len() == 1 && languages[0] == lang::UNKNOWN) {
        languages = vec![series.original_language_id];
        tracing::debug!(
            language = series.original_language_id,
            "language couldn't be parsed from release, fallback to series original language"
        );
    }
    if languages.contains(&lang::ORIGINAL) {
        languages.retain(|l| *l != lang::ORIGINAL);
        if !languages.contains(&series.original_language_id) {
            languages.push(series.original_language_id);
        } else {
            languages.push(lang::UNKNOWN);
        }
    }
    remote.languages = languages;
}

/// `SeedConfigProvider.GetSeedConfiguration`.
fn seed_configuration(ctx: &Context<'_>, remote: &RemoteEpisode) -> Option<SeedConfiguration> {
    if remote.release.protocol != Protocol::Torrent || remote.release.indexer_id == 0 {
        return None;
    }
    let indexer = ctx.catalog.indexer(remote.release.indexer_id)?;
    let full_season = remote.parsed.as_ref().is_some_and(|p| p.full_season);
    let seed_time = if full_season {
        indexer.season_pack_seed_time
    } else {
        indexer.seed_time
    };
    Some(SeedConfiguration {
        ratio: indexer.seed_ratio,
        seed_time_minutes: seed_time,
    })
}

/// `GetDecisionForReport`: the specifications by priority, stopping at the
/// first priority that rejects.
fn decision_for(ctx: &Context<'_>, remote: RemoteEpisode, search: Option<&SearchCriteria>) -> Decision {
    let series = remote.series.clone().expect("a decision needs a series");
    let fallback_profile;
    let profile = match ctx.catalog.profile(series.quality_profile_id) {
        Some(p) => p,
        None => {
            fallback_profile = crate::model::QualityProfile {
                id: 0,
                name: String::new(),
                cutoff: 0,
                upgrade_allowed: false,
                items: Vec::new(),
                format_items: Vec::new(),
                min_format_score: 0,
                cutoff_format_score: 0,
                min_upgrade_format_score: 0,
            };
            &fallback_profile
        }
    };
    let subject = Subject {
        remote: &remote,
        series: &series,
        profile,
        search,
    };
    let mut rejections = Vec::new();
    for priority in [Priority::Default, Priority::Disk] {
        for (name, p, spec) in SPECIFICATIONS {
            if *p != priority {
                continue;
            }
            if let Some(rejection) = spec(ctx, &subject) {
                tracing::trace!(spec = name, reason = rejection.message, "specification rejected");
                rejections.push(rejection);
            }
        }
        if !rejections.is_empty() {
            break;
        }
    }
    Decision { remote, rejections }
}

/// `PrioritizeDecisions`: within each series, best first; decisions without
/// a series keep their order at the end.
pub fn prioritize(catalog: &Catalog, now: DateTime<Utc>, decisions: Vec<Decision>) -> Vec<Decision> {
    let mut with_series: Vec<Decision> = Vec::new();
    let mut without: Vec<Decision> = Vec::new();
    for d in decisions {
        if d.remote.series.is_some() {
            with_series.push(d);
        } else {
            without.push(d);
        }
    }
    // Group by series id in order of first appearance, as `GroupBy` does.
    let mut order: Vec<i64> = Vec::new();
    for d in &with_series {
        let id = d.remote.series.as_ref().unwrap().id;
        if !order.contains(&id) {
            order.push(id);
        }
    }
    let mut out = Vec::new();
    for id in order {
        let mut group: Vec<Decision> = with_series
            .iter()
            .filter(|d| d.remote.series.as_ref().unwrap().id == id)
            .cloned()
            .collect();
        // OrderByDescending is stable: ties keep their order.
        group.sort_by(|a, b| compare(catalog, now, b, a));
        out.extend(group);
    }
    out.extend(without);
    out
}

type Comparer = fn(&Catalog, DateTime<Utc>, &Decision, &Decision) -> Ordering;

/// `DownloadDecisionComparer.Compare`: greater is better.
pub fn compare(catalog: &Catalog, now: DateTime<Utc>, x: &Decision, y: &Decision) -> Ordering {
    let comparers: [Comparer; 9] = [
        compare_quality,
        compare_custom_format_score,
        compare_protocol,
        compare_episode_count,
        compare_episode_number,
        compare_indexer_priority,
        compare_peers_if_torrent,
        compare_age_if_usenet,
        compare_size,
    ];
    for c in comparers {
        let result = c(catalog, now, x, y);
        if result != Ordering::Equal {
            return result;
        }
    }
    Ordering::Equal
}

fn quality_index(catalog: &Catalog, d: &Decision) -> usize {
    let series = d.remote.series.as_ref().unwrap();
    let quality = d.remote.parsed_or_default().quality.quality.quality_id;
    catalog
        .profile(series.quality_profile_id)
        .map(|p| profile::index_of(p, quality))
        .unwrap_or(0)
}

fn compare_quality(catalog: &Catalog, _now: DateTime<Utc>, x: &Decision, y: &Decision) -> Ordering {
    let by_index = quality_index(catalog, x).cmp(&quality_index(catalog, y));
    if catalog.config.download_propers_and_repacks == ProperDownloadTypes::DoNotPrefer {
        return by_index;
    }
    by_index.then_with(|| {
        crate::decision::upgradable::compare_revision(
            &x.remote.parsed_or_default().quality.quality,
            &y.remote.parsed_or_default().quality.quality,
        )
    })
}

fn compare_custom_format_score(_c: &Catalog, _now: DateTime<Utc>, x: &Decision, y: &Decision) -> Ordering {
    x.remote.custom_format_score.cmp(&y.remote.custom_format_score)
}

fn compare_protocol(catalog: &Catalog, _now: DateTime<Utc>, x: &Decision, y: &Decision) -> Ordering {
    let preferred = |d: &Decision| {
        let series = d.remote.series.as_ref().unwrap();
        catalog
            .delay_profile_for(&series.tags)
            .is_some_and(|p| p.preferred_protocol == d.remote.release.protocol)
    };
    preferred(x).cmp(&preferred(y))
}

fn compare_episode_count(_c: &Catalog, _now: DateTime<Utc>, x: &Decision, y: &Decision) -> Ordering {
    let full = |d: &Decision| d.remote.parsed_or_default().full_season;
    let season_pack = full(x).cmp(&full(y));
    if season_pack != Ordering::Equal {
        return season_pack;
    }
    let anime = |d: &Decision| d.remote.series.as_ref().unwrap().series_type == SeriesType::Anime;
    if anime(x) && anime(y) {
        return x.remote.episodes.len().cmp(&y.remote.episodes.len());
    }
    y.remote.episodes.len().cmp(&x.remote.episodes.len())
}

fn compare_episode_number(_c: &Catalog, _now: DateTime<Utc>, x: &Decision, y: &Decision) -> Ordering {
    let min = |d: &Decision| {
        d.remote
            .episodes
            .iter()
            .map(|e| e.episode_number)
            .min()
            .unwrap_or(0)
    };
    min(y).cmp(&min(x))
}

fn compare_indexer_priority(_c: &Catalog, _now: DateTime<Utc>, x: &Decision, y: &Decision) -> Ordering {
    y.remote
        .release
        .indexer_priority
        .cmp(&x.remote.release.indexer_priority)
}

fn log_bucket(n: Option<i64>) -> i64 {
    match n {
        Some(n) if n > 0 => (n as f64).log10().round() as i64,
        _ => 0,
    }
}

fn compare_peers_if_torrent(_c: &Catalog, _now: DateTime<Utc>, x: &Decision, y: &Decision) -> Ordering {
    if x.remote.release.protocol != Protocol::Torrent || y.remote.release.protocol != Protocol::Torrent {
        return Ordering::Equal;
    }
    log_bucket(x.remote.release.seeders)
        .cmp(&log_bucket(y.remote.release.seeders))
        .then_with(|| log_bucket(x.remote.release.peers).cmp(&log_bucket(y.remote.release.peers)))
}

fn compare_age_if_usenet(_c: &Catalog, now: DateTime<Utc>, x: &Decision, y: &Decision) -> Ordering {
    if x.remote.release.protocol != Protocol::Usenet || y.remote.release.protocol != Protocol::Usenet {
        return Ordering::Equal;
    }
    let bucket = |d: &Decision| {
        let hours = specs::release_age_hours(now, &d.remote.release);
        let days = (now - d.remote.release.publish_date).num_days();
        if hours < 1.0 {
            1000
        } else if hours <= 24.0 {
            100
        } else if days <= 7 {
            10
        } else {
            1
        }
    };
    bucket(x).cmp(&bucket(y))
}

/// `long.Round(level)`: to the nearest multiple of `level`.
fn round_to(value: i64, level: i64) -> i64 {
    ((value as f64 / level as f64).round() as i64) * level
}

fn compare_size(catalog: &Catalog, _now: DateTime<Utc>, x: &Decision, y: &Decision) -> Ordering {
    let key = |d: &Decision| {
        let series = d.remote.series.as_ref().unwrap();
        let quality = d.remote.parsed_or_default().quality.quality.quality_id;
        let preferred = catalog.definition(quality).preferred_size;
        let two_hundred = crate::decision::megabytes(200);
        match preferred {
            Some(preferred) if series.runtime > 0 => {
                let preferred_size = (series.runtime as f64 * preferred * 1024.0 * 1024.0) as i64;
                -round_to(d.remote.release.size - preferred_size, two_hundred).abs()
            }
            _ => round_to(d.remote.release.size, two_hundred),
        }
    };
    key(x).cmp(&key(y))
}
