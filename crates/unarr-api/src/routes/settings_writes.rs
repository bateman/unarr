//! The settings pages' writes (`PUT config/*`) and the catalogues the UI
//! and Recyclarr edit (tags, root folders, remote path mappings, delay and
//! release profiles, custom filters, auto tagging, import list exclusions,
//! quality definitions and profiles, custom formats), after the originals'
//! controllers, with their validation.

use axum::extract::{Path, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use unarr_core::store;
use unarr_core::validation::ValidationFailure;

use crate::error::{found, ApiError, ApiResult};
use crate::query::Params;
use crate::schemas::{self, SchemaKind};
use crate::state::AppState;
use unarr_core::model::{CustomFormat, QualityProfile};

// --- helpers

fn object(body: &Value) -> ApiResult<&Map<String, Value>> {
    body.as_object()
        .ok_or_else(|| ApiError::BadRequest("The request body must be a JSON object".into()))
}

fn rule(property: &str, code: &str, message: &str, attempted: Value) -> ValidationFailure {
    ValidationFailure::rule(property, code, message, attempted)
}

fn check(failures: Vec<ValidationFailure>) -> ApiResult<()> {
    if failures.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(failures))
    }
}

/// `'X' must be greater than or equal to 'n'.`
fn at_least(property: &str, value: i64, min: i64) -> Option<ValidationFailure> {
    (value < min).then(|| {
        let mut f = rule(
            property,
            "GreaterThanOrEqualValidator",
            &format!(
                "'{}' must be greater than or equal to '{min}'.",
                unarr_core::validation::display_name(property)
            ),
            json!(value),
        );
        f.placeholders = vec![
            ("comparisonProperty".into(), json!("")),
            ("comparisonValue".into(), json!(min)),
        ];
        f
    })
}

/// `'X' must be between a and b. You entered v.`
fn between(property: &str, value: i64, from: i64, to: i64) -> Option<ValidationFailure> {
    (value < from || value > to).then(|| {
        let mut f = rule(
            property,
            "InclusiveBetweenValidator",
            &format!(
                "'{}' must be between {from} and {to}. You entered {value}.",
                unarr_core::validation::display_name(property)
            ),
            json!(value),
        );
        f.placeholders = vec![("from".into(), json!(from)), ("to".into(), json!(to))];
        f
    })
}

fn created(state: &AppState, path: &str, id: i64, body: Value) -> Response {
    let location = format!("{}/api/v3/{path}/{id}", state.settings().url_base);
    (StatusCode::CREATED, [(header::LOCATION, location)], Json(body)).into_response()
}

fn accepted(body: Value) -> Response {
    (StatusCode::ACCEPTED, Json(body)).into_response()
}

