# dsh-codex in Rust — specification (contract for all crates)

Status: v0 (2026-10-20). Branch `rust`. Owner of this file: the orchestrator.
Workers propose changes in their final report instead of editing this file.

## 1. Goal

`dsh-codex` is a standalone coding agent for DeepSeek, written in Rust, that

1. **behaves like DeepSeek Harness (dsh) towards the model**: same tool names, descriptions and
   schemas, same system prompt and runtime-context messages, same Anthropic-Messages wire format
   (`POST {base}/v1/messages`, SSE), same reasoning replay — so model quality and DeepSeek prefix-cache
   hits match dsh;
2. **writes dsh session format v4**, so a session can be opened and continued by the official `dsh`
   (and vice versa) for the event subset we emit;
3. **carries the dsh-codex product**: the Codex-style look (now as our own web UI) and the special
   tools `browser`, `app`, `computer` with the floating mini window (Swift helper, unchanged);
4. is **fast and lean**: start < 100 ms, < 40 MB RSS idle, one binary (UI assets embedded);
5. **sends nothing extra**: no `dsh_session_log`, no `dsh_plugin_packages`, no anonymous user id,
   no telemetry. Only what the model request needs.

Non-goals: Cordis/JS plugin compatibility, the official DSH web API, Electron desktop, voice,
office and the separate `workflow` tool. PTC (`run_code`) is implemented with a
scoped Node runtime (§5.8a). Goals, schedules and Agent Teams are ported (§5.9).

## 2. Reference material (read-only)

| What | Where |
|---|---|
| Upstream source, pinned 0.2.1-rc.2 (commit 687f282e) | `../ref/deepseek-harness` (a checkout of github.com/deepseek-ai/deepseek-harness at 687f282e, next to this repo) |
| Upstream docs: tool catalog, persistence catalog/schema, wire extensions, config catalog | `ref/deepseek-harness/docs/{tool-catalog.md,persistence-catalog.md,persistence-schema.json,deepseek-llm-api-wire-extensions.md,config-catalog.md}` |
| Installed official dsh 0.2.0-rc.2 (runnable: `dsh --profile headless "..."`) | `/usr/lib/node_modules/@deepseek-ai/dsh` |
| Exact request header (24 tools, full descriptions + schemas) of dsh 0.2.0-rc.2 headless | `docs/rust/upstream/request-header-0.2.0-rc.2.json` |
| System prompt of dsh 0.2.0-rc.2 headless | `docs/rust/upstream/system-prompt-0.2.0-rc.2.txt` |
| Captured wire requests of dsh (4 calls: agent step, title, 2 more steps) | `docs/rust/upstream/wire-0.2.0-rc.2/*.json` |
| dsh's **Minimal** agent preset: request header (one persistent `bash`), system prompt, first wire request | `docs/rust/upstream/{request-header,system-prompt}-minimal-0.2.0-rc.2.*`, `wire-minimal-0.2.0-rc.2/001.json` |
| Instant DeepSeek stand-in that can dump requests | `tools/dev/mock_deepseek.py` |
| Original dsh-codex plugin (JS + Swift), the product to carry over | repo root: `client.js`, `index.js`, `lib/*.js`, `helper/` |

To produce more golden data: start `python3 tools/dev/mock_deepseek.py 18xxx N /dev/null <dumpdir>`,
then run the official dsh with `DEEPSEEK_API_KEY=sk-mock DEEPSEEK_BASE_URL=http://127.0.0.1:18xxx/anthropic dsh --profile headless "..."`
in a scratch directory. Its session lands in `~/.dsh/sessions/--<cwd with / as ->--/<id>/session.v4.jsonl.zstd`
(read with `zstdcat`). Delete scratch sessions you create. **Never use a real API key.** No real DeepSeek
calls in this phase.

## 3. Repository layout and ownership

```
Cargo.toml                 workspace (orchestrator)
crates/dshx-core           shared types and traits (orchestrator; additive changes only, list them)
crates/dshx-llm            DeepSeek adapter                                   (worker: llm)
crates/dshx-testkit        scripted mock DeepSeek server for Rust tests        (worker: llm)
crates/dshx-session        v4 log store, reader, listing, attachments, derive  (worker: session)
crates/dshx-tools          bash, read, write, edit, glob, grep, read_image,
                           todo_write, web_fetch, web_search, sandbox          (worker: tools)
crates/dshx-agent          agent loop, scheduler, retries, prompt, context,
                           AGENTS.md, compaction, subagents                    (worker: agent)
crates/dshx-browser        background Chrome over CDP pipe + `browser` tool    (worker: browser)
crates/dshx-macos          Swift-helper client + `computer`, `app` tools, mini (worker: macos)
crates/dshx-mcp            MCP client, OAuth connector logins, connector catalog
crates/dshx-plugins        plugin manifests, plugin MCP servers, marketplaces
crates/dshx-server         HTTP/WebSocket server for the UI                    (wave 2)
crates/dshx-goal           goals: get_goal/create_goal/update_goal, round driver   (automation)
crates/dshx-schedule       schedules, `storages/schedule.json`, time context       (automation)
crates/dshx-team           Agent Teams: roster, mailbox, task board, tools         (automation)
crates/dsh-codex           the binary: CLI (headless, web, sdk)                (wave 2)
ui/                        Codex-style web UI, no build step                   (worker: ui)
helper/                    Swift helper — unchanged except additive commands   (worker: macos)
client.js index.js lib/ package.json cordis.patch.yml
                           the original DSH plugin; stays working, do not move
.github/workflows/         CI (worker: macos)
docs/rust/                 SPEC, plan, notes
```

Each worker edits only its own crate/dir (plus `Cargo.toml` of that crate). Need something in
`dshx-core`? Make the smallest additive change, keep it compiling for everyone, and list it in the
final report. Never edit another worker's crate.

## 4. Core contracts (`crates/dshx-core`)

Read the source; it is short. Summary:

- `Message { id, role, content: Vec<ContentBlock>, source, toolCallId?, isError?, ..extra }` and
  `ContentBlock` (`text`, `reasoning`, `image`, `tool-call`, `tool-addition`, `tool-removal`,
  unknown preserved) serialize **exactly** like dsh (`tests/serde_compat.rs`).
- `LlmClient::stream(GenerateRequest, CancellationToken) -> BoxStream<StreamChunk>`; one terminal
  `Finish { reason, replay_state }`; errors are `FinishReason::Error { failure: LlmFailure }` with
  dsh codes. Retries are done by the agent, not the adapter.
- `Tool` trait: `schema()`, `concurrency_safe()`, `timeout()`, `describe(args)`, `execute(args, ToolContext)`
  → `ToolOutput { content: Vec<ToolResultPart>, is_error, value, meta, contexts }`; `contexts` are
  messages for the next step of the turn (dsh `exec.deferContext`). `ToolError` becomes an error
  result for the model, never a crash; `ToolError::Coded { name, code, message }` (dsh `HarnessError`)
  additionally logs `error: {name, code}` beside the result.
