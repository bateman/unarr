//! When a movie counts as available, as `Movie.IsAvailable` and
//! `MovieMetadata.GetReleaseDate` decide.

use chrono::{DateTime, Duration, Utc};

use super::model::{Movie, MovieStatus};

/// `GetReleaseDate`: the day the movie is released in the sense its
/// minimum availability asks for. For `released`, the earlier of the
/// digital and the physical release, else whichever there is, else ninety
/// days after the cinemas; for `inCinemas`, the cinema date. `announced`
/// and `tba` have no date: the movie is available as soon as it is known.
pub fn release_date(movie: &Movie) -> Option<DateTime<Utc>> {
    let md = &movie.metadata;
    match movie.minimum_availability {
        MovieStatus::Tba | MovieStatus::Announced | MovieStatus::Deleted => None,
        MovieStatus::InCinemas => md.in_cinemas.or_else(|| released_date(movie)),
        MovieStatus::Released => released_date(movie),
    }
}

fn released_date(movie: &Movie) -> Option<DateTime<Utc>> {
    let md = &movie.metadata;
    match (md.physical_release, md.digital_release) {
        (Some(physical), Some(digital)) => Some(physical.min(digital)),
        (Some(physical), None) => Some(physical),
        (None, Some(digital)) => Some(digital),
        (None, None) => md.in_cinemas.map(|c| c + Duration::days(90)),
    }
}

/// `IsAvailable(delay)`: whether `now` is past the release date, `delay`
/// days later when the settings ask for one.
pub fn is_available(movie: &Movie, now: DateTime<Utc>, delay_days: i64) -> bool {
    match movie.minimum_availability {
        MovieStatus::Tba | MovieStatus::Announced | MovieStatus::Deleted => true,
        MovieStatus::InCinemas | MovieStatus::Released => match release_date(movie) {
            Some(date) => now >= date + Duration::days(delay_days),
            None => false,
        },
    }
}
