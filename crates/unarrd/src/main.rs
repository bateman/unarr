use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Parser;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use unarr_api::{AppState, Lifecycle, LogBuffer, LogFiles, ProcessLog, RuntimeInfo, Settings};
use unarr_core::config::{AuthenticationMethod, AuthenticationRequired, HostConfig};
use unarr_core::{Db, Personality};

/// Unarr daemon: a lightweight, API-compatible replacement for Sonarr, Radarr and Lidarr.
#[derive(Parser, Debug)]
#[command(name = "unarrd", version, about)]
struct Args {
    /// Address to bind each personality to. Defaults to the BindAddress of
    /// its config.xml (`*` meaning every address); the port comes from there too.
    #[arg(long, env = "UNARR_BIND")]
    bind: Option<String>,

    /// Personalities to run.
    #[arg(long, env = "UNARR_PERSONALITIES", value_delimiter = ',', default_values = ["sonarr"])]
    personalities: Vec<String>,

    /// Directory holding Unarr's databases and configuration, one
    /// sub-directory per personality.
    #[arg(long, env = "UNARR_CONFIG", default_value = "/config", global = true)]
    data: PathBuf,

    /// Directory with the original application's config.xml and database to
    /// import on first start. Defaults to the personality's data directory,
    /// then to --data itself (a Sonarr /config mounted as is).
    #[arg(long, env = "UNARR_IMPORT_FROM")]
    import_from: Option<PathBuf>,

    /// The ffprobe binary for media info; `ffprobe` on the PATH when unset.
    #[arg(long, env = "UNARR_FFPROBE")]
    ffprobe: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Ask the daemon running on this machine whether it answers: `GET
    /// /ping` on every personality, exit status 0 when all do. What a
    /// container's health check runs; the image has no shell and no curl.
    Healthcheck,
}

/// The file in the data directory where the daemon says, once every
/// personality listens, which ones it runs and where: a line each, its
/// name, the address, the URL base. What the health check asks, and all
/// it knows: it is another process, which sees neither the arguments the
/// daemon was started with nor the settings in force (a port or a URL
/// base saved in the interface applies at the next start).
const LISTENING_FILE: &str = "listening";

/// A personality as the daemon serves it.
type Listening = (String, SocketAddr, String);

fn write_listening(data: &Path, personalities: &[Listening]) -> std::io::Result<()> {
    let text: String = personalities
        .iter()
        .map(|(name, addr, url_base)| format!("{name} {addr} {url_base}\n"))
        .collect();
    std::fs::write(data.join(LISTENING_FILE), text)
}

/// The personalities the daemon said it serves, each with the address to
/// reach it at from this machine and its URL base.
fn listening(data: &Path) -> anyhow::Result<Vec<Listening>> {
    let file = data.join(LISTENING_FILE);
    let text = std::fs::read_to_string(&file)
        .with_context(|| format!("the daemon has not started listening yet ({})", file.display()))?;
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let name = fields.next().unwrap_or("").to_string();
        let mut addr: SocketAddr = fields
            .next()
            .unwrap_or("")
            .parse()
            .with_context(|| format!("{} holds a line without an address: {line:?}", file.display()))?;
        // Listening on every address is, from here, listening on the loopback.
        if addr.ip().is_unspecified() {
            addr.set_ip(match addr.ip() {
                std::net::IpAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
                std::net::IpAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
            });
        }
        out.push((name, addr, fields.next().unwrap_or("").to_string()));
    }
    anyhow::ensure!(!out.is_empty(), "{} names no personality", file.display());
    Ok(out)
}

/// `GET <base>/ping` over a plain connection: whether the daemon answers
/// 200 with its `{"status":"OK"}`. The body counts: the web interface
/// answers 200 to paths it does not know, so a 200 alone proves nothing.
/// By hand, so that no proxy setting and no runtime stands between the
/// check and the daemon.
fn ping(addr: SocketAddr, url_base: &str, timeout: std::time::Duration) -> anyhow::Result<()> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    // In one piece: there is no reason to make the daemon wait for the rest.
    let request = format!("GET {url_base}/ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;
    // The answer is a few hundred bytes; what is not is not the ping's.
    let mut answer = Vec::new();
    stream.take(8 * 1024).read_to_end(&mut answer)?;
    let answer = String::from_utf8_lossy(&answer);
    let status = answer.lines().next().unwrap_or("").to_string();
    anyhow::ensure!(
        status.split_whitespace().nth(1) == Some("200"),
        "answered {:?}",
        status
    );
    let body = answer.split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or("");
    let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    anyhow::ensure!(
        compact == r#"{"status":"OK"}"#,
        "answered 200 with something that is not the ping's answer"
    );
    Ok(())
}

/// The `healthcheck` command: one line per personality the daemon runs,
/// and whether all of them answered.
fn healthcheck(data: &Path) -> bool {
    let personalities = match listening(data) {
        Ok(personalities) => personalities,
        Err(e) => {
            eprintln!("{e:#}");
            return false;
        }
    };
    let mut healthy = true;
    for (name, addr, url_base) in personalities {
        // Short enough for several personalities to be asked within the
        // time a container's health check is given.
        match ping(addr, &url_base, std::time::Duration::from_secs(3)) {
            Ok(()) => println!("{name}: ok"),
            Err(e) => {
                eprintln!("{name}: http://{addr}{url_base}/ping: {e:#}");
                healthy = false;
            }
        }
    }
    healthy
}

fn parse_personality(s: &str) -> anyhow::Result<Personality> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "sonarr" => Personality::Sonarr,
        "radarr" => Personality::Radarr,
        "lidarr" => Personality::Lidarr,
        other => anyhow::bail!("unknown personality {other:?}"),
    })
}

fn personality_dir(personality: Personality) -> &'static str {
    match personality {
        Personality::Sonarr => "sonarr",
        Personality::Radarr => "radarr",
        Personality::Lidarr => "lidarr",
    }
}