- `ToolContext` gives cwd, sandbox mode, cancel token, `AttachmentStore`, `model_accepts_images`,
  `Approver`, per-session shared `state`, and `caller: Option<CallerInfo>` (root agent or not, the
  open turn and its `turn/start` seq, read access to the caller's own log: dsh `exec.agent` plus the
  turn-boundary projection).
- `SessionLog` trait: `append(Vec<NewEvent>) -> Vec<SessionEvent>`, `flush()`, `events()`, `subscribe()`.
  `SessionEvent { type, seq, time, data, ..extra }`, header `SessionHeader`. Event names in `kinds`.
- `Config` (`~/.dsh-codex/config.toml`, `$DSHX_HOME`), incl. `[codex]` = the plugin's
  `computerUse/apps/browser/mini` settings with the same defaults.

## 5. Crate specs

### 5.1 `dshx-llm` + `dshx-testkit` (worker: llm)
- `DeepSeekClient` implements `LlmClient` for route `deepseek-official`. Base URL from config/
  `DEEPSEEK_BASE_URL` (default `https://api.deepseek.com/anthropic`), key from `DEEPSEEK_API_KEY` or
  the credentials file of `dshx-session`/config (keep a `ApiKeySource` trait so the server can set it).
- Request body: byte-for-byte the same structure dsh sends (see `wire-0.2.0-rc.2/`): `model`, `stream:true`,
  `max_tokens`, `thinking`, `output_config.effort`, `system` (string), `tools` (Anthropic `input_schema`),
  messages mapped from canonical messages (system messages in history, tool results as `tool_result`
  user blocks, images, thinking blocks with signatures replayed from `replayState`). Mirror
  `ref/.../packages/llm/llm-deepseek/src/{serialize,translate,sse,replay,transport,wire-types}.ts`.
  Headers: `anthropic-version: 2023-06-01`, `x-api-key`, `content-type`, `user-agent: dsh-codex/<ver>`.
  **Do not send** `dsh_session_log`, `dsh_plugin_packages`, `x-deepseek-harness-user-id`.
- SSE → `StreamChunk` incl. `thinking_delta`, `signature_delta`, `input_json_delta`, usage mapping
  (cache read/write disjoint like dsh), `replay_state` with kind `deepseek-messages` v1 identical to dsh
  so dsh can continue our sessions.
- Error classification like dsh `transport.ts` (status → code, `retry-after`), `RetryPolicy` struct
  with dsh defaults (5 retries, 500 ms → 10 s, jitter 0.1, codes EMPTY_RESPONSE/RATE_LIMIT/SERVER/
  TIMEOUT/TRANSPORT) exported for the agent.
- Images: inline base64 `image` blocks (Files API is optional/later). Model catalog: `deepseek-flash`
  (text+image, 1M), `deepseek-v4-pro` (text, 1M), efforts off/low/high/max as dsh.
- `dshx-testkit`: `MockDeepSeek` (axum or hyper) on 127.0.0.1:0 with scripted turns
  (text, thinking, tool calls, errors 429/500/overflow, slow streams), records requests for assertions.
- Tests: golden comparison of our request bodies with `wire-0.2.0-rc.2` (modulo ids/telemetry fields).
- Other providers (`provider.rs`, `provider_client.rs`, `anthropic/`, `openai.rs`, `router.rs`): `[providers.<id>]`
  over the built-in catalog (Anthropic), `ProviderClient` for Anthropic Messages (`x-api-key` or bearer,
  `cache_control` on the last tool, the system block and the newest turn, adaptive/budget thinking, signatures
  replayed only to the producing route, replay kind `anthropic-messages` v1) and OpenAI Chat Completions
  (`reasoning_content`/`reasoning` deltas, data-URI images, `stream_options.include_usage`). No in-history system
  prompt and no tool updates there: the system prompt sits at the top and every request lists all tools.
  `ProviderRouter` is the agent's client: it serves DeepSeek itself (unchanged bytes) and hands an agent with
  `AgentConfig.provider` its route through `LlmClient::for_provider`.

### 5.2 `dshx-session` (worker: session)
- File-backed `FileLog: SessionLog`, layout and framing **identical to dsh v4**:
  `<root>/--<cwd encoded>--/<session-id>/session.v4.jsonl.zstd` (+ `session.lock`), one zstd frame per
  append batch, fsync on `flush`, advisory lock, torn-tail recovery on open. Root default
  `~/.dsh-codex/sessions`; configurable so tests can point at `~/.dsh/sessions` for interop.
- Reader for dsh-written v4 logs (reject unknown non-ignorable event types like dsh; ignore the rest).
- `derive_messages(events)` faithful to `ref/.../packages/core/session/src/surface.ts`
  (surfaceOp append/replace, compaction summaries, inbox splices, interrupted-turn repair).
- Session listing (id, title from `session/title`, cwd, created/updated, last status), delete.
- `FileAttachments: AttachmentStore` storing images like dsh `attachment-local` (path layout, ids,
  dimension probing, optional downscale to a pixel budget).
- Credentials file for the API key (`~/.dsh-codex/credentials.toml`, mode 0600).
- **Interop acceptance**: a session written by `FileLog` (scripted events of a 2-step turn) is resumed by
  the official dsh (`dsh --profile headless --session-id <id> "continue"` against the mock, with the
  session copied under `~/.dsh/sessions`), and a dsh-written session is read and derived by us.

### 5.3 `dshx-tools` (worker: tools)
- Implement with the **exact** names, descriptions and JSON schemas from
  `docs/rust/upstream/request-header-0.2.0-rc.2.json`: `bash`, `read`, `write`, `edit`, `glob`, `grep`,
  `read_image`, `todo_write`, `web_fetch`, `web_search`, `job_list`, `job_output`, `job_kill`.
  Output text formats (line numbers, truncation notes, `[exit code: N]`, spill notices) like dsh —
  read the TS implementations.
- bash: fresh `bash -c` per call, env as dsh (`NO_COLOR`, `TERM=dumb`, `PAGER=cat`, strip
  KEY/TOKEN/SECRET/PASSWORD vars), timeout default 120 s max 600 s, 64 KB tail per stream with spill
  file, `run_in_background` → job, process-group kill on cancel/timeout.
- Sandbox (`SandboxMode`): Linux bubblewrap like dsh `sandbox-local` (`--ro-bind / /`, workspace rw,
  `--tmpfs /tmp`, `--unshare-pid`, `--die-with-parent`), fallback none + warning; macOS `sandbox-exec`
  profile like dsh. `sandbox_permissions`/`justification` → `Approver`.
- fs observation policy: read-before-write/edit, freshness check (mtime/size), like
  `fs-observation-policy`; path checks keep writes inside the workspace in `workspace-write`.
- glob/grep natively (crates `ignore`, `globset`, `grep-searcher`/`grep-regex`), same limits/sorting.
- web_fetch: public-IP-only, same-origin redirects, HTML → Markdown; web_search via DeepSeek's
  `web_search_20250305` server tool through `dshx-llm` is optional (stub returning a clear error is ok).
