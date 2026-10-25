# dsh-codex

A standalone coding agent for DeepSeek: one Rust binary
that talks to the model exactly like [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)
(DSH) 0.2 and brings this project's Codex look and macOS tools with it, without
Node.js and without DSH.

> Preview. Everything below is tested offline against a scripted DeepSeek
> stand-in; runs against the real DeepSeek API are still to come.
> Not affiliated with OpenAI or DeepSeek. "Codex" is a trademark of OpenAI;
> this project contains no Codex code or assets.

The repository also contains the original **DSH plugin** that brings the same
look and tools into the DeepSeek Harness desktop app: see [PLUGIN.md](PLUGIN.md)
([Deutsch](PLUGIN.de.md)).

## Why

| Measured on the same Linux host, model answering instantly | DSH 0.2 (Node) | dsh-codex (Rust) |
|---|---:|---:|
| One answer, no tools | 1,713 ms, 183 MB | 18 ms, 17 MB |
| Five bash calls | 2,236 ms, 225 MB | 92 ms, 21 MB |
| Install size | 518 MB (288 packages) | 17 MB (one file, UI included) |

Median of 7 runs, `tools/bench/compare.py`. Real model calls take seconds,
so a single task finishes only slightly faster; the gain shows with many
parallel agents, many short jobs, and on small machines.

## What is the same as DSH

- **Towards the model:** tool names, descriptions and schemas, the system
  prompt, runtime-context messages and the Anthropic-Messages request bodies
  are byte-identical to DSH 0.2.0-rc.2 for the same situation, so answers and
  DeepSeek's prefix cache behave the same.
- **Sessions:** DSH session format v4. A session started by dsh-codex can be
  continued by the official `dsh` and the other way round.
- **SDK:** `dsh-codex sdk` speaks the DSH SDK protocol; the official Python
  SDK works unchanged when pointed at this binary (`dsh_bin=...`).

## What is different

- **Nothing extra leaves your machine.** DSH attaches its full session log
  (`dsh_session_log`), a plugin inventory and an anonymous user id to every
  model request by default. dsh-codex sends only what the request needs.
- **Own web UI** in the Codex style of this project (`dsh-codex web`) instead
  of the DSH app. The original DSH plugin ([PLUGIN.md](PLUGIN.md)) still works.
- **No JS plugins.** Extensions are skills, MCP servers and plugins in the
  Codex/Claude formats (skills and MCP servers; hooks, commands and agents
  are not loaded).
- **Compaction between turns.** DSH condenses history only near the context
  limit (about 678K of DeepSeek's 1M tokens), mid-turn. dsh-codex also
  compacts when a turn ends, from 200K tokens by default or after every turn
  (Settings → Context, `[codex.compaction]`), while DeepSeek still caches the
  prefix. Summaries are written before oversized tool results are pruned and
  with the chat's reasoning effort (DeepSeek's prefix depends on it), so the
  summarizer reads the cached prefix; only the kept tail is pruned. The
  checkpoint carries the open `todo_write` list.
