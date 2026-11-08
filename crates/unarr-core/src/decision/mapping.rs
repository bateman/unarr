//! `ParsingService`: from a parsed title to the series and episodes it names.

use crate::decision::lookup::Lookup;
use crate::decision::scene::{self, AmbiguousMapping};
use crate::decision::{RemoteEpisode, SearchCriteria, SeriesMatchType};
use crate::indexers::rss::ReleaseInfo;
use crate::model::{Episode, Series, SeriesType};
use crate::parser::title::{ParsedEpisodeInfo, SeriesTitleInfo};
use crate::parser::{self, clean_series_title, normalize_episode_title};

/// What stops a mapping: the originals throw and the decision becomes an
/// error.
#[derive(Debug, Clone, PartialEq)]
pub enum MappingError {
    /// `MultipleSeriesFoundException`.
    MultipleSeries(String),
    /// `InvalidSceneMappingException`.
    AmbiguousMapping(AmbiguousMapping),
    /// Several episodes aired on the same day and no part was named.
    MultipleEpisodesSameAirDate(String),
}

impl std::fmt::Display for MappingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MappingError::MultipleSeries(t) => write!(f, "Expected one series to match {t:?}, several did"),
            MappingError::AmbiguousMapping(a) => write!(f, "{a}"),
            MappingError::MultipleEpisodesSameAirDate(d) => {
                write!(f, "Multiple episodes with the same air date found. Date: {d}")
            }
        }
    }
}

impl From<AmbiguousMapping> for MappingError {
    fn from(a: AmbiguousMapping) -> MappingError {
        MappingError::AmbiguousMapping(a)
    }
}

/// `SeriesRepository.ReturnSingleSeriesOrThrow`.
fn single(series: Vec<Series>, title: &str) -> Result<Option<Series>, MappingError> {
    match series.len() {
        0 => Ok(None),
        1 => Ok(series.into_iter().next()),
        _ => Err(MappingError::MultipleSeries(title.to_string())),
    }
}

/// `SeriesService.FindByTitle(title)`.
fn find_by_title(lookup: &dyn Lookup, title: &str) -> Result<Option<Series>, MappingError> {
    let clean = clean_series_title(title);
    single(lookup.series_by_clean_title(&clean), title)
}

fn find_by_title_year(lookup: &dyn Lookup, title: &str, year: i64) -> Result<Option<Series>, MappingError> {
    let clean = clean_series_title(title);
    single(lookup.series_by_clean_title_and_year(&clean, year), title)
}

/// `SeriesService.FindByTitleInexact`: the longest clean title the release
/// title contains, when there is more than one.
fn find_by_title_inexact(lookup: &dyn Lookup, title: &str) -> Option<Series> {
    let clean = clean_series_title(title);
    let mut list = lookup.series_inexact(&clean);
    if list.is_empty() {
        return None;
    }
    if list.len() == 1 {
        return list.pop();
    }
    list.sort_by(|a, b| {
        let pa = clean.find(&a.clean_title).unwrap_or(usize::MAX);
        let pb = clean.find(&b.clean_title).unwrap_or(usize::MAX);
        pa.cmp(&pb)
            .then_with(|| b.clean_title.len().cmp(&a.clean_title.len()))
    });
    list.into_iter().next()
}

/// `ParsingService.GetSeries(title)`.
pub fn get_series(lookup: &dyn Lookup, title: &str) -> Result<Option<Series>, MappingError> {
    let Some(parsed) = parser::parse_title(title) else {
        return find_by_title(lookup, title);
    };
    if let Some(tvdb_id) = scene::find_tvdb_id(
        lookup,
        &parsed.series_title,
        &parsed.release_title,
        parsed.season_number,
    )? {
        return Ok(lookup.series_by_tvdb_id(tvdb_id));
    }
    let mut series = find_by_title(lookup, &parsed.series_title)?;
    if series.is_none() && !parsed.series_title_info.all_titles.is_empty() {
        series = series_by_all_titles(lookup, &parsed)?;
    }
    if series.is_none() {
        series = find_by_title_year(
            lookup,
            &parsed.series_title_info.title_without_year,
            parsed.series_title_info.year,
        )?;
    }
    Ok(series)
}