- `pub fn default_tools(config) -> Vec<Arc<dyn Tool>>`.

### 5.4 `dshx-agent` (worker: agent)
- `Agent::run_turn(user input) ` over `LlmClient`, `ToolRegistry`, `SessionLog`, mirroring
  `ref/.../packages/core/agent-loop` (turn/step lifecycle and the exact events it logs, see a dsh
  session), tool scheduler (concurrency-safe calls in a pool of 10, others exclusive, results committed
  in model order), cancellation semantics (interrupted assistant message, `ABORTED_BEFORE_DISPATCH` etc.),
  tool timeout, retries with `RetryPolicy` (`llm/retry` events), checkpoints (flush before request/dispatch).
- Prompt assembly identical to dsh: system prompt sections (`system-prompt-0.2.0-rc.2.txt` is the target
  output for the default tool set), runtime-context snapshot user message, AGENTS.md/CLAUDE.md chain
  (`context/agent-instructions`), admission rules that keep the DeepSeek prefix cache warm
  (agent-loop README). Codex tools add their own prompt lines if dsh-codex had any (it has none; the tool
  descriptions carry the guidance).
- Compaction: pressure compaction + tool-result pruner like `compaction-basic`, overflow recovery once.
- Session title: first-prompt fallback title + one LLM title request like dsh (`session/title*` events).
- Subagents (`subagent`, `subagent_fork`, `send_message`, `interrupt_agent`, `list_agents` as in-process
  tasks with child sessions and `delegationDepth`, like `packages/subagent`) are **wave 2**; design the loop
  so a child agent is just another `Agent` with its own `SessionLog`.
- Events out: everything the UI needs is in the session log; live text/reasoning deltas additionally go
  through an `AgentObserver` callback (not persisted).
- Tests with `dshx-testkit` (or an in-crate fake `LlmClient`) and `dshx_session::MemoryLog`.

### 5.5 `dshx-browser` (worker: browser)
- Port `lib/browser.js` + `lib/browser-tool.js`: Chrome/Chromium headless with its own profile dir
  (`~/.dsh-codex/browser-profile`; macOS: keep `~/Library/Application Support/dsh-codex/browser-profile`),
  DevTools protocol over `--remote-debugging-pipe` (fds 3/4, no port), same actions, element outline
  format `[3] button "Search" @(x,y)`, screenshots (JPEG quality), `read`, tabs, `show`/`hide`, password
  refusal, downloads blocked, idle exit. Tool name/description/schema **verbatim** from `lib/browser-tool.js`.
- Output through `ToolOutput`: text summary + screenshot via `AttachmentStore` when
  `model_accepts_images`, `value`/`meta` exactly like `index.js` (`summary`, `image`, `screen`,
  `cursor`, `place`, `target`; presentationMeta fields `tool, action, label, x, y, toX, toY, screen,
  cursor, place, target`).
- On macOS also drive the mini window through `dshx-macos`'s helper client (feature-gated call, no hard
  dependency cycle: expose a `MiniSink` trait in `dshx-browser` that `dshx-macos` implements).
- Tests on Linux with Chromium at `/root/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome` against a local test HTTP server.

### 5.6 `dshx-macos` (worker: macos)
- Unix-socket client for the helper (`~/Library/Application Support/dsh-codex/cua.sock`, JSON lines,
  start via `open -g` on ENOENT/ECONNREFUSED) = port of `lib/cua-client.js`.
- `computer` and `app` tools: names, descriptions, schemas, `describe`, activity strings (de/en), request
  mapping and result text **verbatim** from `lib/computer-tool.js`, `lib/app-tool.js`; output shape like
  `index.js` (attachments, scaling note, text-only hint). `configure` once per process with the
  `[codex]` settings and localized mini strings (`STRINGS` in `index.js`), system locale detection.
