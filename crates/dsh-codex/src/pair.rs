//! `dsh-codex pair`: a client token (and QR code) for the Aether app, made
//! here or, with `--ssh`, on a server that is only reachable over SSH
//! (docs/aether/SPEC.md §3.4). Tokens and keys appear only inside the
//! payload the user asked for; nothing is logged.
use std::io::IsTerminal as _;
use std::path::PathBuf;

use clap::ValueEnum;
use dshx_core::config::Config;
use dshx_server::mobile::{self, ClientRef, Payload, Transport, identity, net, qr, tls};

mod ssh;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// The link and a QR code in the terminal.
    Qr,
    /// The link only.
    Link,
    /// The pairing payload as JSON, nothing else.
    Json,
}

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Name of the phone in this server's client list (default: iPhone).
    /// With --ssh: the name the device gets in the app.
    #[arg(long)]
    name: Option<String>,
    #[arg(long, value_enum, default_value_t = Format::Qr)]
    format: Format,
    /// Leave the transports empty (used by --ssh on the remote side).
    #[arg(long)]
    no_https: bool,
    /// Add the loopback URL of a running server: plain HTTP, for development
    /// (needs --port or --ready-file).
    #[arg(long)]
    http: bool,
    /// Port of the running server, for --http.
    #[arg(long)]
    port: Option<u16>,
    /// Ready file of the running server (`dsh-codex web --ready-file`), for --http.
    #[arg(long, value_name = "PATH")]
    ready_file: Option<PathBuf>,
    /// List the paired clients of this server.
    #[arg(long)]
    list: bool,
    /// Revoke a client by id.
    #[arg(long, value_name = "ID")]
    revoke: Option<String>,

    /// Pair a server behind SSH: the host as `ssh` knows it.
    #[arg(long, value_name = "HOST")]
    ssh: Option<String>,
    /// SSH config file for --ssh.
    #[arg(short = 'F', long = "ssh-config", value_name = "FILE")]
    ssh_config: Option<PathBuf>,
    /// Login user for --ssh (default: what the SSH config says).
    #[arg(long)]
    user: Option<String>,
    /// Name of the phone in the remote client list, for --ssh.
    #[arg(long, default_value = "iPhone")]
    client_name: String,
    /// Port of `dsh-codex web` on the remote host.
    #[arg(long, default_value_t = 3080)]
    remote_port: u16,
    /// `DSHX_HOME` of the remote server.
    #[arg(long, default_value = "/var/lib/dsh-codex")]
    remote_home: String,
    /// User the remote server runs as.
    #[arg(long, default_value = "deepseek")]
    remote_user: String,
    /// The `dsh-codex` binary on the remote host.
    #[arg(long, default_value = "/usr/local/bin/dsh-codex")]
    remote_bin: String,
    /// systemd unit of the remote server, for the token command of older builds.
    #[arg(long, default_value = "deepseek-harness")]
    unit: String,
    /// Host the phone connects to (default: the HostName of the SSH config).
    #[arg(long, value_name = "HOST")]
    public_host: Option<String>,
}

pub fn run(args: Args) -> anyhow::Result<i32> {
    if args.list {
        return list();
    }
    if let Some(id) = &args.revoke {
        return revoke(id);
    }
    let payload = match &args.ssh {
        Some(host) => ssh::pair(&args, host)?,
        None => local(&args)?,
    };
    emit(&payload, args.format)?;
    Ok(0)
}

