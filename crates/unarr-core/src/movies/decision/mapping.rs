//! Port of Radarr's `ParsingService.Map`: which movie of the library a
//! parsed release is about. By the ids the indexer gave, when the year
//! agrees; else, in a search, by the titles of the movie searched for;
//! else by title and year across the library.

use crate::movies::decision::library::{LibraryMovie, MovieLookup};
use crate::movies::decision::{MovieMatchType, MovieSearch};
use crate::movies::model::Movie;
use crate::movies::parser::{clean_movie_title, ParsedMovieInfo};
use crate::parser::title::normalize_imdb_id;

/// The arabic numerals the original swaps with roman ones in titles, in
/// its order: 1 to 20.
const ROMAN: [&str; 20] = [
    "I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X", "XI", "XII", "XIII", "XIV", "XV", "XVI",
    "XVII", "XVIII", "XIX", "XX",
];

/// A year the release may have: none parsed, the movie's or its secondary.
fn year_agrees(parsed: &ParsedMovieInfo, movie: &Movie) -> bool {
    parsed.year < 1800
        || movie.metadata.year == parsed.year
        || movie.metadata.secondary_year == Some(parsed.year)
}

/// Several movies answer to one title: the original's
/// `MultipleMoviesFoundException`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipleMovies(pub usize);

impl std::fmt::Display for MultipleMovies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Expected one movie, but found {}", self.0)
    }
}

type Found = Result<Option<(Movie, MovieMatchType)>, MultipleMovies>;

/// `FindMovie`: the movie and how it was found.
pub fn find_movie(
    lookup: &dyn MovieLookup,
    parsed: &ParsedMovieInfo,
    imdb_id: &str,
    tmdb_id: i64,
    search: Option<&MovieSearch>,
) -> Found {
    if !imdb_id.trim().is_empty() && imdb_id != "0" {
        // Fixes practically all the cases of an indexer naming the wrong id.
        if let Some(movie) = normalize_imdb_id(imdb_id)
            .and_then(|id| lookup.find_by_imdb_id(&id))
            .filter(|m| year_agrees(parsed, m))
        {
            return Ok(Some((movie, MovieMatchType::Id)));
        }
    }
    if tmdb_id > 0 {
        if let Some(movie) = lookup.find_by_tmdb_id(tmdb_id).filter(|m| year_agrees(parsed, m)) {
            return Ok(Some((movie, MovieMatchType::Id)));
        }
    }
    match search {
        Some(search) => Ok(by_search_criteria(parsed, imdb_id, tmdb_id, search)),
        None => by_title_and_or_year(lookup, parsed),
    }
}

/// `TryGetMovieBySearchCriteria`.
fn by_search_criteria(
    parsed: &ParsedMovieInfo,
    imdb_id: &str,
    tmdb_id: i64,
    search: &MovieSearch,
) -> Option<(Movie, MovieMatchType)> {
    let movie = &search.movie.movie;
    let mut possible: Vec<&str> = Vec::new();
    possible.extend(movie.metadata.clean_title.as_deref());
    possible.extend(movie.metadata.clean_original_title.as_deref());
    possible.extend(search.movie.alternative_titles.iter().map(String::as_str));
    possible.extend(search.movie.translations.iter().map(String::as_str));
    let clean: Vec<String> = parsed.movie_titles.iter().map(|t| clean_movie_title(t)).collect();
    let numerals = || (1..=20).map(|n| (n.to_string(), ROMAN[n - 1].to_lowercase()));
    let titled = possible.iter().any(|pt| {
        clean.iter().any(|t| t == pt)
            || numerals().any(|(arabic, roman)| {
                clean.contains(&pt.replace(&arabic, &roman))
                    || clean.iter().any(|t| t.replace(&arabic, &roman) == *pt)
            })
    });
    if titled && year_agrees(parsed, movie) {
        return Some((movie.clone(), MovieMatchType::Title));
    }
    if tmdb_id > 0 && tmdb_id == movie.metadata.tmdb_id {
        return Some((movie.clone(), MovieMatchType::Id));
    }
    // Radarr 7 normalises the release's id (`tt` and seven digits) before
    // it compares it with the movie's.
    if let Some(normalized) = normalize_imdb_id(imdb_id.trim()) {
        if movie.metadata.imdb_id.as_deref() == Some(normalized.as_str()) {
            return Some((movie.clone(), MovieMatchType::Id));
        }
    }
    None
}

/// `TryGetMovieByTitleAndOrYear`.
fn by_title_and_or_year(lookup: &dyn MovieLookup, parsed: &ParsedMovieInfo) -> Found {
    let year = (parsed.year > 1800).then_some(parsed.year);
    Ok(find_by_title(lookup, &parsed.movie_titles, year)?.map(|m| (m, MovieMatchType::Title)))
}

/// `MovieService.FindByTitleCandidates` and `FindByTitle`: one movie whose
/// title, original title, alternative title or translation is one of the
/// titles (as they are, or with numerals swapped), of that year when one is
/// given. More than one is an error, as in the original.
pub fn find_by_title(
    lookup: &dyn MovieLookup,
    titles: &[String],
    year: Option<i64>,
) -> Result<Option<Movie>, MultipleMovies> {
    let mut clean_titles = Vec::new();
    let mut other_titles = Vec::new();
    let mut lookup_titles = Vec::new();
    for title in titles {
        let clean = clean_movie_title(title).to_lowercase();
        let mut roman = clean.clone();
        let mut arabic = clean.clone();
        for (n, numeral) in ROMAN.iter().enumerate() {
            let number = (n + 1).to_string();
            roman = roman.replace(&number, numeral);
            arabic = arabic.replace(numeral, &number);
        }
        let roman = roman.to_lowercase();
        other_titles.extend([arabic.clone(), roman.clone()]);
        lookup_titles.extend([clean.clone(), arabic, roman]);
        clean_titles.push(clean);
    }
    let mut distinct = Vec::new();
    for title in lookup_titles {
        if !distinct.contains(&title) {
            distinct.push(title);
        }
    }
    let candidates = lookup.find_by_titles(&distinct);

    let with_year = |m: &&LibraryMovie| {
        year.is_none_or(|y| m.movie.metadata.year == y || m.movie.metadata.secondary_year == Some(y))
    };
    let has = |list: &[String], title: &Option<String>| title.as_ref().is_some_and(|t| list.contains(t));
    let pick = |matches: &dyn Fn(&LibraryMovie) -> bool| -> Vec<&LibraryMovie> {
        candidates
            .iter()
            .filter(|m| matches(m))
            .filter(with_year)
            .collect()
    };
    let mut result = pick(&|m| {
        has(&clean_titles, &m.movie.metadata.clean_title)
            || has(&clean_titles, &m.movie.metadata.clean_original_title)
    });
    if result.is_empty() {
        result = pick(&|m| has(&other_titles, &m.movie.metadata.clean_title));
    }
    if result.is_empty() {
        result = pick(&|m| {
            m.alternative_titles
                .iter()
                .any(|t| clean_titles.contains(t) || other_titles.contains(t))
        });
    }
    if result.is_empty() {
        result = pick(&|m| {
            m.translations
                .iter()
                .any(|t| clean_titles.contains(t) || other_titles.contains(t))
        });
    }
    match result.as_slice() {
        [one] => Ok(Some(one.movie.clone())),
        [] => Ok(None),
        many => Err(MultipleMovies(many.len())),
    }
}