- Implements `dshx_browser::MiniSink` for the browser's mini-window updates.
- Compiles on every OS; tools only registered on macOS. Tests on Linux with a fake helper on a temp socket.
- No GitHub CI (owner's decision): macOS is verified on the owner's Mac. `helper/build.sh --no-sign`
  builds the Swift helper without signing or installing.
- The Swift helper stays as is. Only additive socket commands if the Rust side truly needs them.

### 5.7 `ui/` (worker: ui)
- Codex-style web UI served by `dshx-server`, **no build step**: plain ES modules, vendored
  `preact` + `htm` (+ `marked` for Markdown, small highlighter optional) under `ui/vendor/`.
- Visual spec = `client.js` (tokens, layout, composer, hero start page, transcript rows with screenshot +
  gliding agent cursor) translated from "theme on top of DSH" into our own components. Light/dark/system,
  de/en from `navigator.language`. Respect `ref/deepseek-harness/BRAND_GUIDELINES.md` for any DeepSeek
  mark; if a whale mark is not allowed outside DSH, use a neutral glyph.
- Screens: sidebar (wordmark, new chat, sessions grouped by workspace), conversation (768 px column,
  dark user bubble, Markdown answers, collapsible reasoning, Codex-like tool rows: bash command +
  output, read/edit with diff, todo list, browser/app/computer rows with preview), hero start page
  ("What should we work on in ‹workspace›?"), composer (model chip, effort, access mode in orange when
  full access, image attach, send/stop), approval card, settings (API key, model, effort, sandbox),
  stats line under composer.
- Renders **only from session events** (same envelope as on disk) plus live deltas; protocol in §6.
  Ship `ui/fixtures/*.json` (event logs incl. browser/computer rows) and a fixture mode
  (`index.html?fixture=name`) so it can be developed and screenshotted without a server.
- Verify visually with headless Chromium (Playwright venv `/root/.venvs/playwright`, Chrome `/root/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome`; `webshot --help` takes screenshots + console errors),
  light and dark, desktop and narrow width, no console errors.

### 5.8 `dshx-server` + `dsh-codex` binary (wave 2, orchestrator)
CLI: `dsh-codex "task"` (headless; flags `--json` NDJSON like dsh, `--session-id`, `--model`, `--effort`,
`--sandbox`, `-C dir`), `dsh-codex web [--port] [--no-open]`, `dsh-codex sdk` (dsh SDK JSON-RPC over stdio:
`initialize`, `session/prompt`, `session/resume`, `session/wait`, `session/working-directory/{get,set}`,
`shutdown`; notifications `session.event`, `session.status`, `subagent.started`, `subagent.finished`).

### 5.8a Agent presets (modes): Standard, PTC and Minimal
dsh's web bundle composes each session from an agent preset (`dsh-web-app/presets/*.patch.yml`).
We ship **Standard** (everything above, unchanged), **PTC** (below), and **Minimal**
(`presets/minimal.patch.yml`): `dsh-persona` with `complete: true` and `includeRuntimeContext: false`
(the system prompt is exactly `You are a helpful software engineer assistant.`) plus
`dsh-tool-bash-persistent` (tool `bash`, the preset's own description, schema `{command}`, 300 s
timeout, 16 000 output characters). No workspace instructions, compaction, pruner, skills, MCP,
Codex tools, subagents, goals, Teams or time context; titles, retries, image offload and the
repeat-tool reminder stay (dsh host plane). The goldens were captured from the official dsh
0.2.0-rc.2 `sdk-minimal` profile, which composes the same persona and shell rows, against the mock
with an isolated `DSH_HOME` (`tools/dev/dsh_sdk_capture.py`); our first Minimal request equals it
minus `dsh_session_log`/`dsh_plugin_packages` (`crates/dsh-codex/tests/presets.rs`).

- The preset is chosen when a session is created and recorded like dsh in the header
  (`agentPreset`); `agent-preset/selected` events are honoured when reading dsh sessions. No record
  (older sessions, headless runs without `--preset`) and unknown ids (for example `cordis`) run Standard.
  A session never changes preset (prompt cache); another mode means a new chat.
- The persistent shell is one `bash --noprofile --norc` per session reading commands from a pipe
  (stderr folded into stdout, stdin of each command `/dev/null`), wrapped in dsh's start/end
  markers; results, timeout, truncation and reset texts are dsh's. It runs under the session's
  sandbox mode for its life and restarts when the mode changes; it has no escalation arguments.
- CLI: `dsh-codex --preset minimal "task"` (new sessions only); config `default_preset`.

PTC keeps Standard's capability registry and hooks, but the wire tool set is
only `run_code`. A deterministic SDK in the system prompt describes the native
capabilities as TypeScript functions. The SDK uses generic JSON return types
where a tool has no declared output schema. Each program runs in a fresh Node
process with an empty environment, a separate length-framed RPC channel and a
host-enforced deadline (120 s default, 600 s maximum). Only erasable TypeScript
syntax is supported. The native app bundles pinned Node; CLI/web can use
`DSHX_NODE` or `node` on PATH.

Nested calls go through the normal scheduler policy, validation, approval,
timeout and result hooks. Safe calls can overlap; exclusive calls are barriers.
Only existing `tool/ptc-dispatch-start` / `tool/ptc-dispatch` audit pairs are
written for nested execution; the model sees the curated outer output plus
forwarded image/context blocks. Restricted execution fails closed without an
OS sandbox. Cancellation terminates the process group and drains nested calls.

Prompt acceptance is acknowledged only after inbox persistence succeeds. An
optional client `messageId` makes retries of identical content idempotent;
reuse with different content is rejected. Inbox ownership is FIFO, both pending
targets are visible, and claimed input remains recoverable until its user
message is committed. Recovery reconciles partial promotion/admission batches
by message identity. Explicit cancellation/removal remains authoritative.

### 5.9 Automation: `dshx-goal`, `dshx-schedule`, `dshx-team`
Ports of the dsh 0.2.0-rc.2 packages, with tool names, descriptions and schemas byte-identical to
the goldens in `docs/rust/upstream/tools-0.2.0-rc.2/` (compiled with `tools/dev/extract_tool_schemas.mjs`;
goal and Team tools also checked against captured request headers).

- **Goals** (`dsh-goal`, `dsh-tool-goal`, `dsh-goal-round-driver`; on by default like dsh-base):
  `goal/change` v1 events in the session (strict replay fold, compare-and-set mutations, process-local
  `armed`/`disarmed` activation that starts disarmed on every attach). `create_goal`, `edit`, `pause`,
  `resume` need a direct human turn on a root agent; `complete`/`blocked` are also allowed inside the
  goal's own round (then the `<goal_complete>`/`<goal_blocked>` wrap-up is deferred as context).
  The driver (web and SDK hosts) queues one `<goal_round>` follow-up (`source: {kind: "goal", goalId,
  revision, round}`) whenever the agent is idle and the goal active and armed; a pre-step check admits
  only that exact round and starts a request series for it. Round cap → `blocked` `round-limit`;
  a user abort of a round → `pause`; `max-tokens`, agent errors and other aborted turns → disarm.
- **Schedules** (`dsh-schedule`, opt-in `[codex.schedule] enabled`): host-wide tasks in
  `$DSHX_HOME/storages/schedule.json` (dsh `storage-json` unit `schedule` v1); `after_seconds`, `at`,
  `every_seconds`, `daily`, `weekly`, Vixie `cron` with IANA zones (earlier instant of repeated local
  times, gaps skipped); after downtime a recurring task delivers only its latest missed occurrence; the
  recurring due tasks of one session go out as one `[SCHEDULE REMINDER BATCH]`. Delivery = follow-up
  (`source: {kind: "schedule"}`) into the reopened session, flush, then commit target and receipt.
  Tools only for root agents; only `dsh-codex web` delivers.
- **Time context** (`dsh-time-context`, follows `schedule.enabled` unless `[codex.time_context]
  enabled` says otherwise): a `time-context` snapshot user message as the last message of a step,
  at most once per `refresh_interval_ms` (600000).
- **Agent Teams** (`dsh-experimental-agent-team-profile`, opt-in `[codex.team] enabled`): replaces
  the subagent tools with `spawn_teammate`, `send_message`, `list_agents`, `wait_agent`,
  `interrupt_agent`, `team_task_{create,list,get,update}`; `team/member`, `team/task`,
  `team/message/queued`, `team/message/delivered` v2 events in the Lead session; teammates are
  continuable children (explicit child id, two-block prompt without return guidance, peer messages
  steered with `source: {kind: "team-message", ...}`), the system prompt gets `team:policy` (600).

## 6. UI ↔ server protocol

Server binds 127.0.0.1, random token; the opened URL is `/?token=…`, the UI then sends
`Authorization: Bearer <token>` (HTTP) and `?token=` (WebSocket).

| Method | Path | Body → Response |
|---|---|---|
| GET | `/api/info` | → `{version, platform, locale, models:[ModelInfo], providers:[{id, name, available}], settings:{model, effort, sandbox, approval, defaultPreset, compaction:{mode, thresholdTokens}}, apiKeyConfigured, tools:{browser, app, computer}, extensions:{plugins, pluginsEnabled, marketplaces, skills, mcpServers, mcpConnected, mcpNeedsLogin, personalSkills}}` (`personalSkills`: the personal skills folder, `~`-relative); each model row adds `{provider, providerName, model, maxTokens, available, cost?, offPeakCost?}` (DeepSeek rows: `cost` at peak hours and `offPeakCost`, both from the usage price book) and its `id` is the model reference (bare DeepSeek id or `provider/model`); `apiKeyConfigured` = some route can take a prompt |
| PUT | `/api/settings` | `{model?, effort?, sandbox?, approval?, apiKey?, defaultPreset?, compaction?: {mode?, thresholdTokens?}, ui?: {workDetails?, performanceUsage?, sidebarGroup?, sidebarOrder?, sidebarArchived?, confirmFullAccess?}}` → `/api/info` shape (`defaultPreset` → config `default_preset`; `compaction` → `[codex.compaction]`, `mode` one of `threshold`, `every-turn`, `off`, `thresholdTokens` 20000–1000000, else 400, and open Standard chats follow at once; `ui` goes to `$DSHX_HOME/ui-state.json`, so every browser sees it) |
| GET | `/api/workspaces` | → `[{path, name, lastUsed}]` |
| POST | `/api/workspaces` | `{path}` → workspace |
| GET | `/api/fs/dirs?path=&hidden=` | → `{path, parent, root, home, roots, git, entries:[{name, path, git, hidden}], truncated}`: subfolders only, sorted by name, at most 500; `path` empty = home (or the first root), `~/…` allowed; `hidden=1` adds dot folders (and Finder-hidden ones on macOS). Only inside `Backend::folder_roots` (home, `[folders] roots`, the parent of every project except `/`); outside → 403 `outside-roots`, missing → 404 `not-found`, unreadable → 403 `permission-denied`, no roots → 404 `unsupported` (errors are `{error, code}`) |
| GET | `/api/fs/suggestions` | → `{repos:[{name, path}]}`: up to 12 repositories (folders with `.git`) at most three levels below the roots, most recently changed first; skips dot folders, build output and, on macOS, the privacy-protected home folders |
| POST | `/api/fs/mkdir` | `{path, name}` → 201 `{path}`; `name` is one plain folder name; exists → 409 `exists` |
| POST | `/api/fs/pick` | `{prompt?, defaultPath?}` → `{path \| null}` (`null` = cancelled) from `osascript` "choose folder"; only on macOS when the server was not started from an SSH session, else 501 `unsupported` (the UI then shows its folder browser); a second dialog → 409 `busy` |
| GET | `/api/sessions?cwd=` | → `[{id, title, cwd, createdAt, updatedAt, status, scratch, pinned, archived, unread, preset?, parentSession?}]` (newest first; `preset` when the session recorded one; `parentSession` for branches; subagent children, header `origin: "subagent"`, are left out) |
| POST | `/api/sessions` | `{cwd?, preset?}` → `{id}`; without `cwd` the chat gets its own folder under `$DSHX_HOME/chats/` (`scratch: true`); `preset` (`standard`, `ptc`, `minimal`, default `settings.defaultPreset`) is fixed for the session and recorded as header `agentPreset`; unknown → 400 |
| GET | `/api/sessions/:id` | → `{header, events:[SessionEvent], status}` |
| PATCH | `/api/sessions/:id` | `{title?, pinned?, archived?, seen?, unread?}` → list row; kept in `$DSHX_HOME/ui-state.json`, not in the log |
| POST | `/api/sessions/:id/preview` | → `{base, root}`: `base` is `/preview/<grant>/`, a 2-hour read-only capability for the chat's folder |
| GET | `/api/sessions/:id/files?dir=` | → `{dir, entries:[{name, path, kind:"dir"\|"file"\|"broken", size}], truncated}` (inside the chat's folder only) |
| GET | `/preview/:grant/*path` | no bearer; a file of the granted folder (folders serve `index.html`); HTML/SVG/XML get `CSP: sandbox` (opaque origin), everything `nosniff`, `no-store` |
| POST | `/api/sessions/:id/prompt` | `{text, messageId?, images?:[ImageAttachmentRef], model?, effort?, sandbox?, steer?, timeZone?}` → 202; while a turn runs the text queues as the next turn, with `steer: true` its next step takes it; `timeZone` (IANA) makes the message source dsh's web form `{kind:"user", rpcId, clientTimeZone}` for the time context |
| POST | `/api/sessions/:id/cancel` | → 202 |
| POST | `/api/sessions/:id/fork` | `{turn, title?}` → `{id}`: a new chat in the same folder and mode with the log through the `turn/end` of `turn` (seq and time kept; `0` = none of it), then dsh's `session/end-seed {inherited: true}` cut; header `parentSession` = the source, `isSeeded: true`, no `origin` (so it stays in the list and is a root agent); `title` (default dsh's "Title (1)") goes to `ui-state.json`; a turn without `turn/end` → 400, unknown session → 404. Inherited requests count only in the source's usage |
| DELETE | `/api/sessions/:id` | → 204 |
| POST | `/api/approvals/:callId` | `{decision:"allowed-once"\|"rejected"}` → 204 |
| POST | `/api/attachments` | raw image body (png, jpeg, webp, gif; ≤ 20 MB), `Content-Type` → `ImageAttachmentRef` |
| GET | `/api/attachments/:id` | → image bytes |
| GET | `/api/sessions/:id/goal` | → `{goal: GoalView \| null}`; `GoalView` = `{id, revision, objective, phase, maxGoalRounds, blockedReason?, roundsStarted, createdAt, updatedAt, activation}` |
| POST | `/api/sessions/:id/goal` | `{action: "create"\|"edit"\|"pause"\|"resume"\|"clear", objective?, maxGoalRounds?}` (dsh `/goal`) → `{goal}`; refused transitions → 400 |
| GET | `/api/schedules` | → `[{...record, sessionId, status: "active"\|"inactive", lastDelivery?}]` by `scheduledAt` |
| GET | `/api/sessions/:id/schedules` | → active reminder views `[{...record, state: "scheduled"\|"overdue", deliveryMode: "host"}]` |
| PUT | `/api/schedules/:id` | `{title?, prompt?, at?\|every_seconds?\|daily?\|weekly?\|cron?}` (`schedule_update` fields) → view; invalid/ended/conflict → 400, unknown → 404 |
| DELETE | `/api/schedules/:id` | → `{id, deleted: true}`; unknown → 404 |
| GET | `/api/schedules/:id/history?limit=&before=` | → `{id, records: [{scheduledAt, deliveredAt, messageId, prompt?}], earlierRecordsUnavailable, earlierRecordsPruned, retention, nextBefore?}` (newest first, `limit` 1-100, default 20) |
| GET | `/api/sessions/:id/team` | → `{members: [{id, name, role, phase, error?}], tasks: [TaskView], roster: [list_agents rows], failure?}` |
| GET | `/api/sessions/:id/stats?fromTurn=` | → `SessionStats` (below); `fromTurn` limits `turns` to turns ≥ it, the totals always cover the whole log |
| GET | `/api/usage?since=&until=&tz=&top=` | → `UsageReport` (below) of all sessions; `since`/`until` ms (default the last 30 days), `tz` the reader's UTC offset in minutes (local days), `top` chats (default 5, max 50) |
| WS | `/api/stream?session=:id&after=:seq` | server → client messages below |

### Models page (`Backend::providers`, routes in `dshx-server/src/providers.rs`)

Without providers every route below answers 404. Errors are `{error}` with 400, unknown ids 404.
Keys go in, never out: rows only say whether a key is configured and where it comes from.

| Method | Path | Body → Response |
|---|---|---|
| GET | `/api/providers` | → `{providers:[Provider], catalog:[{id, name, api, baseUrl, apiKeyEnv, models, added}]}` (DeepSeek first, then `[providers.<id>]` by id) |
| PUT | `/api/providers/:id` | `{displayName?, api?, baseUrl?, apiKeyEnv?, titleModel?, headers?, models?, compat?, apiKey?}` (fields sent replace, `null` clears, others stay; a catalog provider whose models equal the catalog's keeps inheriting them; `deepseek-official` takes `baseUrl` and `apiKey`) → `Provider` |
| DELETE | `/api/providers/:id` | → 204 (table and stored key; a default model of that provider falls back to `deepseek-flash`); `deepseek-official` → 400 |
| PUT | `/api/providers/:id/key` | `{key}` (`null`/blank removes; a key from the environment is read-only → 400) → `Provider` |
| POST | `/api/providers/:id/models/fetch` | `{api?, baseUrl?, apiKey?}` (unsaved values of the dialog) → `{models:[{id, name?, contextWindow?, maxTokens?, input?, reasoningEfforts?, defaultEffort?, thinking?, cost?}]}` from Anthropic `GET /v1/models` or OpenAI-compatible `GET /models` |

- `Provider`: `{id, name, api:"anthropic-messages"|"openai-completions", baseUrl, defaultBaseUrl, baseUrlLocked?, apiKeyEnv,
  builtin, catalog, modelsEditable, modelsCustomized, titleModel?, headers:[names], compat?:{auth, cacheControl, maxTokensField,
  reasoningContent}, key:{configured, source:"env"|"stored"|null, locked, env, required}, models:[{id, ref, name, contextWindow,
  maxTokens, input, reasoningEfforts, defaultReasoningEffort, thinking?, cost?:{input, output, cacheRead, cacheWrite}}], error?}`
  (`cost` in USD per million tokens).
- Prompts name a model reference; the agent records the route's provider (`request/context`, `assistant/message`
  source) and the bare model id, so DeepSeek sessions stay byte-identical to dsh.

### Plugins page (`Backend::extensions`, routes in `dshx-server/src/extensions.rs`)

Without extensions every route below answers 404. Errors are `{error}` with 400, unknown ids 404.

| Method | Path | Body → Response |
|---|---|---|
| GET | `/api/plugins` | → `[Plugin]` (every folder under `$DSHX_HOME/plugins/<marketplace>/<name>/`) |
| POST | `/api/plugins` | `{marketplace, name, enable?=true}` → `Plugin` (install = copy/clone, then `[plugins."id"] enabled`) |
| GET | `/api/plugins/:id` | → `Plugin` (`id` = `<name>@<marketplace>`) |
| DELETE | `/api/plugins/:id` | → 204 (folder and config entry removed) |
| POST | `/api/plugins/:id/enable` \| `/disable` | → `Plugin` (skills and MCP servers applied at once) |
| POST | `/api/plugins/:id/update` | → `Plugin` (pulls a git marketplace, reinstalls) |
| GET | `/api/plugins/:id/assets/*path` | → image bytes (png, jpeg, gif, webp, svg, ico inside the plugin only) |
| GET | `/api/marketplaces` | → `[Marketplace]` |
| POST | `/api/marketplaces` | `{source, ref?}` (git URL, `owner/repo`, local path; `#ref` suffix allowed) → `Marketplace` |
| POST | `/api/marketplaces/:name/update` | → `Marketplace` |
| DELETE | `/api/marketplaces/:name` | → 204 (installed plugins stay) |
| GET | `/api/skills?cwd=` | → `[{name, title, description, whenToUse, source, plugin, logoUrl?, path, active, modelInvocable, userInvocable, enabled, model}]` (`modelInvocable` = the frontmatter allows it; `model` = the model may load it on its own under `[skills]`; `enabled` = `[skills.entries."name"] enabled`); skill files that are no valid skill follow as `{name (folder), title, source, plugin, path, active:false, invalid:true, error, modelInvocable:false, userInvocable:false, enabled, model:false}` |
| GET | `/api/skills/:name?cwd=&path=` | → the listed row plus `{content (SKILL.md, max 512 KiB), contentTruncated, frontmatter, body, dir, files:[{path, size}\|{path, dir:true}], filesTruncated}` (folder bundles: up to 300 entries, 4 levels, without `.git`/`node_modules`); `path` picks one of several skills of that name; unlisted → 404 |
| POST | `/api/skills` | `{name, scope:"personal"\|"project", cwd?, description?, lang?}` → the new skill's detail (`lang: "de"` writes the template's headings in German); creates `<agents home>/skills/<name>/SKILL.md` (`$DSH_AGENTS_HOME` or `~/.agents`) or `<project>/.agents/skills/<name>/SKILL.md` (project = nearest `.git` ancestor of `cwd`); a taken name or folder → 400 |
| GET | `/api/mcp/servers` | → `[McpServer]` (config servers, then plugin servers) |
| POST | `/api/mcp/servers` | `{name?, connector?, config?, secrets?}` (`config` = a `[mcp_servers.x]` table as JSON; `connector` fills the URL from the catalog, its id is the default name; `secrets` below) → `McpServer` |
| GET | `/api/mcp/servers/:name` | → `McpServer` whose `tools[]` also carry `inputSchema` |
| PUT | `/api/mcp/servers/:name` | `{config, secrets?}` → `McpServer` (config servers only). Replaces the form's keys (`transport, command, args, cwd, env_vars, url, bearer_token_env_var`, `oauth.enabled` merged into `oauth`); timeouts, reconnect, an OAuth client and any other keys stay |
| GET | `/api/mcp/servers/:name/log` | → `{name, lines:[{time, kind:"stderr"\|"error"\|"info", text}]}`: the last 200 lines (stdio stderr, failed HTTP requests, connection events), kept across restarts |
| POST | `/api/mcp/test` | `{name?, config, secrets?, timeoutMs?}` → `{ok, tools:[{name, description}], error?, needsLogin, stderr:[lines], elapsedMs}`: handshake and `tools/list` in a throwaway client without retries (default 30 s, max 120 s); stdio servers are stopped with their process group afterwards. Masked (`null`) values of the server `name` come from its stored secrets |
| DELETE | `/api/mcp/servers/:name` | → 204 (config servers only; its tokens are revoked and deleted, its secrets removed) |
| POST | `/api/mcp/servers/:name/enable` \| `/disable` \| `/restart` | → `McpServer` |
| POST | `/api/mcp/servers/:name/login` | → `{server, authorizationUrl, redirectUri}`; open the URL in a browser |
| POST | `/api/mcp/servers/:name/logout` | → `McpServer` |
| PUT | `/api/mcp/servers/:name/token` | `{token}` (personal access token; `null` removes) → `McpServer` |
| GET | `/api/connectors` | → `[{id, name, description, url, auth:"oauth"\|"token"\|"none", category, website, brandColor, tokenHelp?, added, servers, status}]` |
| GET | `/api/oauth/callback` | **no token**, loopback Host only; `?code&state&iss` or `?error` → HTML page; bound to the pending `state` |

