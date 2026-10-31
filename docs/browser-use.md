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
reuse an existing installation. Configure **Always load** or activate it per
chat with `@huntproxy`. A connection in Codex/another agent is not inherited.

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

Settings → Context controls both turn-end compaction and intermediate work
rounds in PTC or an active Goal. Only completed model/tool batches form a
boundary; a running program is never cut in half. The newest full batch stays
verbatim. In every-round mode, 1,024 fresh compactable tokens are required for
PTC/Goals (8,000 at turn end for ordinary Standard) so tiny calls do not cause
repeated summary requests. Threshold mode respects its configured limit;
pressure recovery remains available even when extra compaction is off.

The append-only chat log, pending steering/queue, Goal status and round cap
remain intact. Goal context is restored from its canonical state after a
summary. Stop during compaction disarms automatic Goal continuation.
