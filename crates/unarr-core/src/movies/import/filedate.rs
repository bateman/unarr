//! The date of a movie file set to the movie's cinema or release date, as
//! the media management settings ask (`UpdateMovieFileService`).

use std::path::Path;

use chrono::{DateTime, NaiveDateTime, NaiveTime, Utc};

use super::events::MovieImportEvent;
use crate::db::Db;
use crate::mediafiles::transfer;
use crate::movies::model::Movie;
use crate::movies::store;
use crate::store::config::{FileDateType, MediaManagementConfig};

/// The date the setting names for this movie, if the movie has it.
fn target(kind: FileDateType, movie: &Movie) -> Option<DateTime<Utc>> {
    match kind {
        FileDateType::Release => movie.metadata.physical_release.or(movie.metadata.digital_release),
        FileDateType::Cinemas => movie.metadata.in_cinemas,
        // The air dates are Sonarr's values of the setting.
        FileDateType::None | FileDateType::LocalAirDate | FileDateType::UtcAirDate => None,
    }
}

/// `ChangeFileDate(filePath, date)`: whether the file's date differs from
/// the one wanted. The original compares the day the file was written
/// (`FileGetLastWrite` is the UTC time, and only its date survives the
/// round trip through `ToLongDateString`) with the wanted date: a file
/// already on that date at midnight is left alone, whatever the zone of
/// the machine, and a wanted date with a time of day never matches.
fn differs(written: DateTime<Utc>, date: DateTime<Utc>) -> bool {
    let day: NaiveDateTime = written.date_naive().and_time(NaiveTime::MIN);
    day != date.naive_utc()
}

/// `ChangeFileDateForFile`: true when the date was changed.
pub fn change_file_date(config: &MediaManagementConfig, path: &str, movie: &Movie) -> bool {
    let Some(date) = target(config.file_date, movie) else {
        return false;
    };
    let Some(written) = transfer::last_write_time(Path::new(path)) else {
        return false;
    };
    if !differs(written, date) {
        return false;
    }
    // `FileSetLastWriteTime` does not go before 1970.
    let date = date.max(DateTime::<Utc>::UNIX_EPOCH);
    if let Err(e) = transfer::try_set_last_write_time(Path::new(path), date) {
        tracing::warn!(error = %e, "Unable to set date of file [{path}]");
        return false;
    }
    tracing::debug!("Date of file [{path}] changed from '{written}' to '{date}'");
    true
}