/// A 32-character hex API key, as the originals generate. Without a source
/// of randomness there is no safe key, so starting up fails.
fn generate_api_key() -> anyhow::Result<String> {
    unarr_core::random::hex::<16>().context("making the API key")
}

/// Where the original application's files are, if anywhere.
fn legacy_source(
    personality: Personality,
    own_dir: &Path,
    data: &Path,
    explicit: Option<&Path>,
) -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = match explicit {
        Some(p) => vec![p.to_path_buf()],
        None => vec![own_dir.to_path_buf(), data.to_path_buf()],
    };
    candidates
        .into_iter()
        .find(|dir| dir.join(personality.legacy_db_file()).is_file())
}

/// Whether this personality's database was imported from an original: the
/// importer records where the data came from. A blank first start creates the
/// database too, so the file's existence says nothing.
fn already_imported(personality: Personality, own_dir: &Path) -> anyhow::Result<bool> {
    let path = own_dir.join(personality.db_file());
    if !path.is_file() {
        return Ok(false);
    }
    let db = Db::open(&path)?;
    Ok(db
        .call_blocking(|conn| unarr_core::store::config::meta(conn, "source.app"))?
        .is_some())
}

/// `--import-from` names a directory that must hold the database of at least
/// one selected personality, unless that personality was imported already.
/// Otherwise a typo in the path would start a blank installation with a new
/// API key and no login, and silently at that.
fn check_import_source(import_from: &Path, personalities: &[Personality], data: &Path) -> anyhow::Result<()> {
    let mut found = false;
    for p in personalities {
        if import_from.join(p.legacy_db_file()).is_file()
            || already_imported(*p, &data.join(personality_dir(*p)))?
        {
            found = true;
        }
    }
    let expected: Vec<&str> = personalities.iter().map(|p| p.legacy_db_file()).collect();
    anyhow::ensure!(
        found,
        "--import-from {}: no {} found there and nothing imported yet; check the path",
        import_from.display(),
        expected.join(" or ")
    );
    Ok(())
}

/// Written next to a `config.xml` Unarr itself wrote (generated blank, or
/// copied from an original), so a later start can tell such a file from an
/// original the user put, or mounted, in Unarr's folder.
const WRITTEN_BY_UNARR: &str = ".config.xml.unarr";

/// The personality's `config.xml`: its own once an import happened, else the
/// original's (replacing a generated one: a blank first start must not keep
/// a fresh key and no login once the real installation is pointed at), else a
/// new one. An original is read in place and never rewritten: Unarr keeps
/// only the settings it understands, and the user may move back.
fn load_or_create_config(
    personality: Personality,
    own_dir: &Path,
    legacy: Option<&Path>,
    imported: bool,
) -> anyhow::Result<HostConfig> {
    let own = own_dir.join("config.xml");
    let marker = own_dir.join(WRITTEN_BY_UNARR);
    let legacy_xml = legacy.map(|d| d.join("config.xml")).filter(|p| p.is_file());
    if own.is_file() && (imported || legacy_xml.is_none()) {
        return unarr_import::config_xml::read(&own, personality.default_port());
    }
    if let Some(legacy_xml) = legacy_xml {
        let cfg = unarr_import::config_xml::read(&legacy_xml, personality.default_port())?;
        if own.is_file() {
            if same_file(&own, &legacy_xml) {
                // The original lives in Unarr's own folder: use it as it is.
                tracing::info!(path = %own.display(), "using the original config.xml in place");
                return Ok(cfg);
            }
            if !marker.is_file() {
                tracing::warn!(
                    path = %own.display(),
                    original = %legacy_xml.display(),
                    "keeping the config.xml already here, which Unarr did not write; the original is not copied over it (delete this file to take the original's settings)"
                );
                return unarr_import::config_xml::read(&own, personality.default_port());
            }
            tracing::warn!(
                from = %legacy_xml.display(),
                "replacing the config.xml generated by an earlier start with the imported one"
            );
        } else {
            tracing::info!(from = %legacy_xml.display(), "imported config.xml");
        }
        std::fs::write(&own, unarr_import::config_xml::render(&cfg))?;
        std::fs::write(&marker, "")?;
        return Ok(cfg);
    }
    let cfg = HostConfig {
        bind_address: "*".into(),
        port: personality.default_port(),
        ssl_port: 9898,
        enable_ssl: false,
        launch_browser: false,
        api_key: generate_api_key()?,
        authentication_method: AuthenticationMethod::None,
        authentication_required: AuthenticationRequired::Enabled,
        allowed_hosts: String::new(),
        trusted_networks: String::new(),
        branch: "main".into(),
        log_level: "info".into(),
        ssl_cert_path: String::new(),
        ssl_cert_password: String::new(),
        url_base: String::new(),
        instance_name: None,
        analytics_enabled: false,
        theme: "auto".into(),
        trust_cgnat_ip_addresses: false,
    };
    tracing::info!(path = %own.display(), "no config.xml found, writing a new one");
    std::fs::write(&own, unarr_import::config_xml::render(&cfg))?;
    std::fs::write(&marker, "")?;
    Ok(cfg)
}

/// Whether two paths name one file, through symlinks and `..`.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// What `main` needs to serve one personality.
struct Opened {
    state: AppState,
    bind: String,
    port: u16,
}

/// The database files, with SQLite's write-ahead log and its index.
const DB_SUFFIXES: [&str; 3] = ["", "-wal", "-shm"];

