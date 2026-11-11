//! Builders for the tests of the movie engine: a movie, a profile, a
//! release, with the defaults the original's fixtures start from.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::indexers::rss::ReleaseInfo;
use crate::model::{ProfileItem, QualityModel, QualityProfile};
use crate::movies::decision::RemoteMovie;
use crate::movies::model::{Movie, MovieMetadata, MovieStatus};
use crate::movies::parser::{clean_movie_title, parse_movie_title, ParsedMovieInfo};
use crate::movies::quality::MovieQuality;
use crate::parser::quality::ParsedQuality;

pub use crate::movies::decision::library::MemoryMovieLookup;

/// A released, monitored movie on profile 1 with a clean title.
pub fn movie(id: i64, title: &str, year: i64) -> Movie {
    Movie {
        id,
        metadata: MovieMetadata {
            id,
            tmdb_id: 1000 + id,
            imdb_id: Some(format!("tt{:07}", 1000 + id)),
            title: title.to_string(),
            sort_title: None,
            clean_title: Some(clean_movie_title(title)),
            original_title: None,
            clean_original_title: None,
            original_language_id: 1,
            status: MovieStatus::Released,
            overview: None,
            images: Vec::new(),
            genres: Vec::new(),
            keywords: Vec::new(),
            ratings: Value::Null,
            recommendations: Vec::new(),
            runtime: 120,
            in_cinemas: None,
            physical_release: None,
            digital_release: None,
            year,
            secondary_year: None,
            certification: None,
            youtube_trailer_id: None,
            studio: None,
            website: None,
            popularity: 0.0,
            collection_tmdb_id: 0,
            collection_title: None,
            last_info_sync: None,
        },
        path: format!("/movies/{title} ({year})"),
        monitored: true,
        quality_profile_id: 1,
        minimum_availability: MovieStatus::Announced,
        movie_file_id: 0,
        tags: Vec::new(),
        added: None,
        add_options: None,
        last_search_time: None,
    }
}

/// `QualityFixture.GetDefaultQualities`: every quality in Radarr's order,
/// the allowed ones last (all of them when none is named).
pub fn profile(allowed: &[i64], cutoff: i64, upgrade_allowed: bool) -> QualityProfile {
    let all: Vec<i64> = MovieQuality::all().map(|q| q.id).collect();
    let allowed: Vec<i64> = if allowed.is_empty() {
        all.clone()
    } else {
        allowed.to_vec()
    };
    let mut ordered: Vec<i64> = all.iter().copied().filter(|q| !allowed.contains(q)).collect();
    ordered.extend(allowed.iter().copied());
    QualityProfile {
        id: 1,
        name: "Test".into(),
        cutoff,
        upgrade_allowed,
        items: ordered
            .into_iter()
            .map(|q| ProfileItem {
                id: None,
                name: None,
                quality: Some(q),
                items: Vec::new(),
                allowed: allowed.contains(&q),
            })
            .collect(),
        format_items: Vec::new(),
        min_format_score: 0,
        cutoff_format_score: 0,
        min_upgrade_format_score: 1,
    }
}

pub fn quality(quality_id: i64, version: i64) -> QualityModel {
    QualityModel {
        quality_id,
        version,
        real: 0,
        is_repack: false,
    }
}

/// A parsed name with nothing in it but a quality.
pub fn parsed(quality: QualityModel) -> ParsedMovieInfo {
    ParsedMovieInfo {
        movie_titles: Vec::new(),
        original_title: String::new(),
        release_title: String::new(),
        simple_release_title: String::new(),
        quality: ParsedQuality {
            quality,
            ..ParsedQuality::default()
        },
        languages: Vec::new(),
        release_group: None,
        release_hash: String::new(),
        edition: String::new(),
        year: 0,
        imdb_id: String::new(),
        tmdb_id: 0,
        hardcoded_subs: None,
    }
}

pub fn release(title: &str, published: DateTime<Utc>) -> ReleaseInfo {
    ReleaseInfo {
        guid: format!("guid-{title}"),
        title: title.to_string(),
        publish_date: published,
        ..ReleaseInfo::default()
    }
}

/// A release mapped to a movie, parsed from its title when it parses.
pub fn remote(release: ReleaseInfo, movie: &Movie, quality: QualityModel) -> RemoteMovie {
    let mut info = parse_movie_title(&release.title, false).unwrap_or_else(|| parsed(quality));
    info.quality.quality = quality;
    let mut remote = RemoteMovie::new(release);
    remote.languages = info.languages.clone();
    remote.parsed = Some(info);
    remote.movie = Some(movie.clone());
    remote
}