- `Plugin`: `{id, name, marketplace, root, format:"agent-plugins"|"codex"|"claude"|"layout", version, description,
  author, homepage, repository, license, keywords, interface:{displayName, shortDescription, longDescription?,
  developerName?, category?, brandColor?, logo?, composerIcon?, screenshots?, websiteURL?, …}, logoUrl?,
  composerIconUrl?, skillDirs, skills:[{name, description, path}], mcpServers:[{name, originalName, error?,
  warnings?, status}], warnings, error?, enabled, installed:{marketplace, name, version, source, commit,
  installedAt, updatedAt}|null}`.
- `Marketplace`: `{name, source, kind:"git"|"github"|"local", url?, ref?, path, commit?, addedAt, updatedAt,
  displayName, description, owner, format:"codex"|"claude", plugins:[{id, name, source:{kind:"path"|"git"|"unsupported", …},
  description, version, category, author, homepage, keywords, interface, policy?, installed, enabled}], error?}`.
- `McpServer`: `{name, origin:"config"|"plugin", plugin, originalName, connector, enabled, transport, url, command,
  args, cwd, envVars, bearerTokenEnvVar, env:[names], headers:[names], secrets:{env:[names], headers:[names], bearerToken:bool},
  oauthEnabled, always, disabledTools:[rawName] (the config table, for plugin servers the plugin's table),
  auth:{oauth, login:{kind:"oauth"|"token", issuer?, scope?, expiresAt?, refreshable}|null},
  status:"connecting"|"connected"|"needs-login"|"error"|"disabled"|"stopped", error, retrying, challenge?:{status,
  resourceMetadata?, scope?, error?, errorDescription?}, warnings, tools:[{name, rawName, description}]}`.
