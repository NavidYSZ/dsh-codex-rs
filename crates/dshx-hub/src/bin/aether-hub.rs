//! `aether-hub`: the accounts hub as a process.
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use dshx_hub::{Hub, HubOptions};

#[derive(Parser)]
#[command(
    name = "aether-hub",
    version,
    about = "Aether accounts hub: accounts, tiers, servers and the relay"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the API.
    Serve {
        /// Address to listen on.
        #[arg(long, default_value = "127.0.0.1:8787")]
        listen: SocketAddr,
        /// Data directory (hub.db inside); default ~/.aether-hub.
        #[arg(long, value_name = "DIR")]
        data: Option<PathBuf>,
        /// How the hub is reached from outside (reported by /v1/health).
        #[arg(long, value_name = "URL")]
        public_url: Option<String>,
        /// Bearer for /v1/admin/*; without it those routes are off.
        #[arg(
            long,
            env = "AETHER_HUB_ADMIN_TOKEN",
            hide_env_values = true,
            value_name = "TOKEN"
        )]
        admin_token: Option<String>,
        /// Development mode: verification codes are logged and returned by signup.
        #[arg(long)]
        dev: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
    match Cli::parse().command {
        Command::Serve {
            listen,
            data,
            public_url,
            admin_token,
            dev,
        } => serve(listen, data, public_url, admin_token, dev).await,
    }
}

async fn serve(
    listen: SocketAddr,
    data: Option<PathBuf>,
    public_url: Option<String>,
    admin_token: Option<String>,
    dev: bool,
) -> anyhow::Result<()> {
    let data = match data {
        Some(dir) => dir,
        None => default_data_dir()?,
    };
    let options = HubOptions {
        public_url: Some(public_url.unwrap_or_else(|| format!("http://{listen}"))),
        admin_token: admin_token.filter(|t| !t.trim().is_empty()),
        dev,
    };
    if options.admin_token.is_none() {
        tracing::warn!("no admin token: /v1/admin is off");
    }
    if dev {
        tracing::warn!("dev mode: verification codes are logged and returned by signup");
    }
    let hub = Hub::open_with(&data, options)?;
    let (addr, task) = dshx_hub::serve(hub, listen).await?;
    tracing::info!(
        "aether-hub {}: listening on http://{addr} (data: {})",
        dshx_hub::VERSION,
        data.display()
    );
    tokio::select! {
        _ = task => {}
        _ = tokio::signal::ctrl_c() => tracing::info!("stopping"),
        _ = terminated() => tracing::info!("stopping"),
    }
    Ok(())
}

fn default_data_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
        .map(|home| home.join(".aether-hub"))
        .ok_or_else(|| anyhow::anyhow!("--data is required when HOME is not set"))
}

/// Resolves on SIGTERM so a supervisor can stop the hub cleanly.
async fn terminated() {
    #[cfg(unix)]
    if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        sig.recv().await;
        return;
    }
    std::future::pending::<()>().await
}