fn int(body: &Value, key: &str) -> i64 {
    body.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn text(body: &Value, key: &str) -> String {
    body.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn ints(body: &Value, key: &str) -> Vec<i64> {
    body.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .collect()
}

fn strings(body: &Value, key: &str) -> Vec<String> {
    body.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// How a setting is stored in the `Config` table (`ConfigService.SetValue`:
/// `ToString()` of the value).
#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Int,
    Str,
    /// An enum the API names in camelCase and the table in PascalCase.
    Enum,
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// `SaveConfigDictionary`: the given resource fields written to their
/// keys (the property names, as the originals store them lowercased).
async fn save_config(state: &AppState, body: &Value, fields: &[(&str, Kind)]) -> ApiResult<()> {
    let mut rows = Vec::new();
    for (field, kind) in fields {
        let Some(v) = body.get(*field) else { continue };
        let stored = match (kind, v) {
            (Kind::Bool, Value::Bool(b)) => if *b { "True" } else { "False" }.to_string(),
            (Kind::Int, Value::Number(n)) => n.to_string(),
            (Kind::Str, Value::String(s)) => s.clone(),
            (Kind::Str, Value::Null) => String::new(),
            (Kind::Enum, Value::String(s)) => upper_first(s),
            (Kind::Enum, Value::Number(n)) => n.to_string(),
            _ => continue,
        };
        rows.push((upper_first(field).to_ascii_lowercase(), stored));
    }
    state
        .db()
        .call(move |conn| {
            for (k, v) in rows {
                store::config::set_config(conn, &k, &v)?;
            }
            Ok(())
        })
        .await?;
    Ok(())
}

// --- config pages

/// `PUT /config/mediamanagement/{id}`.
pub async fn media_management(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    let mut failures = Vec::new();
    failures.extend(at_least(
        "RecycleBinCleanupDays",
        int(&body, "recycleBinCleanupDays"),
        0,
    ));
    failures.extend(at_least(
        "MinimumFreeSpaceWhenImporting",
        int(&body, "minimumFreeSpaceWhenImporting"),
        100,
    ));
    check(failures)?;
    // The originals' path validator throws on a relative recycling bin
    // before it can report it.
    let bin = text(&body, "recycleBin");
    if !bin.trim().is_empty() && !bin.starts_with('/') {
        return Err(ApiError::ServerException {
            message: format!(
                "value [{bin}]  is not a valid *nix path. paths must start with / (Parameter 'path')"
            ),
            exception: "System.ArgumentException".into(),
        });
    }
    use Kind::*;
    save_config(
        &state,
        &body,
        &[
            ("autoUnmonitorPreviouslyDownloadedEpisodes", Bool),
            ("recycleBin", Str),
            ("recycleBinCleanupDays", Int),
            ("downloadPropersAndRepacks", Enum),
            ("createEmptySeriesFolders", Bool),
            ("deleteEmptyFolders", Bool),
            ("fileDate", Enum),
            ("rescanAfterRefresh", Enum),
            ("setPermissionsLinux", Bool),
            ("chmodFolder", Str),
            ("chownGroup", Str),
            ("episodeTitleRequired", Enum),
            ("skipFreeSpaceCheckWhenImporting", Bool),
            ("minimumFreeSpaceWhenImporting", Int),
            ("copyUsingHardlinks", Bool),
            ("useScriptImport", Bool),
            ("scriptImportPath", Str),
            ("importExtraFiles", Bool),
            ("extraFileExtensions", Str),
            ("enableMediaInfo", Bool),
        ],
    )
    .await?;
    let Json(out) = crate::routes::settings::media_management(State(state)).await?;
    Ok(accepted(out))
}

/// `PUT /config/indexer/{id}`.
pub async fn indexer(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    let mut failures = Vec::new();
    failures.extend(at_least("MinimumAge", int(&body, "minimumAge"), 0));
    failures.extend(at_least("Retention", int(&body, "retention"), 0));
    let rss = int(&body, "rssSyncInterval");
    if rss != 0 && !(10..=120).contains(&rss) {
        failures.push(rule(
            "RssSyncInterval",
            "RssSyncIntervalValidator",
            "Must be between 10 and 120 or 0 to disable",
            json!(rss),
        ));
    }
    check(failures)?;
    use Kind::*;
    save_config(
        &state,
        &body,
        &[
            ("minimumAge", Int),
            ("maximumSize", Int),
            ("retention", Int),
            ("rssSyncInterval", Int),
        ],
    )
    .await?;
    let Json(out) = crate::routes::settings::indexer_config(State(state)).await?;
    Ok(accepted(out))
}

/// `PUT /config/downloadclient/{id}`.
pub async fn download_client(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    use Kind::*;
    save_config(
        &state,
        &body,
        &[
            ("downloadClientWorkingFolders", Str),
            ("enableCompletedDownloadHandling", Bool),
            ("autoRedownloadFailed", Bool),
            ("autoRedownloadFailedFromInteractiveSearch", Bool),
        ],
    )
    .await?;
    let Json(out) = crate::routes::config::download_client(State(state)).await?;
    Ok(accepted(out))
}

/// `PUT /config/importlist/{id}`.
pub async fn import_list(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    let level = text(&body, "listSyncLevel");
    if level.eq_ignore_ascii_case("keepAndTag") && int(&body, "listSyncTag") == 0 {
        let mut f = rule(
            "ListSyncTag",
            "PredicateValidator",
            "Tag must be specified",
            json!(int(&body, "listSyncTag")),
        );
        f.error_code = Some("PredicateValidator".into());
        check(vec![f])?;
    }
    use Kind::*;
    save_config(&state, &body, &[("listSyncLevel", Enum), ("listSyncTag", Int)]).await?;
    let Json(out) = crate::routes::config::import_list(State(state)).await?;
    Ok(accepted(out))
}

/// `PUT /config/ui/{id}`: the theme lives with the host settings.
pub async fn ui(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    let language = int(&body, "uiLanguage");
    if language < 1 {
        let mut f = rule(
            "UILanguage",
            "GreaterThanOrEqualValidator",
            "The UI Language value cannot be less than 1",
            json!(language),
        );
        f.placeholders = vec![
            ("comparisonProperty".into(), json!("")),
            ("comparisonValue".into(), json!(1)),
        ];
        check(vec![f])?;
    }
    use Kind::*;
    save_config(
        &state,
        &body,
        &[
            ("firstDayOfWeek", Int),
            ("calendarWeekColumnHeader", Str),
            ("shortDateFormat", Str),
            ("longDateFormat", Str),
            ("timeFormat", Str),
            ("showRelativeDates", Bool),
            ("enableColorImpairedMode", Bool),
            ("timeZone", Str),
        ],
    )
    .await?;
    if let Some(theme) = body.get("theme").and_then(Value::as_str) {
        let mut overrides = host_overrides(&state).await?;
        overrides.insert("theme".into(), json!(theme));
        save_host_overrides(&state, overrides).await?;
    }
    // `UILanguage` is stored under that name.
    if let Some(v) = body.get("uiLanguage").and_then(Value::as_i64) {
        state
            .db()
            .call(move |conn| store::config::set_config(conn, "uilanguage", &v.to_string()))
            .await?;
    }
    let Json(out) = crate::routes::config::ui(State(state)).await?;
    Ok(accepted(out))
}

// --- naming

/// `FileNameBuilder`'s token patterns the naming validators match.
mod naming_rules {
    use fancy_regex::Regex;
    use std::sync::OnceLock;

    macro_rules! re {
        ($name:ident, $pattern:expr) => {
            pub fn $name() -> &'static Regex {
                static R: OnceLock<Regex> = OnceLock::new();
                R.get_or_init(|| Regex::new($pattern).expect("static regex"))
            }
        };
    }

    re!(episode, r"(?i)(?<episode>\{episode(?::0+)?})");
    re!(season, r"(?i)(?<season>\{season(?::0+)?})");
    re!(
        season_episode,
        r"(?i)(?<separator>(?<=})[- ._]+?)?(?<seasonEpisode>s?{season(?::0+)?}(?<episodeSeparator>[- ._]?[ex])(?<episode>{episode(?::0+)?}))(?<separator2>[- ._]+?(?={))?"
    );
    re!(
        absolute_episode,
        r"(?i)(?<separator>(?<=})[- ._]+?)?(?<absolute>{absolute(?::0+)?})(?<separator2>[- ._]+?(?={))?"
    );
    re!(air_date, r"(?i)\{Air(\s|\W|_)Date\}");
    re!(
        series_title,
        r"(?i)(?<token>\{(?:Series)(?<separator>[- ._])(Clean)?Title(The)?(Without)?(Year)?(?::(?<customFormat>[0-9-]+))?\})"
    );
    re!(season_folder, r"(?i)(\{season(\:\d+)?\})");
    re!(original, r"(?i)(\{original[- ._](?:title|filename)\})");

    pub fn is(re: &Regex, value: &str) -> bool {
        re.is_match(value).unwrap_or(false)
    }
}

/// `NamingConfigController`'s rules: the first failure of each property.
fn naming_failures(body: &Value) -> Vec<ValidationFailure> {
    use naming_rules::*;
    let mut failures = Vec::new();
    let style = int(body, "multiEpisodeStyle");
    failures.extend(between("MultiEpisodeStyle", style, 0, 5));
    let season_and_episode = |v: &str| is(season_episode(), v) || (is(season(), v) && is(episode(), v));
    let formats = [
        ("StandardEpisodeFormat", "standardEpisodeFormat"),
        ("DailyEpisodeFormat", "dailyEpisodeFormat"),
        ("AnimeEpisodeFormat", "animeEpisodeFormat"),
        ("SeriesFolderFormat", "seriesFolderFormat"),
        ("SeasonFolderFormat", "seasonFolderFormat"),
        ("SpecialsFolderFormat", "specialsFolderFormat"),
    ];
    for (property, key) in formats {
        let value = text(body, key);
        if value.trim().is_empty() {
            failures.push(ValidationFailure::not_empty(property, json!(value)));
            continue;
        }
        if value.contains('\0') || value.contains('|') {
            continue;
        }
        let (ok, code, message) = match property {
            "StandardEpisodeFormat" => (
                season_and_episode(&value) || is(original(), &value),
                "ValidStandardEpisodeFormatValidator",
                "Must contain season and episode numbers OR Original Title",
            ),
            "DailyEpisodeFormat" => (
                season_and_episode(&value) || is(air_date(), &value) || is(original(), &value),
                "ValidDailyEpisodeFormatValidator",
                "Must contain Air Date OR Season and Episode OR Original Title",
            ),
            "AnimeEpisodeFormat" => (
                season_and_episode(&value) || is(absolute_episode(), &value) || is(original(), &value),
                "ValidAnimeEpisodeFormatValidator",
                "Must contain Absolute Episode number OR Season and Episode OR Original Title",
            ),
            "SeriesFolderFormat" => (
                is(series_title(), &value),
                "RegularExpressionValidator",
                "Must contain series title",
            ),
            "SeasonFolderFormat" => (
                is(season_folder(), &value),
                "RegularExpressionValidator",
                "Must contain season number",
            ),
            _ => (true, "", ""),
        };
        if !ok {
            failures.push(rule(property, code, message, json!(value)));
        }
    }
    failures
}

/// `PUT /config/naming/{id}`.
/// The naming form's fields laid over a configuration: what a save
/// stores, and what the examples are built from while the form is edited.
pub fn apply_naming(n: &mut unarr_core::model::NamingConfig, b: &Value) {
    let flag = |k: &str, d: bool| b.get(k).and_then(Value::as_bool).unwrap_or(d);
    n.rename_episodes = flag("renameEpisodes", n.rename_episodes);
    n.replace_illegal_characters = flag("replaceIllegalCharacters", n.replace_illegal_characters);
    if let Some(v) = b.get("colonReplacementFormat").and_then(Value::as_i64) {
        n.colon_replacement_format = v;
    }
    if let Some(v) = b.get("customColonReplacementFormat").and_then(Value::as_str) {
        n.custom_colon_replacement_format = v.to_string();
    }
    if let Some(v) = b.get("multiEpisodeStyle").and_then(Value::as_i64) {
        n.multi_episode_style = v;
    }
    let s = |k: &str| b.get(k).and_then(Value::as_str).map(str::to_string);
    if let Some(v) = s("standardEpisodeFormat") {
        n.standard_episode_format = v;
    }
    if let Some(v) = s("dailyEpisodeFormat") {
        n.daily_episode_format = v;
    }
    if let Some(v) = s("animeEpisodeFormat") {
        n.anime_episode_format = v;
    }
    if let Some(v) = s("seriesFolderFormat") {
        n.series_folder_format = v;
    }
    if let Some(v) = s("seasonFolderFormat") {
        n.season_folder_format = v;
    }
    if let Some(v) = s("specialsFolderFormat") {
        n.specials_folder_format = v;
    }
}

pub async fn naming(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    check(naming_failures(&body))?;
    let b = body.clone();
    state
        .db()
        .call(move |conn| {
            let mut n = store::config::naming(conn)?;
            apply_naming(&mut n, &b);
            store::config::set_naming(conn, &n)
        })
        .await?;
    let Json(out) = crate::routes::settings::naming(State(state)).await?;
    Ok(accepted(out))
}

// --- host

/// The `config.xml` settings a `PUT config/host` changed, kept in the
/// database: the original's `config.xml` is never rewritten, and the
/// daemon lays these over the file it reads at start.
pub const HOST_OVERRIDES: &str = "host_overrides";

async fn host_overrides(state: &AppState) -> ApiResult<Map<String, Value>> {
    let raw = state
        .db()
        .call(|conn| store::config::meta(conn, HOST_OVERRIDES))
        .await?;
    Ok(raw
        .and_then(|r| serde_json::from_str::<Value>(&r).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default())
}

async fn save_host_overrides(state: &AppState, overrides: Map<String, Value>) -> ApiResult<()> {
    let text = Value::Object(overrides.clone()).to_string();
    state
        .db()
        .call(move |conn| store::config::set_meta(conn, HOST_OVERRIDES, &text))
        .await?;
    let mut settings = (*state.settings()).clone();
    apply_host_overrides(&mut settings.host, &overrides);
    state.set_settings(crate::state::Settings::from_host_config(
        settings.host,
        state.personality(),
    ));
    Ok(())
}

/// [`apply_host_overrides`] from the stored text, as the daemon reads it.
pub fn apply_saved_host_overrides(h: &mut unarr_core::config::HostConfig, raw: Option<&str>) {
    if let Some(map) = raw
        .and_then(|r| serde_json::from_str::<Value>(r).ok())
        .and_then(|v| v.as_object().cloned())
    {
        apply_host_overrides(h, &map);
    }
}

/// Lay saved host settings over those `config.xml` gave.
pub fn apply_host_overrides(h: &mut unarr_core::config::HostConfig, o: &Map<String, Value>) {
    let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
    let b = |k: &str| o.get(k).and_then(Value::as_bool);
    if let Some(v) = s("bindAddress") {
        h.bind_address = v;
    }
    if let Some(v) = o.get("port").and_then(Value::as_u64) {
        h.port = v as u16;
    }
    if let Some(v) = o.get("sslPort").and_then(Value::as_u64) {
        h.ssl_port = v as u16;
    }
    if let Some(v) = b("enableSsl") {
        h.enable_ssl = v;
    }
    if let Some(v) = b("launchBrowser") {
        h.launch_browser = v;
    }
    if let Some(v) = s("allowedHosts") {
        h.allowed_hosts = v;
    }
    if let Some(v) = s("trustedNetworks") {
        h.trusted_networks = v;
    }
    if let Some(v) = s("branch") {
        h.branch = v;
    }
    if let Some(v) = s("logLevel") {
        h.log_level = v;
    }
    if let Some(v) = s("sslCertPath") {
        h.ssl_cert_path = v;
    }
    if let Some(v) = s("sslCertPassword") {
        h.ssl_cert_password = v;
    }
    if let Some(v) = s("urlBase") {
        h.url_base = v;
    }
    if let Some(v) = s("instanceName") {
        h.instance_name = Some(v);
    }
    if let Some(v) = b("analyticsEnabled") {
        h.analytics_enabled = v;
    }
    if let Some(v) = s("theme") {
        h.theme = v;
    }
    if let Some(v) = b("trustCgnatIpAddresses") {
        h.trust_cgnat_ip_addresses = v;
    }
    if let Some(v) = s("apiKey").filter(|k| !k.trim().is_empty()) {
        h.api_key = v;
    }
    if let Some(v) = s("authenticationMethod")
        .and_then(|m| unarr_core::config::AuthenticationMethod::parse(&m.to_lowercase()))
    {
        h.authentication_method = v;
    }
    if let Some(v) = s("authenticationRequired")
        .and_then(|r| unarr_core::config::AuthenticationRequired::parse(&r.to_lowercase()))
    {
        h.authentication_required = v;
    }
}

/// `UserService.Upsert`: the one user created, or its name and password
/// replaced. A password equal to the stored one is the hash the page was
/// given and sent back: it is left alone.
async fn upsert_user(state: &AppState, username: String, password: String) -> ApiResult<()> {
    state
        .db()
        .call(move |conn| {
            use unarr_core::model::User;
            let existing = store::providers::list_users(conn)?.into_iter().next();
            let mut random = [0u8; 32];
            std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut random)?;
            let hash = |user: &mut User| {
                let salt = &random[..16];
                user.salt = Some(base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    salt,
                ));
                user.iterations = Some(10_000);
                let mut key = [0u8; 32];
                pbkdf2::pbkdf2_hmac::<sha2::Sha512>(password.as_bytes(), salt, 10_000, &mut key);
                user.password = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, key);
            };
            match existing {
                Some(mut user) => {
                    if user.password != password {
                        hash(&mut user);
                    }
                    user.username = username.to_lowercase();
                    store::providers::update_user(conn, &user)
                }
                None => {
                    let g = &random[16..];
                    let mut user = User {
                        id: 1,
                        // A version 4 GUID, as `Guid.NewGuid` gives.
                        identifier: format!(
                            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-4{:01x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                            g[0], g[1], g[2], g[3], g[4], g[5], g[6] & 0x0f, g[7],
                            (g[8] & 0x3f) | 0x80, g[9], g[10], g[11], g[12], g[13], g[14], g[15]
                        ),
                        username: username.to_lowercase(),
                        password: String::new(),
                        salt: None,
                        iterations: None,
                    };
                    hash(&mut user);
                    store::providers::insert_user(conn, &user)
                }
            }
        })
        .await?;
    Ok(())
}

/// A host setting changed outside a save of the page (`ResetApiKey`),
/// from a thread that may block.
pub fn set_host_override_blocking(state: &AppState, key: &str, value: Value) -> anyhow::Result<()> {
    let key = key.to_string();
    let overrides = state.db().call_blocking(move |conn| {
        let mut overrides = store::config::meta(conn, HOST_OVERRIDES)?
            .and_then(|r| serde_json::from_str::<Value>(&r).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        overrides.insert(key, value);
        store::config::set_meta(
            conn,
            HOST_OVERRIDES,
            &Value::Object(overrides.clone()).to_string(),
        )?;
        Ok(overrides)
    })?;
    let mut settings = (*state.settings()).clone();
    apply_host_overrides(&mut settings.host, &overrides);
    state.set_settings(crate::state::Settings::from_host_config(
        settings.host,
        state.personality(),
    ));
    Ok(())
}

/// `AllowedHostsParser.Parse`.
fn parse_hosts(value: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for h in value.split([',', ';']).map(str::trim).filter(|h| !h.is_empty()) {
        let h = if !(h.starts_with('[') && h.ends_with(']'))
            && h.contains(':')
            && h.parse::<std::net::Ipv6Addr>().is_ok()
        {
            format!("[{h}]")
        } else {
            h.to_string()
        };
        if !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

/// `Uri.CheckHostName(host) != Unknown`, and DNS after `*.`.
fn valid_host(host: &str) -> bool {
    let dns = |h: &str| {
        !h.is_empty()
            && h.len() <= 255
            && h.split('.').all(|l| {
                !l.is_empty()
                    && l.len() <= 63
                    && l.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            })
    };
    if let Some(rest) = host.strip_prefix("*.") {
        return dns(rest) && rest.parse::<std::net::IpAddr>().is_err();
    }
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    }
    host.parse::<std::net::IpAddr>().is_ok() || dns(host)
}

/// `PUT /config/host/{id}`.
pub async fn host(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    let mut failures = Vec::new();
    let port = int(&body, "port");
    failures.extend(between("Port", port, 1, 65535));
    let hosts = text(&body, "allowedHosts");
    let auth_required = text(&body, "authenticationRequired");
    if !auth_required.eq_ignore_ascii_case("enabled") && parse_hosts(&hosts).is_empty() {
        failures.push(rule(
            "AllowedHosts",
            "PredicateValidator",
            "Allowed Hosts is required when 'Authentication Required' is not 'Enabled'",
            json!(hosts),
        ));
    }
    if !hosts.trim().is_empty() {
        let parsed = parse_hosts(&hosts);
        if parsed.is_empty() || !parsed.iter().all(|h| valid_host(h)) {
            failures.push(rule(
                "AllowedHosts",
                "PredicateValidator",
                "Must be a comma separated list of host names or IP addresses only, without a protocol or port",
                json!(hosts),
            ));
        }
    }
    let url_base = text(&body, "urlBase");
    let lower = url_base.trim_start_matches('/').to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        failures.push(rule(
            "UrlBase",
            "RegularExpressionValidator",
            "Must be a valid URL path (ie: '/sonarr')",
            json!(url_base),
        ));
    }
    if text(&body, "branch").trim().is_empty() {
        let mut f = ValidationFailure::not_empty("Branch", json!(text(&body, "branch")));
        f.error_message = "Branch name is required, 'main' is the default".into();
        failures.push(f);
    }
    // `IsMatchingPassword`: the stored hash sent back as it was shown, or
    // a new password typed twice.
    let (password, confirmation) = (text(&body, "password"), text(&body, "passwordConfirmation"));
    let stored = state
        .db()
        .call(|conn| {
            Ok(store::providers::list_users(conn)?
                .into_iter()
                .next()
                .map(|u| u.password))
        })
        .await?;
    if stored.as_deref() != Some(password.as_str()) && password != confirmation {
        let mut f = rule(
            "PasswordConfirmation",
            "PredicateValidator",
            "Must match Password",
            json!(confirmation),
        );
        f.placeholders = vec![
            ("propertyName".into(), json!("Password Confirmation")),
            ("propertyValue".into(), json!(confirmation)),
        ];
        failures.push(f);
    }
    if body.get("enableSsl").and_then(Value::as_bool).unwrap_or(false) {
        failures.extend(between("SslPort", int(&body, "sslPort"), 1, 65535));
    }
    failures.extend(between("LogSizeLimit", int(&body, "logSizeLimit"), 1, 10));
    failures.extend(between("BackupInterval", int(&body, "backupInterval"), 1, 7));
    failures.extend(between("BackupRetention", int(&body, "backupRetention"), 1, 90));
    check(failures)?;
    use Kind::*;
    save_config(
        &state,
        &body,
        &[
            ("applicationUrl", Str),
            ("proxyEnabled", Bool),
            ("proxyType", Enum),
            ("proxyHostname", Str),
            ("proxyPort", Int),
            ("proxyUsername", Str),
            ("proxyPassword", Str),
            ("proxyBypassFilter", Str),
            ("proxyBypassLocalAddresses", Bool),
            ("certificateValidation", Enum),
            ("backupFolder", Str),
            ("backupInterval", Int),
            ("backupRetention", Int),
            ("trustCgnatIpAddresses", Bool),
        ],
    )
    .await?;
    let mut overrides = host_overrides(&state).await?;
    for key in [
        "bindAddress",
        "port",
        "sslPort",
        "enableSsl",
        "launchBrowser",
        "allowedHosts",
        "trustedNetworks",
        "branch",
        "logLevel",
        "sslCertPath",
        "sslCertPassword",
        "urlBase",
        "instanceName",
        "analyticsEnabled",
        // Not `apiKey`: the originals' `SaveConfigDictionary` skips it, and
        // only `ResetApiKey` changes the key.
        "authenticationMethod",
        "authenticationRequired",
        "trustCgnatIpAddresses",
    ] {
        if let Some(v) = body.get(key) {
            overrides.insert(key.into(), v.clone());
        }
    }
    save_host_overrides(&state, overrides).await?;
    let credential = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
            .map(str::to_string)
    };
    if let (Some(username), Some(password)) = (credential("username"), credential("password")) {
        upsert_user(&state, username, password).await?;
    }
    state.apply_network_settings().await?;
    state.apply_log_level();
    let Json(out) = crate::routes::config::host(State(state)).await?;
    Ok(accepted(out))
}

// --- the catalogues

/// A catalogue row replaced under its id (an update), in one transaction.
async fn replace<T, F>(state: &AppState, table: &'static str, id: i64, row: T, insert: F) -> ApiResult<bool>
where
    T: Send + 'static,
    F: FnOnce(&rusqlite::Connection, &T) -> anyhow::Result<()> + Send + 'static,
{
    Ok(state
        .db()
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            if !store::catalog::delete_row(&tx, table, id)? {
                return Ok(false);
            }
            insert(&tx, &row)?;
            tx.commit()?;
            Ok(true)
        })
        .await?)
}