fn before_restore(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".before-restore");
    path.with_file_name(name)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Take up a restore the API staged in `<own dir>/restore` (see
/// `unarr_core::backup`). The current database and `config.xml` are set
/// aside as `*.before-restore` first. A backup of Unarr's own replaces the
/// database right here; for a backup the original application wrote, the
/// folder is returned: the importer reads it into a blank database, as on a
/// first start.
fn begin_restore(personality: Personality, own_dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    let dir = own_dir.join(unarr_core::backup::RESTORE_DIR);
    if !dir.is_dir() {
        return Ok(None);
    }
    let own_db = dir.join(personality.db_file());
    let original_db = dir.join(personality.legacy_db_file());
    if !own_db.is_file() && !original_db.is_file() {
        tracing::warn!(path = %dir.display(), "the staged restore holds no database; ignoring it");
        set_restore_aside(&dir);
        return Ok(None);
    }

    let db = own_dir.join(personality.db_file());
    let kept = before_restore(&db);
    for suffix in DB_SUFFIXES {
        let (from, to) = (with_suffix(&db, suffix), with_suffix(&kept, suffix));
        let _ = std::fs::remove_file(&to);
        if from.exists() {
            std::fs::rename(&from, &to).with_context(|| format!("setting {} aside", from.display()))?;
        }
    }
    let config = own_dir.join("config.xml");
    let _ = std::fs::remove_file(before_restore(&config));
    if config.is_file() {
        std::fs::copy(&config, before_restore(&config))?;
    }

    if !own_db.is_file() {
        tracing::info!(from = %original_db.display(), "restoring a backup of the original application through the importer");
        return Ok(Some(dir));
    }
    std::fs::rename(&own_db, &db).with_context(|| format!("moving {} into place", own_db.display()))?;
    // Unarr's own settings file comes back with its database; an original
    // read in place is never rewritten.
    let staged_config = dir.join("config.xml");
    if staged_config.is_file() && (!config.is_file() || own_dir.join(WRITTEN_BY_UNARR).is_file()) {
        std::fs::copy(&staged_config, &config)?;
        std::fs::write(own_dir.join(WRITTEN_BY_UNARR), "")?;
    }
    let _ = std::fs::remove_dir_all(&dir);
    tracing::info!(kept = %kept.display(), "restored the database from the staged backup");
    Ok(None)
}

/// Move a restore that cannot be applied out of the way, so the next start
/// does not try it again.
fn set_restore_aside(dir: &Path) {
    let failed = dir.with_file_name(format!("{}.failed", unarr_core::backup::RESTORE_DIR));
    let _ = std::fs::remove_dir_all(&failed);
    if let Err(e) = std::fs::rename(dir, &failed) {
        tracing::warn!(error = %e, path = %dir.display(), "could not set the staged restore aside");
    }
}

/// Undo `begin_restore` after the importer failed: the previous database
/// and `config.xml` come back.
fn abandon_restore(personality: Personality, own_dir: &Path) -> anyhow::Result<()> {
    let db = own_dir.join(personality.db_file());
    let kept = before_restore(&db);
    for suffix in DB_SUFFIXES {
        let (to, from) = (with_suffix(&db, suffix), with_suffix(&kept, suffix));
        let _ = std::fs::remove_file(&to);
        if from.exists() {
            std::fs::rename(&from, &to).with_context(|| format!("putting {} back", to.display()))?;
        }
    }
    let config = own_dir.join("config.xml");
    if before_restore(&config).is_file() {
        std::fs::rename(before_restore(&config), &config)?;
    }
    set_restore_aside(&own_dir.join(unarr_core::backup::RESTORE_DIR));
    Ok(())
}

/// Open the personality's database and settings, importing the original's
/// from `legacy` when nothing was imported yet.
fn open_database(
    personality: Personality,
    own_dir: &Path,
    legacy: Option<&Path>,
) -> anyhow::Result<(Db, HostConfig)> {
    let db = Db::open(&own_dir.join(personality.db_file()))?;
    let imported = db
        .call_blocking(|conn| unarr_core::store::config::meta(conn, "source.app"))?
        .is_some();
    let host = load_or_create_config(personality, own_dir, legacy, imported)?;

    if !imported {
        if let Some(src) = legacy {
            match personality {
                Personality::Sonarr => {
                    let report = unarr_import::sonarr::import(&src.join(personality.legacy_db_file()), &db)?;
                    tracing::info!(
                        from = %src.display(),
                        migration = report.migration_version,
                        series = report.counts.get("Series").copied().unwrap_or(0),
                        episodes = report.counts.get("Episodes").copied().unwrap_or(0),
                        "imported legacy database"
                    );
                }
                other => tracing::warn!(?other, "import not implemented yet for this personality"),
            }
        }
    }
    // What the originals make sure is there at every start.
    if personality == Personality::Sonarr {
        db.call_blocking(|conn| unarr_core::defaults::ensure(conn))?;
    }
    Ok((db, host))
}

/// `open_database`, after taking up a restore staged before this start. A
/// restore that fails leaves the installation as it was.
fn open_restoring(
    personality: Personality,
    own_dir: &Path,
    legacy: Option<&Path>,
) -> anyhow::Result<(Db, HostConfig)> {
    let Some(staged) = begin_restore(personality, own_dir)? else {
        return open_database(personality, own_dir, legacy);
    };
    match open_database(personality, own_dir, Some(&staged)) {
        Ok(opened) => {
            let _ = std::fs::remove_dir_all(&staged);
            tracing::info!("restored the original application's backup");
            Ok(opened)
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "the staged restore failed; keeping the previous database");
            abandon_restore(personality, own_dir)?;
            open_database(personality, own_dir, legacy)
        }
    }
}