- `secrets` (add, update, test): `{env?: {NAME: value|null}, headers?: {Name: value|null}, bearerToken?: string|null}`.
  Values go to `credentials.toml` (`[mcp_servers.<name>]` `env`, `headers`, `bearer_token`; mode 0600), never to
  `config.toml`; a server runs with its table plus them (a bearer token becomes `Authorization: Bearer …`). `env` and
  `headers` list every name the server keeps: a string sets a value (a plain one in `config.toml` moves to the secrets),
  `null` keeps the stored one, names not listed are removed; a missing table keeps all. `bearerToken`: a string sets,
  `""` removes, `null` keeps. Values in `config.env`/`config.headers` (pasted JSON) count as new secrets. Switching
  to Streamable HTTP drops the stdio values and the reverse. `[mcp_servers.x] env_vars = ["NAME"]` passes parent
  variables through to a stdio server even when their names look like credentials (Codex `env_vars`).
- Login flow: `POST …/login` derives the redirect URI from the request's Host port
  (`http://127.0.0.1:<port>/api/oauth/callback`, so it also works through an SSH tunnel to the same port),
  the UI opens `authorizationUrl`, the authorization server sends the browser to the callback, the server
  restarts and `GET /api/mcp/servers` shows `connected`. A `needs-login` server with `challenge.error =
  "insufficient_scope"` needs the same login again (step-up, wider `scope`).

