# Aether — mobile app, device pairing and accounts (contract)

Status: draft for implementation, 2026-10-25. This file is the contract between
four work packages that are built in parallel: the Rust server changes (P1), the
accounts hub (P2), the iOS app (P3) and the brand package (P4). Anything two
packages must agree on is written here; everything else is up to the package.
The existing contract between the crates and the web UI is
[docs/rust/SPEC.md](../rust/SPEC.md); its §6 (UI ↔ server protocol) is the
protocol the iOS app speaks and stays unchanged except for the additions in §3.

## 1. Product and brand

- **Product name: Aether.** Earlier attempts were called Zeus and Oxygen and
  shipped iOS apps under those names; Aether is the definitive one. The name
  joins both lineages: Aether is the upper air of the Greek gods (Zeus) and the
  classical fifth element (Oxygen); "über den Äther" is how things reach every
  device. Runner-up names, kept here in case the user prefers one: Talos (the
  first automaton), Proteus.
- **Scope of the name.** User-facing surfaces carry Aether: the iOS app, the
  accounts hub, the web UI's title/mark/favicon, the macOS app's display name
  and icon. Crate names (`dshx-*`), the binary (`dsh-codex`), config paths
  (`~/.dsh-codex`), the macOS bundle id (`de.yousufzai.dsh-codex`) and the
  systemd units on the VPS do **not** change in this round (a rename plan goes
  into `brand/BRAND.md`). Docs may say "Aether (dsh-codex)".
- **Identifiers.** iOS bundle id `de.yousufzai.aether`; URL scheme `aether://`;
  hub binary `aether-hub`; Bonjour type `_aether._tcp` (optional, not used yet).
- **Vocabulary.** A *server* is a machine running `dsh-codex web` (the Mac, VPS 1,
  VPS 2). In the iOS app the user calls those **Geräte / devices**. A *client*
  is a phone paired with a server. The hub calls the user's machines *servers*.
- **Design tokens** live in `brand/colors.json` (P4 writes it; P3 and the web UI
  read it). Token names are fixed, values may be refined by P4. Provisional
  values until P4 lands:

| Token | Light | Dark | Use |
|---|---|---|---|
| `accent` | `#5B7CFA` | `#8AA2FF` | buttons, links, selection, the mark's glow |
| `accentSoft` | `#E8EDFF` | `#1D2240` | tinted backgrounds, chips |
| `online` | `#15A37A` | `#5EEAD4` | status dots: ready |
| `warning` | `#D98B00` | `#F5B544` | waiting for approval |
| `danger` | `#D64545` | `#FF7B7B` | errors, destructive actions |
| `bg` | `#FAFAFC` | `#0E0F14` | window background |
| `surface` | `#FFFFFF` | `#161822` | cards, sheets |
| `surfaceRaised` | `#F2F3F8` | `#1E2130` | rows, inputs |
| `border` | `#E3E5EE` | `#2A2E40` | hairlines |
| `text` | `#121420` | `#F2F3F8` | primary text |
| `textMuted` | `#6B7085` | `#9AA0B8` | secondary text |

  Typography: the system font (SF on Apple platforms, the UI's existing stack on
  the web); the wordmark is the system font at weight 650 with tight tracking,
  like the Oxygen package did. No external fonts.

## 2. Connectivity: how the phone reaches a server

Every server keeps its loopback listener exactly as today
(`127.0.0.1:<port>`, start token, Host header must be loopback). The phone
reaches it over one of these **transports**, tried in the order the pairing
payload lists them:

| kind | What it is | Typical use |
|---|---|---|
| `https` | A second listener of `dsh-codex web` with TLS (self-signed, pinned by SHA-256 of the DER certificate) on all interfaces, same router and tokens, Host check off. | The Mac on Wi-Fi or over Tailscale; any server with a reachable port. |
| `http` | Plain HTTP to a URL the app can reach. Only loopback hosts (`127.0.0.1`, `localhost`, `[::1]`) are allowed; the app refuses other hosts. | The simulator talking to a dev server; the local end of an SSH tunnel. |
| `ssh` | The app opens an SSH connection (ed25519 key, pinned host key) and forwards a local port to `remoteHost:remotePort` (direct-tcpip). URLSession then talks plain HTTP to `127.0.0.1:<local>` with Host `127.0.0.1`, which the server's loopback check accepts. | VPS 1 and 2 (only port 22 is open). Works without any change on the VPS. |
| `hub` | Through the accounts hub's relay (§5). Base URL `<hub>/v1/relay/<serverId>` with the hub session as bearer. | Later: the Mac from outside the home network. Implemented, not deployed. |

Credentials, in order of preference:

1. **Client token** (§3.1): a long-lived token the server issued for this
   phone. Carried in `Authorization: Bearer …` (HTTP) and `?token=` (WebSocket)
   exactly like the start token.
2. **Token command** (`ssh` only, fallback for servers that run a build without
   §3): the app runs `tokenCommand` over an SSH exec channel; its stdout is the
   bare start token or the start link (`…?token=<hex>`), last matching line
   wins. The app re-runs it only after a `401`. This is what the Mac app does
   today (`tokenCommand` in `instances.json`).

Both VPS run `dsh-codex web --no-open --port 3080` as unit `deepseek-harness`,
user `deepseek`, `DSHX_HOME=/var/lib/dsh-codex`, CLI `/usr/local/bin/dsh-codex`;
`ubuntu` has passwordless sudo. VPS 1 is also on Tailscale with
`https://oracle-vps-01.tail9ade44.ts.net` (tailnet only) proxying to
`127.0.0.1:3080` with Host `127.0.0.1`, a valid certificate: that is an `https`
transport without a pin. The Mac is `100.79.232.37` on the same tailnet.

### 2.1 Pairing payload

A pairing is a JSON document, carried as `aether://pair#<base64url(JSON)>`
(no padding) in a QR code, a link or pasted text. The app also accepts the bare
base64url text and the bare JSON. Secrets live only here and in the server's
client list; the link is shown once and never logged.

```json
{
  "v": 1,
  "server": {"id": "srv_8f1c…", "name": "MacBook Pro", "hostname": "mbp-480",
             "platform": "darwin", "version": "0.3.0-alpha.0"},
  "client": {"id": "cli_2b7e…", "name": "iPhone"},
  "token": "act_…",
  "transports": [
    {"kind": "https", "urls": ["https://192.168.178.20:7443", "https://100.79.232.37:7443"],
     "certSha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"},
    {"kind": "http", "urls": ["http://127.0.0.1:3190"]},
    {"kind": "ssh", "host": "130.162.241.44", "port": 22, "user": "ubuntu",
     "hostKeySha256": "SHA256:…", "privateKey": "-----BEGIN OPENSSH PRIVATE KEY-----\n…",
     "remoteHost": "127.0.0.1", "remotePort": 3080,
     "tokenCommand": "sudo -n journalctl -u deepseek-harness -b -o cat --no-pager | grep -oE 'token=[0-9a-f]+' | tail -n 1 | cut -d= -f2"},
    {"kind": "hub", "url": "https://hub.example", "serverId": "srv_8f1c…"}
  ]
}
```

- `server.id` is stable per server (`$DSHX_HOME/server.json`, §3.3); pairing the
  same server again replaces the app's entry instead of adding one.
- `token` is optional (absent when only `tokenCommand` is available).
- `transports` has at least one entry; unknown kinds are skipped.
- `https.urls`: every address the listener is reachable on, LAN first, then
  Tailscale (`100.64.0.0/10`), then others. `certSha256`: lowercase hex, 64
  chars, of the DER certificate; the app pins it and ignores the chain.
  When `certSha256` is absent the app validates normally (Tailscale, a real
  certificate).
- `ssh.privateKey`: OpenSSH PEM of an ed25519 key made for this phone;
  `hostKeySha256` in `ssh-keygen -l` form (`SHA256:` + base64, no padding);
  `tokenCommand` optional.

