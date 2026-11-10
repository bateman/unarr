//! Simkl's user watchlist, after the originals' `SimklUserImport`,
//! `SimklImportBase`, `SimklUserRequestGenerator` and `SimklParser`.

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

use crate::metadata::Upstreams;
use crate::store::import_lists::ListItem;
use crate::validation::ValidationFailure;

pub const IMPLEMENTATION: &str = "SimklUserImport";

/// `SimklSettingsBase.BaseUrl`: not a field, so only a database written by
/// hand (or anonymised) holds anything else.
const DEFAULT_BASE_URL: &str = "https://api.simkl.com";

/// The Servarr team's Simkl application, as the originals send it.
const CLIENT_ID: &str = "3281c139f576b2f59c1389b22337140b6b087ee17e000e89dbafdcf20af6dac7";
const OAUTH_URL: &str = "https://simkl.com/oauth/authorize";
const REDIRECT_URI: &str = "https://auth.servarr.com/v1/simkl_sonarr/auth";
const RENEW_URI: &str = "https://auth.servarr.com/v1/simkl_sonarr/renew";

/// `SimklImportBase.MinRefreshInterval`.
pub const MIN_REFRESH_INTERVAL: Duration = Duration::hours(6);

/// `SimklUserSettings`, read from the stored settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub base_url: String,
    pub access_token: String,
    pub refresh_token: String,
    pub expires: Option<DateTime<Utc>>,
    pub list_type: i64,
    pub show_type: i64,
}

impl Settings {
    pub fn from_json(v: &Value) -> Settings {
        let text = |key: &str| v[key].as_str().unwrap_or("").to_string();
        let base = text("baseUrl");
        Settings {
            base_url: if base.trim().is_empty() {
                DEFAULT_BASE_URL.to_string()
            } else {
                base.trim_end_matches('/').to_string()
            },
            access_token: text("accessToken"),
            refresh_token: text("refreshToken"),
            expires: v["expires"]
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
                // `default(DateTime)` is what an unset date deserialises to.
                .filter(|d| d.timestamp() > -62_135_596_800),
            // `SimklUserListType.Watching` and `SimklUserShowType.Shows`.
            list_type: v["listType"].as_i64().unwrap_or(0),
            show_type: v["showType"].as_i64().unwrap_or(0),
        }
    }

    /// `SimklSettingsBaseValidator`: signed in, and with an expiry when
    /// the token can be renewed.
    pub fn validate(&self) -> Vec<ValidationFailure> {
        let mut failures = Vec::new();
        if self.access_token.trim().is_empty() {
            failures.push(ValidationFailure::setting(
                "SignIn",
                "Must authenticate with Simkl",
                json!(self.access_token),
            ));
        }
        if !self.access_token.trim().is_empty()
            && !self.refresh_token.trim().is_empty()
            && self.expires.is_none()
        {
            failures.push(ValidationFailure::setting(
                "SignIn",
                "Must authenticate with Simkl",
                json!("0001-01-01T00:00:00Z"),
            ));
        }
        failures
    }

    /// `SimklUserRequestGenerator.GetSeriesRequest`'s address.
    pub fn list_url(&self) -> String {
        let kind = if self.show_type == 1 { "anime" } else { "shows" };
        let list = match self.list_type {
            1 => "plantowatch",
            2 => "hold",
            3 => "completed",
            4 => "dropped",
            _ => "watching",
        };
        format!("{}/sync/all-items/{kind}/{list}", self.base_url)
    }
}

/// A request to Simkl that failed: the text the originals' exception
/// carries, and how long the answer asked to stay away.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub message: String,
    pub minimum_back_off: Duration,
    /// The host could not be reached at all: counted, not escalated.
    pub connection: bool,
}