/// `GetSeriesByAllTitles`: every alternate title must resolve to the same
/// series.
fn series_by_all_titles(
    lookup: &dyn Lookup,
    parsed: &ParsedEpisodeInfo,
) -> Result<Option<Series>, MappingError> {
    let mut found_series: Option<Series> = None;
    let mut found_tvdb: Option<i64> = None;
    for title in &parsed.series_title_info.all_titles {
        let series = find_by_title(lookup, title)?;
        let tvdb_id = match &series {
            Some(s) => Some(s.tvdb_id),
            None => scene::find_tvdb_id(lookup, title, &parsed.release_title, parsed.season_number)?,
        };
        let Some(tvdb_id) = tvdb_id else {
            continue;
        };
        if found_tvdb.is_some_and(|f| f != tvdb_id) {
            return Ok(None);
        }
        if found_series.is_none() {
            found_series = series;
        }
        found_tvdb = Some(tvdb_id);
    }
    if found_series.is_none() {
        if let Some(tvdb_id) = found_tvdb {
            found_series = lookup.series_by_tvdb_id(tvdb_id);
        }
    }
    Ok(found_series)
}

/// `Map(parsedEpisodeInfo, tvdbId, tvRageId, imdbId, searchCriteria)`.
pub fn map(
    lookup: &dyn Lookup,
    release: ReleaseInfo,
    parsed: ParsedEpisodeInfo,
    search: Option<&SearchCriteria>,
) -> Result<RemoteEpisode, MappingError> {
    map_inner(lookup, release, parsed, None, search)
}

/// `Map(parsedEpisodeInfo, series)`.
pub fn map_to_series(
    lookup: &dyn Lookup,
    release: ReleaseInfo,
    parsed: ParsedEpisodeInfo,
    series: Series,
) -> Result<RemoteEpisode, MappingError> {
    map_inner(lookup, release, parsed, Some(series), None)
}

fn map_inner(
    lookup: &dyn Lookup,
    release: ReleaseInfo,
    parsed: ParsedEpisodeInfo,
    series: Option<Series>,
    search: Option<&SearchCriteria>,
) -> Result<RemoteEpisode, MappingError> {
    let scene_mapping = scene::find_scene_mapping(
        lookup,
        &parsed.series_title,
        &parsed.release_title,
        parsed.season_number,
    )?;
    let tvdb_id = release.tvdb_id;
    let tv_rage_id = release.tv_rage_id;
    let imdb_id = release.imdb_id.clone();
    let mut remote = RemoteEpisode::new(release);
    remote.mapped_season_number = parsed.season_number;
    let mut scene_source = true;
    if let Some(m) = &scene_mapping {
        // `SceneSeasonNumber <= SeasonNumber` on a nullable: false when the
        // mapping names no scene season, so such a mapping shifts nothing.
        if let (Some(season), Some(scene_season)) = (m.season_number, m.scene_season_number) {
            if season >= 0 && scene_season <= parsed.season_number {
                remote.mapped_season_number += season - scene_season;
            }
        }
        if m.scene_origin.as_deref() == Some("tvdb") {
            scene_source = false;
        } else if m.mapping_type.as_deref() == Some("XemService")
            && m.scene_season_number.is_some_and(|s| s >= 0)
            && parsed.season_number == 1
            && m.scene_season_number != Some(parsed.season_number)
        {
            remote.mapped_season_number = m.scene_season_number.unwrap();
        }
    }
    let mut series = series;
    if series.is_none() {
        if let Some((found, match_type)) = find_series(
            lookup,
            &parsed,
            tvdb_id,
            tv_rage_id,
            imdb_id.as_deref(),
            scene_mapping.as_ref(),
            search,
        )? {
            series = Some(found);
            remote.series_match_type = match_type;
        }
    }
    if let Some(s) = &series {
        if validate_for_series_type(&parsed, s) {
            remote.episodes = episodes_for(
                lookup,
                &parsed,
                s,
                remote.mapped_season_number,
                scene_source,
                search,
            )?;
        }
    }
    remote.languages = parsed.languages.clone();
    if let Some(search) = search {
        remote.episode_requested = remote
            .episodes
            .iter()
            .any(|e| search.episodes.iter().any(|r| r.id == e.id));
    }
    remote.scene_mapping = scene_mapping;
    remote.series = series;
    remote.parsed = Some(parsed);
    Ok(remote)
}

/// `ValidateParsedEpisodeInfo.ValidateForSeriesType`: a daily-style release
/// is no use for a standard series.
pub fn validate_for_series_type(parsed: &ParsedEpisodeInfo, series: &Series) -> bool {
    if parsed.is_daily() && series.series_type == SeriesType::Standard {
        tracing::warn!(
            series = series.title,
            "found daily-style episode for non-daily series"
        );
        return false;
    }
    true
}