/// A new row's id from the table's sequence, then the insert.
async fn insert_new<T, F>(
    state: &AppState,
    table: &'static str,
    mut row: T,
    set_id: fn(&mut T, i64),
    insert: F,
) -> ApiResult<i64>
where
    T: Send + 'static,
    F: FnOnce(&rusqlite::Connection, &T) -> anyhow::Result<()> + Send + 'static,
{
    Ok(state
        .db()
        .call(move |conn| {
            let id = store::sequence::next_id(conn, table)?;
            set_id(&mut row, id);
            insert(conn, &row)?;
            Ok(id)
        })
        .await?)
}

async fn delete_from(state: &AppState, table: &'static str, id: i64) -> ApiResult<bool> {
    Ok(state
        .db()
        .call(move |conn| store::catalog::delete_row(conn, table, id))
        .await?)
}

fn ok_empty() -> Response {
    StatusCode::OK.into_response()
}

// tags

/// `POST /tag`: an existing label is answered as is; labels are lower case.
pub async fn tag_create(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let label = text(&body, "label");
    let existing = state
        .db()
        .call({
            let label = label.clone();
            move |conn| {
                Ok(store::catalog::list_tags(conn)?
                    .into_iter()
                    .find(|t| t.label == label))
            }
        })
        .await?;
    let id = match existing {
        Some(t) => t.id,
        None => {
            let tag = unarr_core::model::Tag {
                id: 0,
                label: label.to_lowercase(),
            };
            insert_new(&state, "tags", tag, |t, id| t.id = id, store::catalog::insert_tag).await?
        }
    };
    let Json(out) = crate::routes::catalog::tag(State(state.clone()), Path(id)).await?;
    Ok(created(&state, "tag", id, out))
}

