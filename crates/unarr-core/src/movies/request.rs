//! Port of Radarr's `NewznabRequestGenerator`: what an indexer is asked
//! about movies. RSS is `t=movie` over the configured categories; a search
//! goes by TMDB and IMDb id where the indexer's `movie-search` takes them,
//! and by title and year as a second tier, tried only when the first found
//! nothing. The URLs themselves (categories, key, paging) are built by the
//! shared generator.

use crate::indexers::request::{clean_scene_title, newznabify_title, RequestChain, RequestGenerator};

/// `MovieSearchCriteria`: the movie as an indexer can be asked for it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MovieSearchCriteria {
    pub tmdb_id: i64,
    /// With its `tt`.
    pub imdb_id: Option<String>,
    pub year: i64,
    /// The title, the original title and the alternative ones.
    pub scene_titles: Vec<String>,
}

impl MovieSearchCriteria {
    /// `CleanSceneTitles`: distinct cleaned titles.
    pub fn clean_scene_titles(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for title in &self.scene_titles {
            let clean = clean_scene_title(title);
            if !out.contains(&clean) {
                out.push(clean);
            }
        }
        out
    }
}

/// `GetRecentRequests`. Some indexers forget to enable movie search while
/// the plain one works, so that is asked instead.
pub fn recent_requests(generator: &RequestGenerator<'_>) -> RequestChain {
    let search_type = if generator.caps.supported_movie_search_parameters.is_some() {
        "movie"
    } else if generator.caps.supported_search_parameters.is_some() {
        "search"
    } else {
        return vec![Vec::new()];
    };
    let tier = generator
        .paged_requests(&generator.settings.categories, search_type, "")
        .into_iter()
        .collect();
    vec![tier]
}