WebSocket messages (JSON):
- `{"kind":"event","sessionId","event":SessionEvent}` — every appended event, in order, after `after`.
- `{"kind":"delta","sessionId","turn","step","index","blockType":"text"|"reasoning"|"tool-call","text"}` — live
  streaming text, not persisted; the final `assistant/message` event replaces it. A `tool-call` delta comes once
  when a tool call starts streaming, with the tool's name as `text` (the UI says what is being prepared).
- `{"kind":"status","sessionId","status":"idle"|"running"|"waiting-approval"|"error","error"?}`
- `{"kind":"approval","sessionId","callId","tool","summary","justification"}` — pending approval.
- `{"kind":"goal","sessionId","goal":GoalView|null}` — goal or activation changed (session clients
  and the session-less sidebar feed).
- `{"kind":"schedule-changed"}` — a task was created, updated, delivered or deleted; to every client.
- `{"kind":"live","sessionId", tool, args?, active, ended, activity?, place?, width?, height?, cursor?:{x,y,click},
  image?:base64 JPEG, attachment?:ImageAttachmentRef, meta?, error?, time}` — live view of browser/computer work of
  that session (merged state, at most 4 frames/s), sent from `Backend::live` on its own channel; the latest one also
  follows the backlog. Never persisted; a lagging client just misses frames.
- `/api/info` additionally carries `automation: {goals, schedules, timeContext, team}`,
  `settings.defaultPreset`, `presets: [{id, name, description, help: {intro, explanation, usage}}]`
  (English; help in Markdown from dsh's agent-preset guide; the UI localizes by `id`) and
  `ui: {workDetails: "compact"|"standard"|"detailed"|"verbose", performanceUsage: "compact"|"detailed",
  sidebarGroup: "projects"|"flat", sidebarOrder: "activity"|"created", sidebarArchived: "hide"|"show"|"only",
  confirmFullAccess: bool}` (the sidebar's view options and whether switching to Full access asks first).
- `POST /api/sessions/:id/goal` on a Minimal session → 400 (goals are not part of Minimal).

### Composer: `/` and `@`, skills and MCP on demand (`Backend::composer`, routes in `dshx-server/src/composer.rs`)

Without a composer backend every route below answers 404. Errors are `{error}` with 400, unknown ids 404.

| Method | Path | Body → Response |
|---|---|---|
| GET | `/api/composer/suggest?kind=slash\|mention&q=&cwd=&session=&model=&preset=` | → `{kind, query, sections:[{id, items}], cacheMiss, preset}` (below); `cwd` defaults to the session's folder, `model` (the composer's pick) decides `cacheMiss`, `preset` is the mode the start page will create (a session's own mode wins); Minimal offers no skills, apps or chat-only commands |
| GET | `/api/sessions/:id/mcp` | → `{active:[server]}`: the MCP servers the chat offers (its log, `always` servers, changes not yet recorded); Minimal → `[]` |
| POST \| DELETE | `/api/sessions/:id/mcp/:server` | → `{active}`; switches the server on or off for the chat from its next step on; unknown server → 404, Minimal → 400 |
| POST | `/api/sessions/:id/compact` | → `{compacted:true, items, tokens}` or `{compacted:false}` (nothing to compact yet): dsh `/compact`, also below the pressure threshold, in a bracket with `turn: null`; while a turn runs or input is queued → 400; Minimal → 400 |
| GET | `/api/sessions/:id/export` | → the session log as JSONL (`application/x-ndjson`, attachment `dsh-codex-<id>.jsonl`): the header line, then one event per line |
| POST | `/api/sessions/:id/queue/:item` | `{action:"edit"\|"remove"\|"steer", text?}` → 204: dsh `updateQueue` on a prompt waiting in the inbox (`edit` replaces its text; `steer` moves a next-turn prompt into the running turn); gone → 400 "queued item is no longer pending" |
| POST | `/api/files?name=` | raw body (≤ 100 MB) → `{path, name, bytes, sha256}`: a verbatim read-only copy at `$DSHX_HOME/attachments/files/<sha256[..16]>/<name>`; the UI references it as `@path` in the prompt |
| PUT | `/api/skills/:name/settings` | `{enabled?, model?, cwd?}` → the skill's row of `GET /api/skills` (for `cwd`) |
| PUT | `/api/mcp/servers/:name/settings` | `{always?, disabledTools?:[rawName]}` → `McpServer`; plugin servers keep theirs in `[plugins."id".mcp_servers.<name>]` |

- Suggest items (`insert` is the plain text the pick puts in the prompt): `commands`: `{type:"command", name,
  title, description, insert}` for `compact`, `goal`, `model`, `permission`, `status`, `view`, `export` (the ones
  that work there: without a session only `model`, `permission`, `view`); `skills`: `{type:"skill", name, title,
  description, source, plugin, model, insert:"/name "}` (enabled, user-invocable); `apps`: `{type:"server", name,
  title, description, status, tools, needsLogin, active, always, connector, brandColor, insert:"@name "}` and
  `{type:"plugin", name, id, title, description, servers, skills, active, brandColor, insert:"@name "}`;
  `files`: `{type:"file"|"folder", name, path, dir, insert}` with dsh's `formatFileMention` (`@path`, `@folder/`,
  `@"with spaces"`), at most 20, gitignore-aware, dsh-file-reference-local's excluded folders, at most 50 000
  indexed per project (cached 20 s); a query with `/` lists that folder. Ranking: exact, prefix, word prefix,
  substring, subsequence. `cacheMiss` = the model's route has no tool updates (switching a server on resends the
  whole tool list once).
