# Changelog

## 0.3.0-alpha.2 — 2026-10-31

- Add Settings → Browser Use: saved and active browser configuration, explicit
  restart status, an isolated local open/type/click/read diagnostic, and browser
  MCP connection/tool checks. Existing tabs, profiles and sign-ins stay intact.
- Refresh MCP tool handles and schemas between steps even when names do not
  change; restore instructions after delayed connections, reconnects and
  compaction. Bound cancellation-notification cleanup when a transport stalls.
- Document the distinct browser and MCP result shapes in PTC. Keep website work
  on the requested browser/profile; desktop interaction requires explicit user
  intent. Configure newer helper previews for both Aether and DeepSeek Harness.
- Add text pagination to browser `read`; long pages no longer repeat the same
  prefix after scrolling. Invalid explicit browser paths no longer silently
  select a different executable.
- Apply configured compaction between completed PTC/active Goal work rounds as
  well as at turn end. Every-round mode requires 1,024 new compactable tokens
  for PTC/Goals; ordinary Standard retains its 8,000-token floor. Preserve the
  newest tool batch, pending input and canonical Goal state. Stop during
  compaction disarms further Goal rounds.
- Add regression coverage for real browser interaction, dynamic MCP/PTC
  connections, queued input during compaction, cancellation and Goal counters.


## 0.3.0-alpha.1 — 2026-10-31

- Publish Aether as a standalone macOS app alongside DeepSeek Harness, with a
  clean local-only first-run configuration, bundled Node runtime, and explicit
  links distinguishing the old DSH plugin from this application.
- Add PTC mode: TypeScript `run_code`, generated tools SDK, concurrent safe
  tool calls and filtered results. Nested tools retain validation, approval,
  cancellation and session logging. Standard and Minimal keep their contracts.
- Fix prompt acknowledgement and FIFO queue ownership while a turn is running.
  Accepted input stays visible through steering/queueing, and pending input is
  preserved through recovery. Publish session events in sequence order.
- Keep composer text and attachments until acknowledgement; delayed responses
  cannot overwrite newer drafts or another chat. Show pending steering as well
  as queued follow-ups.
- Include the existing provider catalog and compatibility fixes for additional
  model providers, plus the prepared Aether visual identity.
- Add Linux/macOS CI and regression coverage for busy-chat messages and PTC.


## Earlier development (included in 0.3.0-alpha.1)

### Added

- **Codex-app sidebar:** Pinned / Projects / Chats sections, chats without a
  project, row menu and right-click, activity view behind the bell, ⌘K palette,
  archived chats in Settings. Sidebar state lives in `ui-state.json`
  (`PATCH /api/sessions/:id`).
- **Side panel:** Changes, Files, file previews (code, Markdown, CSV/TSV, JSON,
  images, PDF, media, sandboxed HTML) via `/preview/<grant>/…`; "N files
  changed" card under finished turns.
- **Live view** of the browser screencast and Mac tool steps, with Stop
  (`StreamMessage::Live` on its own channel).
- **Steer or queue** follow-ups while a turn runs (`prompt.steer`),
  notifications and a Dock badge hook for the Mac app.

- **Merged branches:** native Mac app with instances (`desktop/`), plugins,
  marketplaces and OAuth connectors (`dshx-plugins`, `dshx-mcp`), goals,
  schedules and Agent Teams (`dshx-goal`, `dshx-schedule`, `dshx-team`).
- **Plugins** and **Automations** pages, goal bar and goal form, Team tab,
  readable rows for schedule/goal/team tools, goal-round/reminder/Team-mail
  lines in the transcript.