pub async fn tag_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let id = body.get("id").and_then(Value::as_i64).unwrap_or(id);
    let tag = unarr_core::model::Tag {
        id,
        label: text(&body, "label").to_lowercase(),
    };
    if !replace(&state, "tags", id, tag, store::catalog::insert_tag).await? {
        return Err(ApiError::NotFound);
    }
    let Json(out) = crate::routes::catalog::tag(State(state), Path(id)).await?;
    Ok(accepted(out))
}

/// `DELETE /tag/{id}`: refused while something uses it.
pub async fn tag_delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    let details = crate::routes::catalog::tag_detail(State(state.clone()), Path(id)).await;
    if let Ok(Json(d)) = &details {
        let in_use = [
            "delayProfileIds",
            "importListIds",
            "notificationIds",
            "releaseProfileIds",
            "indexerIds",
            "downloadClientIds",
            "autoTagIds",
            "seriesIds",
        ]
        .iter()
        .any(|k| d[*k].as_array().is_some_and(|a| !a.is_empty()));
        if in_use {
            return Err(ApiError::ConflictMessage(format!(
                "'{}' cannot be deleted since it's still in use",
                d["label"].as_str().unwrap_or("")
            )));
        }
    }
    delete_from(&state, "tags", id).await?;
    Ok(ok_empty())
}

// root folders

/// `RootFolderController`'s path rules, the first failure.
async fn root_folder_failure(state: &AppState, path: &str) -> ApiResult<Option<ValidationFailure>> {
    let with = |code: &str, message: String| {
        let mut f = rule("Path", code, &message, json!(path));
        f.placeholders = vec![("path".into(), json!(path))];
        f
    };
    if !path.starts_with('/') {
        return Ok(Some(with("PathValidator", format!("Invalid Path: '{path}'"))));
    }
    let p = path.to_string();
    let (roots, bin) = state
        .db()
        .call(move |conn| {
            Ok((
                store::catalog::list_root_folders(conn)?,
                store::config::config(conn, "recyclebin")?.unwrap_or_default(),
            ))
        })
        .await?;
    if roots
        .iter()
        .any(|r| unarr_core::mediafiles::path_equals(&r.path, &p))
    {
        return Ok(Some(with(
            "RootFolderValidator",
            format!("Path '{path}' is already configured as a root folder"),
        )));
    }
    if !bin.is_empty()
        && (unarr_core::mediafiles::path_equals(&bin, path)
            || unarr_core::mediafiles::is_parent_path(path, &bin))
    {
        let mut f = with(
            "RecycleBinValidator",
            format!(
                "Path '{path}' is {} configured recycle bin folder",
                if unarr_core::mediafiles::path_equals(&bin, path) {
                    "set to"
                } else {
                    "child of"
                }
            ),
        );
        f.placeholders.push(("relationship".into(), json!("set to")));
        return Ok(Some(f));
    }
    if !std::path::Path::new(path).is_dir() {
        return Ok(Some(with(
            "PathExistsValidator",
            format!("Path '{path}' does not exist"),
        )));
    }
    Ok(None)
}

pub async fn root_folder_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let path = text(&body, "path");
    if let Some(f) = root_folder_failure(&state, &path).await? {
        return Err(ApiError::Validation(vec![f]));
    }
    let row = unarr_core::model::RootFolder {
        id: 0,
        path: path.trim_end_matches('/').to_string(),
    };
    let id = insert_new(
        &state,
        "root_folders",
        row,
        |r, id| r.id = id,
        store::catalog::insert_root_folder,
    )
    .await?;
    let Json(out) = crate::routes::catalog::root_folder(State(state.clone()), Path(id)).await?;
    Ok(created(&state, "rootfolder", id, out))
}

pub async fn root_folder_delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    delete_from(&state, "root_folders", id).await?;
    Ok(ok_empty())
}

// remote path mappings

async fn remote_path_mapping_failures(body: &Value) -> Vec<ValidationFailure> {
    let mut failures = Vec::new();
    let host = text(body, "host");
    if host.trim().is_empty() {
        failures.push(ValidationFailure::not_empty("Host", json!(host)));
    }
    let remote = text(body, "remotePath");
    if remote.trim().is_empty() {
        failures.push(ValidationFailure::not_empty("RemotePath", json!(remote)));
    }
    let local = text(body, "localPath");
    let with = |code: &str, message: String| {
        let mut f = rule("LocalPath", code, &message, json!(local));
        f.placeholders = vec![("path".into(), json!(local))];
        f
    };
    if !local.starts_with('/') {
        failures.push(with("PathValidator", format!("Invalid Path: '{local}'")));
    } else if !std::path::Path::new(&local).is_dir() {
        failures.push(with(
            "PathExistsValidator",
            format!("Path '{local}' does not exist"),
        ));
    }
    failures
}

/// The originals keep both paths with a trailing separator.
fn with_slash(p: &str) -> String {
    if p.ends_with('/') || p.ends_with('\\') {
        p.to_string()
    } else if p.contains('\\') && !p.contains('/') {
        format!("{p}\\")
    } else {
        format!("{p}/")
    }
}