fn open_personality(personality: Personality, args: &Args, logs: &LogBuffer) -> anyhow::Result<Opened> {
    let own_dir = args.data.join(personality_dir(personality));
    std::fs::create_dir_all(&own_dir).with_context(|| format!("creating {}", own_dir.display()))?;
    let legacy = legacy_source(personality, &own_dir, &args.data, args.import_from.as_deref());
    let (db, mut host) = open_restoring(personality, &own_dir, legacy.as_deref())?;

    // Host settings saved through the API since: the original's
    // config.xml is never rewritten, they are laid over it.
    let saved = db.call_blocking(|conn| {
        unarr_core::store::config::meta(conn, unarr_api::routes::settings_writes::HOST_OVERRIDES)
    })?;
    unarr_api::routes::settings_writes::apply_saved_host_overrides(&mut host, saved.as_deref());
    // The environment wins over both, and the listener must hear of it:
    // the port and the address are read here.
    host.apply_environment(personality.app_name(), std::env::vars());
    let port = host.port;
    if host.enable_ssl {
        tracing::warn!(
            ssl_port = host.ssl_port,
            "EnableSsl is on in config.xml but Unarr does not serve HTTPS yet; put a reverse proxy in front"
        );
    }
    // `*` is the originals' "every address".
    let bind = match args.bind.as_deref().unwrap_or(&host.bind_address) {
        "*" | "" => "0.0.0.0".to_string(),
        other => other.to_string(),
    };
    let settings = Settings::from_host_config(host, personality);
    let runtime = RuntimeInfo::detect(own_dir);
    // The posters the original already downloaded, copied once into Unarr's
    // own folder: refreshes write there, never into the original's.
    if let Some(covers) = legacy.map(|d| d.join("MediaCover")).filter(|d| d.is_dir()) {
        if !runtime.media_cover_dir.exists() {
            match copy_dir(&covers, &runtime.media_cover_dir) {
                Ok(n) => {
                    tracing::info!(files = n, from = %covers.display(), "copied the original's media covers")
                }
                Err(e) => {
                    tracing::warn!(error = %e, from = %covers.display(), "could not copy the original's media covers")
                }
            }
        }
    }
    Ok(Opened {
        state: AppState::new(personality, db, settings, runtime, logs.clone()),
        bind,
        port,
    })
}

/// Copy a directory tree, files only (no links followed); the number of
/// files copied.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(to)?;
    let mut copied = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copied += copy_dir(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// Ctrl+C, or SIGTERM as `docker stop` sends it.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing the SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}

/// What the process logs at, from `RUST_LOG`; info without it.
fn log_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())
}

/// What a level of Settings > General (`info`, `debug`, `trace`) has
/// logged: Unarr's own code at that level, the libraries under it at info.
fn settings_filter(level: &str) -> tracing_subscriber::EnvFilter {
    match level.trim().to_ascii_lowercase().as_str() {
        "trace" => "info,unarr=trace".into(),
        "debug" => "info,unarr=debug".into(),
        _ => "info".into(),
    }
}

/// The level of Settings > General as the log files know it.
fn settings_level(level: &str) -> tracing::level_filters::LevelFilter {
    match level.trim().to_ascii_lowercase().as_str() {
        "trace" => tracing::level_filters::LevelFilter::TRACE,
        "debug" => tracing::level_filters::LevelFilter::DEBUG,
        _ => tracing::level_filters::LevelFilter::INFO,
    }
}

/// Changes what a personality's dispatcher lets through.
type FilterHandle =
    tracing_subscriber::reload::Handle<tracing_subscriber::EnvFilter, tracing_subscriber::Registry>;

/// Where a personality logs: the console, its recent lines (`GET /log`)
/// and its files; and the handle that changes the level it logs at.
fn personality_dispatch(
    logs: &LogBuffer,
    files: &LogFiles,
    filter: tracing_subscriber::EnvFilter,
) -> (tracing::Dispatch, FilterHandle) {
    let (filter, handle) = tracing_subscriber::reload::Layer::new(filter);
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .with(logs.clone())
            .with(files.clone()),
    );
    (dispatch, handle)
}

/// The runtime a personality runs on. Every thread it starts, workers and
/// blocking ones alike, logs to `dispatch` for as long as it lives: that
/// is how a line lands in the files of the personality that logged it,
/// whichever task or library wrote it.
fn personality_runtime(name: &str, dispatch: &tracing::Dispatch) -> std::io::Result<tokio::runtime::Runtime> {
    let dispatch = dispatch.clone();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name(format!("unarr-{name}"))
        .on_thread_start(move || {
            // Never dropped: the thread keeps the dispatcher until it ends.
            std::mem::forget(tracing::dispatcher::set_default(&dispatch));
        })
        .build()
}

/// A personality being served.
struct Running {
    dispatch: tracing::Dispatch,
    runtime: tokio::runtime::Runtime,
    state: AppState,
}