/// `Handle(MovieScannedEvent)`: the dates of the movie's files after a
/// scan that ran; a skipped scan is another event and changes nothing.
pub fn handle_scanned(db: &Db, config: &MediaManagementConfig, movie: &Movie, events: &[MovieImportEvent]) {
    if config.file_date == FileDateType::None {
        return;
    }
    let scanned = events
        .iter()
        .any(|e| matches!(e, MovieImportEvent::Scanned { movie_id, .. } if *movie_id == movie.id));
    if !scanned {
        return;
    }
    let id = movie.id;
    let files = match db.call_blocking(move |conn| store::movie_files_of(conn, id)) {
        Ok(files) => files,
        Err(e) => {
            tracing::error!(
                error = %format!("{e:#}"),
                "UpdateMovieFileService failed while processing [MovieScannedEvent]"
            );
            return;
        }
    };
    let changed = files
        .iter()
        .filter(|f| {
            let path = Path::new(&movie.path).join(&f.relative_path);
            change_file_date(config, &path.to_string_lossy(), movie)
        })
        .count();
    if changed > 0 {
        tracing::debug!(
            "Changed file date for {changed} files of {} in {}",
            files.len(),
            movie.metadata.title
        );
    } else {
        tracing::debug!("No file dates changed for {}", movie.metadata.title);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movies::decision::testing;
    use chrono::TimeZone;

    fn file(test: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("unarr-filedate-{}-{test}.mkv", std::process::id()));
        std::fs::write(&path, b"movie").unwrap();
        path
    }

    fn config(kind: FileDateType) -> MediaManagementConfig {
        MediaManagementConfig {
            file_date: kind,
            ..Default::default()
        }
    }

    fn written(path: &Path) -> DateTime<Utc> {
        transfer::last_write_time(path).unwrap()
    }

    #[test]
    fn none_leaves_the_file_alone() {
        let path = file("none");
        let before = written(&path);
        let mut movie = testing::movie(1, "The Movie", 2018);
        movie.metadata.in_cinemas = Some(Utc.with_ymd_and_hms(2018, 3, 9, 0, 0, 0).unwrap());
        assert!(!change_file_date(
            &config(FileDateType::None),
            &path.to_string_lossy(),
            &movie
        ));
        assert_eq!(written(&path), before);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cinemas_sets_the_cinema_date() {
        let path = file("cinemas");
        let date = Utc.with_ymd_and_hms(2018, 3, 9, 0, 0, 0).unwrap();
        let mut movie = testing::movie(1, "The Movie", 2018);
        movie.metadata.in_cinemas = Some(date);
        movie.metadata.physical_release = Some(Utc.with_ymd_and_hms(2018, 7, 1, 0, 0, 0).unwrap());
        assert!(change_file_date(
            &config(FileDateType::Cinemas),
            &path.to_string_lossy(),
            &movie
        ));
        assert_eq!(written(&path), date);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn release_prefers_the_physical_date_and_falls_back_to_the_digital_one() {
        let path = file("release");
        let physical = Utc.with_ymd_and_hms(2018, 7, 1, 0, 0, 0).unwrap();
        let digital = Utc.with_ymd_and_hms(2018, 6, 1, 0, 0, 0).unwrap();
        let mut movie = testing::movie(1, "The Movie", 2018);
        movie.metadata.in_cinemas = Some(Utc.with_ymd_and_hms(2018, 3, 9, 0, 0, 0).unwrap());
        movie.metadata.physical_release = Some(physical);
        movie.metadata.digital_release = Some(digital);
        let config = config(FileDateType::Release);
        assert!(change_file_date(&config, &path.to_string_lossy(), &movie));
        assert_eq!(written(&path), physical);
        movie.metadata.physical_release = None;
        assert!(change_file_date(&config, &path.to_string_lossy(), &movie));
        assert_eq!(written(&path), digital);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_movie_without_the_date_changes_nothing() {
        let path = file("missing");
        let before = written(&path);
        let mut movie = testing::movie(1, "The Movie", 2018);
        movie.metadata.in_cinemas = None;
        movie.metadata.physical_release = None;
        movie.metadata.digital_release = None;
        for kind in [FileDateType::Cinemas, FileDateType::Release] {
            assert!(!change_file_date(&config(kind), &path.to_string_lossy(), &movie));
        }
        assert_eq!(written(&path), before);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_date_before_1970_stops_at_1970() {
        let path = file("epoch");
        let mut movie = testing::movie(1, "The Movie", 1942);
        movie.metadata.in_cinemas = Some(Utc.with_ymd_and_hms(1942, 11, 26, 0, 0, 0).unwrap());
        assert!(change_file_date(
            &config(FileDateType::Cinemas),
            &path.to_string_lossy(),
            &movie
        ));
        assert_eq!(written(&path), DateTime::<Utc>::UNIX_EPOCH);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn the_comparison_is_by_utc_day() {
        let date = Utc.with_ymd_and_hms(2018, 3, 9, 0, 0, 0).unwrap();
        // Written on the wanted day, at midnight or later: nothing to do.
        assert!(!differs(date, date));
        assert!(!differs(date + chrono::Duration::hours(23), date));
        // Another day, either side.
        assert!(differs(date - chrono::Duration::seconds(1), date));
        assert!(differs(date + chrono::Duration::days(1), date));
        // A wanted date with a time of day never equals a day at midnight.
        assert!(differs(date, date + chrono::Duration::hours(5)));
    }

    #[test]
    fn a_date_already_set_is_not_set_again() {
        let path = file("twice");
        let mut movie = testing::movie(1, "The Movie", 2018);
        movie.metadata.in_cinemas = Some(Utc.with_ymd_and_hms(2018, 3, 9, 0, 0, 0).unwrap());
        let config = config(FileDateType::Cinemas);
        assert!(change_file_date(&config, &path.to_string_lossy(), &movie));
        assert!(!change_file_date(&config, &path.to_string_lossy(), &movie));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_date_that_cannot_be_set_is_not_reported_as_changed() {
        let mut movie = testing::movie(1, "The Movie", 2018);
        movie.metadata.in_cinemas = Some(Utc.with_ymd_and_hms(2018, 3, 9, 0, 0, 0).unwrap());
        // A file that is not there: nothing to read a date from.
        assert!(!change_file_date(
            &config(FileDateType::Cinemas),
            "/nonexistent/unarr/movie.mkv",
            &movie
        ));
        // A refusal by the file system is an error of the same call.
        let refused =
            transfer::try_set_last_write_time(Path::new("/nonexistent/unarr/movie.mkv"), Utc::now());
        assert!(refused.is_err());
    }
}
