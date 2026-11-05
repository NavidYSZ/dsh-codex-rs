# Browser Use

Aether offers its own background Chrome-family browser and can connect browser
MCP servers such as [HuntProxy](https://github.com/BehiSecc/HuntProxy). Each
backend has its own profiles and accounts. Selecting another backend does not
transfer cookies, tabs or proxy capture.

## Built-in browser

Open **Settings → Browser Use** on the intended instance. Leave the executable
field blank for automatic detection, or enter the absolute executable path of
Chrome, Chromium, Brave or Edge. An invalid explicit path is an error rather
than an instruction to select another browser. Enable/disable and path changes
take effect when that instance restarts; finish active tasks before restarting.

The pane shows saved and active settings separately. **Test browser** uses the
saved executable, a temporary profile and a temporary loopback HTTP page. It
verifies Chrome/DevTools startup, page navigation, typing, clicking and reading,
then closes its browser and removes the profile. It does not touch production
tabs or logins and does not test a particular website. Browser control over CDP
does not require macOS Accessibility or Screen Recording; those permissions
belong to the optional native app/computer controls.

The persistent profile path shown in settings is separate from ordinary Chrome.
For a website login, ask Aether to show that browser, sign in there, then resume.

## The user's own Chrome

`browser` takes `browser: "user"` to work in the user's running Google Chrome
(Chromium, Brave or Edge) instead of the background browser: their tabs,
cookies and sign-ins, no second window. The model is told to use it whenever
the user asks for “my Chrome”, “the open Chrome”, their logged-in browser or
an open tab, and the choice sticks for the chat until `browser: "background"`
is passed.

How it attaches: Chrome 136+ no longer accepts `--remote-debugging-port` for
the default profile. Instead the user allows remote debugging once in
`chrome://inspect/#remote-debugging` (“Allow remote debugging for this browser
instance”); Chrome then writes its DevTools port and browser WebSocket path to
`DevToolsActivePort` in the profile directory, which Aether reads
(`~/Library/Application Support/Google/Chrome` on macOS, `~/.config/google-chrome`
on Linux, plus the Chromium, Brave and Edge equivalents). Chrome 144+ may show
“Allow remote debugging?” once per browser run when Aether connects; the
handshake waits up to 60 s for Allow. Chrome 147+ answers 404 on
`/json/version` for the default profile; the WebSocket path from the file is
used then.

Behaviour in the user's browser:

- The first action without a tab of its own attaches to the tab the user is
  looking at (`document.visibilityState`), so `state` and `read` describe it.
- `open` on a tab the user opened takes a new tab (Chrome brings it to the
  front); `open` on a tab Aether opened navigates in place. `new_tab`,
  `switch_tab` and `close_tab` work as usual; DevTools and extension pages
  are hidden from the tab list.
- `show` activates the current tab; `hide` does nothing. Downloads, device
  metrics and the user's settings are not changed.
- The idle timeout and host shutdown only detach; the user's Chrome keeps
  running.
- Errors name the fix (allow remote debugging, click Allow, reopen Chrome);
  the model must relay them instead of silently starting the background
  browser or desktop control.

Settings → Browser Use → **Your own Chrome** shows whether a running browser
allows remote debugging and whether Aether is attached right now
(`GET /api/browser/status` → `userChrome`).

PTC example:

```ts
const page = await tools.browser({action: "state"});
console.log(page.summary);
const text = await tools.browser({action: "read", maxChars: 12000});
console.log(text.summary);
// A truncated result supplies the next offset for another read.
```

Use element indices from the latest returned state. Text pagination offsets are
UTF-16 characters; `maxChars` ranges from 2 to 40,000. Scrolling changes the
viewport, not the text-read offset.

## HuntProxy

Install HuntProxy using its own documented installation, then add a Local STDIO
server in **Plugins → MCP servers**. Use the absolute HuntProxy executable and
arguments `mcp`, or `--data-dir /absolute/path/to/your/huntproxy-data mcp` to
reuse an existing installation. Turn on **Preselected in new chats**, or pick it
per chat under **Connectors** in the composer or with `@huntproxy`. A
connection in Codex/another agent is not inherited.

Equivalent Aether configuration (replace the executable/data paths):

```toml
[mcp_servers.huntproxy]
command = "/absolute/path/to/HuntProxy"
args = ["--data-dir", "/absolute/path/to/huntproxy-data", "mcp"]
enabled = true
always = true
tool_call_timeout_ms = 180000
```

Browser Use shows its connection state and available browser tools. **Test
connection** verifies MCP initialization and tool discovery; it does not open
a HuntProxy browser or claim that website interaction succeeded. Manage tool
availability, timeouts and server logs through the MCP details panel.

In PTC, call the offered `tools.mcp__huntproxy__…` bindings directly. MCP returns
`{content: [...], structuredContent?: ...}`. Use structuredContent when present;
otherwise inspect text blocks and parse them only when they contain JSON.
Built-in `browser` results instead contain readable text in `summary`.

Keep the intended project/profile/account. Use `browser_manage` at lifecycle
transitions, `browser_start` to resume the matching profile, and the returned
session ID for actions. A run_code program has a fixed set of bindings; newly
connected or changed tools are picked up at the next round. Its overall
timeout includes nested calls and approval waits: keep programs bounded and
choose a suitable `timeoutMs` for several sequential calls.

## Diagnose the actual failure

- **No MCP server/tools:** configure or activate the server in Aether. Inspect
  connection status, disabled tools and the server log.
- **Website navigation:** use the browser's navigation action. For HuntProxy,
  `browser_action` with `type: "navigate"` navigates a page normally.
- **HuntProxy request failure:** `type: "request"` fetches from the active
  page's same origin with browser policies intact and redirects disabled.
  Its generic network error mentions disabled redirects even when another
  network/CSP/proxy problem caused it. Inspect page origin, events and history;
  do not infer a redirect from that message alone. Do not automatically replay
  a state-changing request or disable protections to suppress the error.
- **Missing login or Access Denied:** verify the selected profile and access.
  A successful local diagnostic does not grant website authorization.
- **Stale element index:** request a fresh state and use its current indices.
- **Long text repeats:** follow the returned read offset instead of scrolling
  and requesting the first page of text again.

Keep website work on the selected browser. Browser failures do not authorize
switching to foreground computer control; that requires explicit desktop
interaction intent. Native desktop tasks remain available through app/computer
tools when appropriate.

## Compaction between rounds

Settings → Context applies after a complete answer, including all of its tool
calls, in PTC and active Goals as well as Standard. A tool call, `run_code`
program or model step is not a round. The latest answer stays verbatim, and
the next automatic Goal round starts after any summary finishes. In
`every-turn` mode, 1,024 fresh compactable tokens are required for PTC/Goals
(8,000 for ordinary Standard). Threshold mode checks its configured limit at
the same completed-answer boundary. Delegated subagents and one-shot CLI
runs skip final checkpoints. Context-pressure and overflow recovery remain
available during a long answer, even when extra compaction is off.

The append-only chat log, pending steering/queue, Goal status and round cap
remain intact. Goal context is restored from its canonical state after a
summary. Stop during compaction disarms automatic Goal continuation.