/// Start what a personality runs: the command runtime and the HTTP server.
async fn serve(
    personality: Personality,
    state: AppState,
    bind: String,
    port: u16,
) -> anyhow::Result<SocketAddr> {
    state
        .start_runtime()
        .await
        .with_context(|| format!("starting the command runtime for {personality:?}"))?;
    // The import read whole tables into memory; the allocator keeps the
    // pages unless told otherwise.
    unarr_core::fs::release_memory();
    // A (host, port) pair resolves names and IPv6 literals such as `::`.
    let listener = tokio::net::TcpListener::bind((bind.as_str(), port))
        .await
        .with_context(|| format!("binding {bind}:{port}"))?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, ?personality, "listening");
    tokio::spawn(
        axum::serve(
            listener,
            unarr_api::router(state).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .into_future(),
    );
    Ok(addr)
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if let Some(Command::Healthcheck) = args.command {
        std::process::exit(if healthcheck(&args.data) { 0 } else { 1 });
    }
    unarr_core::fs::tune_allocator();
    // What is logged outside any personality goes to the console and, through
    // `process_log`, to every personality's recent lines and files.
    let process_log = ProcessLog::new();
    tracing_subscriber::registry()
        .with(log_filter())
        .with(tracing_subscriber::fmt::layer())
        .with(process_log.clone())
        .init();

    let personalities: Vec<Personality> = args
        .personalities
        .iter()
        .map(|name| parse_personality(name))
        .collect::<anyhow::Result<_>>()?;
    unarr_core::mediafiles::mediainfo::set_ffprobe_path(args.ffprobe.clone());
    match unarr_core::mediafiles::mediainfo::ffprobe_version() {
        Some(v) => tracing::info!(ffprobe = %v, "media info enabled"),
        None => tracing::warn!(
            path = %unarr_core::mediafiles::mediainfo::ffprobe_path().display(),
            "ffprobe not found; media info and sample detection are off"
        ),
    }
    if let Some(import_from) = &args.import_from {
        check_import_source(import_from, &personalities, &args.data)?;
    }

    // Whatever an earlier run said: nothing listens yet.
    let _ = std::fs::remove_file(args.data.join(LISTENING_FILE));
    let mut listening: Vec<Listening> = Vec::new();
    let mut running = Vec::new();
    for personality in personalities {
        // The log files live with the personality's data, where the
        // originals keep theirs and under their names (`sonarr.txt`).
        let name = personality.app_name().to_lowercase();
        let logs = LogBuffer::new();
        let files = LogFiles::open(&args.data.join(personality_dir(personality)).join("logs"), &name);
        process_log.add(logs.clone(), files.clone());
        let (dispatch, filter) = personality_dispatch(&logs, &files, log_filter());
        let runtime = personality_runtime(&name, &dispatch)
            .with_context(|| format!("starting the runtime for {personality:?}"))?;
        // This thread opens the personality and logs as it while it does;
        // the database thread started here does the same from then on.
        let state = tracing::dispatcher::with_default(&dispatch, || -> anyhow::Result<AppState> {
            let opened = open_personality(personality, &args, &logs)?;
            let state = opened.state;
            // `ReconfigureLogging`: the level of Settings > General, now and
            // whenever it is saved. `RUST_LOG`, when set, is the operator's
            // word and stays in force.
            if std::env::var_os("RUST_LOG").is_some() {
                tracing::info!(
                    "RUST_LOG is set: it decides what is logged, not the Log Level of the settings"
                );
            } else {
                let files = files.clone();
                state.set_log_level_hook(move |level| {
                    files.set_level(settings_level(level));
                    if let Err(e) = filter.reload(settings_filter(level)) {
                        tracing::warn!("The log level could not be changed: {e}");
                    }
                });
                state.apply_log_level();
            }
            // A mock serving every metadata host under `<url>/<host>/`, for
            // replays against `scripts/mock-servarr.py`; unset in real use.
            if let Ok(url) = std::env::var("UNARR_METADATA_UPSTREAM") {
                tracing::warn!(%url, "metadata hosts redirected (UNARR_METADATA_UPSTREAM)");
                state.set_metadata_upstreams(unarr_core::metadata::Upstreams::mock(&url));
            }
            let addr = runtime.block_on(serve(personality, state.clone(), opened.bind, opened.port))?;
            listening.push((name.clone(), addr, state.settings().url_base.clone()));
            Ok(state)
        })?;
        running.push(Running {
            dispatch,
            runtime,
            state,
        });
    }
    // For `unarrd healthcheck`: every personality listens. Not being able
    // to say so stops nothing but the health check, which then reports it.
    if let Err(e) = write_listening(&args.data, &listening) {
        tracing::warn!(error = %e, "could not record the listening addresses in {}", args.data.display());
    }
    // Every personality has the lines of the start; no need to keep them.
    process_log.seal();

    // Until a signal, or a shutdown or restart asked through the API.
    let states: Vec<AppState> = running.iter().map(|p| p.state.clone()).collect();
    let asked = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the signal handler")?
        .block_on(async {
            let from_api = futures_first(states.iter().map(|s| s.lifecycle_requested()));
            tokio::select! {
                _ = shutdown_signal() => Lifecycle::Shutdown,
                what = from_api => {
                    // Let the answer to the request leave first.
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    what
                }
            }
        });
    match asked {
        Lifecycle::Shutdown => tracing::info!("shutting down"),
        Lifecycle::Restart => tracing::info!("restarting"),
    }
    // Running commands get a moment to finish: a grab cut between the
    // client and its history row would leave an untracked download.
    for personality in &running {
        tracing::dispatcher::with_default(&personality.dispatch, || {
            personality.runtime.block_on(async {
                if let Ok(commands) = personality.state.commands() {
                    commands.shutdown(std::time::Duration::from_secs(8)).await;
                }
            });
        });
    }
    if asked == Lifecycle::Restart {
        // The listeners and the databases go with the runtimes; then the
        // same program takes this process's place, with its arguments.
        drop(running);
        restart_in_place()?;
    }
    Ok(())
}

/// The first of several futures to finish; never, when there are none.
async fn futures_first<F: std::future::Future>(futures: impl Iterator<Item = F>) -> F::Output {
    let mut all: Vec<std::pin::Pin<Box<F>>> = futures.map(Box::pin).collect();
    std::future::poll_fn(move |cx| {
        for f in all.iter_mut() {
            if let std::task::Poll::Ready(out) = f.as_mut().poll(cx) {
                return std::task::Poll::Ready(out);
            }
        }
        std::task::Poll::Pending
    })
    .await
}