/// `GetEpisodes(parsedEpisodeInfo, series, sceneSource, searchCriteria)`:
/// the episodes a parsed title names for a known series.
pub fn get_episodes(
    lookup: &dyn Lookup,
    parsed: &ParsedEpisodeInfo,
    series: &Series,
    scene_source: bool,
    search: Option<&SearchCriteria>,
) -> Result<Vec<Episode>, MappingError> {
    if scene_source {
        let remote = map_inner(
            lookup,
            ReleaseInfo::default(),
            parsed.clone(),
            Some(series.clone()),
            search,
        )?;
        return Ok(remote.episodes);
    }
    episodes_for(lookup, parsed, series, parsed.season_number, scene_source, search)
}

fn episodes_for(
    lookup: &dyn Lookup,
    parsed: &ParsedEpisodeInfo,
    series: &Series,
    mapped_season_number: i64,
    scene_source: bool,
    search: Option<&SearchCriteria>,
) -> Result<Vec<Episode>, MappingError> {
    if parsed.full_season {
        if series.use_scene_numbering && scene_source {
            let episodes = lookup.episodes_by_scene_season(series.id, mapped_season_number);
            if !episodes.is_empty() {
                return Ok(episodes);
            }
        }
        return Ok(lookup.episodes_by_season(series.id, mapped_season_number));
    }
    if parsed.is_daily() {
        let air_date = parsed.air_date.as_deref().unwrap_or("");
        return Ok(
            daily_episode(lookup, series, air_date, parsed.daily_part, search)?
                .into_iter()
                .collect(),
        );
    }
    if parsed.is_absolute_numbering() {
        return anime_episodes(lookup, series, parsed, scene_source);
    }
    if parsed.is_possible_scene_season_special() {
        if let Some(special) =
            parse_special_episode_title_for(lookup, Some(parsed), &parsed.release_title, series)
        {
            return Ok(standard_episodes(
                lookup,
                series,
                &special,
                special.season_number,
                false,
                search,
            ));
        }
    }
    if parsed.special && mapped_season_number != 0 {
        return Ok(Vec::new());
    }
    Ok(standard_episodes(
        lookup,
        series,
        parsed,
        mapped_season_number,
        scene_source,
        search,
    ))
}

/// `ParseSpecialEpisodeTitle(parsedEpisodeInfo, releaseTitle, tvdbId, tvRageId, imdbId, searchCriteria)`.
pub fn parse_special_episode_title(
    lookup: &dyn Lookup,
    parsed: Option<&ParsedEpisodeInfo>,
    release_title: &str,
    tvdb_id: i64,
    tv_rage_id: i64,
    imdb_id: Option<&str>,
    search: Option<&SearchCriteria>,
) -> Result<Option<ParsedEpisodeInfo>, MappingError> {
    if let Some(search) = search {
        let s = &search.series;
        let by_id = (tvdb_id != 0 && tvdb_id == s.tvdb_id)
            || (tv_rage_id != 0 && tv_rage_id == s.tv_rage_id)
            || imdb_id.is_some_and(|i| !i.trim().is_empty() && Some(i) == s.imdb_id.as_deref());
        if by_id {
            return Ok(parse_special_episode_title_for(lookup, parsed, release_title, s));
        }
    }
    let mut series = get_series(lookup, release_title)?;
    if series.is_none() {
        series = find_by_title_inexact(lookup, release_title);
    }
    if series.is_none() && tvdb_id > 0 {
        series = lookup.series_by_tvdb_id(tvdb_id);
    }
    if series.is_none() && tv_rage_id > 0 {
        series = lookup.series_by_tv_rage_id(tv_rage_id);
    }
    if series.is_none() {
        if let Some(imdb) = imdb_id.filter(|i| !i.trim().is_empty()) {
            series = lookup.series_by_imdb_id(imdb);
        }
    }
    let Some(series) = series else {
        tracing::debug!(release_title, "no matching series");
        return Ok(None);
    };
    Ok(parse_special_episode_title_for(
        lookup,
        parsed,
        release_title,
        &series,
    ))
}

