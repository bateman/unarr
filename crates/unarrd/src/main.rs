use std::future::IntoFuture;
use std::net::SocketAddr;

use clap::Parser;
use unarr_core::Personality;

/// Unarr daemon: a lightweight, API-compatible replacement for Sonarr, Radarr and Lidarr.
#[derive(Parser, Debug)]
#[command(name = "unarrd", version, about)]
struct Args {
    /// Address to bind each personality to (the port comes from the personality).
    #[arg(long, env = "UNARR_BIND", default_value = "0.0.0.0")]
    bind: String,

    /// Personalities to run.
    #[arg(long, env = "UNARR_PERSONALITIES", value_delimiter = ',', default_values = ["sonarr", "radarr", "lidarr"])]
    personalities: Vec<String>,
}

fn parse_personality(s: &str) -> anyhow::Result<Personality> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "sonarr" => Personality::Sonarr,
        "radarr" => Personality::Radarr,
        "lidarr" => Personality::Lidarr,
        other => anyhow::bail!("unknown personality {other:?}"),
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();

    let mut servers = Vec::new();
    for name in &args.personalities {
        let personality = parse_personality(name)?;
        let addr: SocketAddr = format!("{}:{}", args.bind, personality.default_port()).parse()?;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(%addr, ?personality, "listening");
        servers.push(tokio::spawn(
            axum::serve(listener, unarr_api::router(personality)).into_future(),
        ));
    }

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    Ok(())
}