/// `GET` with the application key and the user's token. Errors name the
/// address the user configured, not where the request went.
async fn get(
    client: &reqwest::Client,
    upstreams: &Upstreams,
    settings: &Settings,
    url: &str,
) -> Result<Vec<u8>, Failure> {
    let failure = |message: String| Failure {
        message,
        minimum_back_off: Duration::zero(),
        connection: false,
    };
    let response = client
        .get(upstreams.url(url))
        .header("simkl-api-key", CLIENT_ID)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", settings.access_token),
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| Failure {
            connection: e.is_connect(),
            ..failure(crate::downloadclients::transport_message(&e))
        })?;
    let status = response.status();
    if !status.is_success() {
        // `TooManyRequestsException`: an hour unless the answer says.
        let minimum_back_off = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<i64>().ok())
                .map_or(Duration::hours(1), Duration::seconds)
        } else {
            Duration::zero()
        };
        return Err(Failure {
            message: crate::downloadclients::http_error_message(status, "GET", url),
            minimum_back_off,
            connection: false,
        });
    }
    crate::http::read_body(response, crate::http::MAX_RESPONSE_BYTES)
        .await
        .map_err(|e| failure(e.to_string()))
}

/// An id as Simkl sends it: a string, sometimes a number.
fn id_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn id_number(v: &Value) -> i64 {
    id_text(v)
        .and_then(|s| s.trim().parse::<i32>().ok())
        .map_or(0, i64::from)
}

/// `SimklParser.ParseResponse`, then `IsValidItem`: the anime entries
/// first (series only, and only with a TVDB id), then the shows.
pub fn parse(body: &[u8]) -> Result<Vec<ListItem>, String> {
    let json: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let mut items = Vec::new();
    for entry in json["anime"].as_array().into_iter().flatten() {
        let show = &entry["show"];
        let tvdb_id = id_number(&show["ids"]["tvdb"]);
        let series = matches!(
            entry["anime_type"]
                .as_str()
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str(),
            "tv" | "ona" | "ova" | "special"
        );
        if tvdb_id > 0 && series {
            items.push(ListItem {
                title: show["title"].as_str().unwrap_or("").to_string(),
                imdb_id: id_text(&show["ids"]["imdb"]),
                tvdb_id,
                mal_id: id_number(&show["ids"]["mal"]),
                ..ListItem::default()
            });
        } else {
            tracing::warn!(
                "Skipping info grabbing for '{}' because it is an unsupported content type.",
                show["title"].as_str().unwrap_or("")
            );
        }
    }
    for entry in json["shows"].as_array().into_iter().flatten() {
        let show = &entry["show"];
        items.push(ListItem {
            title: show["title"].as_str().unwrap_or("").to_string(),
            tvdb_id: id_number(&show["ids"]["tvdb"]),
            imdb_id: id_text(&show["ids"]["imdb"]),
            ..ListItem::default()
        });
    }
    items.retain(|i| {
        !(i.title.trim().is_empty()
            && i.imdb_id.as_deref().is_none_or(|s| s.trim().is_empty())
            && i.tmdb_id == 0)
    });
    Ok(items)
}

/// The list as Simkl has it now.
pub async fn fetch_items(
    client: &reqwest::Client,
    upstreams: &Upstreams,
    settings: &Settings,
) -> Result<Vec<ListItem>, Failure> {
    let body = get(client, upstreams, settings, &settings.list_url()).await?;
    parse(&body).map_err(|message| Failure {
        message,
        minimum_back_off: Duration::zero(),
        connection: false,
    })
}

/// `GetLastActivity`: when the user's shows last changed; `now` when
/// Simkl does not say.
pub async fn last_activity(
    client: &reqwest::Client,
    upstreams: &Upstreams,
    settings: &Settings,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let url = format!("{}/sync/activities", settings.base_url);
    let body = match get(client, upstreams, settings, &url).await {
        Ok(body) => body,
        Err(_) => {
            tracing::warn!("Error fetching user activity");
            return now;
        }
    };
    serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| {
            v["tv_shows"]["all"]
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        })
        .map_or(now, |d| d.with_timezone(&Utc))
}