/// `LifecycleService.Restart`: start again as the same program. Where the
/// process cannot be replaced, it exits and whatever supervises it (the
/// container's restart policy) starts it again.
fn restart_in_place() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let program = std::env::current_exe().context("finding the running program")?;
        let error = std::process::Command::new(program)
            .args(std::env::args_os().skip(1))
            .exec();
        Err(error).context("starting again")
    }
    #[cfg(not(unix))]
    {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_source_must_hold_a_database_or_an_import_must_exist() {
        let dir = std::env::temp_dir().join(format!("unarr-import-source-{}", std::process::id()));
        let source = dir.join("source");
        let data = dir.join("data");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let all = [Personality::Sonarr, Personality::Radarr];

        // Nothing anywhere: a typo in the path, most likely.
        let err = check_import_source(&source, &all, &data).unwrap_err().to_string();
        assert!(err.contains("sonarr.db or radarr.db"), "{err}");

        // The original database is there for one of the personalities.
        std::fs::write(source.join("radarr.db"), b"").unwrap();
        check_import_source(&source, &all, &data).unwrap();

        // A database left by a blank first start is not an import.
        std::fs::remove_file(source.join("radarr.db")).unwrap();
        let own_dir = data.join("sonarr");
        std::fs::create_dir_all(&own_dir).unwrap();
        drop(Db::open(&own_dir.join(Personality::Sonarr.db_file())).unwrap());
        assert!(check_import_source(&source, &all, &data).is_err());

        // Once the importer has recorded its source, the personality counts as imported.
        {
            let db = Db::open(&own_dir.join(Personality::Sonarr.db_file())).unwrap();
            db.call_blocking(|conn| unarr_core::store::config::set_meta(conn, "source.app", "sonarr"))
                .unwrap();
        }
        check_import_source(&source, &all, &data).unwrap();
        assert!(check_import_source(&source, &[Personality::Radarr], &data).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A blank first start writes a config.xml; pointing Unarr at the real
    /// installation afterwards must take the original's settings, not keep
    /// the generated key and the missing login.
    #[test]
    fn generated_config_yields_to_the_imported_one_until_the_import_happened() {
        let dir = std::env::temp_dir().join(format!("unarr-config-{}", std::process::id()));
        let own_dir = dir.join("sonarr");
        std::fs::create_dir_all(&own_dir).unwrap();
        let fixture = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/sonarr"
        ));

        // Blank start: a generated file, random key, no login.
        let generated = load_or_create_config(Personality::Sonarr, &own_dir, None, false).unwrap();
        assert_eq!(generated.authentication_method, AuthenticationMethod::None);
        assert!(own_dir.join("config.xml").is_file());

        // Then --import-from the real installation, nothing imported yet: the original wins.
        let imported = load_or_create_config(Personality::Sonarr, &own_dir, Some(fixture), false).unwrap();
        assert_eq!(imported.api_key, "0123456789abcdef0123456789abcdef");
        assert_eq!(imported.authentication_method, AuthenticationMethod::Forms);

        // After the import, Unarr's own file is authoritative even with the source still around.
        let again = load_or_create_config(Personality::Sonarr, &own_dir, Some(fixture), true).unwrap();
        assert_eq!(again, imported);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The original installation mounted as Unarr's own folder, or an
    /// original the user put there: read in place, byte for byte intact.
    #[test]
    fn an_original_config_in_unarrs_folder_is_never_rewritten() {
        let dir = std::env::temp_dir().join(format!("unarr-config-orig-{}", std::process::id()));
        let own_dir = dir.join("sonarr");
        std::fs::create_dir_all(&own_dir).unwrap();
        let fixture = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/sonarr"
        ));
        let original = std::fs::read(fixture.join("config.xml")).unwrap();
        std::fs::write(own_dir.join("config.xml"), &original).unwrap();

        // Source and destination are the same file.
        let cfg = load_or_create_config(Personality::Sonarr, &own_dir, Some(&own_dir), false).unwrap();
        assert_eq!(cfg.api_key, "0123456789abcdef0123456789abcdef");
        assert_eq!(std::fs::read(own_dir.join("config.xml")).unwrap(), original);
        assert!(!own_dir.join(WRITTEN_BY_UNARR).exists());

        // A file Unarr did not write is kept even when another original is
        // pointed at.
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let mut changed = String::from_utf8(original.clone()).unwrap();
        changed = changed.replace(
            "0123456789abcdef0123456789abcdef",
            "fedcba9876543210fedcba9876543210",
        );
        std::fs::write(other.join("config.xml"), changed).unwrap();
        let cfg = load_or_create_config(Personality::Sonarr, &own_dir, Some(&other), false).unwrap();
        assert_eq!(cfg.api_key, "0123456789abcdef0123456789abcdef");
        assert_eq!(std::fs::read(own_dir.join("config.xml")).unwrap(), original);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn meta(db: &Db, key: &'static str) -> Option<String> {
        db.call_blocking(move |conn| unarr_core::store::config::meta(conn, key))
            .unwrap()
    }

    /// A staged backup of Unarr's own replaces the database at the next
    /// start; what was there is kept next to it.
    #[test]
    fn a_staged_unarr_backup_replaces_the_database() {
        let dir = std::env::temp_dir().join(format!("unarr-restore-own-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let own_dir = dir.join("sonarr");
        let staged = own_dir.join(unarr_core::backup::RESTORE_DIR);
        std::fs::create_dir_all(&staged).unwrap();
        let p = Personality::Sonarr;

        // The backup: a database that says so, and its settings file.
        {
            let db = Db::open(&staged.join(p.db_file())).unwrap();
            db.call_blocking(|conn| unarr_core::store::config::set_meta(conn, "test.state", "backup"))
                .unwrap();
            db.call_blocking(|conn| {
                conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                    .map_err(Into::into)
            })
            .unwrap();
        }
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(with_suffix(&staged.join(p.db_file()), suffix));
        }
        // The installation: a newer database and a generated config.
        let (db, before) = open_database(p, &own_dir, None).unwrap();
        db.call_blocking(|conn| unarr_core::store::config::set_meta(conn, "test.state", "current"))
            .unwrap();
        drop(db);
        std::fs::copy(own_dir.join("config.xml"), staged.join("config.xml")).unwrap();
        // The settings changed since the backup was taken.
        let _ = std::fs::remove_file(own_dir.join("config.xml"));
        let changed = load_or_create_config(p, &own_dir, None, false).unwrap();
        assert_ne!(changed.api_key, before.api_key);

        let (db, host) = open_restoring(p, &own_dir, None).unwrap();
        assert_eq!(meta(&db, "test.state").as_deref(), Some("backup"));
        assert_eq!(host.api_key, before.api_key);
        assert!(!staged.exists());
        drop(db);
        let kept = Db::open(&before_restore(&own_dir.join(p.db_file()))).unwrap();
        assert_eq!(meta(&kept, "test.state").as_deref(), Some("current"));
        drop(kept);

        // Nothing staged any more: the next start changes nothing.
        let (db, _) = open_restoring(p, &own_dir, None).unwrap();
        assert_eq!(meta(&db, "test.state").as_deref(), Some("backup"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A staged backup the original application wrote goes through the
    /// importer; one that cannot be imported leaves everything as it was.
    #[test]
    fn a_staged_original_backup_is_imported_or_abandoned() {
        let dir = std::env::temp_dir().join(format!("unarr-restore-orig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let own_dir = dir.join("sonarr");
        let staged = own_dir.join(unarr_core::backup::RESTORE_DIR);
        std::fs::create_dir_all(&staged).unwrap();
        let p = Personality::Sonarr;
        let fixture = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/sonarr"
        ));

        // A blank installation with its generated key.
        let (db, generated) = open_database(p, &own_dir, None).unwrap();
        db.call_blocking(|conn| unarr_core::store::config::set_meta(conn, "test.state", "current"))
            .unwrap();
        drop(db);

        // A broken backup first: not a database at all.
        std::fs::write(staged.join(p.legacy_db_file()), b"not a database").unwrap();
        std::fs::copy(fixture.join("config.xml"), staged.join("config.xml")).unwrap();
        let (db, host) = open_restoring(p, &own_dir, None).unwrap();
        assert_eq!(meta(&db, "test.state").as_deref(), Some("current"));
        assert_eq!(meta(&db, "source.app"), None);
        assert_eq!(host.api_key, generated.api_key);
        assert!(!staged.exists());
        assert!(own_dir.join("restore.failed").is_dir());
        drop(db);

        // Then Sonarr's own files, as its backup zip holds them.
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::copy(fixture.join("sonarr.db"), staged.join(p.legacy_db_file())).unwrap();
        std::fs::copy(fixture.join("config.xml"), staged.join("config.xml")).unwrap();
        let (db, host) = open_restoring(p, &own_dir, None).unwrap();
        assert!(meta(&db, "source.app").is_some());
        assert_eq!(meta(&db, "test.state"), None);
        assert_eq!(host.api_key, "0123456789abcdef0123456789abcdef");
        let series: i64 = db
            .call_blocking(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM series", [], |r| r.get(0))?))
            .unwrap();
        assert!(series > 0);
        assert!(!staged.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A listener that answers `GET /tv/ping` with `status` and `body`, and
    /// anything else as the web interface does: 200 and a page.
    fn answering(status: &'static str, body: &'static str) -> SocketAddr {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                // The whole request, however many pieces it comes in.
                let mut request = Vec::new();
                let mut piece = [0u8; 512];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut piece) {
                        Ok(n) if n > 0 => request.extend_from_slice(&piece[..n]),
                        _ => break,
                    }
                }
                let request = String::from_utf8_lossy(&request).into_owned();
                let (line, content) = if request.starts_with("GET /tv/ping ") {
                    (status, body)
                } else {
                    ("200 OK", "<!doctype html><title>Sonarr</title>")
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {line}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{content}",
                    content.len()
                );
            }
        });
        addr
    }

    /// Healthy is the ping's own answer and nothing else: not another
    /// status, not the page the web interface serves for a path it does
    /// not know, not silence.
    #[test]
    fn the_ping_is_healthy_only_on_its_own_answer() {
        let wait = std::time::Duration::from_secs(2);
        let up = answering("200 OK", r#"{"status":"OK"}"#);
        ping(up, "/tv", wait).expect("the ping's answer is healthy");
        // The wrong URL base: a 200, and a page.
        let error = ping(up, "", wait).unwrap_err().to_string();
        assert!(error.contains("not the ping's answer"), "{error}");
        let failing = answering("500 Internal Server Error", r#"{"status":"Error"}"#);
        let error = ping(failing, "/tv", wait).unwrap_err().to_string();
        assert!(error.contains("500"), "{error}");
        let lying = answering("200 OK", r#"{"status":"Error"}"#);
        assert!(ping(lying, "/tv", wait).is_err());
        // A port nobody listens on.
        let free = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        assert!(ping(free, "/tv", wait).is_err());
    }

    /// The health check asks the personalities the daemon said it runs,
    /// where it said it listens: nothing before it does, the loopback when
    /// it listens on every address, the URL base it serves under. Its own
    /// arguments name no personality.
    #[test]
    fn the_health_check_asks_what_the_daemon_said() {
        let data = std::env::temp_dir().join(format!("unarr-healthcheck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        std::fs::create_dir_all(&data).unwrap();
        assert!(listening(&data).is_err(), "not started yet");
        assert!(!healthcheck(&data));
        let said = |lines: &[(&str, &str, &str)]| {
            let lines: Vec<Listening> = lines
                .iter()
                .map(|(name, addr, base)| (name.to_string(), addr.parse().unwrap(), base.to_string()))
                .collect();
            write_listening(&data, &lines).unwrap();
            listening(&data)
                .unwrap()
                .into_iter()
                .map(|(name, addr, base)| format!("{name} {addr} {base}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            said(&[("radarr", "0.0.0.0:7878", "/movies"), ("lidarr", "[::]:8686", "")]),
            ["radarr 127.0.0.1:7878 /movies", "lidarr [::1]:8686 "]
        );
        assert_eq!(
            said(&[("sonarr", "192.168.250.251:8989", "")]),
            ["sonarr 192.168.250.251:8989 "]
        );
        for bad in ["", "sonarr\n", "sonarr not-an-address /tv\n"] {
            std::fs::write(data.join(LISTENING_FILE), bad).unwrap();
            assert!(listening(&data).is_err(), "{bad:?}");
        }

        // Two personalities, one of them not answering: unhealthy, and
        // healthy once both do.
        let up = answering("200 OK", r#"{"status":"OK"}"#);
        let free = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        write_listening(
            &data,
            &[
                ("sonarr".into(), up, "/tv".into()),
                ("radarr".into(), free, String::new()),
            ],
        )
        .unwrap();
        assert!(!healthcheck(&data));
        write_listening(
            &data,
            &[
                ("sonarr".into(), up, "/tv".into()),
                ("radarr".into(), up, "/tv".into()),
            ],
        )
        .unwrap();
        assert!(healthcheck(&data));

        // The check takes the data directory and nothing else.
        let args = Args::parse_from(["unarrd", "healthcheck", "--data", data.to_str().unwrap()]);
        assert_eq!(args.data, data);
        assert!(Args::try_parse_from(["unarrd", "healthcheck", "--bind", "10.0.0.5"]).is_err());
        assert!(Args::try_parse_from(["unarrd", "healthcheck", "--personalities", "radarr"]).is_err());
        let _ = std::fs::remove_dir_all(&data);
    }

    /// The level of Settings > General changes what a personality logs
    /// while it runs: debug lines of Unarr's own code start to be written,
    /// to the debug file too, and stop again.
    #[test]
    fn the_saved_log_level_takes_effect_at_once() {
        let dir = std::env::temp_dir().join(format!("unarr-log-level-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let logs = LogBuffer::new();
        let files = LogFiles::open(&dir, "sonarr");
        let (dispatch, filter) = personality_dispatch(&logs, &files, "info".into());
        let apply = |level: &str| {
            files.set_level(settings_level(level));
            filter.reload(settings_filter(level)).unwrap();
        };
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
        tracing::dispatcher::with_default(&dispatch, || {
            apply("info");
            tracing::debug!("quiet at info");
            apply("debug");
            tracing::debug!("heard at debug");
            tracing::trace!("quiet at debug");
            // A library's debug lines stay out at any level.
            tracing::debug!(target: "hyper::client", "a library at debug");
            apply("trace");
            tracing::trace!("heard at trace");
            apply("info");
            tracing::debug!("quiet again");
            tracing::info!("always there");
        });
        let (info, debug, trace) = (
            read("sonarr.txt"),
            read("sonarr.debug.txt"),
            read("sonarr.trace.txt"),
        );
        assert!(
            info.contains("always there") && !info.contains("heard at debug"),
            "{info}"
        );
        // The debug file takes debug and worse; the trace file, written
        // from the moment the level was trace, everything.
        assert!(
            debug.contains("heard at debug") && !debug.contains("heard at trace"),
            "{debug}"
        );
        assert!(
            trace.contains("heard at trace") && !trace.contains("heard at debug"),
            "{trace}"
        );
        for file in [&info, &debug, &trace] {
            for never in [
                "quiet at info",
                "quiet at debug",
                "a library at debug",
                "quiet again",
            ] {
                assert!(!file.contains(never), "'{never}' in:\n{file}");
            }
        }
        // Back at info the debug file takes nothing more.
        assert!(!debug.contains("always there"), "{debug}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One process, two personalities: whatever a personality's threads
    /// log (its tasks, the tasks they spawn, blocking work, its database
    /// thread) lands in its own files and recent lines, and nowhere else.
    #[test]
    fn each_personality_logs_to_its_own_files() {
        let root = std::env::temp_dir().join(format!("unarr-personality-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let start = |name: &'static str| {
            let logs = LogBuffer::new();
            let files = LogFiles::open(&root.join(name).join("logs"), name);
            // Not the environment's level: the test asserts on info lines
            // whatever `RUST_LOG` the shell exports.
            let (dispatch, _) = personality_dispatch(&logs, &files, "info".into());
            let runtime = personality_runtime(name, &dispatch).unwrap();
            // As `main` does: opened, and its database started, as itself.
            let db = tracing::dispatcher::with_default(&dispatch, || {
                tracing::info!("{name} opening");
                Db::open_in_memory().unwrap()
            });
            (logs, dispatch, runtime, db)
        };
        let sonarr = start("sonarr");
        let radarr = start("radarr");
        // Interleaved, so that neither merely happens to be the last set.
        for (name, (_, dispatch, runtime, db)) in
            [("sonarr", &sonarr), ("radarr", &radarr), ("sonarr", &sonarr)]
        {
            let db = db.clone();
            tracing::dispatcher::with_default(dispatch, || {
                runtime.block_on(async move {
                    tokio::spawn(async move {
                        tracing::info!("{name} from a task");
                        tokio::spawn(async move { tracing::info!("{name} from a task of a task") })
                            .await
                            .unwrap();
                        tokio::task::spawn_blocking(move || tracing::warn!("{name} from blocking work"))
                            .await
                            .unwrap();
                        db.call(move |_| {
                            tracing::info!("{name} from the database thread");
                            Ok(())
                        })
                        .await
                        .unwrap();
                    })
                    .await
                    .unwrap();
                });
            });
        }
        // A thread of neither logs to neither.
        tracing::info!("sonarr radarr from the process");

        let wrote = [
            "opening",
            "from a task",
            "from a task of a task",
            "from blocking work",
            "from the database thread",
        ];
        for (name, other, (logs, ..)) in [("sonarr", "radarr", &sonarr), ("radarr", "sonarr", &radarr)] {
            let file =
                std::fs::read_to_string(root.join(name).join("logs").join(format!("{name}.txt"))).unwrap();
            let recent: Vec<String> = logs.entries().into_iter().map(|e| e.message).collect();
            for what in wrote {
                let line = format!("{name} {what}");
                assert!(file.contains(&line), "{name}.txt lacks '{line}':\n{file}");
                assert!(recent.contains(&line), "{name}'s recent lines lack '{line}'");
            }
            assert!(
                !file.contains(other),
                "{name}.txt holds a line of {other}:\n{file}"
            );
            assert!(recent.iter().all(|m| !m.contains(other)));
        }
        drop((sonarr, radarr));
        let _ = std::fs::remove_dir_all(&root);
    }
}