fn remote_path_mapping(body: &Value, id: i64) -> unarr_core::model::RemotePathMapping {
    unarr_core::model::RemotePathMapping {
        id,
        host: text(body, "host"),
        remote_path: with_slash(&text(body, "remotePath")),
        local_path: with_slash(&text(body, "localPath")),
    }
}

pub async fn remote_path_mapping_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(remote_path_mapping_failures(&body).await)?;
    let row = remote_path_mapping(&body, 0);
    let id = insert_new(
        &state,
        "remote_path_mappings",
        row,
        |r, id| r.id = id,
        store::providers::insert_remote_path_mapping,
    )
    .await?;
    let Json(out) = crate::routes::providers::remote_path_mapping(State(state.clone()), Path(id)).await?;
    Ok(created(&state, "remotepathmapping", id, out))
}

pub async fn remote_path_mapping_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(remote_path_mapping_failures(&body).await)?;
    let row = remote_path_mapping(&body, id);
    if !replace(&state, "remote_path_mappings", id, row, |c, r| {
        store::providers::insert_remote_path_mapping(c, r)
    })
    .await?
    {
        return Err(ApiError::NotFound);
    }
    let Json(out) = crate::routes::providers::remote_path_mapping(State(state), Path(id)).await?;
    Ok(accepted(out))
}

pub async fn remote_path_mapping_delete(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    delete_from(&state, "remote_path_mappings", id).await?;
    Ok(ok_empty())
}

// delay profiles

fn delay_profile(body: &Value, id: i64, order: i64) -> unarr_core::model::DelayProfile {
    unarr_core::model::DelayProfile {
        id,
        enable_usenet: body.get("enableUsenet").and_then(Value::as_bool).unwrap_or(false),
        enable_torrent: body
            .get("enableTorrent")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        preferred_protocol: match text(body, "preferredProtocol").to_ascii_lowercase().as_str() {
            "torrent" => unarr_core::model::Protocol::Torrent,
            "usenet" => unarr_core::model::Protocol::Usenet,
            _ => unarr_core::model::Protocol::Unknown,
        },
        usenet_delay: int(body, "usenetDelay"),
        torrent_delay: int(body, "torrentDelay"),
        order,
        tags: ints(body, "tags"),
        bypass_if_highest_quality: body
            .get("bypassIfHighestQuality")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        bypass_if_above_custom_format_score: body
            .get("bypassIfAboveCustomFormatScore")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        minimum_custom_format_score: int(body, "minimumCustomFormatScore"),
    }
}

async fn delay_profile_failures(
    state: &AppState,
    body: &Value,
    id: i64,
) -> ApiResult<Vec<ValidationFailure>> {
    let mut failures = Vec::new();
    let tags = ints(body, "tags");
    if id != 1 && tags.is_empty() {
        failures.push(ValidationFailure::not_empty("Tags", json!(tags)));
    }
    if id == 1 && !tags.is_empty() {
        failures.push(rule(
            "Tags",
            "EmptyCollectionValidator",
            "'Tags' must be empty.",
            json!(tags),
        ));
    }
    let others = state
        .db()
        .call(|c| store::providers::list_delay_profiles(c))
        .await?;
    if others
        .iter()
        .any(|p| p.id != id && p.tags.iter().any(|t| tags.contains(t)))
    {
        failures.push(rule(
            "Tags",
            "DelayProfileTagInUseValidator",
            "One or more tags is used in another profile",
            json!(tags),
        ));
    }
    failures.extend(at_least("UsenetDelay", int(body, "usenetDelay"), 0));
    failures.extend(at_least("TorrentDelay", int(body, "torrentDelay"), 0));
    let flag = |k: &str| body.get(k).and_then(Value::as_bool).unwrap_or(false);
    if !flag("enableUsenet") && !flag("enableTorrent") {
        failures.push(ValidationFailure::bare(
            "",
            "Either Usenet or Torrent should be enabled",
        ));
    }
    Ok(failures)
}

fn delay_profile_json(p: &unarr_core::model::DelayProfile) -> Value {
    json!({
        "id": p.id,
        "enableUsenet": p.enable_usenet,
        "enableTorrent": p.enable_torrent,
        "preferredProtocol": p.preferred_protocol.as_str(),
        "usenetDelay": p.usenet_delay,
        "torrentDelay": p.torrent_delay,
        "bypassIfHighestQuality": p.bypass_if_highest_quality,
        "bypassIfAboveCustomFormatScore": p.bypass_if_above_custom_format_score,
        "minimumCustomFormatScore": p.minimum_custom_format_score,
        "order": p.order,
        "tags": p.tags,
    })
}

/// `DelayProfileService.Add`: the new profile's order is the count.
pub async fn delay_profile_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(delay_profile_failures(&state, &body, 0).await?)?;
    let b = body.clone();
    let id = state
        .db()
        .call(move |conn| {
            let order = store::providers::list_delay_profiles(conn)?.len() as i64;
            let id = store::sequence::next_id(conn, "delay_profiles")?;
            store::providers::insert_delay_profile(conn, &delay_profile(&b, id, order))?;
            Ok(id)
        })
        .await?;
    let Json(out) = crate::routes::providers::delay_profile(State(state.clone()), Path(id)).await?;
    Ok(created(&state, "delayprofile", id, out))
}

pub async fn delay_profile_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(delay_profile_failures(&state, &body, id).await?)?;
    let b = body.clone();
    let done = state
        .db()
        .call(move |conn| {
            let Some(old) = store::providers::list_delay_profiles(conn)?
                .into_iter()
                .find(|p| p.id == id)
            else {
                return Ok(false);
            };
            let tx = conn.unchecked_transaction()?;
            store::catalog::delete_row(&tx, "delay_profiles", id)?;
            store::providers::insert_delay_profile(&tx, &delay_profile(&b, id, old.order))?;
            tx.commit()?;
            Ok(true)
        })
        .await?;
    if !done {
        return Err(ApiError::NotFound);
    }
    let Json(out) = crate::routes::providers::delay_profile(State(state), Path(id)).await?;
    Ok(accepted(out))
}

/// Every delay profile with new orders written back.
fn save_orders(conn: &rusqlite::Connection, all: &[unarr_core::model::DelayProfile]) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    for p in all {
        store::catalog::delete_row(&tx, "delay_profiles", p.id)?;
        store::providers::insert_delay_profile(&tx, p)?;
    }
    tx.commit()?;
    Ok(())
}

/// `DELETE /delayprofile/{id}`: the global one cannot go; the others are
/// numbered again.
pub async fn delay_profile_delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    if id == 1 {
        return Err(ApiError::MethodNotAllowed(
            "Cannot delete global delay profile".into(),
        ));
    }
    state
        .db()
        .call(move |conn| {
            store::catalog::delete_row(conn, "delay_profiles", id)?;
            let mut all = store::providers::list_delay_profiles(conn)?;
            all.sort_by_key(|p| p.order);
            for (i, p) in all.iter_mut().enumerate() {
                if p.id != 1 {
                    p.order = i as i64 + 1;
                }
            }
            save_orders(conn, &all)
        })
        .await?;
    Ok(ok_empty())
}

/// `PUT /delayprofile/reorder/{id}?after=`: `DelayProfileService.Reorder`.
pub async fn delay_profile_reorder(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    uri: Uri,
) -> ApiResult<Json<Vec<Value>>> {
    let after = Params::from_uri(&uri).i64("after");
    let all = state
        .db()
        .call(move |conn| {
            let mut all = store::providers::list_delay_profiles(conn)?;
            all.sort_by_key(|p| p.order);
            let Some(moving) = all.iter().find(|p| p.id == id).cloned() else {
                return Ok(all);
            };
            let after_profile = after.and_then(|a| all.iter().find(|p| p.id == a).cloned());
            let after_order = match &after_profile {
                None => 0,
                Some(a) if moving.order < a.order => a.order - 1,
                Some(a) => a.order,
            };
            let mut after_count = after_order + 2;
            for p in all.iter_mut() {
                if p.id == 1 {
                    continue;
                }
                if p.id == id {
                    p.order = after_order + 1;
                } else if Some(p.id) == after_profile.as_ref().map(|a| a.id) {
                    p.order = after_order;
                } else if p.order > after_order {
                    p.order = after_count;
                    after_count += 1;
                } else if p.order > moving.order {
                    p.order -= 1;
                }
            }
            save_orders(conn, &all)?;
            store::providers::list_delay_profiles(conn)
        })
        .await?;
    Ok(Json(all.iter().map(delay_profile_json).collect()))
}

// release profiles

fn release_profile_failures(body: &Value) -> Vec<ValidationFailure> {
    let required = strings(body, "required");
    let ignored = strings(body, "ignored");
    let mut failures = Vec::new();
    if required.is_empty() && ignored.is_empty() {
        failures.push(ValidationFailure::bare(
            "Required",
            "'Must contain' or 'Must not contain' is required",
        ));
    }
    if required.iter().any(|t| t.trim().is_empty()) {
        failures.push(ValidationFailure::bare(
            "Required",
            "'Must contain' should not contain whitespaces or an empty string",
        ));
    }
    if ignored.iter().any(|t| t.trim().is_empty()) {
        failures.push(ValidationFailure::bare(
            "Ignored",
            "'Must not contain' should not contain whitespaces or an empty string",
        ));
    }
    failures
}