/// `ParseSpecialEpisodeTitle(parsedEpisodeInfo, releaseTitle, series)`: a
/// season 0 episode named by its title.
pub fn parse_special_episode_title_for(
    lookup: &dyn Lookup,
    parsed: Option<&ParsedEpisodeInfo>,
    release_title: &str,
    series: &Series,
) -> Option<ParsedEpisodeInfo> {
    if let Some(p) = parsed {
        // SxxE00 episodes are sometimes mapped via TheXEM: no title parsing then.
        if p.is_possible_scene_season_special()
            && series.use_scene_numbering
            && !lookup
                .episodes_by_scene_numbering(series.id, p.season_number, 0)
                .is_empty()
        {
            return Some(p.clone());
        }
    }
    let episode = find_episode_by_title(lookup, series.id, 0, release_title)?;
    let info = ParsedEpisodeInfo {
        release_title: release_title.to_string(),
        series_title: series.title.clone(),
        series_title_info: SeriesTitleInfo {
            title: series.title.clone(),
            ..Default::default()
        },
        season_number: episode.season_number,
        episode_numbers: vec![episode.episode_number],
        full_season: false,
        quality: parser::quality::parse_quality(release_title),
        release_group: parser::parse_release_group(release_title),
        languages: parser::language::parse_languages(release_title),
        special: true,
        ..Default::default()
    };
    tracing::debug!(release_title, "found special episode");
    Some(info)
}

/// `EpisodeService.FindEpisodeByTitle`: the season's episode whose title
/// appears earliest (then longest) in the release title.
pub fn find_episode_by_title(
    lookup: &dyn Lookup,
    series_id: i64,
    season: i64,
    release_title: &str,
) -> Option<Episode> {
    let normalized = normalize_episode_title(release_title).to_lowercase();
    let clean_normalized = clean_series_title(&normalized);
    let mut matches: Vec<(usize, usize, Episode)> = Vec::new();
    for episode in lookup.episodes_by_season(series_id, season) {
        let title = episode.title.clone().unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        let normalized_title = normalize_episode_title(&title);
        let length = normalized_title.chars().count();
        if let Some(pos) = normalized.find(&normalized_title.to_lowercase()) {
            matches.push((pos, length, episode.clone()));
        }
        let clean_title = clean_series_title(&normalized_title);
        if !clean_title.is_empty() {
            if let Some(pos) = clean_normalized.find(&clean_title) {
                matches.push((pos, length, episode.clone()));
            }
        }
    }
    matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
    matches.into_iter().next().map(|m| m.2)
}

#[derive(Debug)]
struct Found(Series, SeriesMatchType);

/// `FindSeries`.
fn find_series(
    lookup: &dyn Lookup,
    parsed: &ParsedEpisodeInfo,
    tvdb_id: i64,
    tv_rage_id: i64,
    imdb_id: Option<&str>,
    scene_mapping: Option<&crate::model::SceneMapping>,
    search: Option<&SearchCriteria>,
) -> Result<Option<(Series, SeriesMatchType)>, MappingError> {
    let imdb_id = imdb_id.filter(|i| !i.trim().is_empty());
    if let Some(m) = scene_mapping {
        if let Some(search) = search {
            if search.series.tvdb_id == m.tvdb_id {
                return Ok(Some((search.series.clone(), SeriesMatchType::Alias)));
            }
        }
        return Ok(lookup
            .series_by_tvdb_id(m.tvdb_id)
            .map(|s| (s, SeriesMatchType::Alias)));
    }
    if let Some(search) = search {
        let s = &search.series;
        if s.clean_title == clean_series_title(&parsed.series_title) {
            return Ok(Some((s.clone(), SeriesMatchType::Title)));
        }
        if tvdb_id > 0 && tvdb_id == s.tvdb_id {
            return Ok(Some((s.clone(), SeriesMatchType::Id)));
        }
        if tv_rage_id > 0 && tv_rage_id == s.tv_rage_id && tvdb_id <= 0 {
            return Ok(Some((s.clone(), SeriesMatchType::Id)));
        }
        if imdb_id.is_some() && imdb_id == s.imdb_id.as_deref() && tvdb_id <= 0 {
            return Ok(Some((s.clone(), SeriesMatchType::Id)));
        }
    }
    let mut found = find_by_title(lookup, &parsed.series_title)?.map(|s| Found(s, SeriesMatchType::Title));
    if found.is_none() && !parsed.series_title_info.all_titles.is_empty() {
        found = series_by_all_titles(lookup, parsed)?.map(|s| Found(s, SeriesMatchType::Title));
    }
    if found.is_none() && parsed.series_title_info.year > 0 {
        found = find_by_title_year(
            lookup,
            &parsed.series_title_info.title_without_year,
            parsed.series_title_info.year,
        )?
        .map(|s| Found(s, SeriesMatchType::Title));
    }
    if found.is_none() && tvdb_id > 0 {
        found = lookup
            .series_by_tvdb_id(tvdb_id)
            .map(|s| Found(s, SeriesMatchType::Id));
    }
    if found.is_none() && tv_rage_id > 0 && tvdb_id <= 0 {
        found = lookup
            .series_by_tv_rage_id(tv_rage_id)
            .map(|s| Found(s, SeriesMatchType::Id));
    }
    if found.is_none() && tvdb_id <= 0 {
        if let Some(imdb) = imdb_id {
            found = lookup
                .series_by_imdb_id(imdb)
                .map(|s| Found(s, SeriesMatchType::Id));
        }
    }
    if found.is_none() {
        tracing::debug!(title = parsed.series_title, "no matching series");
    }
    Ok(found.map(|Found(s, t)| (s, t)))
}

