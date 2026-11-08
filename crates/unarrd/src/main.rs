use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::Parser;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use unarr_api::{AppState, LogBuffer, RuntimeInfo, Settings};
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
    #[arg(long, env = "UNARR_CONFIG", default_value = "/config")]
    data: PathBuf,

    /// Directory with the original application's config.xml and database to
    /// import on first start. Defaults to the personality's data directory,
    /// then to --data itself (a Sonarr /config mounted as is).
    #[arg(long, env = "UNARR_IMPORT_FROM")]
    import_from: Option<PathBuf>,
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
    let mut bytes = [0u8; 16];
    let mut urandom = std::fs::File::open("/dev/urandom").context("opening /dev/urandom for the API key")?;
    std::io::Read::read_exact(&mut urandom, &mut bytes).context("reading /dev/urandom for the API key")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
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

fn open_personality(personality: Personality, args: &Args, logs: &LogBuffer) -> anyhow::Result<Opened> {
    let own_dir = args.data.join(personality_dir(personality));
    std::fs::create_dir_all(&own_dir).with_context(|| format!("creating {}", own_dir.display()))?;
    let legacy = legacy_source(personality, &own_dir, &args.data, args.import_from.as_deref());

    let db = Db::open(&own_dir.join(personality.db_file()))?;
    let imported = db
        .call_blocking(|conn| unarr_core::store::config::meta(conn, "source.app"))?
        .is_some();
    let host = load_or_create_config(personality, &own_dir, legacy.as_deref(), imported)?;

    if !imported {
        if let Some(src) = &legacy {
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
    let mut runtime = RuntimeInfo::detect(own_dir);
    // Serve the posters the original already downloaded, until Unarr fetches
    // its own.
    if let Some(covers) = legacy.map(|d| d.join("MediaCover")).filter(|d| d.is_dir()) {
        if !runtime.media_cover_dir.is_dir() {
            runtime.media_cover_dir = covers;
        }
    }
    Ok(Opened {
        state: AppState::new(personality, db, settings, runtime, logs.clone()),
        bind,
        port,
    })
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let logs = LogBuffer::new();
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .with(logs.clone())
        .init();
    let args = Args::parse();

    let personalities: Vec<Personality> = args
        .personalities
        .iter()
        .map(|name| parse_personality(name))
        .collect::<anyhow::Result<_>>()?;
    if let Some(import_from) = &args.import_from {
        check_import_source(import_from, &personalities, &args.data)?;
    }

    let mut servers = Vec::new();
    let mut states = Vec::new();
    for personality in personalities {
        let opened = open_personality(personality, &args, &logs)?;
        let (state, port) = (opened.state, opened.port);
        states.push(state.clone());
        state
            .start_runtime()
            .await
            .with_context(|| format!("starting the command runtime for {personality:?}"))?;
        // The import read whole tables into memory; the allocator keeps the
        // pages unless told otherwise.
        unarr_core::fs::release_memory();
        // A (host, port) pair resolves names and IPv6 literals such as `::`.
        let listener = tokio::net::TcpListener::bind((opened.bind.as_str(), port))
            .await
            .with_context(|| format!("binding {}:{port}", opened.bind))?;
        let addr = listener.local_addr()?;
        tracing::info!(%addr, ?personality, "listening");
        servers.push(tokio::spawn(
            axum::serve(
                listener,
                unarr_api::router(state).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .into_future(),
        ));
    }

    shutdown_signal().await;
    tracing::info!("shutting down");
    // Running commands get a moment to finish: a grab cut between the
    // client and its history row would leave an untracked download.
    for state in &states {
        if let Ok(runtime) = state.commands() {
            runtime.shutdown(std::time::Duration::from_secs(8)).await;
        }
    }
    Ok(())
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
}
