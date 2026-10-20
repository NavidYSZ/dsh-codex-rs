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
- **No JS plugins.** Extensions come through MCP servers and skills.
- Not ported: agent teams, workflows/`run_code`, schedules, goals, voice,
  office documents, the Electron app.

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

Sessions live in `~/.dsh-codex/sessions`, attachments in
`~/.dsh-codex/attachments`, the API key in `~/.dsh-codex/credentials.toml`
(mode 0600).

## Layout

| Path | Purpose |
|---|---|
| `crates/dshx-core` | Shared types: messages, streaming, tools, session events, config |
| `crates/dshx-llm` | DeepSeek adapter (Anthropic Messages, SSE, replay, retries) |
| `crates/dshx-session` | Session format v4, attachments, credentials |
| `crates/dshx-tools` | bash, files, search, jobs, web fetch, sandbox |
| `crates/dshx-agent` | Agent loop, prompt, AGENTS.md, compaction |
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

Golden data in `docs/rust/upstream/` and the crates' `tests/fixtures/` was
recorded from the official dsh 0.2.0-rc.2 against the mock, with a fake key.

## License

[MIT](LICENSE). Tool descriptions, prompts and wire formats follow DeepSeek
Harness (MIT); see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
