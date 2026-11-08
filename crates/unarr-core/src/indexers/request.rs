//! Port of the originals' `NewznabRequestGenerator`: the URLs an indexer is
//! asked, for RSS and for every kind of search, in tiers (a later tier is
//! tried only when the earlier ones found nothing) of pageable requests.

use chrono::NaiveDate;

use crate::indexers::caps::Capabilities;
use crate::indexers::settings::NewznabSettings;
use crate::parser::title::remove_accent;

pub const MAX_PAGES: i64 = 30;

/// `SearchMode` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Id search first, then title search as a second tier.
    Default,
    SearchId,
    SearchTitle,
    /// Both at once (the originals' `SearchID | SearchTitle`).
    Both,
}

impl SearchMode {
    fn has_id(self) -> bool {
        matches!(self, SearchMode::SearchId | SearchMode::Both)
    }
    fn has_title(self) -> bool {
        matches!(self, SearchMode::SearchTitle | SearchMode::Both)
    }
}

/// The ids a series is known by.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SeriesIds {
    pub tvdb_id: i64,
    pub imdb_id: Option<String>,
    pub tv_rage_id: i64,
    pub tv_maze_id: i64,
    pub tmdb_id: i64,
}

/// What is being searched for.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchKind {
    SingleEpisode {
        season: i64,
        episode: i64,
    },
    Season {
        season: i64,
    },
    DailyEpisode {
        air_date: NaiveDate,
    },
    DailySeason {
        year: i64,
    },
    AnimeEpisode {
        absolute_episode: i64,
        season: i64,
        episode: i64,
    },
    AnimeSeason {
        season: i64,
    },
    Special {
        episode_query_titles: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchCriteria {
    pub series: SeriesIds,
    pub scene_titles: Vec<String>,
    pub mode: SearchMode,
    pub kind: SearchKind,
}

impl SearchCriteria {
    /// `CleanSceneTitles`: distinct cleaned titles.
    pub fn clean_scene_titles(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in &self.scene_titles {
            let c = clean_scene_title(t);
            if !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }

    /// `AllSceneTitles`: raw and cleaned, distinct.
    pub fn all_scene_titles(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for t in self.scene_titles.iter().cloned().chain(self.clean_scene_titles()) {
            if !out.contains(&t) {
                out.push(t);
            }
        }
        out
    }
}

/// `SearchCriteriaBase.GetCleanSceneTitle`.
pub fn clean_scene_title(title: &str) -> String {
    static BEGINNING_THE: crate::parser::netregex::Lazy = crate::net_regex!(r"^the\s");
    static SPECIAL: crate::parser::netregex::Lazy = crate::net_regex!(r"['.`´‘’]");
    static NON_WORD: crate::parser::netregex::Lazy = crate::net_regex!(r"[\W]");
    static REPEATED_PLUS: crate::parser::netregex::Lazy = crate::net_regex!(r"\+{2,}", case_sensitive);
    let mut t = BEGINNING_THE.replace_all(title, "");
    t = t.replace('&', "and");
    t = SPECIAL.replace_all(&t, "");
    t = NON_WORD.replace_all(&t, "+");
    t = REPEATED_PLUS.replace_all(&t, "+");
    t = remove_accent(&t);
    t.trim_matches(['+', ' ']).to_string()
}

/// A chain of tiers; each tier is a list of pageable requests; each
/// pageable request is the list of page URLs in order.
pub type RequestChain = Vec<Vec<Vec<String>>>;

pub struct RequestGenerator<'a> {
    pub settings: &'a NewznabSettings,
    pub caps: &'a Capabilities,
    pub page_size: i64,
    pub max_pages: i64,
}

/// `Uri.EscapeDataString`.
pub fn escape_data_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `HttpUtility.UrlEncode`: like above but spaces become `+` and `!*()`
/// stay.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'*' | b'(' | b')' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02x}")),
        }
    }
    out
}

fn newznabify_title(title: &str) -> String {
    escape_data_string(&title.replace('+', " "))
}

/// NNTMux treats `season=0` as null; `00` works everywhere.
fn newznabify_season(season: i64) -> String {
    if season == 0 {
        "00".into()
    } else {
        season.to_string()
    }
}