- Config: `[skills] catalog = "manual"|"auto"` (default `manual`: no `<available_skills>` catalog and no `skill`
  tool unless an entry says `model = true`; `auto` = dsh), `[skills.entries."<name>"] enabled` (default `true`;
  `false` = neither listed nor loadable, not even by `/name`) and `model` (default `false`). `[mcp_servers.<name>]
  always` (default `false`: tools offered in every chat) and `disabled_tools = [rawName]` (never offered).
- Prompts: `/name` and `$name` load a skill's `<skill_content>` into that turn (dsh `skill-invocation`). `@server`
  (case-insensitive) or `@plugin` (all of its servers) switches servers on for the chat: their tools join through the
  agent's `developer/message` `tool-addition` (declared with `defer_loading` on DeepSeek's addition-only route,
  sent with the `mid-conversation-tool-changes` beta), followed by a `user/message` with source `{kind:
  "mcp-activation", server}` that names the tools and carries the server's instructions. Server instructions are no
  longer part of the system prompt, except for `always` servers. Switching off writes `{kind: "mcp-deactivation",
  server}`; the addition-only route keeps the declared tools (a call fails with "turned off"), other routes drop
  them. The log is the record: the first request header, `tool-addition`s and these notes decide which servers a
  chat offers after a restart.
- The system prompt carries dsh's file-reference guidance (`context:file-reference`, order 900).

### Usage and costs (`crates/dsh-codex/src/usage.rs`)

The server prices every request; the UI only formats. Costs are USD. A request (`assistant/message` or
`compaction/summary` with `usage`) costs `(inputTokens × input + cacheReadTokens × cacheRead + cacheWriteTokens ×
cacheWrite + outputTokens × output) / 1e6` at the price that applied at the event's `time`. Built in are DeepSeek's
list prices for `deepseek-flash` and `deepseek-v4-pro` (provider `deepseek-*`) on DeepSeek's clock: peak Mon–Fri
01:00–04:00 and 06:00–10:00 UTC except Chinese public holidays (table per year in the code; a year without one
assumes peak and sets `approximate`), half price otherwise; a cache write costs a cache miss. `PriceBook::with_override`
adds flat prices of other providers; a model without a price counts tokens only (`unpricedRequests`).

- `Tally`: `{requests, inputTokens, cacheReadTokens, cacheWriteTokens, outputTokens, reasoningTokens, costUsd,
  cost: {input, cacheRead, cacheWrite, output}, savedUsd, offPeakRequests, unpricedRequests, approximate}`
  (`inputTokens` is uncached input; `savedUsd` = cache hits priced as misses minus their price).
- `SessionStats`: `{currency: "USD", provider, model, turnCount, totals: Tally, timings: Timings, lastRequest:
  {seq, time, turn, provider, model, ...Tally} | null, turns: [{turn, start, end, status, ...Tally, routes:
  ["provider/model"], timings: Timings}], context: {contextWindow, usedTokens, estimated, percent, breakdown:
  {systemTokens, toolsTokens, messageTokens}, compaction: {auto, thresholdTokens, thresholdPercent, mode, turnEndTokens?} | null},
  subagents?: {sessions, ...Tally}}`. `usedTokens` = input and output of the last request (after a compaction the
  heuristic estimate, `estimated: true`); `breakdown` is dsh's four-characters-per-token estimate of the history.
  `compaction.thresholdTokens` is dsh's mid-turn pressure trigger; `mode` is the chat's compaction between
  turns (`threshold` with `turnEndTokens`, `every-turn`, or `off`, also whenever `auto` is false).
- `Timings` (dsh `sessionStats`): `{steps, llmMs, toolMs, ttftMs, ttftSteps, decodeMs, decodeTokens, avgTtftMs,
  tokensPerSecond}`: model time `step/start` → `assistant/message`, first token = first non-empty stream delta,
  decode = first token → message on steps with output tokens, tool time `tool/call` → `tool/result`.
- `UsageReport`: `{currency, since, until, totals: Tally, days: [{day: "YYYY-MM-DD", ...Tally, models: [ModelUsage]}],
  models: [ModelUsage], topChats: [{id, title, cwd, updatedAt, subagents, scratch, pinned, archived, unread, ...Tally}],
  sessions}`; `ModelUsage` = `{provider, model, priced, ...Tally}`; every day of the range has a row (at most 401);
  subagent sessions count for the chat they belong to. Hourly buckets per session live in
  `$DSHX_HOME/usage-index.json` with the listing's `[updatedAt, sizeBytes, eventCount]`, so a report re-reads only
  changed logs (on-disk logs trail a live session by up to 200 ms).

## 7. Conventions
- Rust 2024, stable toolchain in `~/.cargo/bin` (`export PATH=$HOME/.cargo/bin:$PATH`). `cargo fmt`,
  `cargo clippy -- -D warnings` clean for your crate. No `unsafe` except where unavoidable (pipe fds,
  `pre_exec` for process groups), each with a `// SAFETY:` comment.
- Libraries: `thiserror` for typed errors, `anyhow` at edges, `tracing` for logs. Prefer few, well-known
  dependencies; add them to your crate's `Cargo.toml` (workspace-level only if shared).
- Code comments and identifiers in English; comment density like the existing JS (short, why not what).
- **Build politely**: this server also runs production services. Always build/test with
  `nice -n 10 cargo ... -j 4`.
- Tests offline only. Use `tempfile` dirs; never touch `~/.dsh-codex` or `~/.dsh` except scratch
  sessions you delete again.
- Commit on your worktree branch with clear messages; do not push, do not merge. End commit messages with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
