# dshx-hub — the Aether accounts hub

The ChatGPT-style account system behind the Aether app
([docs/aether/SPEC.md §5](../../docs/aether/SPEC.md)): sign up, sign in, a tier
per user, the user's servers, and a relay through which the app reaches a
server (a machine running `dsh-codex web`) that is not on the same network.

Status: **implemented and tested locally, not deployed.** Nothing here runs
on a public host yet; the checklist at the end says what has to happen first.

The crate is a library with a thin binary:

| Piece | What it is |
|---|---|
| `Hub::open(dir)`, `router(hub)`, `serve(hub, addr)` | the hub itself (axum 0.8, SQLite in `<dir>/hub.db`) |
| `aether-hub serve …` | the binary (`src/bin/aether-hub.rs`) |
| `relay_client::run(RelayConfig)` | the piece that runs inside `dsh-codex web`: keeps a WebSocket to the hub and answers forwarded requests from the loopback listener |
| `enroll(hub_url, code, ServerInfo)` | registers a server with a code the user made in the app |

## Run it locally

```sh
cargo run -p dshx-hub --bin aether-hub -- serve --listen 127.0.0.1:8787 \
    --data /tmp/aether-hub --dev --admin-token change-me
```

- `--listen` (default `127.0.0.1:8787`), `--data` (default `~/.aether-hub`,
  created with mode 0700; `hub.db` inside, SQLite in WAL mode, migrations run
  at start), `--public-url` (reported by `/v1/health`), `--admin-token` (or
  `AETHER_HUB_ADMIN_TOKEN`; without it `/v1/admin/*` answers 403
  `admin-disabled`), `--dev` (verification codes are logged and returned by
  signup; never use it on a public host). Logs go to stderr; `RUST_LOG`
  selects the level (default `info`).

```sh
H=http://127.0.0.1:8787
curl -s $H/v1/health
curl -s $H/v1/auth/signup -H 'content-type: application/json' \
    -d '{"email":"ada@example.com","password":"correct horse battery","name":"Ada"}'
# → {"session":"ahs_…","user":{…},"verification":{"code":"123456",…}}   (--dev)
S=ahs_…
curl -s $H/v1/me -H "Authorization: Bearer $S"
curl -s $H/v1/tiers
curl -s -X POST $H/v1/servers/enroll-code -H "Authorization: Bearer $S"
# → {"code":"ABCD-1234","expiresAt":…}
```

## API

All bodies and answers are JSON. Timestamps are milliseconds since the Unix
epoch. Errors are `{"error":"<sentence>","code":"<kebab-id>"}`; the app
switches on `code`, people read `error`.

| Method | Path | Auth | Body → Response |
|---|---|---|---|
| GET | `/v1/health` | – | `{ok:true, version, publicUrl?}` |
| POST | `/v1/auth/signup` | – | `{email, password (≥ 10 chars), name?}` → 201 `{session, user, verification?}` |
| POST | `/v1/auth/login` | – | `{email, password}` → `{session, user}`; wrong → 401 `invalid-credentials` |
| POST | `/v1/auth/logout` | session | → 204 |
| POST | `/v1/auth/verify` | session | `{code}` → `{user}`; wrong or expired → 400 `invalid-code` |
| POST | `/v1/auth/password` | session | `{current, new}` → 204; the account's other sessions end |
| GET | `/v1/me` | session | `{user, tier, usage:{servers}}` |
| PATCH | `/v1/me` | session | `{name?}` → `{user}` (an empty name clears it) |
| GET | `/v1/tiers` | – | `[Tier]` |
| GET | `/v1/servers` | session | `[Server]` |
| POST | `/v1/servers/enroll-code` | session | → `{code:"ABCD-1234", expiresAt}`; over the limit → 403 `tier-limit` |
| POST | `/v1/servers/enroll` | – | `{code, name, hostname, platform, version, serverId?}` → 201 `{server, serverKey}` |
| PATCH | `/v1/servers/:id` | session | `{name?}` → `Server` |
| DELETE | `/v1/servers/:id` | session | → 204; its relay connection closes, its key is gone |
| WS | `/v1/relay/server` | `Bearer ask_…` | the server side of the relay |
| ANY | `/v1/relay/:serverId/api/*` | session | forwarded to the server; offline → 503 `server-offline` |
| WS | `/v1/relay/:serverId/api/stream` | session (bearer or `?token=`) | forwarded as a WebSocket |
| POST | `/v1/admin/users/:id/tier` | admin token | `{tierId}` → `{user, tier}` |