impl RequestGenerator<'_> {
    fn supports_search(&self) -> bool {
        self.caps.supports_search_param("q")
    }
    fn supports_tv_query_search(&self) -> bool {
        self.caps.supports_tv_param("q")
    }
    fn supports_tv_title_search(&self) -> bool {
        self.caps.supports_tv_param("title")
    }
    fn supports_tv_text_searches(&self) -> bool {
        self.supports_tv_query_search() || self.supports_tv_title_search()
    }
    fn supports_tv_id_searches(&self) -> bool {
        ["tvdbid", "imdbid", "rid", "tvmazeid", "tmdbid"]
            .iter()
            .any(|p| self.caps.supports_tv_param(p))
    }
    fn supports_season_search(&self) -> bool {
        self.caps.supports_tv_param("season")
    }
    fn supports_episode_search(&self) -> bool {
        self.caps.supports_tv_param("season") && self.caps.supports_tv_param("ep")
    }

    /// `GetRecentRequests`.
    pub fn recent_requests(&self) -> RequestChain {
        let mut tier = Vec::new();
        if self.caps.supported_tv_search_parameters.is_some() {
            let cats: Vec<i64> = self
                .settings
                .categories
                .iter()
                .chain(self.settings.anime_categories.iter())
                .copied()
                .collect();
            if let Some(pages) = self.paged_requests(&cats, "tvsearch", "") {
                tier.push(pages);
            }
        } else if self.caps.supported_search_parameters.is_some() {
            if let Some(pages) = self.paged_requests(&self.settings.anime_categories, "search", "") {
                tier.push(pages);
            }
        }
        vec![tier]
    }

    /// `GetSearchRequests` for every criteria kind.
    pub fn search_requests(&self, criteria: &SearchCriteria) -> RequestChain {
        match &criteria.kind {
            SearchKind::SingleEpisode { season, episode } => {
                if !self.supports_episode_search()
                    || (!self.supports_tv_text_searches() && !self.supports_tv_id_searches())
                {
                    return Vec::new();
                }
                self.standard_search(
                    criteria,
                    &format!("&season={}&ep={episode}", newznabify_season(*season)),
                )
            }
            SearchKind::Season { season } => {
                if !self.supports_season_search()
                    || (!self.supports_tv_text_searches() && !self.supports_tv_id_searches())
                {
                    return Vec::new();
                }
                self.standard_search(criteria, &format!("&season={}", newznabify_season(*season)))
            }
            SearchKind::DailyEpisode { air_date } => {
                if !self.supports_episode_search()
                    || (!self.supports_tv_text_searches() && !self.supports_tv_id_searches())
                {
                    return Vec::new();
                }
                self.standard_search(
                    criteria,
                    &format!(
                        "&season={}&ep={}/{}",
                        air_date.format("%Y"),
                        air_date.format("%m"),
                        air_date.format("%d")
                    ),
                )
            }
            SearchKind::DailySeason { year } => {
                if !self.supports_episode_search()
                    || (!self.supports_tv_text_searches() && !self.supports_tv_id_searches())
                {
                    return Vec::new();
                }
                self.standard_search(criteria, &format!("&season={year}"))
            }
            SearchKind::AnimeEpisode {
                absolute_episode,
                season,
                episode,
            } => {
                let mut tier = Vec::new();
                if self.supports_search() {
                    self.add_tv_id_requests(
                        &mut tier,
                        &self.settings.anime_categories,
                        &criteria.series,
                        &format!("&q={absolute_episode:02}"),
                    );
                    let include_standard =
                        self.settings.anime_standard_format_search && *season > 0 && *episode > 0;
                    if include_standard && self.supports_episode_search() {
                        self.add_tv_id_requests(
                            &mut tier,
                            &self.settings.anime_categories,
                            &criteria.series,
                            &format!("&season={}&ep={episode}", newznabify_season(*season)),
                        );
                    }
                    let titles = if self.caps.text_search_engine == "raw" {
                        criteria.all_scene_titles()
                    } else {
                        criteria.clean_scene_titles()
                    };
                    for title in titles {
                        if let Some(p) = self.paged_requests(
                            &self.settings.anime_categories,
                            "search",
                            &format!("&q={}+{absolute_episode:02}", newznabify_title(&title)),
                        ) {
                            tier.push(p);
                        }
                        if include_standard && self.supports_episode_search() {
                            if let Some(p) = self.paged_requests(
                                &self.settings.anime_categories,
                                "tvsearch",
                                &format!(
                                    "&q={}&season={}&ep={episode}",
                                    newznabify_title(&title),
                                    newznabify_season(*season)
                                ),
                            ) {
                                tier.push(p);
                            }
                        }
                    }
                }
                vec![tier]
            }
            SearchKind::AnimeSeason { season } => {
                let mut tier = Vec::new();
                if self.supports_search() && self.settings.anime_standard_format_search && *season > 0 {
                    self.add_tv_id_requests(
                        &mut tier,
                        &self.settings.anime_categories,
                        &criteria.series,
                        &format!("&season={}", newznabify_season(*season)),
                    );
                    let titles = if self.caps.text_search_engine == "raw" {
                        criteria.all_scene_titles()
                    } else {
                        criteria.clean_scene_titles()
                    };
                    for title in titles {
                        if let Some(p) = self.paged_requests(
                            &self.settings.anime_categories,
                            "tvsearch",
                            &format!(
                                "&q={}&season={}",
                                newznabify_title(&title),
                                newznabify_season(*season)
                            ),
                        ) {
                            tier.push(p);
                        }
                    }
                }
                vec![tier]
            }
            SearchKind::Special { episode_query_titles } => {
                let mut tier = Vec::new();
                if self.supports_search() {
                    let cats: Vec<i64> = self
                        .settings
                        .categories
                        .iter()
                        .chain(self.settings.anime_categories.iter())
                        .copied()
                        .collect();
                    for title in episode_query_titles {
                        let query = url_encode(&title.replace('+', " "));
                        if let Some(p) = self.paged_requests(&cats, "search", &format!("&q={query}")) {
                            tier.push(p);
                        }
                    }
                }
                vec![tier]
            }
        }
    }

    /// Standard and daily searches: ids and/or titles per the mode, then the
    /// title search as a fallback tier in the default mode.
    fn standard_search(&self, criteria: &SearchCriteria, parameters: &str) -> RequestChain {
        let mut first = Vec::new();
        if criteria.mode.has_id() || criteria.mode == SearchMode::Default {
            self.add_tv_id_requests(
                &mut first,
                &self.settings.categories,
                &criteria.series,
                parameters,
            );
        }
        if criteria.mode.has_title() {
            self.add_title_requests(&mut first, criteria, parameters);
        }
        let mut second = Vec::new();
        if criteria.mode == SearchMode::Default {
            self.add_title_requests(&mut second, criteria, parameters);
        }
        vec![first, second]
    }

    fn add_tv_id_requests(
        &self,
        tier: &mut Vec<Vec<String>>,
        categories: &[i64],
        series: &SeriesIds,
        parameters: &str,
    ) {
        let tvdb = self.caps.supports_tv_param("tvdbid") && series.tvdb_id > 0;
        let imdb = self.caps.supports_tv_param("imdbid")
            && series.imdb_id.as_deref().is_some_and(|s| !s.trim().is_empty());
        let rage = self.caps.supports_tv_param("rid") && series.tv_rage_id > 0;
        let maze = self.caps.supports_tv_param("tvmazeid") && series.tv_maze_id > 0;
        let tmdb = self.caps.supports_tv_param("tmdbid") && series.tmdb_id > 0;
        let imdb_id = series.imdb_id.clone().unwrap_or_default();

        if self.caps.supports_aggregate_id_search && (tvdb || rage || maze || tmdb) {
            let mut ids = String::new();
            if tvdb {
                ids.push_str(&format!("&tvdbid={}", series.tvdb_id));
            }
            if imdb {
                ids.push_str(&format!("&imdbid={imdb_id}"));
            }
            if rage {
                ids.push_str(&format!("&rid={}", series.tv_rage_id));
            }
            if maze {
                ids.push_str(&format!("&tvmazeid={}", series.tv_maze_id));
            }
            if tmdb {
                ids.push_str(&format!("&tmdbid={}", series.tmdb_id));
            }
            if let Some(p) = self.paged_requests(categories, "tvsearch", &format!("{ids}{parameters}")) {
                tier.push(p);
            }
            return;
        }
        let single = if tvdb {
            Some(format!("&tvdbid={}{parameters}", series.tvdb_id))
        } else if imdb {
            Some(format!("&imdbid={imdb_id}{parameters}"))
        } else if rage {
            Some(format!("&rid={}{parameters}", series.tv_rage_id))
        } else if maze {
            Some(format!("&tvmazeid={}{parameters}", series.tv_maze_id))
        } else if tmdb {
            Some(format!("&tmdbid={}{parameters}", series.tmdb_id))
        } else {
            None
        };
        if let Some(params) = single {
            if let Some(p) = self.paged_requests(categories, "tvsearch", &params) {
                tier.push(p);
            }
        }
    }

    fn add_title_requests(&self, tier: &mut Vec<Vec<String>>, criteria: &SearchCriteria, parameters: &str) {
        if self.supports_tv_title_search() {
            for term in &criteria.scene_titles {
                if let Some(p) = self.paged_requests(
                    &self.settings.categories,
                    "tvsearch",
                    &format!("&title={}{parameters}", escape_data_string(term)),
                ) {
                    tier.push(p);
                }
            }
        } else if self.supports_tv_query_search() {
            let titles = if self.caps.tv_text_search_engine == "raw" {
                criteria.all_scene_titles()
            } else {
                criteria.clean_scene_titles()
            };
            for title in titles {
                if let Some(p) = self.paged_requests(
                    &self.settings.categories,
                    "tvsearch",
                    &format!("&q={}{parameters}", newznabify_title(&title)),
                ) {
                    tier.push(p);
                }
            }
        }
    }

    /// `GetPagedRequests`: the page URLs of one query, `None` without
    /// categories.
    fn paged_requests(&self, categories: &[i64], search_type: &str, parameters: &str) -> Option<Vec<String>> {
        if categories.is_empty() {
            return None;
        }
        let mut distinct: Vec<i64> = Vec::new();
        for c in categories {
            if !distinct.contains(c) {
                distinct.push(*c);
            }
        }
        let cats: Vec<String> = distinct.iter().map(|c| c.to_string()).collect();
        let mut base = format!(
            "{}{}?t={search_type}&cat={}&extended=1{}",
            self.settings.base_url.trim_end_matches('/'),
            self.settings.api_path.trim_end_matches('/'),
            cats.join(","),
            self.settings.additional_parameters
        );
        if !self.settings.api_key.trim().is_empty() {
            base.push_str(&format!("&apikey={}", self.settings.api_key));
        }
        if self.page_size == 0 {
            return Some(vec![format!("{base}{parameters}")]);
        }
        Some(
            (0..self.max_pages)
                .map(|page| {
                    format!(
                        "{base}&offset={}&limit={}{parameters}",
                        page * self.page_size,
                        self.page_size
                    )
                })
                .collect(),
        )
    }
}

