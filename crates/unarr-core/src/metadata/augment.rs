//! The data the originals add to the provider's: scene mappings from the
//! Servarr services (`SceneMappingProxy`) and the scene numbering and names
//! from XEM (`XemProxy`), and the cover downloads, all through the same
//! client and [`Upstreams`] as the provider.

use serde::Deserialize;

use super::Upstreams;

/// `SceneMappingProxy`: the services' list.
const SCENE_MAPPINGS: &str = "https://services.sonarr.tv/v1/scenemapping";
/// `XemProxy.ROOT_URL`; `origin=tvdb` is the suffix query parameter of
/// every request.
const XEM: &str = "https://thexem.info/map";

/// The HTTP side of the data augmentation and the covers.
#[derive(Clone)]
pub struct Augment {
    client: reqwest::Client,
    upstreams: Upstreams,
}

/// One mapping as `SceneMapping` deserialises it from the services.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RemoteSceneMapping {
    pub title: Option<String>,
    #[serde(rename = "searchTitle")]
    pub search_term: Option<String>,
    pub tvdb_id: i64,
    #[serde(rename = "season")]
    pub season_number: Option<i64>,
    pub scene_season_number: Option<i64>,
    pub scene_origin: Option<String>,
    pub search_mode: Option<serde_json::Value>,
    pub comment: Option<String>,
    pub filter_regex: Option<String>,
}

/// `XemValues`.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct XemValues {
    pub season: i64,
    pub episode: i64,
    pub absolute: i64,
}

/// `XemSceneTvdbMapping`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct XemMapping {
    pub scene: Option<XemValues>,
    pub tvdb: XemValues,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct XemResult<T> {
    result: String,
    data: Option<T>,
    message: String,
}

impl<T> Default for XemResult<T> {
    fn default() -> Self {
        XemResult {
            result: String::new(),
            data: None,
            message: String::new(),
        }
    }
}

/// `XemProxy.IgnoredErrors`.
const XEM_IGNORED_ERRORS: &[&str] = &["no single connection", "no show with the tvdb_id"];

impl Augment {
    pub fn new(client: reqwest::Client, upstreams: Upstreams) -> Augment {
        Augment { client, upstreams }
    }

    pub fn upstreams(&self) -> &Upstreams {
        &self.upstreams
    }

    /// A GET with Sonarr's user agent, its body when the status is 2xx.
    async fn get(&self, url: &str, servarr: bool) -> anyhow::Result<Vec<u8>> {
        let mut request = self.client.get(self.upstreams.url(url));
        if servarr {
            request = request.header(
                reqwest::header::USER_AGENT,
                crate::http::servarr_proxy_user_agent(crate::Personality::Sonarr),
            );
        }
        let response = request.send().await.map_err(|e| anyhow::anyhow!(cause(&e)))?;
        let status = response.status();
        let body = crate::http::read_body(response, crate::http::MAX_RESPONSE_BYTES)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        if !status.is_success() {
            anyhow::bail!(
                "HTTP request failed: [{}:{}] [GET] at [{url}]",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            );
        }
        Ok(body)
    }

    /// `SceneMappingProxy.Fetch`.
    pub async fn scene_mappings(&self) -> anyhow::Result<Vec<RemoteSceneMapping>> {
        Ok(serde_json::from_slice(&self.get(SCENE_MAPPINGS, true).await?)?)
    }

    /// `XemProxy.GetXemSeriesIds`.
    pub async fn xem_series_ids(&self) -> anyhow::Result<Vec<i64>> {
        let body = self.get(&format!("{XEM}/havemap?origin=tvdb"), false).await?;
        let result: XemResult<Vec<String>> = serde_json::from_slice(&body)?;
        check_xem_failure(&result)?;
        Ok(result
            .data
            .unwrap_or_default()
            .iter()
            .filter_map(|d| d.parse::<i64>().ok())
            .filter(|id| *id > 0)
            .collect())
    }

    /// `XemProxy.GetSceneTvdbMappings`: the mappings with a scene side.
    pub async fn xem_mappings(&self, tvdb_id: i64) -> anyhow::Result<Vec<XemMapping>> {
        let body = self
            .get(&format!("{XEM}/all?id={tvdb_id}&origin=tvdb"), false)
            .await?;
        let result: XemResult<Vec<XemMapping>> = serde_json::from_slice(&body)?;
        Ok(result
            .data
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.scene.is_some())
            .collect())
    }

    /// `XemProxy.GetSceneTvdbNames`: one mapping per name and season, the
    /// names whose season is not a number skipped, Fate/Zero's later
    /// seasons too.
    pub async fn xem_names(&self) -> anyhow::Result<Vec<RemoteSceneMapping>> {
        let body = self
            .get(&format!("{XEM}/allNames?seasonNumbers=True&origin=tvdb"), false)
            .await?;
        let result: XemResult<serde_json::Map<String, serde_json::Value>> = serde_json::from_slice(&body)?;
        let mut out = Vec::new();
        for (tvdb, names) in result.data.unwrap_or_default() {
            let Ok(tvdb_id) = tvdb.parse::<i64>() else {
                continue;
            };
            for name in names.as_array().into_iter().flatten() {
                for (title, season) in name.as_object().into_iter().flatten() {
                    let season = match season {
                        serde_json::Value::Number(n) => n.as_i64(),
                        serde_json::Value::String(s) => s.parse().ok(),
                        _ => None,
                    };
                    let Some(season) = season else { continue };
                    if tvdb_id == 79151 && season > 1 {
                        continue;
                    }
                    out.push(RemoteSceneMapping {
                        title: Some(title.clone()),
                        search_term: Some(title.clone()),
                        scene_season_number: Some(season),
                        tvdb_id,
                        ..RemoteSceneMapping::default()
                    });
                }
            }
        }
        Ok(out)
    }

    /// `HttpClient.Head(url)`: the `Content-Length` the server reports.
    pub async fn content_length(&self, url: &str) -> anyhow::Result<Option<u64>> {
        let response = self
            .client
            .head(self.upstreams.url(url))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!(cause(&e)))?;
        if !response.status().is_success() {
            anyhow::bail!(
                "HTTP request failed: [{}] [HEAD] at [{url}]",
                response.status().as_u16()
            );
        }
        Ok(response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok()))
    }

    /// `HttpClient.DownloadFile`: the body of `url`.
    pub async fn download(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        self.get(url, false).await
    }
}

fn check_xem_failure<T>(result: &XemResult<T>) -> anyhow::Result<()> {
    if result.result.eq_ignore_ascii_case("failure")
        && !XEM_IGNORED_ERRORS.iter().any(|e| result.message.contains(e))
    {
        anyhow::bail!("Error response received from Xem: {}", result.message);
    }
    Ok(())
}

fn cause(e: &reqwest::Error) -> String {
    let mut cause: &dyn std::error::Error = e;
    while let Some(next) = cause.source() {
        cause = next;
    }
    cause.to_string()
}
