//! `dsh-codex` command line: headless runs, the web UI and the dsh SDK protocol.
mod automation;
mod chats;
mod config_file;
mod fork;
mod headless;
mod jsonrpc;
mod live;
mod presets;
mod runtime;
mod sdk;
mod usage;
mod web;

use std::io::{IsTerminal as _, Read as _};
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use dshx_core::SandboxMode;

use crate::runtime::{Overrides, Runtime};

#[derive(Parser)]
#[command(
    name = "dsh-codex",
    version,
    about = "Codex DeepSeek Harness: a fast, lean coding agent for DeepSeek"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// The task. Read from stdin when omitted (or `-`) and stdin is not a terminal.
    task: Option<String>,
    /// Workspace directory (default: the current directory).
    #[arg(short = 'C', long = "cd", global = true)]
    cwd: Option<PathBuf>,
    #[arg(long, global = true)]
    model: Option<String>,
    /// Reasoning effort: off, low, high or max.
    #[arg(long, global = true)]
    effort: Option<String>,
    /// read-only, workspace-write or danger-full-access.
    #[arg(long, global = true, value_parser = parse_sandbox)]
    sandbox: Option<SandboxMode>,
    /// Print dsh's newline-delimited JSON event stream instead of the answer.
    #[arg(long)]
    json: bool,
    /// Continue an existing session.
    #[arg(long = "session-id")]
    session_id: Option<String>,
    /// Agent preset of a new session: standard, ptc or minimal (default: the
    /// config's `default_preset`). A session keeps its preset.
    #[arg(long, value_parser = parse_preset)]
    preset: Option<dshx_agent::Preset>,
    /// Compatibility with `dsh --profile <headless|web|sdk>` (used by the dsh Python SDK).
    #[arg(long, hide = true)]
    profile: Option<String>,
    /// Accepted for dsh compatibility and ignored.
    #[arg(long, hide = true)]
    patch: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the web UI.
    Web {
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Do not open a browser window.
        #[arg(long)]
        no_open: bool,
        /// Once listening, write `{"url","token","port"}` here as JSON (mode 0600)
        /// for a supervising app such as the macOS desktop shell.
        #[arg(long, value_name = "PATH")]
        ready_file: Option<PathBuf>,
    },
    /// Serve the dsh SDK JSON-RPC protocol on stdio.
    Sdk,
    /// Store a DeepSeek API key (read from stdin).
    Login,
    /// Remove the stored API key.
    Logout,
}

fn parse_preset(s: &str) -> Result<dshx_agent::Preset, String> {
    presets::parse(s).map_err(|e| e.to_string())
}

fn parse_sandbox(s: &str) -> Result<SandboxMode, String> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(|_| {
        format!("unknown sandbox mode {s:?} (read-only, workspace-write, danger-full-access)")
    })
}

/// How long `web` waits for MCP servers before it serves the UI; a server
/// that hangs in its handshake (an `npx` download, a dead remote) would
/// otherwise hold the page, and the Mac app's instance, for up to a minute.
const WEB_MCP_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Loads the runtime and connects the configured MCP servers, waiting at
/// most `mcp_wait` (`None`: until every first attempt finished).
async fn start(mcp_wait: Option<std::time::Duration>) -> anyhow::Result<Arc<Runtime>> {
    let runtime = Runtime::load()?;
    runtime.start_mcp(mcp_wait).await?;
    Ok(runtime)
}

fn main() {
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let code = rt.block_on(async_main(cli)).unwrap_or_else(|e| {
        eprintln!("dsh-codex: {e:#}");
        1
    });
    // Background jobs and the browser must not keep the process alive.
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
    std::process::exit(code);
}

async fn async_main(cli: Cli) -> anyhow::Result<i32> {
    let cwd = match &cli.cwd {
        Some(dir) => dir
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?,
        None => std::env::current_dir()?,
    };
    let overrides = Overrides {
        model: cli.model.clone(),
        effort: cli.effort.clone(),
        sandbox: cli.sandbox,
    };
    let command = match (&cli.command, cli.profile.as_deref()) {
        (Some(_), _) => cli.command,
        (None, Some("sdk")) => Some(Command::Sdk),
        (None, Some("web")) => Some(Command::Web {
            port: 0,
            no_open: false,
            ready_file: None,
        }),
        (None, Some("headless") | None) => None,
        (None, Some(other)) => anyhow::bail!("unknown profile {other:?} (headless, web, sdk)"),
    };
    match command {
        Some(Command::Login) => {
            let runtime = Runtime::load()?;
            let mut key = String::new();
            if std::io::stdin().is_terminal() {
                eprint!("DeepSeek API key: ");
            }
            std::io::stdin().read_line(&mut key)?;
            anyhow::ensure!(!key.trim().is_empty(), "no key given");
            runtime.set_api_key(Some(key.trim()))?;
            eprintln!(
                "dsh-codex: key saved to {}",
                runtime.credentials.path().display()
            );
            Ok(0)
        }
        Some(Command::Logout) => {
            Runtime::load()?.set_api_key(None)?;
            Ok(0)
        }
        Some(Command::Web {
            port,
            no_open,
            ready_file,
        }) => web::run(start(Some(WEB_MCP_WAIT)).await?, port, !no_open, ready_file).await,
        Some(Command::Sdk) => sdk::run(start(None).await?).await,
        None => {
            let task = match cli.task.as_deref() {
                Some(t) if t != "-" => t.to_owned(),
                _ if !std::io::stdin().is_terminal() => {
                    let mut text = String::new();
                    std::io::stdin().read_to_string(&mut text)?;
                    text
                }
                _ => anyhow::bail!("a task is required, for example: dsh-codex \"run the tests\""),
            };
            anyhow::ensure!(!task.trim().is_empty(), "the task is empty");
            let runtime = start(None).await?;
            anyhow::ensure!(
                cli.preset.is_none() || cli.session_id.is_none(),
                "--preset only applies to new sessions; a session keeps the preset it started with"
            );
            let args = headless::HeadlessArgs {
                task,
                cwd,
                session_id: cli.session_id,
                json: cli.json,
                overrides,
                preset: cli.preset,
            };
            headless::run(runtime, args).await
        }
    }
}