/// `GetDailyEpisode`.
fn daily_episode(
    lookup: &dyn Lookup,
    series: &Series,
    air_date: &str,
    part: Option<i64>,
    search: Option<&SearchCriteria>,
) -> Result<Option<Episode>, MappingError> {
    if let Some(search) = search {
        let matching: Vec<&Episode> = search
            .episodes
            .iter()
            .filter(|e| {
                e.air_date
                    .is_some_and(|d| d.format("%Y-%m-%d").to_string() == air_date)
            })
            .collect();
        if matching.len() == 1 {
            return Ok(Some(matching[0].clone()));
        }
    }
    find_one_by_air_date(lookup, series.id, air_date, part)
}

/// `EpisodeService.FindOneByAirDate`.
fn find_one_by_air_date(
    lookup: &dyn Lookup,
    series_id: i64,
    air_date: &str,
    part: Option<i64>,
) -> Result<Option<Episode>, MappingError> {
    let mut episodes = lookup.episodes_by_air_date(series_id, air_date);
    if episodes.is_empty() {
        return Ok(None);
    }
    if episodes.len() == 1 {
        return Ok(episodes.pop());
    }
    let mut regular: Vec<Episode> = episodes.into_iter().filter(|e| e.season_number > 0).collect();
    if regular.len() == 1 && part.is_none() {
        return Ok(regular.pop());
    }
    if let Some(part) = part {
        if part >= 1 && part as usize <= regular.len() {
            regular.sort_by_key(|e| (e.season_number, e.episode_number));
            return Ok(Some(regular.swap_remove(part as usize - 1)));
        }
    }
    Err(MappingError::MultipleEpisodesSameAirDate(air_date.to_string()))
}

/// `GetAnimeEpisodes`.
fn anime_episodes(
    lookup: &dyn Lookup,
    series: &Series,
    parsed: &ParsedEpisodeInfo,
    scene_source: bool,
) -> Result<Vec<Episode>, MappingError> {
    let mut result = Vec::new();
    let scene_season = scene::scene_season_number(lookup, &parsed.series_title, &parsed.release_title)?;
    for &absolute in &parsed.absolute_episode_numbers {
        let mut episodes: Vec<Episode> = Vec::new();
        if parsed.special {
            episodes.extend(lookup.episode_by_number(series.id, 0, absolute));
        } else if scene_source {
            if let Some(scene_season) = scene_season {
                episodes = lookup.episodes_by_scene_numbering(series.id, scene_season, absolute);
                if episodes.is_empty() {
                    episodes.extend(lookup.episode_by_number(series.id, scene_season, absolute));
                }
            } else if parsed.season_number > 1 && parsed.episode_numbers.is_empty() {
                episodes = lookup.episodes_by_scene_numbering(series.id, parsed.season_number, absolute);
                if episodes.is_empty() {
                    episodes.extend(lookup.episode_by_number(series.id, parsed.season_number, absolute));
                }
            } else {
                episodes = lookup.episodes_by_scene_absolute(series.id, absolute);
                // No multiple results without a scene name mapping.
                if episodes.len() > 1 {
                    episodes.clear();
                }
            }
        }
        if episodes.is_empty() {
            episodes.extend(lookup.episode_by_absolute(series.id, absolute));
        }
        for episode in episodes {
            tracing::debug!(
                absolute,
                series = series.title,
                season = episode.season_number,
                episode = episode.episode_number,
                "using absolute episode number"
            );
            result.push(episode);
        }
    }
    Ok(result)
}