/// `RefreshToken`: a new access token from the Servarr team's relay.
/// `None` when the relay does not give one.
pub async fn refresh_token(
    client: &reqwest::Client,
    upstreams: &Upstreams,
    settings: &Settings,
    now: DateTime<Utc>,
) -> Option<Settings> {
    let mut url = reqwest::Url::parse(RENEW_URI).ok()?;
    url.query_pairs_mut()
        .append_pair("refresh_token", &settings.refresh_token);
    let answer = async {
        let response = client.get(upstreams.url(url.as_str())).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body = crate::http::read_body(response, crate::http::MAX_RESPONSE_BYTES)
            .await
            .ok()?;
        serde_json::from_slice::<Value>(&body).ok()
    }
    .await;
    let Some(token) = answer else {
        tracing::warn!("Error refreshing simkl access token");
        return None;
    };
    let mut renewed = settings.clone();
    renewed.access_token = token["access_token"].as_str().unwrap_or("").to_string();
    renewed.expires = Some(now + Duration::seconds(token["expires_in"].as_i64().unwrap_or(0)));
    if let Some(refresh) = token["refresh_token"].as_str() {
        renewed.refresh_token = refresh.to_string();
    }
    Some(renewed)
}

/// `RequestAction`: the sign-in's two steps. The first only builds the
/// address the browser goes to; the second asks Simkl who the token
/// belongs to.
pub async fn action(
    client: &reqwest::Client,
    upstreams: &Upstreams,
    settings: &Settings,
    name: &str,
    query: &[(String, String)],
    now: DateTime<Utc>,
) -> Result<Value, Failure> {
    let param = |key: &str| {
        query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    // The originals' routes are case-insensitive, the action's name included.
    match name.to_ascii_lowercase().as_str() {
        "startoauth" => {
            let mut url = reqwest::Url::parse(OAUTH_URL).expect("a constant URL");
            url.query_pairs_mut()
                .append_pair("client_id", CLIENT_ID)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", REDIRECT_URI)
                .append_pair("state", &param("callbackUrl"));
            Ok(json!({"oauthUrl": url.as_str()}))
        }
        "getoauthtoken" => {
            let token = param("access_token");
            let with_token = Settings {
                access_token: token.clone(),
                ..settings.clone()
            };
            let url = format!("{}/users/settings", settings.base_url);
            let body = get(client, upstreams, &with_token, &url).await?;
            let user = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| v["account"]["id"].as_i64())
                .map(|id| id.to_string());
            let expires = now + Duration::seconds(param("expires_in").parse::<i64>().unwrap_or(0));
            Ok(json!({
                "accessToken": token,
                "expires": crate::time::format_utc(expires),
                "refreshToken": param("refresh_token"),
                "authUser": user,
            }))
        }
        _ => Ok(json!({})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parser_keeps_series_and_reads_ids_leniently() {
        let body = br#"{
            "anime": [
                {"show": {"title": "A", "ids": {"tvdb": "10", "mal": "7"}}, "anime_type": "tv"},
                {"show": {"title": "A film", "ids": {"tvdb": "11"}}, "anime_type": "movie"},
                {"show": {"title": "No id", "ids": {}}, "anime_type": "ona"}
            ],
            "shows": [
                {"show": {"title": "B", "ids": {"tvdb": 20, "imdb": "tt1"}}},
                {"show": {"title": "C", "ids": {"imdb": "tt2"}}},
                {"show": {"title": "", "ids": {}}}
            ]
        }"#;
        let items = parse(body).unwrap();
        let seen: Vec<(&str, i64, i64)> = items
            .iter()
            .map(|i| (i.title.as_str(), i.tvdb_id, i.mal_id))
            .collect();
        assert_eq!(seen, [("A", 10, 7), ("B", 20, 0), ("C", 0, 0)]);
        assert_eq!(parse(b"null").unwrap(), Vec::new());
    }

    #[test]
    fn the_stored_base_url_is_honoured() {
        let stored = Settings::from_json(&json!({"baseUrl": "http://simkl.invalid/", "listType": 3}));
        assert_eq!(
            stored.list_url(),
            "http://simkl.invalid/sync/all-items/shows/completed"
        );
        let fresh = Settings::from_json(&json!({"listType": 1, "showType": 1}));
        assert_eq!(
            fresh.list_url(),
            "https://api.simkl.com/sync/all-items/anime/plantowatch"
        );
    }
}
