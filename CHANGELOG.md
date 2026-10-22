# Changelog

## Unreleased (branch `codex-app`)

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

### Changed

- Edit diffs stay closed in the transcript in the Compact and Standard views
  (the "N files changed" card and the Review panel still show them).
- Mac app: the local instance reuses its last port, so browser storage
  (sidebar, theme, panel) survives restarts.
- Codex app default theme values: accent `#0285ff`, grey user bubble, 16 px
  composer and bubble radius, 46 px header.
- Debug builds serve `ui/` with `no-cache`.
- macOS: an explicit `$DSHX_HOME` gets its own `browser-profile` instead of
  sharing the plugin's; `[codex.browser] mockKeychain` /
  `DSHX_BROWSER_MOCK_KEYCHAIN=1` start Chrome with `--use-mock-keychain`, which
  the Chrome tests now always do, so they never prompt for the login keychain.

### Fixed

- Image-heavy chats (many browser screenshots) no longer end with
  `IMAGE_OFFLOAD_REQUIRED`: like dsh `compaction-image-offload`, the agent
  logs `image/offload`, sends the oldest images as text with their read-only
  path and retries without spending the retry budget. Compaction summaries
  and the tool-result pruner keep those marks; resumed chats keep them too.

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
