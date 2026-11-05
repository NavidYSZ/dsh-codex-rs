# Aether (dsh-codex)

[Deutsch](README.de.md)

A standalone coding agent with a native macOS app, a web interface and a Rust
CLI. It supports DeepSeek and additional configured model providers, MCP,
skills, subagents, goals and schedules.

**Aether installs alongside DeepSeek Harness.** It has its own application ID
and stores chats and settings in `~/.dsh-codex`. The repository and executable
retain the technical name `dsh-codex`.

Looking for the extension to the original DeepSeek Harness? That is the
separate [dsh-codex plugin](https://github.com/NavidYSZ/dsh-codex).
The plugin installation instructions do not install Aether.

> Preview software. Not affiliated with OpenAI or DeepSeek. “Codex” is an
> OpenAI trademark. This project contains no OpenAI Codex code or assets.

## Install the macOS app

1. Download the macOS ARM64 disk image from
   [Releases](https://github.com/NavidYSZ/dsh-codex-rs/releases).
2. Open it and drag **Aether.app** into **Applications**.
3. Launch Aether, choose “This Mac”, and configure your model provider in
   Settings. The app includes its own agent, Node runtime for PTC, and
   optional Computer Use helper. DeepSeek Harness is not required.

Requires **macOS 14 or newer and Apple Silicon**. The preview is ad-hoc signed,
not notarized. If macOS blocks its first launch, use the app-specific
“Open Anyway” control in System Settings → Privacy & Security after checking
that the download came from this repository. Do not disable Gatekeeper globally.

For Computer Use, allow **DSH Computer Use** under Accessibility and Screen
Recording. Those permissions are optional for ordinary chat and coding.

Upgrading an earlier `dsh-codex.app`: quit it before opening Aether. Your data,
provider credentials and saved instances retain their existing paths. Keep
only one installed copy of this standalone app; the original DeepSeek Harness
is a separate application. New installations start with one local instance;
add your own remote servers through the instance editor.

## Aether for iPhone

The iOS app in `ios/` connects to every machine that runs `dsh-codex web`:
this Mac, a VPS that only exposes SSH, or a server enrolled with the accounts
hub. It shows the same chats, tool rows, approvals and live view as the web
UI, in German first.

Pairing is one scan:

1. Web UI → Settings → Aether App: switch the listener on (TLS on port 7443
   by default; the app pins the certificate) and pair a device. The page shows
   a QR code and an `aether://pair#…` link.
2. From a terminal: `dsh-codex pair` prints the QR code or the link;
   `dsh-codex pair --list` and `dsh-codex pair --revoke <id>` manage devices.
3. For a VPS behind SSH: `dsh-codex pair --ssh <host> -F <ssh_config>` creates
   an ed25519 key for the phone, installs a restricted `authorized_keys` line
   (port forwarding to the server's loopback port only) and prints the QR
   code. The app then tunnels over port 22.

On a Mac with the application firewall on, allow `dsh-codex` (or the Aether
app) under System Settings → Network → Firewall once; until then only the
loopback address answers and the phone cannot reach the listener.

Client tokens are stored hashed in `$DSHX_HOME/mobile/clients.json`; revoking
one closes its connections. The listener is off by default and never serves
previews or the OAuth callback. The protocol is in
[docs/aether/SPEC.md](docs/aether/SPEC.md); building the app is described in
[ios/README.md](ios/README.md).

The accounts hub (`crates/dshx-hub`, binary `aether-hub`: sign-up, sessions,
tiers and a relay to enrolled servers) is implemented but not deployed.
`dsh-codex hub enroll --url … --code …` connects a server to a hub.

## Transfer folders between devices

The macOS app copies, moves or syncs a project folder from one instance to
another (this Mac, a VPS behind SSH, …): File → “Ordner übertragen …”, or in
the web UI the project menu → “Transfer to another device…”. Pick the source
folder, the target instance and its parent folder, and a mode:

- **Copy** creates the folder on the target; a non-empty target is refused.
- **Move** copies, checks every file on the target (size and modification
  time) and only then deletes the source folder.
- **Sync** sends new and changed files; optionally it deletes files that only
  exist on the target.

`node_modules`, `target`, `.venv`, `__pycache__`, `DerivedData` and similar
build folders are excluded by default; `.git` and hidden files are included.
“Preview” shows counts and sizes first. The instances never talk to each
other: the app streams the folder as tar+zstd through a 0600 file in its cache
and adds the folder to the target's projects. Every server only accepts paths
inside the home folder and its project folders, never writes through a
symlink and refuses archive entries with `..` or absolute paths. The API is
in [docs/aether/TRANSFER.md](docs/aether/TRANSFER.md).

## Modes

| Mode | Behavior |
|---|---|
| Standard | The model calls tools directly. |
| PTC | The model writes TypeScript in `run_code`, calls `tools.<name>(args)`, and returns selected output instead of entire intermediate results. |
| Minimal | One persistent shell and a minimal prompt. |

Select **PTC** when creating a chat, or set it as the default under Settings.
Existing chats retain the mode they were created with. PTC keeps Standard's
skills, MCP, subagents, goals and compaction; tool permissions, approval and
cancellation still apply. Programs run with fresh state, support erasable
TypeScript syntax (types, interfaces and assertions), and default to a
120-second limit, capped at 10 minutes. A TypeScript enum or other syntax
requiring transpilation is not supported.

For example, the agent can read several files using `Promise.all`, filter the
structured results, then `console.log` only the lines relevant to your task.
The separate DSH `workflow` orchestration tool is not implemented.

## Messages while working

By default, **Enter** steers a new message into the running task at its next
step; **⌘Enter** (Ctrl+Enter in the browser) queues a new turn afterwards.
Settings can swap those defaults. Pending steering and queued messages stay
visible until consumed or explicitly removed. Queued follow-ups run in order.
A failed submission keeps the draft and its attachments.

## Browser Use and HuntProxy

**Settings → Browser Use** shows the browser executable, separate profile,
saved versus active configuration and any required restart. **Test browser**
checks launch, navigation, typing, clicking and reading with an isolated local
page and temporary profile. Browser MCP connections have their own connection
and tool-discovery check. These checks do not certify access to every website.

**Your own Chrome.** Ask for “my Chrome”, “the open Chrome” or an open tab and
the agent attaches to your running Google Chrome (or Chromium, Brave, Edge)
with your tabs and sign-ins instead of starting a second browser
(`browser: "user"`). Allow it once in Chrome under
`chrome://inspect/#remote-debugging` → “Allow remote debugging for this
browser instance”, and click Allow if Chrome asks when the agent connects.
Aether keeps that connection while it runs, so Chrome does not ask again for
each action. The agent starts on the tab you are looking at, opens its own
tab for new pages, never navigates your tab away and never closes your
browser. Settings → Browser Use shows whether attaching is possible right
now.

Connect HuntProxy in Aether's **Plugins → MCP servers**, even if another agent
already uses it: applications keep their own MCP configuration. Switch it on
for a chat under **Connectors** in the composer, mention `@huntproxy`, or turn
on **Preselected in new chats** on its card. PTC calls the connected tools
directly. See [browser setup, HuntProxy and troubleshooting](docs/browser-use.md).

Website errors should be diagnosed through the selected browser backend;
desktop mouse/keyboard control for web work requires explicit user intent.

## Build from source

Install Rust 1.97.1 or newer. For the native Mac app, use Xcode 26.5 or newer
(including the macOS 26 SDK) and Python 3, then:

```sh
git clone https://github.com/NavidYSZ/dsh-codex-rs.git
cd dsh-codex-rs
desktop/build.sh --ad-hoc
```

This produces `desktop/build/Aether.app` and `desktop/build/Aether.dmg`.
The newer SDK is a build requirement; the resulting app still targets macOS 14+.
The build downloads a pinned Node.js runtime from nodejs.org and verifies its
SHA-256 before packaging it. No personal settings or API keys enter the app.
See [the desktop build notes](desktop/README.md) for signing and instance setup.

For the CLI or web interface on macOS/Linux:

```sh
cargo build --locked --release -p dsh-codex
./target/release/dsh-codex login
./target/release/dsh-codex --preset ptc "Read the project and summarize it"
./target/release/dsh-codex web
```

PTC outside the app requires **Node.js 22.18 or newer** on PATH, or an explicit
`DSHX_NODE` executable path. Restricted PTC execution also requires a working
OS sandbox (`sandbox-exec` on macOS or bubblewrap on Linux); it fails with an
error if confinement is unavailable. Standard and Minimal do not need Node.

Useful options: `-C <folder>`, `--model <id>`, `--effort off|low|high|max`,
`--sandbox read-only|workspace-write|danger-full-access`, `--json`,
`--session-id <id>`, and `sdk` for the DSH-compatible SDK protocol.

## Interface and capabilities

- Projects, pinned chats, search, file/diff previews and a side panel.
- Background browser, macOS app controls and optional foreground Computer Use.
- Skills, MCP servers, OAuth connectors and Codex/Claude-format plugin catalogs.
- Provider/model selection, goals, schedules, agent teams and context compaction.
- Optional remote instances through SSH or an authenticated server URL.

Session format v4 and the SDK follow DeepSeek Harness. Standard tool schemas
and protocol compatibility are covered by fixtures from DSH 0.2.0-rc.2; PTC
follows the `run_code` contract of DSH 0.2.1-rc.5. This is an independent
implementation, not a claim of full feature parity. Web search providers,
DSH workflows, voice and office-document integrations are not implemented.

## Development and releases

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
for script in ui/dev/*_check.mjs; do node "$script"; done
```

GitHub Actions checks Linux and macOS and builds the standalone Mac app on each
push to `main`. Releases are built from tagged source; see
[CHANGELOG.md](CHANGELOG.md). Model tests use local scripted providers, so the
suite needs no API keys. Browser/sandbox-specific checks may skip when their
host dependencies are absent.

## Configuration

`~/.dsh-codex/config.toml` (or `$DSHX_HOME/config.toml`). The `[codex]` table
takes the same keys and defaults as the plugin's `cordis.patch.yml` row:

```toml
model = "deepseek-flash"
sandbox = "workspace-write"
approval = "ask"

[codex.computerUse]
enabled = true
locale = "de"

[codex.browser]
enabled = true
chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"

[codex.mini]
doneAfter = 6
```

Settings → Models offers DeepSeek, Anthropic, OpenCode Go, Xiaomi Token Plan
AMS, Z.ai Coding Plan and Command Code. Add a provider with its existing key;
its models then appear together in the composer's model picker. Keys are
stored separately in `credentials.toml` (0600). A chat can switch providers
between turns; reasoning is replayed only to the provider and model that
generated it.

To transfer providers and keys from the original DSH desktop profile into a
running instance (requires Python and PyYAML):

```sh
python3 tools/dev/import_dsh_providers.py --dry-run
python3 tools/dev/import_dsh_providers.py \
  --ready-file "$HOME/Library/Application Support/dsh-codex/run/local-local.json" \
  --backup-home "$HOME/.dsh-codex"
```

Existing provider routes are skipped unless `--overwrite` is supplied.
The import keeps the default model, permissions and other settings. See
[provider compatibility and verification](docs/providers.md).

Goals, schedules and Agent Teams work like in DSH 0.2.0-rc.2. Goals are on
(as in dsh-base); schedules (with the request clock) and Agent Teams are
opt-in like the DSH experimental bundles:

```toml
[codex.goal]
default_max_goal_rounds = 256          # cap of a goal created without max_goal_rounds
blocked_after_consecutive_rounds = 3   # rounds before the model may report "blocked"

[codex.schedule]
enabled = true                         # schedule_* tools; `dsh-codex web` delivers reminders
delivery_history_days = 30
delivery_history_records = 200

[codex.time_context]                   # follows schedule.enabled unless set
time_zone = "Europe/Berlin"            # default: the system zone
refresh_interval_ms = 600000

[codex.team]
enabled = true                         # Team tools replace subagent/subagent_fork
max_members = 8
max_tasks = 256
max_pending_messages_per_member = 64
max_message_bytes = 65536
```

When a chat condenses older history (Settings → Context in the
web UI; open chats follow a change at once):

```toml
[codex.compaction]
mode = "threshold"                     # "threshold", "every-turn" or "off"
threshold_tokens = 200000              # context size that triggers it; a fifth stays verbatim
```

A round is a complete agent answer, including all of its tool calls and PTC
programs. This policy runs after that answer, before the next Goal round or
follow-up; individual tool calls never trigger it. `every-turn` requires at
least 1,024 fresh compactable estimated tokens for PTC/active Goals, or 8,000
for ordinary Standard. The latest answer stays verbatim, pending input remains
queued, and Goal state/counters are restored from the journal. Delegated
subagents and one-shot CLI runs skip this final checkpoint.
Stop cancels compaction and prevents automatic Goal continuation. `off` leaves
pressure compaction near the context limit enabled. Minimal chats never compact.

Reminders live in `~/.dsh-codex/storages/schedule.json`. The web server
exposes them, the session goal and the Team under `/api/schedules`,
`/api/sessions/:id/goal` and `/api/sessions/:id/team`
([protocol](docs/rust/SPEC.md#6-ui--server-protocol)).

The Aether app's listener and the accounts hub are off until configured
(the web UI's Settings → Aether App and `dsh-codex hub enroll` write these):

```toml
[codex.mobile]
enabled = false                        # TLS listener for the Aether app on every interface
port = 7443                            # 0 = any free port
name = ""                              # how the server introduces itself; default: the computer name

[codex.hub]
url = ""                               # accounts hub; the server key lives in credentials.toml [hub]
```

MCP servers and skills work like in DSH:

```toml
[mcp_servers.files]                    # stdio server, tools appear as mcp__files__<tool>
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/notes"]

[mcp_servers.docs]                     # Streamable HTTP server
url = "https://example.com/mcp"
always = true                          # preselected in new chats
```

A server joins a chat only when the chat picks it. **Connectors** in the
composer lists every enabled server with its status and tool count; its switch
turns the server on or off for this chat (on the start page: for the chat the
first prompt creates). `@name` in a prompt also turns a server on. Servers
with `always = true` (**Preselected in new chats**, shown as *Default*) start
in every new chat, and each chat can still switch them off: a switched-off
server contributes neither tools nor instructions to that chat. A chat keeps
the preselection it was created with, so changing the switch later leaves
existing chats alone; only chats from before this was recorded that never
chose a server themselves still follow it. Minimal mode
has no MCP servers. Subagents and PTC programs only see the servers of their
chat.

Skills are read from `<project>/.dsh/skills`, `<project>/.agents/skills`,
the skill folders of enabled plugins, `~/.dsh-codex/skills`, `~/.dsh/skills`
and `~/.agents/skills` (`<name>/SKILL.md` or `<name>.md` with YAML front
matter).

### Plugins, marketplaces and connectors

Plugins live in `~/.dsh-codex/plugins/<marketplace>/<name>/`. The manifest is
found like Codex does: `plugin.json` with the Agent Plugins 1.0 `$schema`,
`.codex-plugin/plugin.json`, `.claude-plugin/plugin.json`, or none (Claude
standard layout). A plugin brings skill folders (Codex `skills` replaces
`skills/`, Claude `skills` adds to it) and MCP servers (`.mcp.json`,
`mcp.json` or inline `mcpServers`, named `<plugin>_<server>`;
`${CLAUDE_PLUGIN_ROOT}`/`${PLUGIN_ROOT}` are expanded). Only enabled plugins
count:

```toml
[plugins."linear@openai-curated"]
enabled = true
```

Marketplaces (`.agents/plugins/marketplace.json` or
`.claude-plugin/marketplace.json`) are added by git URL, `owner/repo` or
local path; git ones are cloned to `~/.dsh-codex/marketplaces/`. The web UI's
Plugins page installs, updates, enables and removes plugins, manages MCP
servers and offers a catalog of remote MCP servers (Linear, Notion, Sentry,
Atlassian, Cloudflare, Canva, Hugging Face, Exa, Context7, Stripe, Supabase,
Neon, Netlify, Zapier, GitHub). Every change applies at once: unchanged MCP
servers stay connected, open chats get the new tools on their next prompt.

Streamable HTTP servers log in with OAuth as in the MCP authorization
specification (discovery, dynamic client registration, PKCE, refresh). The
callback is `http://127.0.0.1:<ui-port>/api/oauth/callback`, which also works
through an SSH tunnel that forwards the UI port. Tokens are stored per
authorization server in `~/.dsh-codex/oauth/` (mode 0600). For servers
without dynamic registration (GitHub) enter a personal access token or set
your own OAuth app:

```toml
[mcp_servers.github]
url = "https://api.githubcopilot.com/mcp/"
oauth = { client_id = "Ov23li…", client_secret = "…", scopes = ["repo"] }
# or: bearer_token_env_var = "GITHUB_PAT"
```

Sessions live in `~/.dsh-codex/sessions`, attachments in
`~/.dsh-codex/attachments`, the API key in `~/.dsh-codex/credentials.toml`
(mode 0600).

## Layout

| Path | Purpose |
|---|---|
| `crates/dshx-core` | Shared types: messages, streaming, tools, session events, config |
| `crates/dshx-llm` | DeepSeek adapter (Anthropic Messages, SSE, replay, retries) |
| `crates/dshx-session` | Session format v4, attachments, credentials |
| `crates/dshx-tools` | bash, files, search, jobs, web fetch, skills, sandbox |
| `crates/dshx-agent` | Agent loop, prompt, AGENTS.md, compaction |
| `crates/dshx-subagent` | In-process subagents (`subagent`, `subagent_fork`, `send_message`, …) |
| `crates/dshx-goal` | Goals (`get_goal`, `create_goal`, `update_goal`) and the goal-round driver |
| `crates/dshx-schedule` | Reminders (`schedule_*`), their store and delivery, the request clock |
| `crates/dshx-team` | Agent Teams: teammates, Team mailbox, shared task board |
| `crates/dshx-mcp` | MCP client (stdio and Streamable HTTP), OAuth logins, connector catalog |
| `crates/dshx-plugins` | Plugin manifests, plugin MCP servers, marketplaces |
| `crates/dshx-browser` | Background Chrome over a DevTools pipe (`browser` tool) |
| `crates/dshx-macos` | Swift-helper client, `computer` and `app` tools, mini window |
| `crates/dshx-server` | Local HTTP/WebSocket server for the UI |
| `crates/dshx-hub` | Accounts hub (`aether-hub`): accounts, tiers, server enrolment, relay |
| `crates/dsh-codex` | The binary |
| `crates/dshx-testkit` | Scripted DeepSeek mock for tests |
| `ui/` | Web UI (Preact + htm, no build step) |
| `ios/` | Aether iOS app (SwiftUI, XcodeGen) |
| `brand/` | Brand assets: mark, wordmark, colour tokens, app icons |
| `docs/rust/SPEC.md` | The contract between the crates |
| `docs/aether/SPEC.md` | The contract for the iOS app, pairing and the hub |

## Develop

```sh
cargo test --workspace                 # offline; Chromium/bwrap tests skip when missing
cargo clippy --workspace --all-targets -- -D warnings
python3 tools/dev/mock_deepseek.py 18090 2   # instant DeepSeek stand-in
python3 tools/bench/compare.py               # speed vs. the official dsh
```

On macOS, dev and test runs must not touch the login keychain or the real
browser profile: set your own `DSHX_HOME` (it then gets its own
`browser-profile`) and `DSHX_BROWSER_MOCK_KEYCHAIN=1` (Chrome starts with
`--use-mock-keychain`; the Chrome tests do this themselves). Never change
`HOME` for such runs: Chrome then misses the keychain and macOS offers to
reset it on every start.

Golden data in `docs/rust/upstream/` and the crates' `tests/fixtures/` was
recorded from the official dsh 0.2.0-rc.2 against the mock, with a fake key.

## License

[MIT](LICENSE). Tool descriptions, prompts and wire formats follow DeepSeek
Harness (MIT); see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