fn release_profile(body: &Value, id: i64) -> unarr_core::model::ReleaseProfile {
    unarr_core::model::ReleaseProfile {
        id,
        name: body.get("name").and_then(Value::as_str).map(str::to_string),
        enabled: body.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        required: strings(body, "required"),
        ignored: strings(body, "ignored"),
        indexer_id: int(body, "indexerId"),
        tags: ints(body, "tags"),
    }
}

pub async fn release_profile_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(release_profile_failures(&body))?;
    let id = insert_new(
        &state,
        "release_profiles",
        release_profile(&body, 0),
        |r, id| r.id = id,
        store::providers::insert_release_profile,
    )
    .await?;
    let Json(out) = crate::routes::providers::release_profile(State(state.clone()), Path(id)).await?;
    Ok(created(&state, "releaseprofile", id, out))
}

pub async fn release_profile_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(release_profile_failures(&body))?;
    if !replace(
        &state,
        "release_profiles",
        id,
        release_profile(&body, id),
        store::providers::insert_release_profile,
    )
    .await?
    {
        return Err(ApiError::NotFound);
    }
    let Json(out) = crate::routes::providers::release_profile(State(state), Path(id)).await?;
    Ok(accepted(out))
}

pub async fn release_profile_delete(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    delete_from(&state, "release_profiles", id).await?;
    Ok(ok_empty())
}

// custom filters

fn custom_filter(body: &Value, id: i64) -> unarr_core::model::CustomFilter {
    unarr_core::model::CustomFilter {
        id,
        filter_type: text(body, "type"),
        label: text(body, "label"),
        filters: body.get("filters").cloned().unwrap_or(json!([])),
    }
}

pub async fn custom_filter_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let id = insert_new(
        &state,
        "custom_filters",
        custom_filter(&body, 0),
        |r, id| r.id = id,
        store::providers::insert_custom_filter,
    )
    .await?;
    let Json(out) = crate::routes::providers::custom_filter(State(state.clone()), Path(id)).await?;
    Ok(created(&state, "customfilter", id, out))
}

pub async fn custom_filter_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    if !replace(&state, "custom_filters", id, custom_filter(&body, id), |c, r| {
        store::providers::insert_custom_filter(c, r)
    })
    .await?
    {
        return Err(ApiError::NotFound);
    }
    let Json(out) = crate::routes::providers::custom_filter(State(state), Path(id)).await?;
    Ok(accepted(out))
}

pub async fn custom_filter_delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    delete_from(&state, "custom_filters", id).await?;
    Ok(ok_empty())
}

// import list exclusions

async fn exclusion_failures(state: &AppState, body: &Value, id: i64) -> ApiResult<Vec<ValidationFailure>> {
    let mut failures = Vec::new();
    let tvdb = int(body, "tvdbId");
    if tvdb == 0 {
        failures.push(ValidationFailure::not_empty("TvdbId", json!(tvdb)));
    } else {
        let all = state
            .db()
            .call(|c| store::providers::list_import_list_exclusions(c))
            .await?;
        if all.iter().any(|e| e.tvdb_id == tvdb && e.id != id) {
            failures.push(rule(
                "TvdbId",
                "ImportListExclusionExistsValidator",
                "This exclusion has already been added.",
                json!(tvdb),
            ));
        }
    }
    let title = text(body, "title");
    if title.trim().is_empty() {
        failures.push(ValidationFailure::not_empty("Title", json!(title)));
    }
    Ok(failures)
}

fn exclusion(body: &Value, id: i64) -> unarr_core::model::ImportListExclusion {
    unarr_core::model::ImportListExclusion {
        id,
        tvdb_id: int(body, "tvdbId"),
        title: text(body, "title"),
    }
}

fn exclusion_json(e: &unarr_core::model::ImportListExclusion) -> Value {
    json!({"id": e.id, "tvdbId": e.tvdb_id, "title": e.title})
}

pub async fn exclusion_create(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    check(exclusion_failures(&state, &body, 0).await?)?;
    let row = exclusion(&body, 0);
    let id = insert_new(
        &state,
        "import_list_exclusions",
        row,
        |r, id| r.id = id,
        store::providers::insert_import_list_exclusion,
    )
    .await?;
    let out = exclusion_json(&exclusion(&body, id));
    Ok(created(&state, "importlistexclusion", id, out))
}

pub async fn exclusion_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(exclusion_failures(&state, &body, id).await?)?;
    if !replace(
        &state,
        "import_list_exclusions",
        id,
        exclusion(&body, id),
        store::providers::insert_import_list_exclusion,
    )
    .await?
    {
        return Err(ApiError::NotFound);
    }
    Ok(accepted(exclusion_json(&exclusion(&body, id))))
}

pub async fn exclusion_delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    delete_from(&state, "import_list_exclusions", id).await?;
    Ok(ok_empty())
}

/// `DELETE /importlistexclusion/bulk`: the selected exclusions removed;
/// an id that is not there is no error.
pub async fn exclusion_bulk_delete(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let ids: Vec<i64> = body["ids"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    state
        .db()
        .call(move |conn| {
            for id in ids {
                store::catalog::delete_row(conn, "import_list_exclusions", id)?;
            }
            Ok(())
        })
        .await?;
    Ok(Json(json!({})))
}

// --- specifications (custom formats, auto tagging)

/// API specifications to the stored `[{type, body}]`: the schema's fields
/// read into the body, with the condition's own flags beside them.
fn stored_specifications(kind: SchemaKind, specs: &Value) -> Value {
    let list = specs.as_array().cloned().unwrap_or_default();
    Value::Array(
        list.iter()
            .map(|spec| {
                let implementation = spec["implementation"].as_str().unwrap_or("");
                let mut body = match schemas::lookup(kind, implementation) {
                    Some(schema) => {
                        let mut b = schemas::read_fields(schema, &spec["fields"], None);
                        b["implementationName"] = schema["implementationName"].clone();
                        if schema.get("infoLink").is_some() {
                            b["infoLink"] = schema["infoLink"].clone();
                        }
                        b
                    }
                    None => json!({}),
                };
                body["name"] = spec["name"].clone();
                body["negate"] = json!(spec["negate"].as_bool().unwrap_or(false));
                body["required"] = json!(spec["required"].as_bool().unwrap_or(false));
                json!({"type": implementation, "body": body})
            })
            .collect(),
    )
}

/// `Name` not empty and unique, then the rules the two controllers share.
fn named_failures(body: &Value, id: i64, taken: &[(i64, String)]) -> Vec<ValidationFailure> {
    let mut failures = Vec::new();
    let name = text(body, "name");
    if name.trim().is_empty() {
        failures.push(ValidationFailure::not_empty("Name", json!(name)));
    }
    if taken.iter().any(|(other, n)| *n == name && *other != id) {
        failures.push(rule("Name", "PredicateValidator", "Must be unique.", json!(name)));
    }
    failures
}

fn condition_failures(body: &Value) -> Vec<ValidationFailure> {
    let specs = body.get("specifications").cloned().unwrap_or(json!([]));
    let list = specs.as_array().cloned().unwrap_or_default();
    let mut failures = Vec::new();
    if list.is_empty() {
        failures.push(ValidationFailure::not_empty("Specifications", specs.clone()));
        failures.push(ValidationFailure::bare("", "Must contain at least one Condition"));
    }
    if list
        .iter()
        .any(|s| s["name"].as_str().is_none_or(|n| n.trim().is_empty()))
    {
        failures.push(ValidationFailure::bare(
            "",
            "Condition name(s) cannot be empty or consist of only spaces",
        ));
    }
    failures
}

// auto tagging

async fn auto_tagging_failures(state: &AppState, body: &Value, id: i64) -> ApiResult<Vec<ValidationFailure>> {
    let all = state
        .db()
        .call(|c| store::providers::list_auto_tagging(c))
        .await?;
    let taken: Vec<(i64, String)> = all.into_iter().map(|a| (a.id, a.name)).collect();
    let mut failures = named_failures(body, id, &taken);
    let tags = ints(body, "tags");
    if tags.is_empty() {
        failures.push(ValidationFailure::not_empty("Tags", json!(tags)));
    }
    failures.extend(condition_failures(body));
    Ok(failures)
}

fn auto_tagging_row(body: &Value, id: i64) -> unarr_core::model::AutoTagging {
    unarr_core::model::AutoTagging {
        id,
        name: text(body, "name"),
        remove_tags_automatically: body
            .get("removeTagsAutomatically")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        tags: ints(body, "tags"),
        specifications: stored_specifications(SchemaKind::AutoTaggingSpecification, &body["specifications"]),
    }
}

async fn auto_tagging_json(state: &AppState, id: i64) -> ApiResult<Value> {
    let Json(all) = crate::routes::providers::auto_tagging(State(state.clone())).await?;
    found(all.into_iter().find(|a| a["id"] == json!(id)))
}

pub async fn auto_tagging_one(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    Ok(Json(auto_tagging_json(&state, id).await?))
}

pub async fn auto_tagging_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(auto_tagging_failures(&state, &body, 0).await?)?;
    let id = insert_new(
        &state,
        "auto_tagging",
        auto_tagging_row(&body, 0),
        |r, id| r.id = id,
        store::providers::insert_auto_tagging,
    )
    .await?;
    let out = auto_tagging_json(&state, id).await?;
    Ok(created(&state, "autotagging", id, out))
}

