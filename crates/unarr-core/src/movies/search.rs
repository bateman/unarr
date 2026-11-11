//! Port of Radarr's `ReleaseSearchService` and of the RSS side of its
//! `ReleaseController`: the indexers asked about one movie, or for what is
//! new, and the answers decided.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::db::Db;
use crate::indexers::rss::ReleaseInfo;
use crate::indexers::IndexerService;
use crate::model::QualityProfile;
use crate::movies::decision::{self, MovieCatalog, MovieDecision, MovieSearch, QueuedMovie};
use crate::movies::model::Movie;
use crate::movies::request::MovieSearchCriteria;
use crate::movies::store;

/// Where a search reads the queue from, without depending on the
/// downloads service itself.
pub type MovieQueueSource = Arc<dyn Fn() -> Vec<QueuedMovie> + Send + Sync>;

#[derive(Debug)]
pub enum MovieSearchError {
    MovieNotFound(i64),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for MovieSearchError {
    fn from(e: anyhow::Error) -> Self {
        MovieSearchError::Other(e)
    }
}

#[derive(Clone)]
pub struct MovieSearchService {
    db: Db,
    indexers: IndexerService,
    queue: Option<MovieQueueSource>,
}

impl MovieSearchService {
    pub fn new(db: Db, indexers: IndexerService) -> MovieSearchService {
        MovieSearchService {
            db,
            indexers,
            queue: None,
        }
    }

    pub fn with_queue(mut self, queue: MovieQueueSource) -> MovieSearchService {
        self.queue = Some(queue);
        self
    }

    fn queue(&self) -> Vec<QueuedMovie> {
        self.queue.as_ref().map(|q| q()).unwrap_or_default()
    }

    /// `MovieSearch(movieId, userInvokedSearch, interactiveSearch)`: the
    /// decisions, one per release, the best of two alike.
    pub async fn movie_search(
        &self,
        movie_id: i64,
        user_invoked: bool,
        interactive: bool,
        now: DateTime<Utc>,
    ) -> Result<Vec<MovieDecision>, MovieSearchError> {
        let found = self
            .db
            .call(move |conn| {
                let Some(movie) = store::get_movie(conn, movie_id)? else {
                    return Ok(None);
                };
                let translations = store::translation_titles(conn, movie.metadata.id)?;
                let profile = crate::store::catalog::list_quality_profiles(conn)?
                    .into_iter()
                    .find(|p| p.id == movie.quality_profile_id);
                let formats = crate::store::catalog::list_custom_formats(conn)?;
                let language = store::profile_languages(conn)?
                    .get(&movie.quality_profile_id)
                    .copied();
                let library = store::library_movie(conn, movie)?;
                Ok(Some((library, translations, profile, formats, language)))
            })
            .await?;
        let Some((library, translations, profile, formats, language)) = found else {
            return Err(MovieSearchError::MovieNotFound(movie_id));
        };
        let movie = &library.movie;
        let wanted = acceptable_languages(profile.as_ref(), &formats, language);
        let criteria = MovieSearchCriteria {
            tmdb_id: movie.metadata.tmdb_id,
            imdb_id: movie.metadata.imdb_id.clone(),
            year: movie.metadata.year,
            scene_titles: scene_titles(movie, &translations, &wanted),
        };
        let result = self
            .indexers
            .search_movie(&criteria, interactive, &movie.tags, now)
            .await?;
        if result.indexers_searched > 0 {
            self.db
                .call(move |conn| store::set_last_search_time(conn, movie_id, now))
                .await?;
        }
        let search = MovieSearch {
            movie: library,
            user_invoked,
            interactive,
        };
        let queue = self.queue();
        let releases = result.releases;
        let decisions = self
            .db
            .call(move |conn| {
                let catalog = MovieCatalog::load(conn, now)?.with_queue(queue);
                let lookup = decision::StoreMovieLookup { conn };
                Ok(decision::decide(
                    &catalog,
                    &lookup,
                    &releases,
                    Some(&search),
                    false,
                ))
            })
            .await?;
        Ok(dedupe(decisions))
    }