### 2.2 What the user does

- **Mac:** Settings → "Aether App" in the web UI (or the Mac app) → switch on
  the mobile listener → scan the QR with the iPhone (Camera app or the Aether
  app's scanner), or "Link kopieren" and send it to the phone. Same Wi-Fi or
  Tailscale.
- **VPS, from the Mac:** `dsh-codex pair --ssh oracle-vps-02 -F
  /Users/yousufzai/Documents/Navid/OracleVPS/ssh_config --name "VPS 2"` makes a
  key for the phone, installs it restricted on the VPS, creates a client token
  on the VPS when its build has §3 (else falls back to `tokenCommand`), and
  prints the QR in the terminal.
- **Any server, in a terminal:** `dsh-codex pair --name "iPhone"` creates a
  client token and prints the QR (https transport when the listener is on).
- The app: "Gerät hinzufügen" → scan / paste link / enter by hand; a device
  appears with a status dot and its chats.

## 3. Server additions (P1, `crates/dshx-server`, `crates/dsh-codex`, `crates/dshx-core`, `ui/`)

### 3.1 Clients and tokens

- `$DSHX_HOME/mobile/clients.json` (dir 0700, file 0600):
  `{"version":1,"clients":[{"id":"cli_…","name":"iPhone","platform":"ios","tokenSha256":"<hex>","createdAt":<ms>,"lastSeenAt":<ms>|null,"pairedVia":"ui"|"cli"|"ssh"}]}`.
- Tokens: `act_` + 32 random bytes base64url (no padding). Only the SHA-256 is
  stored. The middleware accepts the start token **or** any client token
  (constant-time compare of hashes); a client token that authenticates updates
  `lastSeenAt` at most once a minute. The server re-reads the file when its
  mtime changes (the CLI writes it while the server runs).
- Routes (bearer, under `/api`):

| Method | Path | Body → Response |
|---|---|---|
| GET | `/api/mobile` | → `{enabled, port, configuredPort, addresses:[{ip, interface, kind:"lan"\|"tailscale"\|"public"\|"loopback"\|"other"}], certSha256, server:{id, name, hostname, platform, version}, clients:[Client], sshPair:{user, home, bin, port, unit}}`; `addresses` and `certSha256` only while the listener runs; `port` is the listening port while it runs (else the saved one), `configuredPort` always the saved one (0 = a free port at every start); `sshPair` is what `dsh-codex pair --ssh` needs about this server (`--remote-user`, `--remote-home`, `--remote-bin`, `--remote-port`, `--unit`), each `null` when unknown |
| PUT | `/api/mobile` | `{enabled?, port?, name?}` → same shape; starts or stops the TLS listener at once and writes `[codex.mobile]`; a port in use → 400 |
| POST | `/api/mobile/pair` | `{name?, transports?:["https","http"]}` → 201 `{client:Client, token, payload, link, qrSvg}`: the token appears exactly once; `payload` is §2.1 with the `https` transport when the listener runs (and `http` with the loopback URL when `transports` asks for it: dev only); `qrSvg` is an SVG string of `link` |
| PATCH | `/api/mobile/clients/:id` | `{name}` → `Client` |
| DELETE | `/api/mobile/clients/:id` | → 204 (revoked at once; open WebSockets of that token are closed) |

  `Client` = `{id, name, platform, createdAt, lastSeenAt, pairedVia, current}`
  (`current`: the request was made with this client's token).
- `GET /api/info` adds `server: {id, name, hostname, platform, version}` and
  `mobile: {enabled, port, listening}`.
- Revoking a client also drops it from the file; a missing file means no clients.

### 3.2 The mobile listener

- Config in `config.toml`:

  ```toml
  [codex.mobile]
  enabled = false     # second listener for the Aether app
  port = 7443         # TLS on 0.0.0.0 and [::]; 0 = any free port
  name = ""           # how the server introduces itself (empty: the computer name)
  ```

- TLS with rustls; a self-signed certificate made with `rcgen` at first use,
  kept in `$DSHX_HOME/mobile/cert.pem` and `key.pem` (0600), CN
  `Aether on <name>`, SANs hostname + `localhost` + current IPs, valid 10
  years, ECDSA P-256. `certSha256` is the SHA-256 of the DER certificate.
- Same router, same tokens; the loopback Host check is skipped for requests
  that arrived on this listener (they are bearer-protected; the Host check is a
  DNS-rebinding guard for the loopback listener only).
- `/api/oauth/callback` and `/preview/*` stay reachable only on the loopback
  listener (404 on the mobile one).
- Started by `dsh-codex web` when `enabled`, and live by `PUT /api/mobile`.
  Log line on start: `aether: listening on https://<ip>:<port> (fingerprint …)`.
- Addresses come from the machine's interfaces (crate `if-addrs` or similar);
  `kind` by range: `127/8`,`::1` loopback; `10/8`, `172.16/12`, `192.168/16`,
  `fe80::/10`, `fd00::/8` lan; `100.64/10` tailscale; else public.
- `server.name`: `[codex.mobile] name`, else on macOS `scutil --get
  ComputerName`, else the hostname. `server.id`: §3.3.

### 3.3 Server identity

`$DSHX_HOME/server.json` (0600): `{"id":"srv_<uuid-simple>","createdAt":<ms>}`,
created on first read. The id goes into `/api/info.server`, pairing payloads and
hub enrolment.

### 3.4 CLI

- `dsh-codex pair [--name <client>] [--format qr|link|json] [--no-https]
  [--http]`: writes a client into `clients.json`, prints the link (always), a
  QR (default; Unicode half-blocks, quiet zone 2) and, with `--format json`,
  the payload only. `--no-https` leaves transports empty (used by `--ssh`
  remotely); `--http` adds the loopback URL of a running server when the ready
  file or `--port` says which (dev only). Works while `dsh-codex web` runs
  (the server picks the file up) and when it does not.
- `dsh-codex pair --ssh <host> [-F <ssh_config>] [--user ubuntu] [--name …]
  [--remote-port 3080] [--remote-home /var/lib/dsh-codex] [--remote-user deepseek]
  [--remote-bin /usr/local/bin/dsh-codex] [--unit deepseek-harness]
  [--public-host <ip or name>]` runs on the admin's Mac:
  1. `ssh-keygen`-equivalent ed25519 key pair in memory (crate `ssh-key` with
     `ed25519`), comment `aether-<clientId>`.
  2. Over `ssh -F <config> -o BatchMode=yes <host>`: tries
     `sudo -n -u <remote-user> env DSHX_HOME=<remote-home> <remote-bin> pair --format json --no-https --name "<name>"`
     and reads the payload (client token made on the server). When that fails
     (old build, no `pair` subcommand): no token, `tokenCommand` from `--unit`
     as in §2.1, and the key gets a forced command.
  3. Appends to the remote user's `~/.ssh/authorized_keys`:
     `restrict,port-forwarding,permitopen="127.0.0.1:<remote-port>"[,command="<tokenCommand>"] ssh-ed25519 <pub> aether-<clientId>`
     (one line, idempotent for the same comment).
  4. Host key fingerprint from the `ssh_config`'s known_hosts
     (`ssh-keygen -l -F <hostname> -f <known_hosts>`), else `ssh-keyscan`.
     `host` in the payload: `--public-host`, else the `HostName` that
     `ssh -G <host>` resolves, `port` likewise.
  5. Prints the link and the QR. The private key is printed only inside the
     payload. Nothing is logged.
- `dsh-codex pair --list` and `dsh-codex pair --revoke <id>` manage
  `clients.json` locally.

### 3.5 Web UI page

Settings → **Aether App** (`#/settings/mobile`, EN "Aether app", DE "Aether-App"):

- A switch "Für die Aether-App erreichbar" with the port, the addresses the
  listener is on (chips: LAN / Tailscale / public) and the fingerprint (short).
- "iPhone verbinden": a card with the QR (server-rendered `qrSvg`, 232 px), the
  client name field (default "iPhone"), "Link kopieren" and a note that the
  link holds a secret and is valid until revoked. The QR is made only when the
  user asks (a POST creates a client), with a "Neu erzeugen" action.
- "Verbundene Geräte": rows with name, last seen (relative), a rename and a
  revoke with confirmation; the current client (when the page itself is opened
  through a client token) is marked.
- For servers without the listener (VPS): a short hint with the
  `dsh-codex pair --ssh …` command to run on the Mac.
- Strings in both languages in `ui/src/i18n.js` at the end of the EN and DE
  blocks in a marked `// Aether app` section; the page follows the existing
  settings panes (`ui/src/components/settings.js`, cards, confirm dialogs).

## 4. iOS app (P3, `ios/`)

- **Project:** `ios/project.yml` for XcodeGen (`xcodegen generate` → `ios/Aether.xcodeproj`,
  checked in like the user's other apps), target `Aether` (SwiftUI, iOS 17.0+,
  Swift 6, strict concurrency), `AetherTests` (unit), bundle id
  `de.yousufzai.aether`, `Config/Base.xcconfig` + `Debug/Release.xcconfig` +
  git-ignored `Local.xcconfig` for `DEVELOPMENT_TEAM` (the developer's own
  team id; simulator builds sign ad hoc with an empty team). Packages:
  `apple/swift-nio` (2.x), `apple/swift-nio-ssh` (0.15.0), `apple/swift-crypto`
  — nothing else. QR scanning with `VisionKit`'s `DataScannerViewController`
  (falls back to `AVCaptureSession`), markdown with `AttributedString` plus an
  own code-block renderer, diffs rendered by own code (`ui/src/diff.js` is the
  reference).
- **Protocol:** [docs/rust/SPEC.md §6](../rust/SPEC.md#6-ui--server-protocol)
  verbatim; `ui/src/api.js` (requests, the stream with `after` and backoff),
  `ui/src/reduce.js` (events → transcript items), `ui/src/store.js` (status,
  approvals, deltas, live frames) and `ui/src/worklog.js` (how tool calls are
  summarised: "Ran ls", "Edited x.rs", browser/computer rows) are the reference
  implementation to port, not to reinvent. Prompts send `timeZone`.
- **Transports:** §2. One `ServerClient` (REST + WebSocket) over a
  `Transport` that yields a base URL and an `Authorization` provider:
  `DirectTransport` (http/https with optional pin via `URLSessionDelegate`
  server-trust challenge: accept when SHA-256(DER leaf) == `certSha256`, else
  system validation), `SSHTunnelTransport` (NIOSSH client, ed25519 user auth,
  host key accepted only when its SHA-256 equals `hostKeySha256`; a local
  `ServerBootstrap` on `127.0.0.1:0` whose accepted connections are piped into
  `directTCPIP` child channels to `remoteHost:remotePort`; `tokenCommand` via
  an exec child channel), `HubTransport` (§5; base URL
  `<hub>/v1/relay/<serverId>`, bearer = hub session). The app tries transports
  in payload order, remembers the one that worked, and shows which it uses.
- **Storage:** devices (servers) as JSON in Application Support (no secrets)
  + Keychain items per device for `token`, `privateKey`, `tokenCommand`
  results; hub session in the Keychain. Nothing in UserDefaults but
  preferences.
- **Screens (German first, English second; String Catalog):**
  1. *Welcome* (first run): mark + wordmark, one sentence, "Gerät verbinden"
     (scan / link einfügen / manuell), "Mit Konto anmelden" (hub, §5, "Später"
     allowed). Also a "Demo ansehen" that loads a fixture
     (`ui/fixtures/coding.json` converted or embedded) so the app shows a
     full chat offline.
  2. *Geräte*: cards per device — name, kind icon (laptop / server / cloud),
     status dot (ready / connecting / error / offline), transport chip, unread
     and running counts (from `/api/sessions`), last connected. Swipe/menu:
     rename, re-pair, remove. "+" opens *Gerät hinzufügen* (scanner sheet with
     paste field and "Manuell": URL + token, or SSH host/user/key).
  3. *Chats* of a device: projects (folders) with their chats and the chats
     without a project, newest activity first, pinned on top, archived hidden;
     status marks (running, waiting for approval, unread); search; "Neuer
     Chat" with project choice: recent workspaces, a folder browser
     (`/api/fs/dirs`, `/api/fs/suggestions`), "Ohne Projekt". A device switcher
     in the navigation bar jumps between devices.
  4. *Chat*: transcript with user bubbles, assistant markdown, reasoning
     collapsed with its duration, tool rows as in the web UI's "Standard" view
     (one row per call: "Ran …", "Read …", "Edited …" with an expandable diff,
     "Browser · Open …" with the screenshot, errors in red), status chip
     (running / waiting / idle / error), live deltas, the plan (todo list),
     compaction notices, the live view image when a `live` message arrives
     (floating, tap to enlarge), turn usage line (from `/api/sessions/:id/stats`).
     *Approval banner*: tool, summary, justification, "Einmal erlauben" /
     "Ablehnen". *Composer*: multiline field, "+" for photos (upload to
     `/api/attachments`), model / effort / sandbox pickers (from `/api/info`),
     send; while running: Stop (cancel), Enter adds a steer or queue per the
     web UI's rule (steer by default, long-press "Danach senden"). Pull to
     reconnect; the stream resumes with `after`.
  5. *Einstellungen*: account (signed out: sign in / sign up; signed in:
     email, tier badge, "Geräte über das Konto" list, sign out), appearance
     (system / light / dark), about (version, licences), developer (hub URL
     override, allow plain http to loopback).
- **Behaviour:** reconnects automatically when the app returns to the
  foreground; a device's chats refresh on each visit; errors are sentences,
  never codes alone; nothing blocks the UI on a slow device: every device
  connects on its own task. VoiceOver labels on all controls; Dynamic Type.
- **Tests:** pairing link parsing and both base64 forms; the reducer against
  `ui/fixtures/coding.json` and `approval.json` (same items, same order as the
  web UI's `reduce.js` for text, tool and approval items); `DirectTransport`
  with a stubbed `URLProtocol`; the SSH tunnel against the Mac's sshd
  (`127.0.0.1:22`, Remote Login is on) with a temporary restricted key that the
  test removes again — skipped when sshd is not reachable.
- **Verification:** `xcodebuild -scheme Aether -destination 'platform=iOS
  Simulator,name=iPhone 17 Pro' build test`, then run in the simulator against
  a dev server (`python3 tools/dev/mock_deepseek.py 18090 2`, then
  `DEEPSEEK_BASE_URL=http://127.0.0.1:18090/anthropic DEEPSEEK_API_KEY=x
  DSHX_HOME=<scratch> target/debug/dsh-codex web --no-open --port 3190`; the
  start link is on stderr) with an `http` pairing payload, and take screenshots
  of Welcome, Geräte, Chats, Chat (streaming and a finished turn), Approval,
  Einstellungen in light and dark. Keep them under `ios/screenshots/`.

## 5. Accounts hub (P2, `crates/dshx-hub`, binary `aether-hub`)

The ChatGPT-style account system: sign up, sign in, a tier per user, the
user's servers, and a relay so the app reaches a server that is not on the
same network. Implemented and tested locally, **not deployed**.

- `aether-hub serve [--listen 127.0.0.1:8787] [--data ~/.aether-hub]
  [--public-url http://127.0.0.1:8787] [--admin-token …]`; SQLite
  (`rusqlite`, bundled) at `<data>/hub.db`; migrations in code. Axum 0.8 like
  `dshx-server`. Errors are `{error, code}` with 400/401/403/404/409.
- **Auth:** email + password (argon2id, `argon2` crate), sessions are opaque
  tokens `ahs_` + 32 random bytes base64url, stored as SHA-256, 90 days,
  refreshed on use; bearer in `Authorization`. Email verification is a 6-digit
  code written to the hub's log and returned in the signup response only when
  `--dev` is set; no mail is sent in this round.

| Method | Path | Body → Response |
|---|---|---|
| POST | `/v1/auth/signup` | `{email, password (≥ 10 chars), name?}` → 201 `{session, user, verification?}` |
| POST | `/v1/auth/login` | `{email, password}` → `{session, user}`; wrong → 401 `invalid-credentials` |
| POST | `/v1/auth/logout` | → 204 |
| POST | `/v1/auth/verify` | `{code}` → `{user}` |
| GET | `/v1/me` | → `{user:{id, email, name, emailVerified, createdAt}, tier:Tier, usage:{servers}}` |
| PATCH | `/v1/me` | `{name?}` → `{user}` |
| POST | `/v1/auth/password` | `{current, new}` → 204 |
| GET | `/v1/tiers` | (no auth) → `[Tier]` |
| GET | `/v1/servers` | → `[Server]` |
| POST | `/v1/servers/enroll-code` | → `{code:"ABCD-1234", expiresAt}` (10 min, single use; refused over the tier's `servers` limit with 403 `tier-limit`) |
| POST | `/v1/servers/enroll` | (no auth) `{code, name, hostname, platform, version, serverId}` → 201 `{server:Server, serverKey}` (`ask_…`, shown once; stored as SHA-256) |
| PATCH | `/v1/servers/:id` | `{name?}` → `Server` |
| DELETE | `/v1/servers/:id` | → 204 (its relay connection is closed, its key revoked) |
| WS | `/v1/relay/server` | server side; `Authorization: Bearer <serverKey>`; one connection per server, a newer one replaces the older |
| ANY | `/v1/relay/:serverId/api/*` | app side; user session bearer, the server must belong to the user and be connected, else 503 `server-offline`; forwarded to the server as an HTTP request (§5.1) |
| WS | `/v1/relay/:serverId/api/stream` | app side; forwarded as a WebSocket (§5.1) |
| GET | `/v1/health` | → `{ok:true, version}` |
| POST | `/v1/admin/users/:id/tier` | admin token → `{tierId}` → `{user, tier}` |

  `Tier` = `{id:"free"\|"pro"\|"team", name, limits:{servers:int\|null, relay:bool}, priceEurMonthly:number\|null}`;
  seeded: free (2 servers, relay), pro (10, 9.99), team (null = unlimited, 29).
  `Server` = `{id, name, hostname, platform, version, online, lastSeenAt, createdAt}`.
  `user.id` are `usr_…`; server ids are the `srv_…` from `server.json` (§3.3)
  when given, else `srv_` + uuid.

### 5.1 Relay framing

On the server WebSocket, JSON text frames; bodies base64 (standard, padded):

- hub → server `{"t":"req","id":"r12","method":"GET","path":"/api/info","headers":{"content-type":"…"},"body":null}`
- server → hub `{"t":"res","id":"r12","status":200,"headers":{…},"body":"<b64>"}`
  (one response per id; a failed local request answers 502 with `{error}`)
- hub → server `{"t":"ws-open","id":"w3","path":"/api/stream?session=…&after=0"}`;
  server → hub `{"t":"ws-opened","id":"w3"}` or `{"t":"ws-error","id":"w3","error":"…"}`
- both ways `{"t":"ws-msg","id":"w3","text":"…"}` and `{"t":"ws-close","id":"w3","code":1000}`
- keepalive `{"t":"ping"}` / `{"t":"pong"}` every 30 s; a server that misses
  three is marked offline.
- The hub strips the app's `Authorization`, `Host`, `Content-Length` and
  hop-by-hop headers before forwarding and drops `?token=` from the stream
  path; the **relay client** (the piece that runs inside `dsh-codex web`) adds
  its own loopback token and `Host: 127.0.0.1:<port>`. The hub never holds a
  server's token. Request bodies up to 100 MB; responses are buffered.
- Library API for the integration into `dsh-codex` (done by the orchestrator
  after merge, not by P2): `dshx_hub::relay_client::run(RelayConfig {hub_url,
  server_key, local_base: "http://127.0.0.1:<port>", local_token, server:
  ServerInfo}) -> impl Future` with reconnect/backoff (1 s … 60 s), and
  `dshx_hub::enroll(hub_url, code, ServerInfo) -> Result<(server, server_key)>`.
  Planned config: `[codex.hub] url = "…"`, key in `credentials.toml`
  `[hub] server_key = "ask_…"`, CLI `dsh-codex hub enroll --url … --code …`,
  `/api/info.hub = {url, enrolled, connected}`.
- Tests (`cargo test -p dshx-hub`): signup/login/me/logout, verify, tiers and
  the limit, enroll code lifecycle, relay round-trip (a tiny axum "server"
  behind the relay client answering `/api/info`, an app-side GET and a
  WebSocket echo through the hub), auth failures.

## 6. Brand package (P4, `brand/`, `ui/`, `desktop/`)

- `brand/BRAND.md`: name and story (§1), voice (calm, precise, friendly; German
  informal "du" in the UI; no exclamation marks), the mark and its construction,
  colour tokens (light/dark, contrast ≥ 4.5:1 for text), typography,
  spacing, iconography rules (SF Symbols on Apple, the UI's existing icons on
  the web), do/don't, and a rename plan for the parts that keep old names.
- `brand/aether-mark.svg` (the icon, works at 16 px, no text),
  `brand/aether-wordmark.svg`, `brand/aether-lockup.svg` (mark + wordmark),
  each in a light and a dark variant where needed; `brand/colors.json`
  (`{"light": {token: hex}, "dark": {…}}`, the token names of §1).
- iOS icons: `brand/ios/AppIcon.appiconset/` with `Contents.json` and a 1024 px
  PNG (plus dark and tinted variants for iOS 18+), rendered by a Swift
  `CoreGraphics` script in `brand/tools/` (like `desktop/Tools/make-icon.swift`)
  so the icon is pixel-exact; `brand/ios/LaunchMark.png` @1x/2x/3x.
- macOS: update `desktop/Tools/make-icon.swift` so the Mac app icon shows the
  new mark; `desktop/Info.plist` `CFBundleName`/`CFBundleDisplayName` →
  `Aether` (bundle id unchanged; `NSHumanReadableCopyright` "Aether – …").
- Web UI: `ui/favicon.svg` → the mark (light/dark via `prefers-color-scheme`
  as today), `ui/index.html` `<title>Aether</title>`, `ui/src/i18n.js` `brand:
  'Aether'` (both languages; other strings that name the `dsh-codex` binary
  stay). The sidebar shows `t.brand` in `ui/src/components/instances.js` when
  no instances exist; the start page may get the mark next to the name if it
  has a brand slot (keep changes to `ui/` minimal and verified in the browser
  with `?fixture=coding`).
- A preview sheet `brand/preview.png` (icon at 1024/180/60 px, mark, wordmark,
  the palette, a mocked iPhone card) for the user.

## 7. Integration and acceptance (orchestrator)

1. Merge P1 and P2 into `codex-app`; `cargo build --workspace`,
   `cargo test --workspace --exclude dshx-browser`, `cargo clippy --workspace
   --all-targets -- -D warnings`.
2. Wire the relay client into `dsh-codex` (§5.1), `cargo test`.
3. `cd ios && xcodegen generate && xcodebuild … test`; run the app in the
   simulator against a dev server through an `https` pairing from
   `POST /api/mobile/pair`, through `dsh-codex pair --ssh` against the Mac's own
   sshd, and through the hub (local `aether-hub`, enrol the dev server).
4. Brand assets into the iOS asset catalog; desktop app rebuilt
   (`desktop/build.sh`), web UI checked with `?fixture=coding`.
5. Docs: README ("Aether app" section), `desktop/README.md`, `CHANGELOG.md`;
   in the OracleVPS folder `state.md` and `log.md` as its `Agents.md` demands.
   Pushing, VPS rollout and anything on the hub's deployment need the user's
   go.