pub async fn auto_tagging_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(auto_tagging_failures(&state, &body, id).await?)?;
    if !replace(&state, "auto_tagging", id, auto_tagging_row(&body, id), |c, r| {
        store::providers::insert_auto_tagging(c, r)
    })
    .await?
    {
        return Err(ApiError::NotFound);
    }
    Ok(accepted(auto_tagging_json(&state, id).await?))
}

pub async fn auto_tagging_delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    delete_from(&state, "auto_tagging", id).await?;
    Ok(ok_empty())
}

// custom formats

/// The regular expressions a specification compiles when it is read
/// (`RegexSpecificationBase.Value`'s setter).
fn bad_regex(specs: &Value) -> Option<String> {
    for spec in specs.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let implementation = spec["implementation"].as_str().unwrap_or("");
        if !matches!(
            implementation,
            "ReleaseTitleSpecification" | "ReleaseGroupSpecification"
        ) {
            continue;
        }
        let value = spec["fields"]
            .as_array()
            .and_then(|f| f.iter().find(|f| f["name"] == "value"))
            .and_then(|f| f["value"].as_str())
            .unwrap_or("");
        if let Err(e) = fancy_regex::Regex::new(value) {
            return Some(format!("Invalid pattern '{value}': {e}"));
        }
    }
    None
}

async fn custom_format_failures(
    state: &AppState,
    body: &Value,
    id: i64,
) -> ApiResult<Vec<ValidationFailure>> {
    let all = state
        .db()
        .call(|c| store::catalog::list_custom_formats(c))
        .await?;
    let taken: Vec<(i64, String)> = all.into_iter().map(|f| (f.id, f.name)).collect();
    let mut failures = named_failures(body, id, &taken);
    failures.extend(condition_failures(body));
    Ok(failures)
}

fn custom_format_row(body: &Value, id: i64) -> ApiResult<CustomFormat> {
    if let Some(inner) = bad_regex(&body["specifications"]) {
        tracing::debug!(%inner, "custom format with a regular expression that does not compile");
        return Err(ApiError::ServerException {
            message: "Exception has been thrown by the target of an invocation.".into(),
            exception: "System.Reflection.TargetInvocationException".into(),
        });
    }
    Ok(CustomFormat {
        id,
        name: text(body, "name"),
        include_when_renaming: body
            .get("includeCustomFormatWhenRenaming")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        specifications: stored_specifications(SchemaKind::CustomFormatSpecification, &body["specifications"]),
    })
}

/// `CustomFormatService.Insert`: the new format joins every quality
/// profile at the top, scored 0.
pub async fn custom_format_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(custom_format_failures(&state, &body, 0).await?)?;
    let mut row = custom_format_row(&body, 0)?;
    let id = state
        .db()
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            row.id = store::sequence::next_id(&tx, "custom_formats")?;
            store::catalog::insert_custom_format(&tx, &row)?;
            for mut p in store::catalog::list_quality_profiles(&tx)? {
                p.format_items.insert(
                    0,
                    unarr_core::model::ProfileFormatItem {
                        format: row.id,
                        score: 0,
                    },
                );
                store::catalog::delete_row(&tx, "quality_profiles", p.id)?;
                store::catalog::insert_quality_profile(&tx, &p)?;
            }
            tx.commit()?;
            Ok(row.id)
        })
        .await?;
    let Json(out) = crate::routes::catalog::custom_format(State(state.clone()), Path(id)).await?;
    Ok(created(&state, "customformat", id, out))
}

pub async fn custom_format_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(custom_format_failures(&state, &body, id).await?)?;
    let row = custom_format_row(&body, id)?;
    if !replace(&state, "custom_formats", id, row, |c, r| {
        store::catalog::insert_custom_format(c, r)
    })
    .await?
    {
        return Err(ApiError::NotFound);
    }
    let Json(out) = crate::routes::catalog::custom_format(State(state), Path(id)).await?;
    Ok(accepted(out))
}

/// `CustomFormatService.Delete`: gone from the profiles too.
fn delete_custom_formats(conn: &rusqlite::Connection, ids: &[i64]) -> anyhow::Result<()> {
    let tx = conn.unchecked_transaction()?;
    for id in ids {
        store::catalog::delete_row(&tx, "custom_formats", *id)?;
    }
    for mut p in store::catalog::list_quality_profiles(&tx)? {
        let before = p.format_items.len();
        p.format_items.retain(|f| !ids.contains(&f.format));
        let mut changed = p.format_items.len() != before;
        // `Handle(CustomFormatDeletedEvent)`: a profile left without
        // formats has no score to ask for.
        if p.format_items.is_empty()
            && (
                p.min_format_score,
                p.cutoff_format_score,
                p.min_upgrade_format_score,
            ) != (0, 0, 1)
        {
            p.min_format_score = 0;
            p.cutoff_format_score = 0;
            p.min_upgrade_format_score = 1;
            changed = true;
        }
        if changed {
            store::catalog::delete_row(&tx, "quality_profiles", p.id)?;
            store::catalog::insert_quality_profile(&tx, &p)?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub async fn custom_format_delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    let exists = state
        .db()
        .call(move |c| Ok(store::catalog::list_custom_formats(c)?.iter().any(|f| f.id == id)))
        .await?;
    if !exists {
        return Err(ApiError::ModelNotFound(format!(
            "CustomFormat with ID {id} does not exist"
        )));
    }
    state.db().call(move |c| delete_custom_formats(c, &[id])).await?;
    Ok(ok_empty())
}

/// `PUT /customformat/bulk`: the renaming flag of several formats.
pub async fn custom_format_bulk_update(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let ids = ints(&body, "ids");
    let include = body
        .get("includeCustomFormatWhenRenaming")
        .and_then(Value::as_bool);
    let formats = state
        .db()
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut out = Vec::new();
            for mut f in store::catalog::list_custom_formats(&tx)? {
                if !ids.contains(&f.id) {
                    continue;
                }
                if let Some(include) = include {
                    f.include_when_renaming = include;
                }
                store::catalog::delete_row(&tx, "custom_formats", f.id)?;
                store::catalog::insert_custom_format(&tx, &f)?;
                out.push(f);
            }
            tx.commit()?;
            Ok(out)
        })
        .await?;
    Ok(accepted(Value::Array(
        formats
            .iter()
            .map(crate::routes::catalog::custom_format_resource)
            .collect(),
    )))
}

pub async fn custom_format_bulk_delete(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let ids = ints(&body, "ids");
    state.db().call(move |c| delete_custom_formats(c, &ids)).await?;
    Ok(Json(json!({})))
}

// quality definitions

fn size(body: &Value, key: &str) -> Option<f64> {
    body.get(key).and_then(Value::as_f64)
}

fn quality_definition_row(body: &Value, id: i64) -> unarr_core::model::QualityDefinition {
    unarr_core::model::QualityDefinition {
        id,
        quality_id: body["quality"]["id"].as_i64().unwrap_or(0),
        title: text(body, "title"),
        min_size: size(body, "minSize"),
        max_size: size(body, "maxSize"),
        preferred_size: size(body, "preferredSize"),
    }
}

pub async fn quality_definition_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let id = body.get("id").and_then(Value::as_i64).unwrap_or(id);
    if !replace(
        &state,
        "quality_definitions",
        id,
        quality_definition_row(&body, id),
        store::catalog::insert_quality_definition,
    )
    .await?
    {
        return Err(ApiError::NotFound);
    }
    let Json(out) = crate::routes::catalog::quality_definition(State(state), Path(id)).await?;
    Ok(accepted(out))
}

