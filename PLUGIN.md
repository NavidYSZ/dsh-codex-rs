# dsh-codex: plugin for DeepSeek Harness

[Deutsch](PLUGIN.de.md)

A plugin bundle for [DeepSeek Harness](https://www.deepseek.com/harness/)
(DSH). It does two things:

- It brings the DSH desktop app closer to the look of OpenAI's Codex app.
- It adds three macOS tools with a visible agent cursor:
  - `browser` drives its own Chrome in the background.
  - `app` operates one Mac app in the background through Accessibility.
  - `computer` takes over the whole screen for work across apps.

A floating mini window shows what the agent is doing. It has a live preview
and a Stop button.

> **New: dsh-codex as a standalone Rust agent (preview).** The same look and
> tools without DSH or Node.js, about 100× faster to start and with a tenth of
> the memory. See [README.md](README.md).

> Not affiliated with OpenAI or DeepSeek. "Codex" is a trademark of OpenAI.
> This project contains no Codex code or assets. Colors and sizes were
> measured from the app. The cursor, mini window and animations are drawn
> from scratch.

## Requirements

- **DeepSeek Harness desktop 0.2.x.** Tested with 0.2.0-rc.2 and
  0.2.1-rc.2. DSH is in preview, so its plugin API and the CSS hooks this
  bundle uses can change with any release.
- **macOS 14 or later on Apple silicon** for the tools. The look works
  wherever the DSH web UI runs.
- **Xcode Command Line Tools** (`xcode-select --install`) to build the
  helper.
- **For `browser`:** Google Chrome, Chromium, Brave or Microsoft Edge.
- **A model that accepts images** works best. Text-only models work from
  the element outlines alone.

## Install

1. Clone this repository and build the helper. This installs
   `~/Applications/DSH Computer Use.app`:
   ```sh
   git clone https://github.com/NavidYSZ/dsh-codex.git
   cd dsh-codex
   helper/build.sh --install
   ```
   The script signs the app with your first "Apple Development" identity,
   or with `DSH_CUA_SIGN_IDENTITY` if that is set. Without either it signs
   ad hoc, and macOS then asks for the permissions again after every
   rebuild.
2. In **System Settings → Privacy & Security**, allow "DSH Computer Use"
   under **Accessibility** and under **Screen & System Audio Recording**.
3. Quit DeepSeek Harness completely. The desktop profile belongs to the
   app, and the CLI needs the profile lock.
4. Add the bundle with the CLI that ships inside the app:
   ```sh
   "/Applications/DeepSeek Harness.app/Contents/Resources/runtime/cli/bin/dsh" \
     plugin --profile desktop add "$PWD"
   ```
5. Start DSH. **Plugins** now lists `@yousufzai/dsh-codex`.

**Only the look:** set `computerUse.enabled`, `apps.enabled` and
`browser.enabled` to `false` (see [Configuration](#configuration)). You then
need neither the helper nor the permissions.

**Web profile** (`dsh web`): use `dsh plugin --profile web add <path>`. On
Linux, `computer` and `app` are off. `browser` registers only when
`browser.chrome` points to a Chrome binary, and that setup is untested.

**Updating:** restart DSH after changes to `client.js`, `index.js` or
`lib/`. After changes to the helper, run `helper/build.sh --install`.

**Removing:**
1. Quit DSH.
2. Run `dsh plugin --profile desktop remove @yousufzai/dsh-codex`.
3. Delete `~/Applications/DSH Computer Use.app` and
   `~/Library/Application Support/dsh-codex`. That folder holds the socket
   and the browser profile.

## What it does

### Look (`client.js`)

- **Colors** come from `ctx.theme.overrideTokens`:
  - neutral greys instead of the blue-tinted DSH palette;
  - ink `#1a1c1f`, accent `#339cff`, backgrounds `#fff` / `#181818`.

  Light, dark and system themes still work.
- **Layout:**
  - 768 px reading width;
  - dark user bubble;
  - flat sidebar with a "DeepSeek" wordmark.
- **Composer:**
  - borderless, with a soft shadow and a 22 px radius;
  - round black send button;
  - model name in ink, effort level in grey;
  - "Full access" in orange.

  The stats line under the composer stays faint until you hover over it.
- **New session:** a grey DeepSeek whale, and the question "What should we
  work on in ‹workspace›?" The workspace name opens the picker. The
  composer sits at the bottom, with workspace and mode in a grey tray
  behind its top edge.
- **Transcript rows for the three tools,** for example "Browser · Click
  “Search” · en.wikipedia.org". Each row has a screenshot preview where the
  agent cursor glides to its target.

### Background browser (`lib/browser.js`, `lib/browser-tool.js`)

- **Actions:**
  - pages: `open`, `state`, `read`, `wait`;
  - input: `click`, `type`, `key`, `scroll`, `select`;
  - navigation: `back`, `forward`, `reload`;
  - tabs: `tabs`, `new_tab`, `switch_tab`, `close_tab`;
  - window: `show`, `hide`.
- **Own Chrome profile** in
  `~/Library/Application Support/dsh-codex/browser-profile`, separate from
  your Chrome. Cookies persist between uses.
- **Headless.** Driven over the DevTools protocol through a pipe, so no
  debugging port is open. You see no windows and no Dock icon, and focus
  never moves.
- **What the model sees:** a numbered outline of the visible controls, such
  as `[3] button "Search" @(x,y)`, plus a screenshot. `read` returns the
  page text.
- **`show` and `hide`:** `show` brings the browser up as a normal window
  with its tabs intact, so you can sign in. `hide` sends it back.
- **Off limits:** password fields are refused and downloads are blocked.
- **Idle exit:** Chrome quits after 15 idle minutes.

### Background apps (`lib/app-tool.js`, `helper/Sources/AppControl.swift`)

- **Actions:** `state`, `click`, `type`, `set_value`, `key`, `scroll`,
  `menu`, `action`, `list_apps` and `show`.
- **How it works:** it uses the Accessibility API (AXPress, AXValue, the
  menu bar). The app stays behind your windows, and your pointer and
  keyboard are left alone.
- **Screenshots** show only the app's window, even when other windows
  cover it.
- **Fallback:** where Accessibility offers no route, the tool sends events
  straight to the app's process. That can bring the app to the front, and
  the tool says so when it happens.
- **Off limits:** right-click menus are unavailable in the background, and
  password fields are refused.

### Mini window (`helper/Sources/Mini.swift`)

- **What it shows:**
  - a floating glass panel, at the bottom right until you drag it
    elsewhere;
  - the app icon, a title such as "Browser · en.wikipedia.org" or
    "Calculator", and the current step;
  - a live preview of the browser or app window, where the agent cursor
    glides to each target before it clicks.
- **Stop** halts the agent. Actions are refused for the next 20 seconds,
  and the model is told to ask you how to continue.
- **Preview and arrow:** clicking the preview brings the app or browser to
  the front. The arrow collapses the preview.
- **Behavior:**
  - shows "Done" after a few quiet seconds, then hides;
  - never takes focus;
  - excluded from screenshots and screen sharing.

### Foreground computer use (`lib/computer-tool.js`, `helper/`)

- **Actions:** `screenshot`, `click`, `double_click`, `right_click`, `move`,
  `drag`, `scroll`, `type`, `key`, `open_app` and `wait`. Use it for work
  across apps, drag and drop, and context menus.
- **Real mouse:** it moves your actual pointer. A click-through overlay
  draws the agent cursor, and a banner reads "DeepSeek is using your
  computer · Esc to cancel".
- **Esc** stops this mode only. In background mode you may be typing
  somewhere yourself.

### Guard

The helper refuses clicks, typing and app launches in several places:

- macOS permission, login and Touch ID dialogs;
- System Settings;
- Keychain Access;
- password managers (Passwords, 1Password, Bitwarden).

This applies to `computer` and `app`, whatever the model asks for.

## Configuration

Add a row to your profile's `cordis.patch.yml`. For the desktop profile that
file is `~/.dsh/profiles/desktop/cordis.patch.yml`. Keys you leave out keep
their defaults:

```yaml
- id: dsh-codex
  name: '@yousufzai/dsh-codex'
  config:
    computerUse:
      enabled: true          # false: no "computer" tool
      locale: en             # de or en; default: macOS language
      accent: '#339cff'      # glow, click ring, banner dot, mini cursor
      label: DeepSeek        # name tag on the cursor (showLabel)
      banner: DeepSeek is using your computer
      escHint: Esc to cancel
      showLabel: false       # Codex shows no name tag
      showGlow: false        # glowing screen edge
      showActivity: true     # "Clicking", "Typing …" next to the cursor
      clickPulse: true       # ring on click
      maxWidth: 1280         # screenshot width in pixels = coordinate space
      quality: 0.8           # JPEG quality
      blockedApps: []        # extra bundle IDs to refuse
    apps:
      enabled: true          # "app" tool (macOS only)
    browser:
      enabled: true          # "browser" tool
      chrome: /Applications/Google Chrome.app/Contents/MacOS/Google Chrome
      width: 1280            # viewport = coordinate space
      height: 800
      idleMinutes: 15        # Chrome quits after this
      quality: 0.7
    mini:
      doneAfter: 6           # quiet seconds before "Done"
      hideAfter: 24          # then hide
```

The interface text follows the language of the DSH window. Tool activity,
the banner and the mini window follow `computerUse.locale`.

## Safety

- **The tools act on your real Mac.** Read [Guard](#guard) for what the
  helper refuses. Review what the agent does, especially in a browser
  profile where you are signed in.
- **Data leaves your Mac.** Screenshots and page text go to your model
  provider, and DSH stores the screenshots as session attachments.
- **Content can carry instructions.** Web pages and app windows can try
  prompt injection. The tool descriptions tell the model to treat that
  content as untrusted, and to ask before it sends, buys, deletes or
  publishes anything. Models do not always comply.
- **Stopping:** click Stop in the mini window, or press Esc in foreground
  mode. Either one locks the tools for 20 seconds.

## Development

| Path | Purpose |
|---|---|
| `client.js` | Browser module: tokens, layout and composer CSS, start-page headline, transcript rows |
| `index.js` | Host plugin: registers `computer`, `app` and `browser`, stores screenshots as attachments |
| `lib/computer-tool.js` | Schema and mapping for foreground mode |
| `lib/app-tool.js` | Schema and mapping for background apps |
| `lib/browser.js`, `lib/browser-tool.js` | Background Chrome over a DevTools pipe, element outline, schema |
| `lib/cua-client.js` | Unix socket client; starts the helper when needed |
| `helper/` | Swift helper: overlay, mini window, ScreenCaptureKit screenshots, Accessibility, mouse and keyboard, Esc, guard |

`client.js` is written directly in DSH's browser module format, so there is
no build step.

**Helper socket:** the helper listens on
`~/Library/Application Support/dsh-codex/cua.sock`, with mode `0600` and one
JSON line per request. For debugging:

| Request | Effect |
|---|---|
| `{"cmd":"status"}` | Reports the helper's version and permissions |
| `{"cmd":"render_cursor","path":"/tmp/c.png"}` | Draws the cursor in several poses |
| `{"cmd":"guard_probe"}` | Shows what the guard would block right now |
| `{"cmd":"configure","mini":{"debugShareable":true}}` | Makes the mini window visible in your own screenshots |

**Transcript rows without a session:**
1. Run `localStorage['dsh-codex:preview'] = '1'` in the DSH window.
2. Reload.
3. Use `window.__dshCodexPreview`.

**Testing:** test in a separate instance with its own `DSH_HOME`, and with
no API keys in the environment.

```sh
DSH_HOME=/tmp/dsh-test env -u DEEPSEEK_API_KEY dsh web --no-open --port 4719
```

DSH reads `DEEPSEEK_API_KEY` from the environment. If a test instance has
the key and the tools are enabled, a test prompt reaches a real model, and
the agent acts on your Mac. Disable the three tools in the test profile.
Test them directly instead: import `lib/*-tool.js` from a Node script and
point it at harmless targets such as Calculator or example.com.

**After DSH updates:** part of the CSS hooks into module class names such as
`[class*="_composerHero"]` or `[class*="_heroWorkspaceRow"]`. Check those
after every DSH update.

## Limits

- `computer` moves the real mouse. Use `app` and `browser` for background
  work.
- `app` depends on each app's Accessibility tree. Where that tree is thin,
  as in games and some canvas apps, the tool falls back to coordinates and
  to events that can bring the app forward.
- Background apps often ignore ⌘ shortcuts. Use `menu` instead.
- The background browser is not your Chrome. It knows your logins only
  after you sign in once through `show`.
- Some sites detect headless browsers and show captchas. The agent does not
  solve them.
- The sidebar width cannot be set through the DSH API. Codex's icon rail
  on the far left is missing.
- With several displays, `computer` works on the display of its last
  screenshot. The mini window appears on the display with the pointer, or
  wherever you dragged it.

## License

[MIT](LICENSE)