/// `GetStandardEpisodes`.
fn standard_episodes(
    lookup: &dyn Lookup,
    series: &Series,
    parsed: &ParsedEpisodeInfo,
    mapped_season_number: i64,
    scene_source: bool,
    search: Option<&SearchCriteria>,
) -> Vec<Episode> {
    let mut result = Vec::new();
    for &number in &parsed.episode_numbers {
        if series.use_scene_numbering && scene_source {
            let mut episodes: Vec<Episode> = match search {
                Some(search) => search
                    .episodes
                    .iter()
                    .filter(|e| {
                        e.scene_season_number == Some(parsed.season_number)
                            && e.scene_episode_number == Some(number)
                    })
                    .cloned()
                    .collect(),
                None => Vec::new(),
            };
            if episodes.is_empty() {
                episodes = lookup.episodes_by_scene_numbering(series.id, mapped_season_number, number);
            }
            if !episodes.is_empty() {
                tracing::debug!(
                    series = series.title,
                    scene_season = parsed.season_number,
                    scene_episode = number,
                    "using scene to TVDB mapping"
                );
                result.extend(episodes);
                continue;
            }
        }
        let mut episode: Option<Episode> = None;
        if let Some(search) = search {
            let matching: Vec<&Episode> = search
                .episodes
                .iter()
                .filter(|e| e.season_number == mapped_season_number && e.episode_number == number)
                .collect();
            if matching.len() == 1 {
                episode = Some(matching[0].clone());
            }
        }
        if episode.is_none() {
            episode = lookup.episode_by_number(series.id, mapped_season_number, number);
        }
        match episode {
            Some(e) => result.push(e),
            None => tracing::debug!(
                series = series.title,
                season = mapped_season_number,
                episode = number,
                "unable to find episode"
            ),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    //! `GetEpisodesFixture` and `MapFixture` of the originals assert on mock
    //! calls; here the same scenarios are checked on what comes out.

    use super::*;
    use crate::decision::testing::{episode, series, MemoryLookup};
    use crate::model::SeriesType;
    use chrono::NaiveDate;

    fn parsed(season: i64, episodes: &[i64]) -> ParsedEpisodeInfo {
        ParsedEpisodeInfo {
            series_title: "30 Stone".into(),
            release_title: "30.Stone.S01E01".into(),
            season_number: season,
            episode_numbers: episodes.to_vec(),
            languages: vec![1],
            ..Default::default()
        }
    }

    fn search_for(s: &Series, episodes: Vec<Episode>) -> SearchCriteria {
        let first = episodes.first().cloned().unwrap();
        SearchCriteria {
            series: s.clone(),
            episodes,
            kind: crate::decision::SearchKind::SingleEpisode {
                season_number: first.season_number,
                episode_number: first.episode_number,
            },
            monitored_episodes_only: false,
            user_invoked_search: false,
            interactive_search: false,
        }
    }

    fn library() -> (Series, MemoryLookup) {
        let s = series(1, "30 Stone", 1);
        let lookup = MemoryLookup {
            series: vec![s.clone()],
            episodes: vec![episode(1, 1, 1, 1), episode(2, 1, 1, 2), episode(3, 1, 0, 1)],
            ..Default::default()
        };
        (s, lookup)
    }

    #[test]
    fn standard_numbering_finds_the_episode_and_prefers_the_search_criteria() {
        let (s, lookup) = library();
        let remote = map(&lookup, ReleaseInfo::default(), parsed(1, &[1]), None).unwrap();
        assert_eq!(remote.series.as_ref().map(|s| s.id), Some(1));
        assert_eq!(remote.episodes.iter().map(|e| e.id).collect::<Vec<_>>(), [1]);
        assert_eq!(remote.series_match_type, SeriesMatchType::Title);

        // The search criteria's episode is taken as is, even when it differs
        // from the library's.
        let mut renamed = episode(1, 1, 1, 1);
        renamed.title = Some("From the search".into());
        let search = search_for(&s, vec![renamed.clone()]);
        let remote = map(&lookup, ReleaseInfo::default(), parsed(1, &[1]), Some(&search)).unwrap();
        assert_eq!(remote.episodes[0].title.as_deref(), Some("From the search"));
        assert!(remote.episode_requested);

        // When the criteria do not match, the library is consulted.
        let search = search_for(&s, vec![episode(10, 1, 1, 10)]);
        let remote = map(&lookup, ReleaseInfo::default(), parsed(1, &[1]), Some(&search)).unwrap();
        assert_eq!(remote.episodes.iter().map(|e| e.id).collect::<Vec<_>>(), [1]);
        assert!(!remote.episode_requested);
    }

    #[test]
    fn daily_series_match_by_air_date_and_part() {
        let (mut s, mut lookup) = library();
        s.series_type = SeriesType::Daily;
        lookup.series = vec![s.clone()];
        let day = NaiveDate::from_ymd_opt(2026, 11, 7).unwrap();
        for e in &mut lookup.episodes {
            e.air_date = Some(day);
        }
        let daily = ParsedEpisodeInfo {
            air_date: Some("2026-11-07".into()),
            ..parsed(0, &[])
        };
        // Three episodes air that day: one is the special, two are regular,
        // so a part is needed.
        let err = map(&lookup, ReleaseInfo::default(), daily.clone(), None).unwrap_err();
        assert!(matches!(err, MappingError::MultipleEpisodesSameAirDate(_)));
        let with_part = ParsedEpisodeInfo {
            daily_part: Some(2),
            ..daily.clone()
        };
        let remote = map(&lookup, ReleaseInfo::default(), with_part, None).unwrap();
        assert_eq!(remote.episodes.iter().map(|e| e.id).collect::<Vec<_>>(), [2]);
        // The search criteria's episode on that date wins without a lookup.
        let search = search_for(&s, vec![lookup.episodes[0].clone()]);
        let remote = map(&lookup, ReleaseInfo::default(), daily.clone(), Some(&search)).unwrap();
        assert_eq!(remote.episodes.iter().map(|e| e.id).collect::<Vec<_>>(), [1]);
        // A daily release for a standard series maps to no episodes.
        let (standard, lookup2) = library();
        let remote = map(&lookup2, ReleaseInfo::default(), daily, None).unwrap();
        assert_eq!(remote.series.as_ref().map(|s| s.id), Some(standard.id));
        assert!(remote.episodes.is_empty());
    }

    /// `SceneSeasonNumber <= SeasonNumber` is false on a null in the
    /// originals: a mapping with a season but no scene season shifts
    /// nothing. Over a hundred rows of the fixture have that shape.
    #[test]
    fn a_scene_mapping_without_a_scene_season_shifts_nothing() {
        let (s, mut lookup) = library();
        lookup.scene_mappings = vec![crate::model::SceneMapping {
            id: 1,
            tvdb_id: s.tvdb_id,
            season_number: Some(2),
            search_term: Some("30 Stone".into()),
            parse_term: Some("30stone".into()),
            title: "30 Stone".into(),
            mapping_type: Some("TheXemServiceProvider".into()),
            scene_season_number: None,
            filter_regex: None,
            scene_origin: None,
            search_mode: None,
            comment: None,
        }];
        let remote = map(&lookup, ReleaseInfo::default(), parsed(1, &[1]), None).unwrap();
        assert_eq!(remote.mapped_season_number, 1);
        assert_eq!(remote.episodes.iter().map(|e| e.id).collect::<Vec<_>>(), [1]);
        // With a scene season, the shift applies as before.
        lookup.scene_mappings[0].scene_season_number = Some(1);
        lookup.scene_mappings[0].season_number = Some(1);
        let remote = map(&lookup, ReleaseInfo::default(), parsed(1, &[1]), None).unwrap();
        assert_eq!(remote.mapped_season_number, 1);
    }

    #[test]
    fn scene_numbering_is_used_when_the_series_asks_for_it() {
        let (mut s, mut lookup) = library();
        s.use_scene_numbering = true;
        lookup.series = vec![s.clone()];
        // Scene 1x01 is TVDB 1x02.
        lookup.episodes[1].scene_season_number = Some(1);
        lookup.episodes[1].scene_episode_number = Some(1);
        let remote = map(&lookup, ReleaseInfo::default(), parsed(1, &[1]), None).unwrap();
        assert_eq!(remote.episodes.iter().map(|e| e.id).collect::<Vec<_>>(), [2]);
        // A matching search criteria episode by scene numbers comes first.
        let search = search_for(&s, vec![lookup.episodes[1].clone()]);
        let remote = map(&lookup, ReleaseInfo::default(), parsed(1, &[1]), Some(&search)).unwrap();
        assert_eq!(remote.episodes.iter().map(|e| e.id).collect::<Vec<_>>(), [2]);
        // Without scene numbers on the episodes, the plain numbers apply.
        let mut e = episode(5, 1, 1, 5);
        e.scene_episode_number = Some(10);
        let search = search_for(&s, vec![e]);
        let plain = ParsedEpisodeInfo {
            season_number: 1,
            episode_numbers: vec![5],
            ..parsed(1, &[5])
        };
        let remote = map(&lookup, ReleaseInfo::default(), plain, Some(&search)).unwrap();
        assert_eq!(remote.episodes.iter().map(|e| e.id).collect::<Vec<_>>(), [5]);
    }

    #[test]
    fn absolute_numbering_falls_back_from_scene_to_tvdb_and_specials_to_season_zero() {
        let (s, mut lookup) = library();
        lookup.episodes[1].absolute_episode_number = Some(7);
        lookup.episodes[1].scene_absolute_episode_number = Some(9);
        let absolute = |n: i64, special: bool| ParsedEpisodeInfo {
            absolute_episode_numbers: vec![n],
            episode_numbers: vec![],
            special,
            ..parsed(1, &[])
        };
        // Scene absolute first.
        let remote = map(&lookup, ReleaseInfo::default(), absolute(9, false), None).unwrap();
        assert_eq!(remote.episodes.iter().map(|e| e.id).collect::<Vec<_>>(), [2]);
        // Then the TVDB absolute number.
        let remote = map(&lookup, ReleaseInfo::default(), absolute(7, false), None).unwrap();
        assert_eq!(remote.episodes.iter().map(|e| e.id).collect::<Vec<_>>(), [2]);
        // A special looks in season 0 by episode number.
        let remote = map(&lookup, ReleaseInfo::default(), absolute(1, true), None).unwrap();
        assert_eq!(remote.episodes.iter().map(|e| e.id).collect::<Vec<_>>(), [3]);
        // Several episodes with the same scene absolute number: none without
        // a mapping.
        lookup.episodes[0].scene_absolute_episode_number = Some(9);
        let remote = map(&lookup, ReleaseInfo::default(), absolute(9, false), None).unwrap();
        assert!(remote.episodes.is_empty(), "{:?}", remote.episodes);
        let _ = s;
    }

    #[test]
    fn full_seasons_and_unknown_series() {
        let (_, lookup) = library();
        let pack = ParsedEpisodeInfo {
            full_season: true,
            ..parsed(1, &[])
        };
        let remote = map(&lookup, ReleaseInfo::default(), pack, None).unwrap();
        assert_eq!(remote.episodes.len(), 2);
        let unknown = ParsedEpisodeInfo {
            series_title: "Nobody Knows".into(),
            ..parsed(1, &[1])
        };
        let remote = map(&lookup, ReleaseInfo::default(), unknown, None).unwrap();
        assert!(remote.series.is_none());
        assert!(remote.episodes.is_empty());
    }

    #[test]
    fn series_are_found_by_ids_and_aliases() {
        let (mut s, mut lookup) = library();
        s.tvdb_id = 777;
        s.imdb_id = Some("tt1".into());
        lookup.series = vec![s.clone()];
        let unknown_title = ParsedEpisodeInfo {
            series_title: "Thirty Stone".into(),
            ..parsed(1, &[1])
        };
        let by_tvdb = ReleaseInfo {
            tvdb_id: 777,
            ..Default::default()
        };
        let remote = map(&lookup, by_tvdb, unknown_title.clone(), None).unwrap();
        assert_eq!(remote.series_match_type, SeriesMatchType::Id);
        assert_eq!(remote.episodes.len(), 1);
        let by_imdb = ReleaseInfo {
            imdb_id: Some("tt1".into()),
            ..Default::default()
        };
        let remote = map(&lookup, by_imdb, unknown_title.clone(), None).unwrap();
        assert_eq!(remote.series_match_type, SeriesMatchType::Id);
        // An alias from the scene mappings.
        lookup.scene_mappings.push(crate::model::SceneMapping {
            id: 1,
            tvdb_id: 777,
            season_number: None,
            search_term: Some("Thirty Stone".into()),
            parse_term: Some(clean_series_title("Thirty Stone")),
            title: "Thirty Stone".into(),
            mapping_type: Some("ServicesProvider".into()),
            scene_season_number: None,
            filter_regex: None,
            scene_origin: None,
            search_mode: None,
            comment: None,
        });
        let remote = map(&lookup, ReleaseInfo::default(), unknown_title, None).unwrap();
        assert_eq!(remote.series_match_type, SeriesMatchType::Alias);
        assert_eq!(remote.scene_mapping.as_ref().map(|m| m.tvdb_id), Some(777));
    }

    #[test]
    fn special_episodes_are_found_by_title() {
        let (s, mut lookup) = library();
        lookup.episodes[2].title = Some("The Christmas Special".into());
        let found = parse_special_episode_title_for(
            &lookup,
            None,
            "30.Stone.The.Christmas.Special.720p.HDTV.x264-GRP",
            &s,
        )
        .expect("the special is named");
        assert_eq!(found.season_number, 0);
        assert_eq!(found.episode_numbers, [1]);
        assert!(found.special);
        assert_eq!(found.series_title, "30 Stone");
        assert!(parse_special_episode_title_for(&lookup, None, "30.Stone.Some.Other.Title", &s).is_none());
    }
}