- Prompts carry the browser's time zone (dsh `clientTimeZone`).
- **Modes Standard and Minimal** (dsh agent presets): a mode chip on the start
  page with a "When to choose which?" guide (English and German), the mode of
  a chat beside its title with "New chat in … mode", and the default mode in
  Settings. Minimal is dsh's Minimal preset: the fixed persona as the whole
  system prompt and one persistent `bash` shell, nothing else; its request
  equals the one the official dsh sends (`docs/rust/upstream/*-minimal-*`).
  Sessions record the mode in the header like dsh (`agentPreset`) and keep it.
  API: `POST /api/sessions {preset}`, `/api/info` `presets` and
  `settings.defaultPreset`; config `default_preset`; CLI `--preset`.
- **Work details** like DSH: Compact, Standard (default), Detailed and Verbose
  in Settings → Appearance and the chat's … menu, kept in `ui-state.json`.
  Reasoning and tool steps between answers fold behind one status line with
  the live activity ("Running commands · npm test", "Preparing to write
  files"), reasoning shows one line, and a finished turn folds into
  "Completed in 1m 20s · 12 tool calls · 3 messages · $0.0031".
- **Usage bar** under the composer: context ring (composition, compaction
  threshold), cache hit rate and cost in USD, each with details; the cost of
  each turn; Settings → Usage per day, model and chat. Prices are DeepSeek's
  list prices with peak and off-peak hours, computed by the server
  (`GET /api/sessions/:id/stats`, `GET /api/usage`, `usage-index.json`).
- **Project folders by dialog** instead of a path field: "Choose folder…"
  (⌘O, start page chip, sidebar "Add project", Mac app File → Open project)
  uses the Mac app's folder sheet for the local instance, the server's macOS
  dialog in a browser on the same Mac (`POST /api/fs/pick`), else a folder
  browser (`GET /api/fs/dirs`, `/api/fs/suggestions`, `POST /api/fs/mkdir`)
  with breadcrumbs, recent projects, repositories, new folder, edit path and
  hidden folders. Roots: home, the parents of known projects and
  `[folders] roots` in `config.toml`. Folders dropped on the Mac app become
  projects.
- **Mac app: switching instances without a grey screen.** Instances connect
  at launch ("Beim Start verbinden") into invisible, rendered pages; the
  window keeps the current page until the target reports `ready()`, then
  crossfades, showing the target's last snapshot meanwhile. Progress line,
  inline error banner, page-colored window background, status dots, unread
  counts and ⌃1–⌃9 in the switcher.
- **Model providers** besides DeepSeek: Anthropic from the built-in catalog
  and custom APIs speaking Anthropic Messages or OpenAI Chat Completions
  (`[providers.<id>]` in `config.toml`, keys per provider in
  `credentials.toml`, an environment variable wins). Models are referenced
  as `provider/model`; bare DeepSeek ids keep working and the DeepSeek wire
  stays byte for byte. Anthropic requests use `cache_control` breakpoints,
  adaptive thinking with effort, and replay thinking signatures only on the
  route that produced them; titles, compaction and subagents follow the
  chat's provider (Anthropic titles use Claude Haiku 5.5). Routes
  `/api/providers…`, `tools/dev/mock_openai.py` for offline tests.
- **Settings → Models:** provider cards with key status, "Add model
  provider" (catalog or custom API), "Fetch available models", model rows
  with context window, output cap, input types, reasoning levels and
  prices. The composer's model picker groups by provider, searches,
  lists recently used models first and shows context, image support and
  price; ⌃⇧M opens it.

- **Message actions** (DSH `message.branch`, Codex): copy, "Send again" and
  "Edit and send again" under prompts; copy and "Branch into a new
  conversation" under a finished turn's answer. `POST /api/sessions/:id/fork
  {turn}` copies the log through that turn behind dsh's inherited cut into a
  new chat in the same folder and mode (header `parentSession`); "Edit and
  send again" branches right before the prompt and puts it into the composer.
- **Failure notices per code** with the action that helps: invalid key →
  the provider's settings in Models, exhausted balance → the provider's top-up
  page (DeepSeek, Anthropic), rate limit, full context (`/compact` or a new
  chat), connection and timeout, refusal; "Send again" on the last turn. The
  output limit shows DSH's hint with "Continue", sent in the prompt's language.
- **Compaction** is readable: "Compacted 12 history items (~48,213 tokens)"
  with the summary to unfold, "Compacting context…" while it runs.
- **Long chats** render their latest 30 turns with "Load earlier"; slim turn
  markers on the right edge preview each prompt and jump to any turn.
  **Find in chat** (⌘F) highlights matches and loads the turns they are in.
- **Keyboard shortcuts** like Codex: ⌘1–9 open the chats in sidebar order,
  ⇧⌘[ / ⇧⌘] (⌃⇧Tab / ⌃Tab) previous and next chat, ⌥⌘A the next chat that
  needs you, ⇧Esc mark all read, ⇧⌘A archive, ⌥⌘P pin, ⌥⌘R rename, and ⌘/ an
  overview; the Mac app has a Chat menu, "Find in chat" and Help → Keyboard
  shortcuts for them.
- **Full access asks first** (DSH `confirm.*`) in the composer and Settings,
  with "I understand the risks" and "Don't ask again" (`ui-state.json`).
- **Sidebar view options** (DSH workspace): group by project or one list,
  order by last activity or creation, hide, show or only archived chats.
- **`/` and `@` in the composer** (DSH input triggers): `/` lists the
  commands that work there (`/compact`, `/goal`, `/model`, `/permission`,
  `/status`, `/view`, `/export`) and the skills, `@` the MCP servers, plugins
  and the project's files and folders (gitignore-aware, Tab opens a folder),
  ranked prefix › substring › fuzzy. Picks become pills in the text and in
  the sent bubble; the text stays plain (`/name`, `@server`, `@path`).
  `GET /api/composer/suggest`.
- **Skills and MCP servers on demand:** with `[skills] catalog = "manual"`
  (default) the model sees no skill catalog and no `skill` tool; `/name` or
  `$name` loads a skill into that turn. Per skill `[skills.entries."name"]
  enabled` and `model`. MCP servers join a chat only through `@server`,
  `@plugin` or the active-server chips ("Active: linear ✕"): deferred
  `tool-addition` plus a note with the server's instructions, so DeepSeek's
  cached prefix stays valid; `always = true` and `disabled_tools` per server.
  Routes: `GET/POST/DELETE /api/sessions/:id/mcp…`,
  `PUT /api/skills/:name/settings`, `PUT /api/mcp/servers/:name/settings`.
- **`/compact`** and "Compact now" in the context popover
  (`POST /api/sessions/:id/compact`, dsh `compactNow`); **`/export`** downloads
  the session log (`GET /api/sessions/:id/export`).
- **Composer parity:** ↑ in an empty composer brings back this chat's last
  prompts; any file can be attached (stored read-only, referenced as `@path`,
  `POST /api/files`); a folder dropped from the project becomes an
  `@folder/` reference; queued prompts show above the composer, editable,
  deletable or steered one by one, ⌘↩ on an empty composer steers them all
  (`POST /api/sessions/:id/queue/:item`); DSH's image limits and texts
  (count, size, model without image input).
- **Skills page:** search, filters (All · Project · Personal · Plugins ·
  System), readable titles in the system font with `/name` as a hint, the
  source, "Active" and "Let the model load it" switches (need the per-skill
  settings route), invalid frontmatter as a warning in the row, and a drawer
  with the rendered `SKILL.md`, the folder's files, the path, "Show in
  Finder"/"Open in editor" (Mac app, local instance) and "Use in chat". "New
  skill" starts a chat with `skill-creator` or creates a blank `SKILL.md` in
  `~/.agents/skills` or the project (`GET /api/skills/:name`,
  `POST /api/skills`).
- **MCP servers page:** cards with type, status, tool count, "Active" and
  "Always load"; a drawer with Tools (per-tool switches, parameters and JSON
  schema), Settings, Log (last 200 lines of stderr, failed requests and
  connection events) and Sign-in (account, scope, expiry, sign in again or
  out, hand-entered token). The Apps tab (formerly Connectors) opens the same
  drawer.
- **Add server dialog** instead of the one-line input: from the catalog, a
  custom server (Local STDIO: command, arguments, masked environment values,
  variables to pass through, working folder; Remote HTTP: URL and None /
  Bearer token / OAuth / custom headers) with **Test connection**
  (`POST /api/mcp/test`: handshake and `tools/list` in a throwaway client),
  or pasted JSON from Claude, Cursor, VS Code, Codex or a README (several
  servers as a checklist). Environment values, header values and bearer
  tokens are stored in `credentials.toml`, never in `config.toml`; servers
  can be edited (`PUT /api/mcp/servers/:name`) and their log read
  (`GET /api/mcp/servers/:name/log`). `[mcp_servers.x] env_vars` passes
  credential-like parent variables through on purpose.
- **Plan above the composer** (dsh TodoPanel): while a turn works through a
  `todo_write` list, a bar shows the item in progress and "1 of 3 done" and
  unfolds into the checklist; like dsh it stays while items are left, and a
  finished list goes when its turn ends. A finished turn's summary adds
  "Plan 2/3". Turns that a finished background job starts show a "Background
  job" line like goal rounds and reminders.

- **Compaction between turns** (Settings → Context, `[codex.compaction]`): a
  finished turn condenses older history once the context reached a limit
  (200K tokens by default, 100K–500K in the UI) or after every turn that added
  at least about 8,000 tokens; `off` keeps DSH's behaviour. It runs while the
  provider still caches the prefix, prompts sent meanwhile wait for it, Stop
  cancels it, subagents and `dsh-codex "<task>"` skip it. The checkpoint
  carries the open `todo_write` list. The context ring fills up to the limit
  and links to the setting; `/api/info` `settings.compaction`, stats
  `context.compaction.mode` and `turnEndTokens`.

### Changed

- DSH's pressure compaction summarizes before it prunes oversized tool
  results: pruning first rewrote the prefix from the first such result on, so
  the summary request missed DeepSeek's cache. Now the summarizer replays the
  cached prefix and only the kept tail is pruned; pruning alone still runs
  when nothing can be summarized.
- The summary request takes the conversation's reasoning effort instead of
  the adapter default. DeepSeek's prompt prefix depends on it: measured with
  a 19K-token chat at `max` (and at `off`), the default-effort summary reused
  384 cached tokens, the same-effort one 98 % of them.
- The system prompt carries DSH's file-reference guidance; MCP server
  instructions moved out of it (only `always` servers keep theirs there), so
  existing chats see one prompt-cache rebuild on their next request.
- Image uploads accept the documented 20 MB (the server's body limit was 2 MB).

- Edit diffs stay closed in the transcript in the Compact and Standard views
  (the "N files changed" card and the Review panel still show them).
- Mac app: the local instance reuses its last port, so browser storage
  (sidebar, theme, panel) survives restarts.
- Mac app: instances switch with ⌃1–⌃9 (⌘1–⌘9 stay free for chats); the SSH
  tunnel is a detached master with `ControlPersist=10m` that survives a quit
  for a quick restart, and the access token is reused until a 401.
- Codex app default theme values: accent `#0285ff`, grey user bubble, 16 px
  composer and bubble radius, 46 px header.
- Debug builds serve `ui/` with `no-cache`.
- macOS: an explicit `$DSHX_HOME` gets its own `browser-profile` instead of
  sharing the plugin's; `[codex.browser] mockKeychain` /
  `DSHX_BROWSER_MOCK_KEYCHAIN=1` start Chrome with `--use-mock-keychain`, which
  the Chrome tests now always do, so they never prompt for the login keychain.
- DeepSeek models show their list prices like the other providers: the model
  picker, Settings → Models and the default model in Settings give the peak
  price per 1M tokens and note that off-peak hours cost half (`/api/info`
  and `/api/providers` rows carry `cost` and `offPeakCost` from the usage
  price book). Prices use the same currency format as the usage bar.
- **Settings in sections** like the Codex app: General, Appearance, Models,
  New chats, Usage, Archived chats and About each have their own pane at
  `#/settings/<section>`, behind a navigation rail on wide pages and a
  scrolling tab row on narrow ones (the page's width decides). Links into
  Settings ("Manage models…", a missing key, "All usage", a provider's
  failure notice) open the right pane. The Plugins page's tabs are
  deep-linkable the same way (`#/plugins/skills`, `#/plugins/mcp`), and
  "Manage skills" / "Manage apps" in the composer menus open their tab.
- **"Add marketplace" is a dialog** like "Add server": the source (GitHub
  `owner/repo`, a Git URL or a local folder) with a live note on what will
  be cloned or read, an optional branch or tag, the accepted forms, progress
  while the repository is cloned and errors in plain words (already added,
  no catalog, repository not found or private). The Plugins tab gets the
  button in its toolbar, and empty marketplaces say so.
- One term per concept in the copy: German says "Tools" (no more
  "Werkzeuge"), "Ziel" for goals, "Denkstufe" for the reasoning level and
  "entfernen" for providers like plugins and servers; English says "chat",
  never "conversation". Settings switches read "On"/"Off". Usage shows
  model and provider names (DeepSeek-V4.1-Flash · DeepSeek) instead of ids.
- Removing a chat, plugin, marketplace, MCP server, goal or automation asks
  in an in-app dialog (Cancel focused, Esc cancels) instead of the browser's
  `confirm()`; "…" menus close when an item runs.

### Fixed

- Settings → About showed "Tools" instead of "Mac tools": the About label had
  the same i18n key as the MCP tools count (now `macTools`).

- Image-heavy chats (many browser screenshots) no longer end with
  `IMAGE_OFFLOAD_REQUIRED`: like dsh `compaction-image-offload`, the agent
  logs `image/offload`, sends the oldest images as text with their read-only
  path and retries without spending the retry budget. Compaction summaries
  and the tool-result pruner keep those marks; resumed chats keep them too.
- A `/skill` or `@server` pill in a sent prompt (and an active-server chip)
  opens that skill's or server's drawer on its Plugins tab instead of only
  the Plugins page; an `@plugin` pill highlights the plugin's card, and a
  skill or server that is gone says so.
- `dsh-codex web` (and with it the Mac app's instances) no longer waits up
  to a minute for an MCP server that hangs in its handshake: after 3 s it
  serves the UI and slow servers keep connecting in the background, shown
  as "Connecting…". `run` and `sdk` still wait like dsh before the first turn.
- Stopping or timing out a bash command stops the script: the shell itself
  is killed at once (bash survives a SIGTERM that arrives while it forks and
  then ran its next command), while the commands it started still get
  SIGTERM and the grace period. The bash tool's kill tests are no longer
  flaky.
- Changing a setting in the UI (skills, MCP switches, servers, providers,
  defaults) edits only the affected keys of `config.toml`: comments, order
  and formatting stay, and defaults that were left out are not written.
- A model price left blank in Settings → Models is unknown, not free: the
  form no longer saves blank cache prices as 0, and cache reads and writes
  without a price are counted like uncached input (`[providers.*.models]`
  `cost.cache_read` / `cache_write` are optional).
- Composer and chat: a prompt sent while a turn runs is no longer also shown
  as a pending bubble; each chat keeps its own unsent draft; Minimal mode's
  `/` and `@` menus offer only what works there (`suggest?preset=`); a model
  whose provider was removed is marked unavailable with "Choose model";
  `@server` inside inline code or code fences is text; a queued message that
  starts while being edited hands the edit back to the composer.
- Failures: no raw provider text above the composer when the failure card
  shows; a full context offers "Compact now" and "Send again".
- Focus returns to the opener or the composer after menus, the find bar,
  renames, queue edits and dialogs; the modes guide takes the focus and Esc.
- Usage: past costs keep the price of their time when a provider's prices
  change or the provider is removed (`$DSHX_HOME/prices.json`); tokens per
  second are left out for instant streams; reasoning shows its unit.
- Branches sort by when they were made and get numbered titles; the
  Verbose view keeps finished reasoning open; the `/` menu uses the skill
  title from the frontmatter; folder errors say how to allow more roots;
  inputs no longer spell-check paths and keys; the Skills and MCP pages stop
  probing their settings routes; ⌘K reaches the new pages; archiving offers
  Undo; German copy for the skill template, catalog texts and loader errors.

## 0.2.0 – 2026-10-17

First public release.

### Added

- **Codex-style composer.** Model name in ink, effort level and chevrons in
  grey, "Full access" in orange, a larger "+" button. The stats line stays
  faint until hovered.
- **New-session start page.** A grey whale mark and "What should we work on
  in ‹workspace›?" sit in the middle. The workspace name opens the picker.
  The composer sits at the bottom, with workspace and mode in a grey tray
  behind it.
- **`browser` tool.** Headless Chrome with its own profile, driven over
  `--remote-debugging-pipe`. Highlights:
  - numbered element outlines, screenshots and page text;
  - tabs, `show`/`hide` for signing in, popup handling;
  - downloads blocked, password fields refused, idle exit after 15 minutes.
- **`app` tool.** Operates one app window in the background through
  Accessibility. Highlights:
  - AXPress, AXValue, insertion at the caret, menu bar and AX actions;
  - window-only screenshots, even when the window is covered;
  - falls back to events posted straight to the app's process;
  - window chrome buttons, right-click menus and password fields are
    excluded.
- **Mini window.** A floating glass panel (`NSGlassEffectView` on macOS 26)
  with:
  - title, current step, live preview and agent cursor;
  - Stop, collapse and drag;
  - exclusion from screenshots.
- **Transcript rows for `app` and `browser`,** with a preview and the name
  of the target element.
- **Language follows macOS** (German or English). Set `computerUse.locale`
  to override it.

### Changed

- **Helper 0.2.0:**
  - new `app_*` and `mini_*` socket commands;
  - Stop in the mini window locks all actions for 20 seconds, as Esc does;
  - Esc acts only in foreground mode.
- The `computer` tool description points the model to `browser` for
  websites and to `app` for single apps.
- `presentationMeta` now carries `tool`, `place` (app or host) and `target`
  (element name).
- New config sections: `apps`, `browser` and `mini`.
- `lib/cua-client.js` has a `launch: false` option, so preview images do
  not start the helper.
- Defaults are English. The bundle's own `cordis.patch.yml` sets no
  strings.

### Tested

On macOS 26 with Apple silicon:

- **DSH 0.2.0-rc.2:**
  - `app` with Calculator: background launch, 7 × 6 = 42 through AXPress,
    menu listing, Escape;
  - `browser` with example.com and a Wikipedia search, using a throwaway
    profile;
  - the mini window, the tool schemas against DSH's validator, and light
    and dark themes.
- **DSH 0.2.1-rc.2:** in an isolated web instance, the bundle loads, and the
  start page, composer and plugin page render without console errors.

### Known gaps

- **No end-to-end run with a live model yet.** Untested: how the model
  picks tools, outlines inside a real conversation, and the mini window
  during a real session.
- **`app` is only tested with Calculator.** Apps with web content
  (Electron, Safari) and text insertion through `AXSelectedText` are
  untested.

## 0.1.0 – 2026-10-17

Not published.

- Codex colors, layout, "DeepSeek" wordmark and a flat sidebar.
- `computer` tool with:
  - the Swift helper, overlay cursor and banner;
  - Esc to cancel;
  - a guard for permission dialogs, System Settings and password apps.