/// `PUT /qualitydefinition/update`: every definition sent. The originals
/// answer from a five-second cache, often with what was there before;
/// Unarr answers with what it wrote.
pub async fn quality_definition_update_many(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let rows: Vec<_> = body
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|d| d["id"].as_i64().map(|id| quality_definition_row(d, id)))
        .collect();
    state
        .db()
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            for r in &rows {
                if store::catalog::delete_row(&tx, "quality_definitions", r.id)? {
                    store::catalog::insert_quality_definition(&tx, r)?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    let Json(out) = crate::routes::catalog::quality_definitions(State(state)).await?;
    Ok(accepted(Value::Array(out)))
}

// quality profiles

fn profile_items(v: &Value) -> Vec<unarr_core::model::ProfileItem> {
    v.as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .map(|i| unarr_core::model::ProfileItem {
            id: i["id"].as_i64().filter(|id| *id > 0),
            name: i["name"].as_str().map(str::to_string),
            quality: i["quality"]["id"].as_i64(),
            items: profile_items(&i["items"]),
            allowed: i["allowed"].as_bool().unwrap_or(false),
        })
        .collect()
}

fn quality_profile_row(body: &Value, id: i64) -> QualityProfile {
    QualityProfile {
        id,
        name: text(body, "name"),
        cutoff: int(body, "cutoff"),
        upgrade_allowed: body
            .get("upgradeAllowed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        items: profile_items(&body["items"]),
        format_items: body["formatItems"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .map(|f| unarr_core::model::ProfileFormatItem {
                format: f["format"].as_i64().unwrap_or(0),
                score: f["score"].as_i64().unwrap_or(0),
            })
            .collect(),
        min_format_score: int(body, "minFormatScore"),
        cutoff_format_score: int(body, "cutoffFormatScore"),
        min_upgrade_format_score: int(body, "minUpgradeFormatScore"),
    }
}

/// `QualityProfileController`'s rules.
async fn quality_profile_failures(
    state: &AppState,
    body: &Value,
    id: i64,
) -> ApiResult<Vec<ValidationFailure>> {
    let (profiles, formats) = state
        .db()
        .call(|c| {
            Ok((
                store::catalog::list_quality_profiles(c)?,
                store::catalog::list_custom_formats(c)?,
            ))
        })
        .await?;
    let taken: Vec<(i64, String)> = profiles.into_iter().map(|p| (p.id, p.name)).collect();
    let mut failures = named_failures(body, id, &taken);
    let p = quality_profile_row(body, id);
    let cutoff_ok = p.items.iter().any(|i| {
        i.allowed && (i.quality == Some(p.cutoff) || (i.quality.is_none() && i.id == Some(p.cutoff)))
    });
    if !cutoff_ok {
        failures.push(rule(
            "Cutoff",
            "ValidCutoffValidator`1",
            "Cutoff must be an allowed quality or group",
            json!(p.cutoff),
        ));
    }
    if p.items.is_empty() {
        failures.push(rule(
            "Items",
            "AllowedValidator",
            "Must contain at least one quality",
            body["items"].clone(),
        ));
    } else if !p.items.iter().any(|i| i.allowed) {
        failures.push(rule(
            "Items",
            "AllowedValidator",
            "Must contain at least one allowed quality",
            body["items"].clone(),
        ));
    }
    let mut sent: Vec<i64> = p.format_items.iter().map(|f| f.format).collect();
    let mut known: Vec<i64> = formats.iter().map(|f| f.id).collect();
    sent.sort_unstable();
    known.sort_unstable();
    if sent != known {
        failures.push(rule(
            "FormatItems",
            "PredicateValidator",
            "All Custom Formats and no extra ones need to be present inside your Profile! Try refreshing your browser.",
            body["formatItems"].clone(),
        ));
    }
    let positive: i64 = p.format_items.iter().map(|f| f.score).filter(|s| *s > 0).sum();
    let best = p.format_items.iter().map(|f| f.score).max().unwrap_or(0);
    if positive < p.min_format_score && best < p.min_format_score {
        failures.push(ValidationFailure::bare(
            "",
            "Minimum Custom Format Score can never be satisfied",
        ));
    }
    Ok(failures)
}

pub async fn quality_profile_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(quality_profile_failures(&state, &body, 0).await?)?;
    let id = insert_new(
        &state,
        "quality_profiles",
        quality_profile_row(&body, 0),
        |r, id| r.id = id,
        store::catalog::insert_quality_profile,
    )
    .await?;
    let Json(out) = crate::routes::catalog::quality_profile(State(state.clone()), Path(id)).await?;
    Ok(created(&state, "qualityprofile", id, out))
}

pub async fn quality_profile_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let id = body
        .get("id")
        .and_then(Value::as_i64)
        .filter(|i| *i > 0)
        .unwrap_or(id);
    check(quality_profile_failures(&state, &body, id).await?)?;
    if !replace(
        &state,
        "quality_profiles",
        id,
        quality_profile_row(&body, id),
        store::catalog::insert_quality_profile,
    )
    .await?
    {
        return Err(ApiError::NotFound);
    }
    let Json(out) = crate::routes::catalog::quality_profile(State(state), Path(id)).await?;
    Ok(accepted(out))
}

/// `QualityProfileService.Delete`: refused while a series or an import
/// list uses the profile.
pub async fn quality_profile_delete(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    let in_use = state
        .db()
        .call(move |conn| {
            let Some(p) = store::catalog::list_quality_profiles(conn)?
                .into_iter()
                .find(|p| p.id == id)
            else {
                return Ok(None);
            };
            let by_series: i64 = conn.query_row(
                "SELECT COUNT(*) FROM series WHERE quality_profile_id = ?1",
                [id],
                |r| r.get(0),
            )?;
            let by_list =
                store::providers::list_providers(conn, unarr_core::model::ProviderKind::ImportList)?
                    .iter()
                    .any(|l| l.attributes["qualityProfileId"].as_i64() == Some(id));
            Ok(Some((p.name, by_series > 0 || by_list)))
        })
        .await?;
    match in_use {
        None => Err(ApiError::ModelNotFound(format!(
            "QualityProfile with ID {id} does not exist"
        ))),
        Some((name, true)) => Err(ApiError::ClientException {
            message: format!("QualityProfile [{name}] is in use."),
            exception: "NzbDrone.Core.Profiles.Qualities.QualityProfileInUseException".into(),
        }),
        Some((_, false)) => {
            delete_from(&state, "quality_profiles", id).await?;
            Ok(ok_empty())
        }
    }
}

/// `GET /qualityprofile/schema`: `QualityProfileService.GetDefaultProfile("")`,
/// the qualities grouped by weight and every custom format at 0.
pub async fn quality_profile_schema(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    use unarr_core::quality::{Quality, SONARR_QUALITIES};
    let mut items = Vec::new();
    let mut group_id = 1000;
    let mut weights: Vec<i64> = Vec::new();
    for d in SONARR_QUALITIES {
        if !weights.contains(&d.weight) {
            weights.push(d.weight);
        }
    }
    for w in weights {
        let group: Vec<_> = SONARR_QUALITIES.iter().filter(|d| d.weight == w).collect();
        let quality = |id: i64| json!({"allowed": false, "items": [], "quality": Quality::by_id(id)});
        if group.len() == 1 {
            items.push(quality(group[0].quality.id));
            continue;
        }
        items.push(json!({
            "allowed": false,
            "id": group_id,
            "items": group.iter().map(|d| quality(d.quality.id)).collect::<Vec<_>>(),
            "name": format!("WEB {}p", group[0].quality.resolution),
        }));
        group_id += 1;
    }
    let formats = state
        .db()
        .call(|c| store::catalog::list_custom_formats(c))
        .await?;
    Ok(Json(json!({
        "cutoff": 0,
        "cutoffFormatScore": 0,
        "formatItems": formats.iter().map(|f| json!({"format": f.id, "name": f.name, "score": 0})).collect::<Vec<_>>(),
        "items": items,
        "minFormatScore": 0,
        "minUpgradeFormatScore": 1,
        "name": "",
        "upgradeAllowed": false,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_hosts_follow_the_original() {
        assert!(parse_hosts("").is_empty());
        assert_eq!(
            parse_hosts("a.local, 192.168.1.5; b"),
            ["a.local", "192.168.1.5", "b"]
        );
        assert_eq!(parse_hosts("::1,[::1]"), ["[::1]"]);
        assert!(valid_host("*.example.com"));
        assert!(valid_host("[::1]"));
        assert!(!valid_host("*"));
        assert!(!valid_host("http://x"));
        assert!(!valid_host("host:8989"));
    }

    #[test]
    fn naming_rules_follow_the_original() {
        let body = json!({
            "multiEpisodeStyle": 0,
            "standardEpisodeFormat": "{Series Title}",
            "dailyEpisodeFormat": "{Series Title} - {Air-Date}",
            "animeEpisodeFormat": "{Series Title} - {absolute:000}",
            "seriesFolderFormat": "{Series Title}",
            "seasonFolderFormat": "Season {season:00}",
            "specialsFolderFormat": "Specials",
        });
        let f = naming_failures(&body);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].property_name, "StandardEpisodeFormat");
    }
}
