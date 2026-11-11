//! What the engine asks of the library: the movies a release may be about,
//! and what is known of a movie (its file, its history, its blocklist, the
//! releases waiting for it).

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::movies::model::{Movie, MovieBlocklistEntry, MovieFile, MovieHistoryEntry};
use crate::movies::store;

/// A movie with the clean titles it is also known by: the alternative
/// titles and the translations of its metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryMovie {
    pub movie: Movie,
    pub alternative_titles: Vec<String>,
    pub translations: Vec<String>,
}

impl LibraryMovie {
    /// A movie without further titles, for tests and for movies reached by id.
    pub fn bare(movie: Movie) -> LibraryMovie {
        LibraryMovie {
            movie,
            alternative_titles: Vec::new(),
            translations: Vec::new(),
        }
    }
}

/// The per-release queries.
pub trait MovieLookup {
    /// `MovieRepository.FindByTitles`: the movies whose clean title, clean
    /// original title, alternative title or translation is one of these.
    fn find_by_titles(&self, clean_titles: &[String]) -> Vec<LibraryMovie>;
    /// By `tt` id, already normalised.
    fn find_by_imdb_id(&self, imdb_id: &str) -> Option<Movie>;
    fn find_by_tmdb_id(&self, tmdb_id: i64) -> Option<Movie>;
    fn movie_file(&self, id: i64) -> Option<MovieFile>;
    /// The history of a movie, the most recent first.
    fn history(&self, movie_id: i64) -> Vec<MovieHistoryEntry>;
    fn blocklist(&self, movie_id: i64) -> Vec<MovieBlocklistEntry>;
    /// When the oldest release waiting for this movie was published.
    fn oldest_pending(&self, movie_id: i64) -> Option<DateTime<Utc>>;
    /// The movies some release is waiting for.
    fn pending_movie_ids(&self) -> Vec<i64>;
    /// Free bytes where the path is, `None` when it cannot be told.
    fn free_space(&self, path: &str) -> Option<i64>;
}

/// The library in the database.
pub struct StoreMovieLookup<'a> {
    pub conn: &'a Connection,
}

impl StoreMovieLookup<'_> {
    fn log<T: Default>(result: anyhow::Result<T>) -> T {
        result.unwrap_or_else(|e| {
            tracing::warn!("library lookup failed: {e:#}");
            T::default()
        })
    }
}

impl MovieLookup for StoreMovieLookup<'_> {
    fn find_by_titles(&self, clean_titles: &[String]) -> Vec<LibraryMovie> {
        Self::log(store::movies_by_clean_titles(self.conn, clean_titles))
    }

    fn find_by_imdb_id(&self, imdb_id: &str) -> Option<Movie> {
        Self::log(store::movie_by_imdb_id(self.conn, imdb_id))
    }

    fn find_by_tmdb_id(&self, tmdb_id: i64) -> Option<Movie> {
        Self::log(store::movies_by_tmdb_id(self.conn, tmdb_id))
            .into_iter()
            .next()
    }

    fn movie_file(&self, id: i64) -> Option<MovieFile> {
        Self::log(store::get_movie_file(self.conn, id))
    }

    fn history(&self, movie_id: i64) -> Vec<MovieHistoryEntry> {
        Self::log(store::history_of_movie(self.conn, movie_id, None))
    }

    fn blocklist(&self, movie_id: i64) -> Vec<MovieBlocklistEntry> {
        Self::log(store::blocklist_of_movie(self.conn, movie_id))
    }

    fn oldest_pending(&self, movie_id: i64) -> Option<DateTime<Utc>> {
        Self::log(store::oldest_pending_release(self.conn, movie_id))
    }

    fn pending_movie_ids(&self) -> Vec<i64> {
        Self::log(store::pending_movie_ids(self.conn))
    }

    /// The original asks the mount the path is on, whether the movie's
    /// folder exists yet or not: the nearest folder that does answers.
    fn free_space(&self, path: &str) -> Option<i64> {
        std::path::Path::new(path)
            .ancestors()
            .find_map(crate::fs::free_space)
    }
}

/// A library held in memory, for the tests of the engine.
#[derive(Debug, Default, Clone)]
pub struct MemoryMovieLookup {
    pub movies: Vec<LibraryMovie>,
    pub files: Vec<MovieFile>,
    pub history: Vec<MovieHistoryEntry>,
    pub blocklist: Vec<MovieBlocklistEntry>,
    /// `(movie id, publish date)` of the releases waiting.
    pub pending: Vec<(i64, DateTime<Utc>)>,
    pub free_space: Option<i64>,
}

impl MovieLookup for MemoryMovieLookup {
    fn find_by_titles(&self, clean_titles: &[String]) -> Vec<LibraryMovie> {
        let wanted = |t: &Option<String>| t.as_ref().is_some_and(|t| clean_titles.contains(t));
        self.movies
            .iter()
            .filter(|m| {
                wanted(&m.movie.metadata.clean_title)
                    || wanted(&m.movie.metadata.clean_original_title)
                    || m.alternative_titles.iter().any(|t| clean_titles.contains(t))
                    || m.translations.iter().any(|t| clean_titles.contains(t))
            })
            .cloned()
            .collect()
    }

    fn find_by_imdb_id(&self, imdb_id: &str) -> Option<Movie> {
        self.movies
            .iter()
            .find(|m| m.movie.metadata.imdb_id.as_deref() == Some(imdb_id))
            .map(|m| m.movie.clone())
    }

    fn find_by_tmdb_id(&self, tmdb_id: i64) -> Option<Movie> {
        self.movies
            .iter()
            .find(|m| m.movie.metadata.tmdb_id == tmdb_id)
            .map(|m| m.movie.clone())
    }

    fn movie_file(&self, id: i64) -> Option<MovieFile> {
        self.files.iter().find(|f| f.id == id).cloned()
    }

    fn history(&self, movie_id: i64) -> Vec<MovieHistoryEntry> {
        let mut rows: Vec<MovieHistoryEntry> = self
            .history
            .iter()
            .filter(|h| h.movie_id == movie_id)
            .cloned()
            .collect();
        rows.sort_by(|a, b| b.date.cmp(&a.date).then(b.id.cmp(&a.id)));
        rows
    }

    fn blocklist(&self, movie_id: i64) -> Vec<MovieBlocklistEntry> {
        self.blocklist
            .iter()
            .filter(|b| b.movie_id == movie_id)
            .cloned()
            .collect()
    }

    fn oldest_pending(&self, movie_id: i64) -> Option<DateTime<Utc>> {
        self.pending
            .iter()
            .filter(|(id, _)| *id == movie_id)
            .map(|(_, date)| *date)
            .min()
    }

    fn pending_movie_ids(&self) -> Vec<i64> {
        self.pending.iter().map(|(id, _)| *id).collect()
    }

    fn free_space(&self, _path: &str) -> Option<i64> {
        self.free_space
    }
}