/// A client of this server: the token goes into the payload, the hash into
/// `clients.json`; the server picks the file up while it runs.
fn local(args: &Args) -> anyhow::Result<Payload> {
    let home = dshx_core::config::home_dir();
    let config = Config::load()?;
    let settings = config.codex.mobile;
    let name = args.name.clone().unwrap_or_else(|| "iPhone".into());
    let store = mobile::ClientStore::new(&home);
    let (client, token) = store.create(&name, "ios", "cli")?;
    let server = identity::server_info(&home, &settings.name, env!("CARGO_PKG_VERSION"))?;
    let mut transports = Vec::new();
    if !args.no_https && settings.enabled {
        if settings.port == 0 {
            eprintln!(
                "dsh-codex pair: [codex.mobile] port = 0 picks a port at start, so the payload \
                 cannot name it; set a fixed port or pair from the web UI"
            );
        } else {
            let cert = tls::ensure_cert(&home, &identity::server_name(&settings.name))?;
            let urls = net::urls(&net::addresses(), settings.port);
            if urls.is_empty() {
                eprintln!(
                    "dsh-codex pair: no network address found; the payload has no https transport"
                );
            } else {
                transports.push(Transport::Https {
                    urls,
                    cert_sha256: Some(cert.sha256),
                });
            }
        }
    }
    if args.http {
        let port = loopback_port(args)?;
        transports.push(Transport::Http {
            urls: vec![format!("http://127.0.0.1:{port}")],
        });
    }
    Ok(Payload {
        v: mobile::pairing::VERSION,
        server,
        client: ClientRef {
            id: client.id,
            name: client.name,
        },
        token: Some(token),
        transports,
    })
}

/// The loopback port for `--http`: `--port`, else the ready file's `port`.
fn loopback_port(args: &Args) -> anyhow::Result<u16> {
    if let Some(port) = args.port {
        return Ok(port);
    }
    let Some(path) = &args.ready_file else {
        anyhow::bail!("--http needs --port or --ready-file to know where the server listens");
    };
    let text =
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    let ready: serde_json::Value = serde_json::from_str(&text)?;
    ready["port"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .ok_or_else(|| anyhow::anyhow!("{}: no port in the ready file", path.display()))
}

fn list() -> anyhow::Result<i32> {
    let store = mobile::ClientStore::new(&dshx_core::config::home_dir());
    let clients = store.list();
    if clients.is_empty() {
        println!("no clients paired (file: {})", store.path().display());
        return Ok(0);
    }
    println!(
        "{:<37} {:<24} {:<8} {:<6} {:<20} LAST SEEN",
        "ID", "NAME", "PLATFORM", "VIA", "CREATED"
    );
    for c in clients {
        let name: String = c.name.chars().take(24).collect();
        println!(
            "{:<37} {:<24} {:<8} {:<6} {:<20} {}",
            c.id,
            name,
            c.platform,
            c.paired_via,
            stamp(c.created_at),
            c.last_seen_at.map_or_else(|| "never".to_owned(), stamp),
        );
    }
    Ok(0)
}

fn revoke(id: &str) -> anyhow::Result<i32> {
    let store = mobile::ClientStore::new(&dshx_core::config::home_dir());
    if store.revoke(id)? {
        eprintln!("dsh-codex pair: {id} revoked");
        Ok(0)
    } else {
        eprintln!("dsh-codex pair: no client {id}");
        Ok(1)
    }
}

/// `2026-10-25 14:03` in UTC, from milliseconds.
fn stamp(ms: u64) -> String {
    let secs = i64::try_from(ms / 1000).unwrap_or(0);
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(t) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02} UTC",
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute()
        ),
        Err(_) => ms.to_string(),
    }
}

/// Prints what `--format` asks for; the QR gets colours on a terminal so the
/// theme cannot invert it.
fn emit(payload: &Payload, format: Format) -> anyhow::Result<()> {
    match format {
        Format::Json => println!("{}", serde_json::to_string_pretty(payload)?),
        Format::Link => println!("{}", payload.link()),
        Format::Qr => {
            let link = payload.link();
            println!("{link}\n");
            print!("{}", qr::terminal(&link, std::io::stdout().is_terminal())?);
            println!();
            eprintln!(
                "Scan the code with the iPhone camera or the Aether app. The link holds a secret \
                 and is shown only now; revoke the client to invalidate it."
            );
        }
    }
    Ok(())
}
