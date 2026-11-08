//! Newznab and Torznab settings as stored in a provider's `settings` JSON,
//! with the originals' defaults and validation rules.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::net_regex;
use crate::parser::netregex::Lazy;
use crate::validation::ValidationFailure;

/// `SeedCriteriaSettings`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SeedCriteria {
    pub seed_ratio: Option<f64>,
    pub seed_time: Option<i64>,
    pub season_pack_seed_time: Option<i64>,
}

/// `NewznabSettings`, which `TorznabSettings` extends with the torrent
/// fields; the two are stored in the same shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewznabSettings {
    pub base_url: String,
    pub api_path: String,
    pub api_key: String,
    pub categories: Vec<i64>,
    pub anime_categories: Vec<i64>,
    pub anime_standard_format_search: bool,
    pub additional_parameters: String,
    pub multi_languages: Vec<i64>,
    pub fail_downloads: Vec<i64>,
    // Torznab only
    pub minimum_seeders: i64,
    pub seed_criteria: SeedCriteria,
    pub reject_blocklisted_torrent_hashes_while_grabbing: bool,
}

impl Default for NewznabSettings {
    fn default() -> Self {
        NewznabSettings {
            base_url: String::new(),
            api_path: "/api".into(),
            api_key: String::new(),
            categories: vec![5030, 5040],
            anime_categories: Vec::new(),
            anime_standard_format_search: false,
            additional_parameters: String::new(),
            multi_languages: Vec::new(),
            fail_downloads: Vec::new(),
            minimum_seeders: 1,
            seed_criteria: SeedCriteria::default(),
            reject_blocklisted_torrent_hashes_while_grabbing: false,
        }
    }
}

/// Indexers whose API needs a key, so an empty one is an error.
const API_KEY_WHITE_LIST: &[&str] = &[
    "nzbs.org",
    "nzb.su",
    "dognzb.cr",
    "nzbplanet.net",
    "nzbid.org",
    "nzbndx.com",
    "nzbindex.in",
];

static ADDITIONAL_PARAMETERS_RE: Lazy = net_regex!(r"(&.+?\=.+?)+", case_sensitive);
static URL_BASE_RE: Lazy = net_regex!(r"^(?!\/?https?://[-_a-z0-9.]+)");

impl NewznabSettings {
    /// Read the stored JSON leniently: missing keys take the defaults,
    /// numbers given as strings are accepted, as the originals' schema
    /// reader converts them.
    pub fn from_value(v: &Value) -> NewznabSettings {
        let mut lenient = v.clone();
        if let Some(obj) = lenient.as_object_mut() {
            if let Some(Value::String(s)) = obj.get("minimumSeeders") {
                let n = s.trim().parse::<i64>().unwrap_or(0);
                obj.insert("minimumSeeders".into(), json!(n));
            }
            for key in ["categories", "animeCategories", "multiLanguages", "failDownloads"] {
                if let Some(Value::String(s)) = obj.get(key) {
                    let list: Vec<i64> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                    obj.insert(key.into(), json!(list));
                }
            }
            for key in ["baseUrl", "apiPath", "apiKey", "additionalParameters"] {
                if obj.get(key).is_some_and(Value::is_null) {
                    obj.remove(key);
                }
            }
        }
        serde_json::from_value(lenient).unwrap_or_default()
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// The originals' `IsValidUrl`: absolute, well formed, no surrounding
    /// spaces; and `ValidRootUrl` adds "starts with http".
    fn is_valid_root_url(url: &str) -> bool {
        if url.is_empty() || url.starts_with(' ') || url.ends_with(' ') {
            return false;
        }
        let lower = url.to_ascii_lowercase();
        if !lower.starts_with("http://") && !lower.starts_with("https://") {
            return false;
        }
        reqwest::Url::parse(url).is_ok_and(|u| u.host_str().is_some())
    }

    /// `NewznabSettingsValidator` / `TorznabSettingsValidator`.
    pub fn validate(&self, torznab: bool) -> Vec<ValidationFailure> {
        let mut failures = Vec::new();
        if self.categories.is_empty() && self.anime_categories.is_empty() {
            failures.push(ValidationFailure::setting(
                "",
                "Either 'Categories' or 'Anime Categories' must be provided",
                Value::Null,
            ));
        }
        if self.base_url.trim().is_empty() {
            failures.push(ValidationFailure::setting_not_empty(
                "BaseUrl",
                json!(self.base_url),
            ));
        } else if !Self::is_valid_root_url(&self.base_url) {
            failures.push(ValidationFailure::setting(
                "BaseUrl",
                "must be valid URL that starts with http(s)://",
                json!(self.base_url),
            ));
        }
        if !URL_BASE_RE.is_match(&self.api_path) {
            failures.push(ValidationFailure::setting(
                "ApiPath",
                "Must be a valid URL path (ie: '/api')",
                json!(self.api_path),
            ));
        }
        let needs_key = !torznab
            && API_KEY_WHITE_LIST
                .iter()
                .any(|host| self.base_url.to_lowercase().contains(host));
        if needs_key && self.api_key.trim().is_empty() {
            failures.push(ValidationFailure::setting_not_empty(
                "ApiKey",
                json!(self.api_key),
            ));
        }
        if !self.additional_parameters.trim().is_empty()
            && !ADDITIONAL_PARAMETERS_RE.is_match(&self.additional_parameters)
        {
            failures.push(ValidationFailure::setting(
                "AdditionalParameters",
                "'Additional Parameters' is not in the correct format.",
                json!(self.additional_parameters),
            ));
        }
        if torznab {
            let sc = &self.seed_criteria;
            if sc.seed_ratio.is_some_and(|r| r <= 0.0) {
                failures.push(ValidationFailure::warning(
                    "SeedCriteria.SeedRatio",
                    "Should be greater than zero",
                ));
            }
            if sc.seed_time.is_some_and(|t| t <= 0) {
                failures.push(ValidationFailure::warning(
                    "SeedCriteria.SeedTime",
                    "Should be greater than zero",
                ));
            }
            if sc.season_pack_seed_time.is_some_and(|t| t <= 0) {
                failures.push(ValidationFailure::warning(
                    "SeedCriteria.SeasonPackSeedTime",
                    "Should be greater than zero",
                ));
            }
        }
        failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_lenient_parsing_and_validation() {
        let s = NewznabSettings::from_value(
            &json!({"baseUrl": "http://x.invalid", "categories": "5030,5040", "minimumSeeders": "3"}),
        );
        assert_eq!(s.api_path, "/api");
        assert_eq!(s.categories, vec![5030, 5040]);
        assert_eq!(s.minimum_seeders, 3);
        assert!(s.validate(false).is_empty());

        let bad = NewznabSettings::from_value(
            &json!({"baseUrl": "ftp://nope", "apiPath": "http://x", "categories": [], "animeCategories": []}),
        );
        let failures = bad.validate(false);
        let names: Vec<&str> = failures.iter().map(|f| f.property_name.as_str()).collect();
        assert_eq!(names, vec!["", "BaseUrl", "ApiPath"]);
        assert_eq!(
            failures[1].error_message,
            "must be valid URL that starts with http(s)://"
        );

        let key = NewznabSettings::from_value(&json!({"baseUrl": "https://api.nzb.su"}));
        assert_eq!(key.validate(false)[0].property_name, "ApiKey");
        assert!(key.validate(true).is_empty());
    }
}