/// `caps` URL for the settings.
pub fn caps_url(settings: &NewznabSettings) -> String {
    let mut url = format!(
        "{}{}?t=caps",
        settings.base_url.trim_end_matches('/'),
        settings.api_path.trim_end_matches('/')
    );
    if !settings.api_key.trim().is_empty() {
        url.push_str(&format!("&apikey={}", settings.api_key));
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings() -> NewznabSettings {
        NewznabSettings::from_value(&json!({
            "baseUrl": "http://127.0.0.1:18989/newznab", "apiPath": "/api", "apiKey": "0123456789abcdef0123456789abcdef",
            "categories": [5030, 5040, 5045], "animeCategories": [5070]
        }))
    }

    fn criteria(kind: SearchKind) -> SearchCriteria {
        SearchCriteria {
            series: SeriesIds {
                tvdb_id: 448176,
                imdb_id: Some("tt31938062".into()),
                tv_rage_id: 0,
                tv_maze_id: 75632,
                tmdb_id: 0,
            },
            scene_titles: vec!["The Pitt".into()],
            mode: SearchMode::Default,
            kind,
        }
    }

    #[test]
    fn recent_request_matches_what_sonarr_sent_the_mock() {
        let s = settings();
        let caps = Capabilities::default();
        let g = RequestGenerator {
            settings: &s,
            caps: &caps,
            page_size: 100,
            max_pages: MAX_PAGES,
        };
        let chain = g.recent_requests();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].len(), 1);
        assert_eq!(chain[0][0].len(), 30);
        assert_eq!(
            chain[0][0][0],
            "http://127.0.0.1:18989/newznab/api?t=tvsearch&cat=5030,5040,5045,5070&extended=1&apikey=0123456789abcdef0123456789abcdef&offset=0&limit=100"
        );
        assert_eq!(
            caps_url(&s),
            "http://127.0.0.1:18989/newznab/api?t=caps&apikey=0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn episode_search_uses_ids_then_titles() {
        let s = settings();
        let caps_xml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/indexers/newznab/caps.xml"
        ))
        .unwrap();
        let caps = crate::indexers::caps::parse_caps(&caps_xml, "u").unwrap();
        let g = RequestGenerator {
            settings: &s,
            caps: &caps,
            page_size: 100,
            max_pages: 2,
        };
        let chain = g.search_requests(&criteria(SearchKind::SingleEpisode {
            season: 1,
            episode: 1,
        }));
        assert_eq!(chain.len(), 2);
        assert_eq!(
            chain[0][0][0],
            "http://127.0.0.1:18989/newznab/api?t=tvsearch&cat=5030,5040,5045&extended=1&apikey=0123456789abcdef0123456789abcdef&offset=0&limit=100&tvdbid=448176&imdbid=tt31938062&tvmazeid=75632&season=1&ep=1"
        );
        assert_eq!(
            chain[1][0][0],
            "http://127.0.0.1:18989/newznab/api?t=tvsearch&cat=5030,5040,5045&extended=1&apikey=0123456789abcdef0123456789abcdef&offset=0&limit=100&q=Pitt&season=1&ep=1"
        );
        let daily = g.search_requests(&criteria(SearchKind::DailyEpisode {
            air_date: NaiveDate::from_ymd_opt(2024, 4, 30).unwrap(),
        }));
        assert!(daily[0][0][0].ends_with("&season=2024&ep=04/30"));
        assert_eq!(clean_scene_title("The Pitt's & Co. (2025)"), "Pitts+and+Co+2025");
        assert_eq!(escape_data_string("a b/c"), "a%20b%2Fc");
    }
}