- Not ported: web search, workflows/`run_code`, voice, office documents, the
  Electron app. Goals, schedules and Agent Teams are ported (see
  [Configuration](#configuration)).

## Build

```sh
cargo build --release -p dsh-codex     # → target/release/dsh-codex
```

On macOS also build the helper for `computer`, `app` and the mini window:
`helper/build.sh --install` (see [PLUGIN.md](PLUGIN.md#install) for the
macOS permissions it needs).

## Use

```sh
dsh-codex login                        # store your DeepSeek API key (or set DEEPSEEK_API_KEY)
dsh-codex "run the tests and fix what fails"
dsh-codex --json "…"                   # DSH's NDJSON event stream
dsh-codex --session-id <id> "continue"
dsh-codex web                          # Codex-style UI in your browser
dsh-codex sdk                          # DSH SDK JSON-RPC on stdio
```

Options: `-C <dir>`, `--model deepseek-flash|deepseek-v4-pro`,
`--effort off|low|high|max`, `--sandbox read-only|workspace-write|danger-full-access`.

## Web UI

`dsh-codex web` follows the layout of the Codex app:

- **Sidebar:** Pinned, Projects (one group per folder, new chat per project)
  and Chats without a project, each in a private folder under
  `$DSHX_HOME/chats/`. Rename, pin, mark unread, archive and delete from the
  row menu or with a right-click. The bell lists every chat by activity with
  running, waiting and unread marks. ⌘K searches chats and actions.
- **Side panel** (button top right): all file changes of the chat, the chat's
  folder, open files and the live view. Files preview as code, Markdown,
  CSV/TSV, JSON, images, PDF, video/audio or rendered HTML; they load through a
  read-only grant for the chat's folder, and pages run sandboxed. Paths in tool
  rows, links and inline code open in the panel.
- **Live view:** the background browser's screencast (and the Mac tools'
  screenshots) as a floating picture or in the panel, with Stop.
- **Plugins** (sidebar): installed plugins and marketplaces (Codex or Claude
  format), connectors with sign-in (OAuth or token), skills and MCP servers.
- **Automations** (sidebar): reminders and recurring tasks the agent created
  with `schedule_create`, with next run, chat, prompt and delivery history.
  The **goal bar** above the composer shows a chat's goal (set one from the
  composer's +), and a **Team** tab in the panel shows an Agent Team.
- **While the agent works,** Enter adds a message to the running task and ⌘↩
  queues it for afterwards (switchable in Settings). Notifications report
  chats that finish, fail or wait for approval in the background.

## Try it on a Mac

Everything so far was tested on Linux and against a scripted DeepSeek
stand-in. This is the checklist for a first run on macOS 14 or later (Apple
silicon) with the real DeepSeek API.

**1. Build** (about 3–5 minutes the first time)

```sh
xcode-select --install                 # once, if the command line tools are missing
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # once, installs Rust
git clone https://github.com/NavidYSZ/dsh-codex-rs.git
cd dsh-codex-rs
cargo build --release -p dsh-codex
helper/build.sh --install              # "DSH Computer Use.app" for computer, app and the mini window
```

Then allow "DSH Computer Use" in **System Settings → Privacy & Security**
under **Accessibility** and **Screen & System Audio Recording**.

**2. Key and first answer**

```sh
./target/release/dsh-codex login       # paste your DeepSeek API key
./target/release/dsh-codex "Say hello in one sentence."
```

**3. Web UI**

```sh
./target/release/dsh-codex web         # opens the Codex-style UI in your browser
```

Pick a folder on the start page and try, one after another:

| Try | Expect |
|---|---|
| "List the files here and summarize the README." | `Ran ls`, `Read …` rows, an answer |
| "Create hello.py that prints hi, then run it." | an `Edited`/`Wrote` row with a diff, `Ran python3 hello.py` |
| "Open example.com in the browser and tell me the heading." | `Browser · Open …` rows with screenshots; needs Chrome, Chromium, Brave or Edge in /Applications |
| "Open Calculator and compute 7 × 6." | `Calculator · Click …` rows; the mini window at the bottom right shows the steps; Stop halts the agent |
| "Take a screenshot of my screen and describe it." | `Computer · Look at screen`; the agent cursor and the "DeepSeek is using your computer" banner; Esc cancels |

**If something fails**

- Shell commands are refused with a sandbox message: switch the access mode
  under the composer to **Full access**, or run with
  `--sandbox danger-full-access`. The macOS sandbox profile (`sandbox-exec`)
  is new and has not run on a Mac yet.
- A tool is missing: Settings in the web UI lists Browser, Apps and Computer
  as on or off. `computer` and `app` need the helper from step 1; without it
  they fail with a clear error and everything else still works. The browser
  looks for Chrome, Chromium, Brave and Edge; for another path set
  `[codex.browser] chrome = "…"` in `~/.dsh-codex/config.toml`.
- Run `cargo test --workspace` once; Chromium and bubblewrap tests skip
  themselves where those are missing.

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

When a chat condenses older history between turns (Settings → Context in the
web UI; open chats follow a change at once):

```toml
[codex.compaction]
mode = "threshold"                     # "threshold", "every-turn" or "off"
threshold_tokens = 200000              # context size that triggers it; a fifth stays verbatim
```

`every-turn` keeps only the final answer verbatim and skips turns that added
less than about 10,000 tokens. `off` leaves DSH's own compaction near the
context limit, which stays on in every mode. Minimal chats never compact.

Reminders live in `~/.dsh-codex/storages/schedule.json`. The web server
exposes them, the session goal and the Team under `/api/schedules`,
`/api/sessions/:id/goal` and `/api/sessions/:id/team`
([protocol](docs/rust/SPEC.md#6-ui--server-protocol)).

MCP servers and skills work like in DSH:

```toml
[mcp_servers.files]                    # stdio server, tools appear as mcp__files__<tool>
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/notes"]

[mcp_servers.docs]                     # Streamable HTTP server
url = "https://example.com/mcp"
```

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
| `crates/dsh-codex` | The binary |
| `crates/dshx-testkit` | Scripted DeepSeek mock for tests |
| `ui/` | Web UI (Preact + htm, no build step) |
| `docs/rust/SPEC.md` | The contract between the crates |

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