Shapes:

- `user` = `{id:"usr_…", email, name, emailVerified, createdAt}`
- `Tier` = `{id:"free"|"pro"|"team", name, limits:{servers:int|null, relay:bool}, priceEurMonthly:number|null}`
  — seeded: free (2 servers, relay, free), pro (10, 9.99), team (unlimited, 29).
  The rows live in the `tiers` table; limits and prices can be changed there
  without a release.
- `Server` = `{id:"srv_…", name, hostname, platform, version, online, lastSeenAt, createdAt}`
- a session is `ahs_` + 32 random bytes (base64url, no padding), sent as
  `Authorization: Bearer …`; the hub stores only its SHA-256. It lasts 90 days
  from its last use (the expiry is pushed at most once a minute).
- a server key is `ask_` + 32 random bytes, shown once by the enrolment,
  stored as SHA-256.

Status codes and `code` values:

| Status | `code` | When |
|---|---|---|
| 400 | `invalid-body`, `invalid-email`, `weak-password`, `invalid-name`, `invalid-code`, `invalid-server-id`, `unknown-tier`, `enroll-code-expired` | the request is malformed or breaks a rule |
| 401 | `unauthorized` | no or unknown session / server key / admin token |
| 401 | `invalid-credentials` | login, or the current password in a change |
| 403 | `tier-limit`, `forbidden` (someone else's server), `relay-disabled` (a tier without relay), `admin-disabled` | not allowed |
| 404 | `not-found`, `enroll-code-unknown` | no such route, user, server or code |
| 405 | `method-not-allowed` | |
| 409 | `email-taken`, `enroll-code-used`, `server-exists` (the `serverId` belongs to another account) | |
| 413 | `payload-too-large` (relay bodies over 100 MB), `invalid-body` (JSON bodies over axum's 2 MB default) | |
| 429 | `too-many-attempts` | more than 10 logins a minute for one email; more than 60 enrolments a minute hub-wide |
| 500 | `internal` | storage failure (logged, never detailed to the client) |
| 502 | `relay-error` | the server's answer could not be read |
| 503 | `server-offline` | the server is not connected to the hub |
| 504 | `relay-timeout` | the server did not answer within 120 s |

Validation: the email is trimmed and lower-cased and must look like
`local@domain.tld`; passwords have 10 to 512 characters; names (users and
servers) at most 80 characters; a given `serverId` is `srv_` + up to 64
letters, digits, `-` or `_`.

## How a server enrols

1. In the app (Settings → account) the user asks for an enrol code:
   `POST /v1/servers/enroll-code` → `ABCD-1234`, valid 10 minutes, single
   use. A new code replaces the user's earlier unused one, so at most one
   code per user is live.
2. On the server: `dsh-codex hub enroll --url https://hub.example --code ABCD-1234`
   (wired by the orchestrator after the merge) calls
   `dshx_hub::enroll(hub_url, code, ServerInfo)` → `POST /v1/servers/enroll`
   with the server's `srv_…` id from `server.json`, name, hostname, platform
   and version. The answer carries the `Server` and the `serverKey`; the CLI
   keeps the key in `credentials.toml` (`[hub] server_key`). The key never
   appears again.
3. `dsh-codex web` starts `relay_client::run(RelayConfig { hub_url,
   server_key, local_base: "http://127.0.0.1:<port>", local_token, server })`
   as a task. It connects to `wss://hub/v1/relay/server` with
   `Authorization: Bearer ask_…`, sends a `hello` frame, and reconnects with
   backoff (1 s, doubling to 60 s; a connection that lasted 30 s resets it).
   `RelayStatus::connected()` (see `run_with_status`) feeds
   `/api/info.hub.connected`.
4. The same `serverId` enrolled again by its owner (a reinstalled server, a
   lost key) gets a new key and keeps its row; the connection made with the
   old key ends. A user at the tier's limit must delete a server before a new
   code is issued, also for a re-enrolment.

## How the app reaches a server through the relay

The pairing payload lists a `hub` transport: `{"kind":"hub","url":"https://hub.example","serverId":"srv_…"}`.
The app then uses `https://hub.example/v1/relay/srv_…` as the base URL and
the hub session as bearer, and otherwise speaks the normal server protocol:
`GET …/api/info`, `POST …/api/sessions/:id/prompt`, the WebSocket at
`…/api/stream?session=…&after=0` (the hub session may also come as
`?token=`, which the hub removes before forwarding). The server must belong
to the user (else 403 `forbidden`), the tier must include the relay, and the
server must be connected (else 503 `server-offline`).

Responses are buffered: the hub waits for the whole answer before it
replies. Request bodies up to 100 MB (attachments) are accepted.

### Relay framing (SPEC §5.1)

On the server's WebSocket, JSON text frames; bodies base64 (standard
alphabet, padded), `null` when empty; headers as a `{name: value}` object
(repeated headers are joined with `, `):

```
hub → server   {"t":"req","id":"r12","method":"GET","path":"/api/info","headers":{"accept":"application/json"},"body":null}
server → hub   {"t":"res","id":"r12","status":200,"headers":{"content-type":"application/json"},"body":"<b64>"}
hub → server   {"t":"ws-open","id":"w3","path":"/api/stream?session=s1&after=0"}
server → hub   {"t":"ws-opened","id":"w3"}   or   {"t":"ws-error","id":"w3","error":"…"}
both ways      {"t":"ws-msg","id":"w3","text":"…"}   and   {"t":"ws-close","id":"w3","code":1000}
keepalive      {"t":"ping"} / {"t":"pong"}
server → hub   {"t":"hello","server":{"serverId":"srv_…","name":"…","hostname":"…","platform":"…","version":"…"}}
```

- One `res` per `req` id. A request the relay client could not make locally
  (listener down, timeout) answers `502` with `{"error":"…"}`.
- Before forwarding, the hub strips the app's `Authorization`, `Host`,
  `Content-Length` and the hop-by-hop headers (`Connection`, `Keep-Alive`,
  `Proxy-Authenticate`, `Proxy-Authorization`, `TE`, `Trailer`,
  `Transfer-Encoding`, `Upgrade`). The relay client adds the server's own
  start token (`Authorization: Bearer …`) and `Host: 127.0.0.1:<port>` taken
  from `local_base`, so the loopback listener's checks pass unchanged. The
  hub never holds a server's token. On the way back, `Content-Length` and
  the hop-by-hop headers are dropped again; everything else (content type,
  cache control, CSP, …) passes through.
- The hub sends `ping` every 30 s; a server that leaves three in a row
  unanswered is closed and marked offline. Both sides answer a `ping` with
  `pong`; the client also drops a connection that stayed silent for 100 s.
- `hello` is an addition to the spec: it refreshes `hostname`, `platform`
  and `version` of the server row on every connect (not the name, which the
  user may have changed in the hub). The hub ignores frame types it does not
  know, so both sides can grow.
- One connection per server: a newer one replaces the older, whose in-flight
  requests answer 503 and whose app streams close with code 1012.
- Ids are `r<n>` and `w<n>`, counted per connection.

## Decisions where the spec is silent

- **Verification codes** are 6 digits, valid 24 hours, logged and returned
  only with `--dev` (the brief forbids secrets in logs; without `--dev` and
  without mail delivery nobody sees them). An unverified account can use
  everything in this round; what verification should gate is a decision for
  the public launch.
- **Login limiter**: 10 attempts a minute per email address, in memory,
  counting every attempt. A refused attempt answers 429
  `too-many-attempts`. Unknown emails verify against a decoy hash so the
  timing does not reveal whether an account exists; both cases answer 401
  `invalid-credentials`.
- **Password change** keeps the session that made it and ends the others.
- **Enrol codes** use the letters A–Z without I and O plus four digits
  (`ABCD-1234`); the hub accepts them without the dash and in lower case.
  Unknown → 404 `enroll-code-unknown`, expired → 400 `enroll-code-expired`,
  reused → 409 `enroll-code-used`. A failed guess is as cheap as a real
  enrolment, so enrolment is limited to 60 attempts a minute hub-wide.
- **The tier limit** counts enrolled servers. Lowering a user's tier below
  that count keeps the servers; only new enrol codes are refused.
- **Deleting a user** is not a route yet (cascades are in the schema).
- `/v1/health` adds `publicUrl` when `--public-url` is set.
- Tiers without relay (`limits.relay = false`) exist in the model and answer
  403 `relay-disabled`; none is seeded.
- The hub trusts no proxy headers: client IPs are not read, which is why the
  limiters key on the email or are global.

## Implemented but not deployed

The hub, its tests and the relay client exist in this repository and run on
a developer's machine. No instance runs on a public host, no DNS name points
at one, no user data exists, and `dsh-codex` is not yet wired to it (that is
the orchestrator's step after the merge). The iOS app's `hub` transport
therefore works only against a local `aether-hub`.

Checklist before an instance serves anyone but its developer:

- [ ] **TLS termination** in front of the hub (Caddy or nginx; the hub
      itself speaks plain HTTP on `--listen`, bind it to loopback). The app
      pins nothing for the hub, so the certificate must be a real one.
      WebSockets must pass through the proxy (`Upgrade` headers; idle
      timeouts above 30 s so the keepalive keeps the server socket alive).
      The relay client needs no extra configuration for `https://` hub URLs:
      rustls with the `ring` provider is the only TLS stack in its
      dependency tree.
- [ ] **A systemd unit** with `--data /var/lib/aether-hub` (mode 0700, a
      dedicated user), `--public-url https://hub.example`, the admin token in
      an environment file (`AETHER_HUB_ADMIN_TOKEN`), `--dev` **off**.
- [ ] **Backups of `hub.db`**: it holds the accounts, the password hashes,
      the session hashes and the server keys' hashes. Use `sqlite3 hub.db
      ".backup …"` (consistent in WAL mode) on a schedule, and test a restore.
      A lost database means every server has to enrol again.
- [ ] **The admin token** is long, random and only in that environment
      file; rotate it when someone leaves.
- [ ] **Mail delivery** for the verification code (and a resend route), then
      decide what an unverified account may do.
- [ ] **Password reset** by email; account deletion; changing the email.
- [ ] **Rate limits by client IP** (behind the proxy: `X-Forwarded-For`
      from the proxy only) for signup, login and enrolment, and a cap on
      accounts per address; the in-memory limiters here are per process.
- [ ] **Abuse controls**: request logging with retention, a way to suspend
      an account and to revoke a server, limits on relay bandwidth per user,
      alerts on error rates.
- [ ] **Payments** for pro and team (the tiers are seeded, nothing charges);
      until then tiers are set by the admin route.
- [ ] Privacy notice and terms for the sign-up screen; a contact address.
- [ ] Monitoring of `/v1/health`; log rotation.

## Tests

```sh
cargo test -p dshx-hub
cargo clippy -p dshx-hub --all-targets -- -D warnings
```

Unit tests cover the validators, the limiter, the hashing, the migrations and
the frame encoding. `tests/hub.rs` runs a hub on a free port and covers
signup → me → logout → 401, login (wrong password, unknown email, the
limiter), the verification flow in `--dev`, validation and the error shape,
password change, tiers and the free limit with the admin tier change, the
enrol code lifecycle (unknown, replaced, expired, reused, re-enrolment, a
foreign id), server list/rename/delete with ownership, and the relay: a tiny
axum stand-in for `dsh-codex web` (bearer and loopback-Host checks,
`/api/info`, a binary `/api/echo`, a `/api/stream` echo WebSocket) behind
`relay_client::run`, reached through the hub with a user session (503 while
disconnected, 403 for another user's server, 401 without a session), a
WebSocket round trip, and deletion closing the relay.