/// `GetSearchRequests(MovieSearchCriteria)`.
pub fn search_requests(generator: &RequestGenerator<'_>, criteria: &MovieSearchCriteria) -> RequestChain {
    let caps = generator.caps;
    let categories = &generator.settings.categories;
    let imdb = criteria.imdb_id.as_deref().filter(|id| !id.trim().is_empty());
    let include_tmdb = caps.supports_movie_param("tmdbid") && criteria.tmdb_id > 0;
    let include_imdb = caps.supports_movie_param("imdbid") && imdb.is_some();
    let tmdb_parameter = format!("&tmdbid={}", criteria.tmdb_id);
    // `ImdbId.Substring(2)`: the id without its `tt`.
    let imdb_parameter = format!(
        "&imdbid={}",
        imdb.map(|id| id.chars().skip(2).collect::<String>())
            .unwrap_or_default()
    );

    let mut ids = Vec::new();
    if caps.supports_aggregate_movie_id_search && (include_tmdb || include_imdb) {
        let mut parameters = String::new();
        if include_tmdb {
            parameters.push_str(&tmdb_parameter);
        }
        if include_imdb {
            parameters.push_str(&imdb_parameter);
        }
        ids.extend(generator.paged_requests(categories, "movie", &parameters));
    } else if include_tmdb {
        ids.extend(generator.paged_requests(categories, "movie", &tmdb_parameter));
    } else if include_imdb {
        ids.extend(generator.paged_requests(categories, "movie", &imdb_parameter));
    }
    let mut chain = vec![ids];

    if caps.supports_search_param("q") && criteria.year > 0 {
        let titles = if caps.text_search_engine == "raw" {
            criteria.scene_titles.clone()
        } else {
            criteria.clean_scene_titles()
        };
        let mut text = Vec::new();
        for title in titles {
            let mut query = title;
            if !generator.settings.remove_year {
                query.push_str(&format!(" {}", criteria.year));
            }
            text.extend(generator.paged_requests(
                categories,
                "search",
                &format!("&q={}", newznabify_title(&query)),
            ));
        }
        chain.push(text);
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexers::caps::{parse_caps, Capabilities};
    use crate::indexers::settings::NewznabSettings;

    fn settings() -> NewznabSettings {
        NewznabSettings {
            base_url: "http://indexer.invalid/7/".into(),
            api_key: "k".into(),
            categories: vec![2000, 2010, 2040],
            ..NewznabSettings::default()
        }
    }

    fn generator<'a>(settings: &'a NewznabSettings, caps: &'a Capabilities) -> RequestGenerator<'a> {
        RequestGenerator {
            settings,
            caps,
            page_size: 100,
            max_pages: 2,
        }
    }

    fn caps(movie_params: &str) -> Capabilities {
        parse_caps(
            &format!(
                r#"<caps><searching><search available="yes" supportedParams="q"/>
                <movie-search available="yes" supportedParams="{movie_params}"/></searching></caps>"#
            ),
            "http://indexer.invalid/7/api?t=caps",
        )
        .unwrap()
    }

    fn holdovers() -> MovieSearchCriteria {
        MovieSearchCriteria {
            tmdb_id: 840430,
            imdb_id: Some("tt14849194".into()),
            year: 2023,
            scene_titles: vec!["The Holdovers".into(), "The Holdovers".into()],
        }
    }

    // The shapes are those of the requests the maintainer's Radarr sent to
    // its indexers, read from its trace log: paging before the ids, both
    // ids in one request, the text search in a tier of its own.
    #[test]
    fn rss_asks_for_movies_in_the_categories() {
        let (s, c) = (settings(), caps("q,imdbid,tmdbid"));
        let chain = recent_requests(&generator(&s, &c));
        assert_eq!(
            chain,
            vec![vec![vec![
                "http://indexer.invalid/7/api?t=movie&cat=2000,2010,2040&extended=1&apikey=k&offset=0&limit=100"
                    .to_string(),
                "http://indexer.invalid/7/api?t=movie&cat=2000,2010,2040&extended=1&apikey=k&offset=100&limit=100"
                    .to_string(),
            ]]]
        );
    }

    #[test]
    fn a_search_goes_by_both_ids_then_by_title_and_year() {
        let (s, c) = (settings(), caps("q,imdbid,tmdbid"));
        let chain = search_requests(&generator(&s, &c), &holdovers());
        assert_eq!(chain.len(), 2);
        assert_eq!(
            chain[0][0][0],
            "http://indexer.invalid/7/api?t=movie&cat=2000,2010,2040&extended=1&apikey=k&offset=0&limit=100&tmdbid=840430&imdbid=14849194"
        );
        assert_eq!(chain[1].len(), 1, "the two equal titles are asked once");
        assert_eq!(
            chain[1][0][0],
            "http://indexer.invalid/7/api?t=search&cat=2000,2010,2040&extended=1&apikey=k&offset=0&limit=100&q=Holdovers%202023"
        );
    }

    #[test]
    fn an_indexer_that_takes_one_id_gets_that_one() {
        let (s, c) = (settings(), caps("q,imdbid"));
        let chain = search_requests(&generator(&s, &c), &holdovers());
        assert!(chain[0][0][0].ends_with("&offset=0&limit=100&imdbid=14849194"));
        let mut no_imdb = holdovers();
        no_imdb.imdb_id = None;
        let chain = search_requests(&generator(&s, &c), &no_imdb);
        assert!(chain[0].is_empty());
        assert_eq!(chain[1].len(), 1);
    }

    #[test]
    fn the_year_can_be_left_out_and_is_needed_for_a_text_search() {
        let (mut s, c) = (settings(), caps("q,imdbid,tmdbid"));
        s.remove_year = true;
        let chain = search_requests(&generator(&s, &c), &holdovers());
        assert!(chain[1][0][0].ends_with("&q=Holdovers"));
        let mut no_year = holdovers();
        no_year.year = 0;
        assert_eq!(search_requests(&generator(&s, &c), &no_year).len(), 1);
    }

    #[test]
    fn without_movie_search_rss_falls_back_on_the_plain_one() {
        let s = settings();
        let c = parse_caps(
            r#"<caps><searching><search available="yes" supportedParams="q"/></searching></caps>"#,
            "http://indexer.invalid/7/api?t=caps",
        )
        .unwrap();
        assert!(c.supported_movie_search_parameters.is_none());
        let chain = recent_requests(&generator(&s, &c));
        assert!(chain[0][0][0].contains("?t=search&cat=2000,2010,2040&"));
    }
}