    /// `FetchAndParseRssService.Fetch` and `GetRssDecision`: what the RSS
    /// feeds hold now, decided.
    pub async fn rss_decisions(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<MovieDecision>> {
        let releases = self.indexers.fetch_recent(now).await?;
        self.decide_rss(releases, now).await
    }

    /// What the indexers list as new, undecided.
    pub async fn fetch_recent(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<ReleaseInfo>> {
        self.indexers.fetch_recent(now).await
    }

    /// `GetRssDecision`: reports decided on with no search behind them.
    pub async fn decide_rss(
        &self,
        releases: Vec<ReleaseInfo>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Vec<MovieDecision>> {
        let queue = self.queue();
        self.db
            .call(move |conn| {
                let catalog = MovieCatalog::load(conn, now)?.with_queue(queue);
                let lookup = decision::StoreMovieLookup { conn };
                Ok(decision::decide(&catalog, &lookup, &releases, None, false))
            })
            .await
    }

    /// `PrioritizeDecisionsForMovies` on the catalogue as it is now.
    pub async fn prioritize(
        &self,
        decisions: Vec<MovieDecision>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Vec<MovieDecision>> {
        self.db
            .call(move |conn| {
                let catalog = MovieCatalog::load(conn, now)?;
                Ok(decision::prioritize(&catalog, decisions))
            })
            .await
    }
}

/// `QualityProfileService.GetAcceptableLanguages`: the languages the
/// profile's positively scored formats ask for, and the profile's own.
fn acceptable_languages(
    profile: Option<&QualityProfile>,
    formats: &[crate::model::CustomFormat],
    profile_language: Option<i64>,
) -> Vec<i64> {
    let mut wanted = Vec::new();
    if let Some(profile) = profile {
        for item in profile.format_items.iter().filter(|i| i.score > 0) {
            let Some(format) = formats.iter().find(|f| f.id == item.format) else {
                continue;
            };
            for spec in format.specifications.as_array().into_iter().flatten() {
                let body = &spec["body"];
                let value = body["value"].as_i64().unwrap_or(0);
                if spec["type"] == "LanguageSpecification"
                    && !body["negate"].as_bool().unwrap_or(false)
                    && value > 0
                    && !wanted.contains(&value)
                {
                    wanted.push(value);
                }
            }
        }
    }
    wanted.extend(profile_language);
    wanted
}

/// The titles a text search asks for: the title, the original title and
/// the translations in the languages the profile accepts, each once
/// whatever its case.
fn scene_titles(movie: &Movie, translations: &[(i64, String)], wanted: &[i64]) -> Vec<String> {
    let mut titles: Vec<String> = Vec::new();
    let candidates = std::iter::once(movie.metadata.title.clone())
        .chain(movie.metadata.original_title.clone())
        .chain(
            translations
                .iter()
                .filter(|(language, _)| wanted.contains(language))
                .map(|(_, title)| title.clone()),
        );
    for title in candidates {
        if title.trim().is_empty() {
            continue;
        }
        if !titles.iter().any(|t| t.to_lowercase() == title.to_lowercase()) {
            titles.push(title);
        }
    }
    titles
}

/// `DeDupeDecisions`: one decision per guid, the one with the fewest
/// rejections and then the indexer of higher priority.
pub fn dedupe(decisions: Vec<MovieDecision>) -> Vec<MovieDecision> {
    let mut best: Vec<MovieDecision> = Vec::new();
    for decision in decisions {
        match best
            .iter_mut()
            .find(|d| d.remote.release.guid == decision.remote.release.guid)
        {
            None => best.push(decision),
            Some(current) => {
                let key = |d: &MovieDecision| (d.rejections.len(), d.remote.release.indexer_priority);
                if key(&decision) < key(current) {
                    *current = decision;
                }
            }
        }
    }
    best
}
