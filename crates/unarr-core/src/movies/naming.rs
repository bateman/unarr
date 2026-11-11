//! The name of a movie's folder, as the originals' `FileNameBuilder`
//! derives it from the movie folder format. Collections show it for the
//! movies they name; adding a movie will create it. The file name, with
//! its quality, media info and custom format tokens, comes with the
//! milestone that renames.

use std::sync::OnceLock;

use super::model::MovieMetadata;
use crate::naming::{clean_folder_name, clean_title, title_the};

/// A token of a format: `{prefix token suffix}`, the prefix and suffix
/// (`(`, `[`, `-`, blanks...) kept only when the token has a value.
fn token_re() -> &'static fancy_regex::Regex {
    static RE: OnceLock<fancy_regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        fancy_regex::RegexBuilder::new(
            r"\{(?<prefix>[- ._\[(]*)(?<token>(?:[a-z0-9]+)(?:(?<separator>[- ._]+)(?:[a-z0-9]+))?)(?::(?<customFormat>[ ,a-z0-9+-]+(?<![- ])))?(?<suffix>[- ._)\]]*)\}",
        )
        .case_insensitive(true)
        .build()
        .expect("the token pattern compiles")
    })
}

/// The value of a movie token, by its name without blanks, dots, dashes
/// and underscores, lower case; `None` for a token of another kind.
fn movie_token(name: &str, md: &MovieMetadata) -> Option<String> {
    let title = md.title.as_str();
    Some(match name {
        "movietitle" => title.to_string(),
        "moviecleantitle" => clean_title(title),
        "movietitlethe" => title_the(title),
        "moviecleantitlethe" => clean_title(&title_the(title)),
        "movieoriginaltitle" => md.original_title.clone().unwrap_or_default(),
        "moviecleanoriginaltitle" => clean_title(md.original_title.as_deref().unwrap_or("")),
        "movietitlefirstcharacter" => crate::naming::title_first_character(&title_the(title)),
        "moviecertification" => md.certification.clone().unwrap_or_default(),
        "moviecollection" => md.collection_title.clone().unwrap_or_default(),
        // A movie without a year yet has none to show.
        "releaseyear" => {
            if md.year > 0 {
                md.year.to_string()
            } else {
                String::new()
            }
        }
        "imdbid" => md.imdb_id.clone().unwrap_or_default(),
        "tmdbid" => md.tmdb_id.to_string(),
        _ => return None,
    })
}

/// `GetMovieFolder`: the folder format with the movie's tokens replaced.
pub fn movie_folder(format: &str, md: &MovieMetadata) -> String {
    let replaced = token_re().replace_all(format, |caps: &fancy_regex::Captures<'_, str>| {
        let token: String = caps["token"]
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match movie_token(&token, md) {
            Some(value) if value.is_empty() => String::new(),
            Some(value) => format!("{}{}{}", &caps["prefix"], value, &caps["suffix"]),
            // Not a movie token: left for nobody, as the originals leave it.
            None => caps[0].to_string(),
        }
    });
    clean_folder_name(&replaced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movies::model::MovieStatus;

    fn metadata(title: &str, year: i64, tmdb_id: i64) -> MovieMetadata {
        MovieMetadata {
            id: 1,
            tmdb_id,
            imdb_id: Some("tt0000001".into()),
            title: title.into(),
            sort_title: None,
            clean_title: None,
            original_title: None,
            clean_original_title: None,
            original_language_id: 1,
            status: MovieStatus::Released,
            overview: None,
            images: Vec::new(),
            genres: Vec::new(),
            keywords: Vec::new(),
            ratings: serde_json::json!({}),
            recommendations: Vec::new(),
            runtime: 0,
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
        }
    }

    #[test]
    fn folders_as_the_original_names_them() {
        let format = "{Movie CleanTitle} ({Release Year}) {tmdb-{TmdbId}}";
        assert_eq!(
            movie_folder(format, &metadata("Alien: Romulus", 2024, 945961)),
            "Alien Romulus (2024) {tmdb-945961}"
        );
        assert_eq!(
            movie_folder(format, &metadata("Candyman: Day of the Dead", 1999, 12485)),
            "Candyman Day of the Dead (1999) {tmdb-12485}"
        );
        // No year yet: the parentheses of the format stay, empty.
        assert_eq!(
            movie_folder(format, &metadata("Untitled Alien: Romulus Sequel", 0, 1434936)),
            "Untitled Alien Romulus Sequel () {tmdb-1434936}"
        );
        assert_eq!(
            movie_folder(
                "{Movie TitleThe} {(Release Year)}",
                &metadata("The Thing", 1982, 1091)
            ),
            "Thing, The (1982)"
        );
        assert_eq!(
            movie_folder("{Movie Title} {(Release Year)}", &metadata("Soon", 0, 1)),
            "Soon"
        );
    }
}
