# OpenClaw als Personal Agent: technische Analyse

**Stand:** 28.10.2026 (Europe/Berlin)

**Analysierter Code:**
- Repository `github.com/openclaw/openclaw`, Branch `main`
- Commit `4757ac8eb6b8bc891f813983de8d02996cc9b61b`, Commit-Zeit 28.10.2026 19:58 MEZ
- Version laut `package.json`: `2026.10.8`

**Releases laut GitHub:**
- Letztes stabiles Release: **v2026.10.8** („Latest“, 27.10.2026 12:56 MEZ)
- Parallel dazu: `extended-stable` v2026.9.35 (27.10.2026) und Beta v2026.10.10-beta.4 (27.10.2026) [GH-API]

**Methode:**
- Das Repository wurde flach geklont und **nur gelesen** (grep/sed/cat). Nichts wurde gebaut, installiert oder ausgeführt.
- **Primärquellen:**
  - Code
  - Repo-Dokumentation unter `docs/`; das ist die Quelle von docs.openclaw.ai
  - `CHANGELOG/`, `SECURITY.md`, `VISION.md`
  - GitHub-API: Releases, Repo-Statistik, 722 veröffentlichte Security Advisories
- **Sekundärquellen:** Presse, Sicherheitsfirmen und Foren. Sie sind immer datiert.
- **Arbeitsteilung:** Die Subsystem-Lektüre lief in fünf parallelen, rein lesenden Durchgängen. Die tragenden Defaults habe ich danach selbst im Code nachgeprüft. Nachgeprüft sind unter anderem:
  - Bootstrap-Grenzen, Dateifilter je Sitzungstyp und Cache-Grenze
  - Memory-Flush-Prompt, Hybrid-Gewichte, Chunking und Dreaming-Default
  - Proactive-Check-Job, Queue-Defaults und Pairing-Konstanten
  - Sandbox- und Exec-Defaults, Gateway-Port

**Legende:**

| Kennzeichnung | Bedeutung |
|---|---|
| `[Code: pfad]` | Im Quellcode gelesen; Pfade relativ zur Repo-Wurzel |
| `[Docs: pfad]` | Repo-Dokumentation oder Changelog |
| `[GH-API]` | GitHub-API, abgerufen am 28.10.2026 |
| `[Sekundär: Quelle, Datum]` | Presse, Blogs, Foren |
| **unsicher** | Nicht abschließend geklärt |

---

## 0. Kernaussagen

1. **OpenClaw ist heute vor allem eine Gateway-Plattform, nicht nur ein Agent.**
   - Ein einziger, langlebiger Node.js-Prozess („Gateway“) bündelt alles: Messenger-Verbindungen, Sitzungen, Agent-Läufe, Zeitpläne, Geräte-Nodes sowie Web- und App-Clients. Clients sprechen mit ihm über WebSocket und JSON [Docs: docs/concepts/architecture.md].
   - Der Code umfasst etwa 4,3 Mio. Zeilen TypeScript ohne Tests, generierte Dateien eingerechnet. Dazu kommen 189 Extension-Einträge und rund 1.400 Doku-Seiten [Code: Zählung über `src/`, `extensions/`, `packages/`, `docs/`].
2. **Die Qualität als Personal Agent entsteht aus fünf ineinandergreifenden Mechanismen.** Ein einzelnes Feature erklärt sie nicht.
   - **Messenger als Oberfläche:** 28 Kanäle im Repo, mit DM-Pairing.
   - **Persönlichkeit und Regeln** als Markdown-Dateien im Workspace (`AGENTS.md`, `SOUL.md`, `IDENTITY.md`, `USER.md`), die bei jedem Lauf gecacht eingespeist werden.
   - **Markdown-Gedächtnis:**
     - `MEMORY.md` und Tagesnotizen
     - ein hybrider SQLite-Index
     - ein Memory-Flush vor jeder Komprimierung
     - nächtliches „Dreaming“
   - **Proaktivität:** Automations, darunter ein „Proactive check“, mit stiller Unterdrückung über `NO_REPLY`.
   - **Echte Werkzeuge auf echter Hardware:** Shell, Browser über CDP und Playwright, Geräte-Nodes für Kamera, Standort und Benachrichtigungen.
3. **Der Stand von Oktober 2026 weicht stark vom Bild in vielen Blogartikeln (Frühjahr 2026) ab:**
   - **Heartbeat** und `HEARTBEAT.md` sind als eigenes Subsystem zurückgezogen und in normale Automation-Jobs überführt.
   - `TOOLS.md` ist ebenfalls zurückgezogen.
   - Sitzungen liegen in SQLite statt in JSONL.
   - Die pi-Agent-Laufzeit ist in eigene Pakete übernommen.
   - Neu dazugekommen sind Dreaming, Skill Workshop, Standing Intents, Code Mode und Tool Search.
4. **Das Sicherheitsmodell ist ausdrücklich „ein vertrauenswürdiger Operator pro Gateway“** [Docs: SECURITY.md].
   - Die Defaults sind auf Leistung ausgelegt: Sandbox aus, Exec ohne Freigaben (`full`), und das lokale Onboarding setzt das Tool-Profil `full`.
   - Fremde Absender in Messengern sind dagegen per Default ausgesperrt (DM-Pairing).
   - Bis zum 28.10.2026 hat das Projekt **722** Security Advisories veröffentlicht, davon 14 kritisch und 249 hoch [GH-API].
5. **OpenClaw läuft grundsätzlich auf 1 vCPU / 6 GB ARM64.**
   - Das dokumentierte Minimum ist 1 vCPU und 1 GB.
   - Gemessen wurden Spitzen-RSS-Werte von 0,6–0,8 GB in Smoke-Tests.
   - Teuer sind verwalteter Chromium-Browser, Docker-Sandbox und lokale Embeddings [Docs: docs/help/faq-first-run/providers-and-hosting.md; docs/reference/release-performance-sweep.md].
6. **Am meisten übertragbar sind die Konventionen und Zustandsmaschinen, nicht der Code.**
   - Workspace-Dateien mit Budget und Filter je Sitzungstyp
   - Markdown plus SQLite-FTS5/Vektor-Gedächtnis mit Pre-Compaction-Flush
   - Kanal-Adaptervertrag mit Session-Key-Grammatik und Pairing
   - Proaktive Jobs mit `idleOnly`, Scratch und `NO_REPLY`
   - Steering-Semantik für Nachrichten, die während eines Laufs eintreffen
   - Umhüllung nicht vertrauenswürdiger Inhalte

---

## 1. Kurzprofil

| Merkmal | Befund |
|---|---|
| Zweck | „Open-source AI assistant that runs on your own computer and meets you in the channels you already use“. Persönlicher Assistent oder geteilter Team-Gateway; laut README ist der Unterschied nur Konfiguration [Docs: README.md] |
| Herkunft | Gestartet von **Peter Steinberger**. Namensfolge: Warelay → Clawdbot („Clawd“, ab 25.11.2025) → Moltbot (27.01.2026, nach Markenrechts-Mail von Anthropic) → **OpenClaw** (30.01.2026) [Docs: docs/start/lore.md, VISION.md] |
| Steinberger heute | Wechsel zu OpenAI am 15.02.2026, angekündigt von Sam Altman [Sekundär: Bloomberg/Engadget, 15.02.2026]. Leitet dort laut Foundation-Launch „Claw Labs“ und trifft weiter die technischen Entscheidungen bei OpenClaw [Sekundär: Trending Topics, 09.07.2026] |
| Trägerschaft | **OpenClaw Foundation**, unabhängige 501(c)(3), öffentlicher Launch 08./09.07.2026. Beschäftigt das Kernteam und signiert Releases. Spender: Amazon, Lobster Computer Company, Offline Holdings, OpenAI, Red Hat, University of Michigan. Infrastruktur: Blacksmith, Convex, GitHub, NVIDIA, Vercel. README: „OpenAI is a donor, not an owner.“ [Docs: README.md] Copyright im MIT-Lizenztext: „OpenClaw Foundation“ [Code: LICENSE] |
| Lizenz | MIT [Code: LICENSE, package.json] |
| Stack | TypeScript auf Node.js. Harte Untergrenze `>=24.16.0 <25 \|\| >=26.1.0`; empfohlen 24.22.1+/26.12.1+ [Code: node-version.mjs, package.json]. pnpm-Monorepo. Apps in Swift (macOS, iOS) und Kotlin (Android); Tauri-Desktop für Linux und Windows. Zwei Rust-Crates: `openclaw-gateway-client` und `openclaw-node-host` [Code: crates/, apps/] |
| Version | Kalender-Versionierung `JJJJ.M.N`. Stabil v2026.10.8 vom 27.10.2026, ein Security- und Patch-Release ohne neue Features. Kanäle: stable, extended-stable (LTS-Äquivalent), beta, dev [GH-API; Docs: CHANGELOG/2026.10.8.md, docs/install/development-channels.md] |
| Verbreitung | 393.285 Stars, 82.668 Forks, 7.749 offene Issues/PRs. Repo angelegt am 24.11.2025 [GH-API]. Foundation-Angabe: „4,5 Mio. neue Claws pro Woche“, 34 ClawCon-Events [Sekundär: Trending Topics, 09.07.2026; Eigenangabe, nicht prüfbar] |
| Ökosystem | **ClawHub** (Skill- und Plugin-Registry, clawhub.ai). 50 gebündelte Skills; 178 Plugin-Manifeste, davon 90 per Default aktiv [Code: skills/, extensions/*/openclaw.plugin.json]. Abgeleitete Produkte: NVIDIA „NemoClaw“, „Microsoft Scout“ [Sekundär: Trending Topics, 09.07.2026]. Importer für Codex, Claude Code und **Hermes** [Code: extensions/migrate-hermes, extensions/migrate-claude] |
| Reifegrad | Sehr hohe Änderungsrate: v2026.9.5 allein enthielt 4.179 PRs von 502 Accounts [Docs: docs/releases/2026.9.5.md]. Config-Brüche werden über `openclaw doctor --fix` migriert, ohne Kompatibilitäts-Aliase [Docs: VISION.md]. Funktional breit, im Detail spürbar „in Bewegung“; mehrere Doku/Code-Widersprüche, siehe §8 |

---

## 2. Was macht OpenClaw als Personal Agent gut?

| # | Eigenschaft aus Nutzersicht | Technischer Mechanismus | Beleg |
|---|---|---|---|
| 1 | **Ist dort, wo ich ohnehin schreibe** (Telegram, WhatsApp, iMessage, Signal, Slack, Discord …) | Kanal-Plugins hinter einem gemeinsamen Vertrag (`ChannelPlugin`). Eingehende Nachrichten werden auf einen einheitlichen Umschlag (`MsgContext`) normalisiert. Das Outbound arbeitet mit einem Markdown-Zwischenformat, das je Kanal gerendert und gestückelt wird | [Code: src/channels/plugins/types.plugin.ts, src/auto-reply/templating.ts] |
| 2 | **Ein Assistent, ein Gedächtnis über alle Kanäle** | Default `session.dmScope: "main"`: alle DMs aller Kanäle landen in `agent:<id>:main`. Mit `identityLinks` werden dieselben Personen kanalübergreifend zusammengeführt | [Code: src/routing/resolve-route.ts, src/routing/session-key.ts] |
| 3 | **Hat eine stabile Persönlichkeit und kennt mich** | `SOUL.md`, `IDENTITY.md` und `USER.md` werden bei jedem Lauf in einen gecachten Prompt-Präfix eingespeist. Ein `BOOTSTRAP.md`-Ritual beim ersten Start lässt Name, Stil und Avatar festlegen | [Code: src/agents/workspace.ts, src/agents/system-prompt.ts; Docs: docs/reference/templates/BOOTSTRAP.md] |
| 4 | **Erinnert sich langfristig, und ich kann es lesen und korrigieren** | Markdown ist die Quelle der Wahrheit: `MEMORY.md` und `memory/JJJJ-MM-TT.md`. Darüber liegt ein SQLite-Index (FTS5/BM25 + sqlite-vec) mit den Tools `memory_search` und `memory_get`. Vor jeder Komprimierung schreibt ein stiller Flush-Turn Erinnerungen weg; nächtlich konsolidiert „Dreaming“ | [Code: extensions/memory-core/, src/agents/memory-search.ts; Docs: docs/concepts/memory.md] |
| 5 | **Meldet sich von selbst, aber nur wenn es etwas zu sagen gibt** | Der „Proactive check“ läuft alle 30 min als Automation mit diesen Eigenschaften: `idleOnly` (nur im Leerlauf), `skipIfScratchEmpty` (kein Lauf bei leerer Checkliste), Antwort `NO_REPLY` oder `heartbeat_respond(notify=false)` (keine Zustellung), Zustellung an den Owner | [Code: src/cron/default-proactive-job.ts, src/auto-reply/heartbeat.ts] |
| 6 | **Erledigt Termine und Routinen zuverlässig** | Automations-Engine mit `at`/`every`/`cron`-Zeitplänen, Zeitzone, Haupt- oder isolierter Sitzung, Retries, Auto-Disable und Run-Historie. Das Modell legt Jobs selbst über das Tool `automations` (Alias `cron`) an | [Docs: docs/automation/cron-jobs/*.md; Code: src/cron/] |
| 7 | **Tut echte Dinge auf echter Hardware** | `exec`/`process` mit Hintergrundprozessen, Datei-Tools, `apply_patch`, verwalteter Chromium über CDP/Playwright mit Ref-Snapshots, Computer Use. Geräte-Nodes (macOS, iOS, Android, headless) für Kamera, Bildschirm, Standort, Kontakte, Kalender und SMS | [Code: src/agents/tool-catalog.ts, extensions/browser/; Docs: docs/nodes/command-policy.md] |
| 8 | **Reagiert natürlich im Chat** | Nachrichten, die während eines Laufs eintreffen, werden am nächsten Tool-Übergang in den Lauf **gesteuert** („steer“), statt den Lauf abzubrechen. Dazu kommen Tippindikatoren, Fortschritts-Entwürfe (eine Nachricht, die live editiert wird), Erwähnungsregeln in Gruppen und gezieltes Schweigen (`NO_REPLY`) | [Code: src/auto-reply/reply/queue/; Docs: docs/concepts/queue-steering.md, docs/concepts/progress-drafts.md] |
| 9 | **Ist sicher genug, um im Messenger erreichbar zu sein** | `dmPolicy: "pairing"` per Default: Unbekannte erhalten einen 8-stelligen Code, der Owner gibt per CLI frei. Gruppen stehen per Default auf Allowlist mit Erwähnungspflicht. Externe Inhalte werden als „untrusted“ umhüllt | [Code: src/security/dm-policy-shared.ts, src/pairing/pairing-store-model.ts, src/security/external-content.ts] |
| 10 | **Wächst mit, ohne den Kern zu verändern** | Skills im AgentSkills-Format (`SKILL.md`), installierbar aus ClawHub. Plugins (Kanäle, Provider, Tools, Hooks), MCP-Client und -Server. Der „Skill Workshop“ lässt den Agenten eigene Skills aus erledigter Arbeit anlegen | [Docs: docs/tools/skills.md, docs/tools/plugin.md; Code: src/skills/workshop/config.ts] |
| 11 | **Freie Modellwahl, Abos nutzbar, fällt nicht aus** | 62 Provider-Plugins. Auth-Profile (API-Key, OAuth, Token) mit Rotation und Cooldowns; Fallback-Kette primary → fallbacks. ChatGPT/Codex-OAuth und Claude-CLI-Wiederverwendung | [Docs: docs/concepts/model-failover.md, docs/concepts/oauth.md; Code: src/agents/auth-profiles/usage-failure-state.ts] |
| 12 | **Delegiert und spezialisiert** | `sessions_spawn` für Sub-Agents, die isoliert laufen und ihr Ergebnis zurückmelden. Mehrere Agenten mit eigenem Workspace werden über Bindings an Kanäle oder Chats geroutet. Externe Harnesses (Claude Code, Codex, Gemini CLI) sind per ACP anbindbar | [Code: src/agents/tools/sessions-spawn-tool.ts, src/routing/resolve-route.ts; Docs: docs/tools/acp-agents.md] |

---

## 3. Architektur

```
                         ┌──────────────────────────────────────────────────────────────┐
  Messenger-Plattformen  │                    GATEWAY (1 Node-Prozess/Host)             │
  (Telegram, WhatsApp,   │  WS+HTTP auf 127.0.0.1:18789 (Default)                       │
   Slack, Discord,       │                                                              │
   Signal, iMessage …)   │  ┌──────────────┐   ┌─────────────┐   ┌──────────────────┐   │
        ▲   │            │  │ Kanal-Plugins│──▶│ Routing/    │──▶│ Session-Controller│  │
        │   ▼            │  │ (grammY,     │   │ Bindings →  │   │ + Queue (steer/   │  │
   ─────┴───────────────▶│  │  Baileys, …) │   │ Session-Key │   │  followup/collect)│  │
                         │  └──────▲───────┘   └─────────────┘   └────────┬─────────┘   │
                         │         │ Outbound (IR→Kanalformat,           │              │
                         │         │ Chunking, Streaming)                 ▼              │
                         │  ┌──────┴───────────────────────────────────────────────┐    │
                         │  │ Agent-Laufzeit „openclaw“ (packages/agent-core, ex-pi)│    │
                         │  │  Prompt-Builder (Workspace-Dateien, Skills, Tools)    │    │
                         │  │  Tool-Loop · Compaction · Memory-Flush · Failover     │    │
                         │  │  alternative Harnesses: Codex app-server, Claude CLI, │    │
                         │  │  ACP (acpx)                                           │    │
                         │  └──┬───────────┬───────────────┬──────────────┬────────┘    │
                         │     │Tools      │Memory-Plugin  │Automations   │Plugins/MCP  │
                         │     ▼           ▼ (Slot)        ▼ (Scheduler)  ▼             │
                         │  exec/browser  memory-core     cron_jobs,     Provider,      │
                         │  nodes/web/…   FTS5+vec        Proactive      Hooks, Skills  │
                         │                Dreaming        check, Hooks                  │
                         └───┬──────────────┬─────────────────────┬─────────────────────┘
                             │WS role=node  │WS role=operator     │Dateien/SQLite
                ┌────────────▼───┐   ┌──────▼─────────────┐  ┌────▼───────────────────────┐
                │ Nodes: macOS/  │   │ Clients: Control UI│  │ ~/.openclaw/               │
                │ iOS/Android/   │   │ (Web), CLI, TUI,   │  │  openclaw.json (JSON5)     │
                │ headless       │   │ macOS-Menüleisten- │  │  state/openclaw.sqlite     │
                │ (Kamera, Screen│   │ App, Tauri-Desktop │  │  agents/<id>/agent/        │
                │  Standort,     │   └────────────────────┘  │    openclaw-agent.sqlite   │
                │  system.run …) │                           │  workspace/ (AGENTS.md …)  │
                └────────────────┘                           │  credentials/, sandboxes/  │
                                                             └────────────────────────────┘
```

**Komponenten:**

- **Gateway**
  - Ein Prozess pro Host. Er hält alle Messenger-Verbindungen; nur er öffnet z. B. die WhatsApp-Sitzung.
  - Er bietet eine typisierte WS-API mit `req`/`res`/`event`-Frames. Der erste Frame muss `connect` sein, sonst wird die Verbindung hart geschlossen.
  - Seiteneffekt-Methoden (`send`, `agent`) verlangen Idempotency-Keys. Events werden nicht nachgeliefert.
  - Das Protokoll ist in TypeBox definiert. Daraus werden JSON Schema und Swift-Modelle generiert.
  - [Docs: docs/concepts/architecture.md]
- **Scheduler**
  - Ein `GatewayScheduler` pro Kernel treibt Wartung und Cron mit einem einzigen Host-Timer.
  - Persistente Stores halten Deadlines; die Zeitpläne werden beim Start rekonstruiert, statt einen zweiten Scheduler-Zustand zu speichern.
  - Verpasste Ticks werden zusammengefasst; die Uhr ist robust gegen Schlafphasen und Zeitsprünge.
  - [Docs: docs/concepts/architecture.md]
- **Clients und Nodes**
  - Beide verbinden sich über denselben WS-Server, unterschieden durch `role: operator|node`.
  - Jeder Client bringt eine Geräte-Identität mit (Ed25519) und signiert die Server-Nonce (`connect.challenge`).
  - Neue Geräte brauchen eine Freigabe. Lokale Loopback-Verbindungen werden automatisch genehmigt, Tailnet und LAN nie.
  - [Docs: docs/concepts/architecture.md, docs/gateway/protocol/auth.md]
- **Konfiguration**
  - Datei `~/.openclaw/openclaw.json` im Format JSON5, mit `$include` und Env-Substitution.
  - Hot-Reload per File-Watcher. Auth-Modus und `tools.gatewayPolicy` brauchen einen Neustart.
  - Die Laufzeit liest nur das aktuelle Schema; alte Schlüssel migriert `openclaw doctor --fix`.
  - [Docs: docs/gateway/configuration.md, VISION.md]
- **Datenverzeichnisse** (Default `~/.openclaw`, überschreibbar mit `OPENCLAW_STATE_DIR`):

  | Pfad | Inhalt |
  |---|---|
  | `state/openclaw.sqlite` | Globale Control-Plane: Exec-Approvals, Cron-Jobs, Pairing, Secret-Store, MCP-OAuth, Workspace-Attestierung |
  | `agents/<id>/agent/openclaw-agent.sqlite` | Pro Agent: Sitzungen, Transkripte (`transcript_events`, optional zstd), Memory-Index, Auth-Profile, Standing Intents |
  | `workspace/` (weitere Agenten: `workspace-<id>`) | `AGENTS.md`, `SOUL.md`, `IDENTITY.md`, `USER.md`, `MEMORY.md`, `memory/`, `skills/` |
  | `credentials/` | z. B. WhatsApp-Credentials |
  | `sandboxes/` | Sandbox-Arbeitsverzeichnisse |
  | `/tmp/openclaw/openclaw-JJJJ-MM-TT.log` | Logs; Rotation bei 100 MB, 5 Archive |

  [Docs: docs/reference/database-schemas/layout.md, docs/help/faq/where-things-live-on-disk.md, docs/gateway/logging.md]
- **Datenbank**
  - Default ist SQLite. PostgreSQL 16–18 ist optional für die Store-Familien `state`, `agent`, `workboard`, `logbook` und `team-reports`.
  - Schema-Versionen: `state` 36, `agent` 38.
  - [Code: package.json `openclaw.databaseEngines/schemaVersions`; Docs: docs/reference/database-engines.md]

---

## 4. Subsysteme im Detail

### 4.1 Agent-Laufzeit und Agent-Loop

**Herkunft der Laufzeit**
- OpenClaw nutzte bis mindestens 2026.5.19 Mario Zechners pi-Pakete (`@mariozechner/pi-agent-core`, `pi-ai`, `pi-coding-agent`), ab 2026.5.12 unter `@earendil-works` [Docs: CHANGELOG/2026.2.21.md, CHANGELOG/2026.5.19.md].
- Heute steckt der Code in den eigenen Paketen `packages/agent-core` und `packages/ai`. Extern bleibt nur `@earendil-works/pi-tui@1.3.0` für die TUI [Code: package.json; Docs: docs/agent-runtime-architecture.md, THIRD_PARTY_NOTICES.md].
- Die Laufzeit-ID ist `openclaw`; `pi` ist nur noch ein Alias.
- **unsicher:** In welchem Release genau die Ablösung passierte.

**Der Loop**
- `runAgentLoop`: Eine äußere Schleife arbeitet Follow-ups ab. Eine innere Schleife führt Tool-Batches aus und arbeitet dazwischen Steering-Nachrichten ein. Nach dem Follow-up-Drain wird erneut auf Steering geprüft, damit eine angenommene Steer-Nachricht nicht verloren geht [Code: packages/agent-core/src/agent-loop.ts].
- Die `Agent`-Klasse hat zwei `PendingMessageQueue`s, eine für Steering und eine für Follow-up [Code: packages/agent-core/src/agent.ts].
- Sitzungen sind ein Baum aus Einträgen mit `parentId`. Typen sind `message`, `compaction`, `branch_summary`, `model_change` usw.; das ist pi-Erbe [Code: packages/agent-core/src/harness/types.ts].

**Ablauf eines Laufs** [Docs: docs/concepts/agent-loop.md]
1. Das RPC `agent` antwortet sofort mit `{runId, acceptedAt}`.
2. `agentCommand` löst Modell, Thinking-Level und Skills-Snapshot auf.
3. `runEmbeddedAgent` reserviert den Turn beim Session-Controller **vor** der globalen Kapazität. Danach wählt er das Auth-Profil, baut die Sitzung und setzt das Timeout.
4. Events laufen als Streams `tool`, `assistant` und `lifecycle`.
5. `agent.wait` wartet per Default 30 s.

**Schreibschutz:** Jeder Transkript-Append muss `expectedWriterRunId == activeWriterRunId` tragen. Ein überholter Lauf kann so nichts mehr committen.

**Grenzen und Retries**

| Grenze | Wert | Beleg |
|---|---|---|
| Retries im äußeren Loop | 24 + 8 pro Auth-Profil, geklemmt auf 32–160 | [Code: src/agents/embedded-agent-runner/run/helpers.ts] |
| Budget je Versuch | 48 h | [Code: src/agents/timeout.ts] |
| Modell-Idle-Timeout | 120 s Cloud, 300 s selbst gehostet | [Docs: docs/concepts/agent-loop.md] |
| Globale Parallelität (Lane) | `max(8, CPUs × 4)` | [Code: src/config/agent-limits.ts] |

**Alternative Harnesses** [Docs: docs/concepts/agent-runtimes.md; Code: extensions/{codex,copilot,acpx,anthropic}/]
- `codex` (Codex app-server) als Plugin-Harness. Codex besitzt dann Loop, Thread-State und Komprimierung; OpenClaw spiegelt das Transkript.
- `claude-cli` als CLI-Backend.
- ACP über `acpx` für Claude Code, Gemini CLI, OpenCode, Cursor u. a.
- Auswahl in dieser Reihenfolge: Modell-Override → Provider-Override → `auto` (ein Plugin beansprucht die Route) → `openclaw`.

**Nachrichten während eines Laufs (Queue/Steering)**
- Defaults: `mode: "steer"`, Debounce 500 ms, `cap: 20`, `drop: "summarize"` [Code: src/auto-reply/reply/queue/state.ts, settings.ts].

| Modus | Verhalten |
|---|---|
| `steer` | Wird am nächsten Tool- oder Modell-Übergang in den laufenden Lauf eingespielt |
| `followup` | Eigener Turn nach dem Lauf |
| `collect` | Wartende Nachrichten werden zu einem Turn zusammengefasst; auf Telegram und Discord verhält er sich wie `followup` |
| `interrupt` | Bricht den Lauf ab |

- Ein laufendes Tool wird nie unterbrochen.
- Bei sequenziellen Tool-Aufrufen werden die noch nicht gestarteten übersprungen und erhalten das synthetische Ergebnis „Skipped to process an incoming message.“; parallele Batches nie [Code: packages/agent-core/src/tool-call-outcome.ts; Docs: docs/concepts/queue-steering.md].
- `allowCollaborativeSteering: true` (Default): Die Nachricht eines anderen Teilnehmers kann einen Lauf steuern, der unter der Autorität des ursprünglichen Absenders läuft [Docs: docs/concepts/queue.md].

### 4.2 Prompt-Aufbau, Workspace-Dateien und Caching

**Aufbau des System-Prompts**
- Der System-Prompt wird bei jedem Lauf neu gebaut, aber in zwei Teile getrennt [Code: src/agents/system-prompt.ts]:
  - **Stabiler Präfix** (gecacht): Identität („You are a personal assistant running inside OpenClaw.“), Execution-Bias, „Promised Work“, Provider-Präfix, Workspace-Hinweise, Bootstrap-Hinweise und schließlich `# Project Context` mit den Workspace-Dateien.
  - **Grenze:** `<!-- OPENCLAW_CACHE_BOUNDARY -->` [Code: packages/ai/src/utils/system-prompt-cache-boundary.ts].
  - **Volatiler Suffix:** Tool-Liste, Skills, Memory Recall, Modell-Aliase, Sandbox, Silent Replies, Authorized Senders, Delegation, Reaktionen und die Zeile `## Runtime` (`agent=… | session=… | host | os (arch) | model | channel | channel_capabilities`) [Code: src/agents/system-prompt-runtime.ts].
- **Datum und Zeitzone** stehen **nicht** im System-Prompt. Sie kommen als eigene Runtime-Context-Nachricht pro Turn, und zwar nur das Datum, damit der Cache den Tageswechsel übersteht. Die genaue Uhrzeit liefert das Tool `session_status` [Code: src/agents/date-time.ts, src/agents/runtime-facts-prompt.ts]. `docs/concepts/context.md` behauptet noch etwas anderes.
- **Prompt-Modi:**
  - `full`
  - `minimal`: für Sub-Agents; ohne Memory Recall, User Identity, Messaging und Silent Replies
  - `none`

**Workspace-Dateien**
- Reihenfolge der Einspeisung: `AGENTS.md`, `SOUL.md`, `IDENTITY.md`, `USER.md`, `BOOTSTRAP.md`, `MEMORY.md` [Code: src/agents/workspace-bootstrap-policy.ts, src/agents/system-prompt-context-files.ts].
- Welche Dateien welcher Sitzungstyp sieht [Code: src/agents/workspace.ts `filterBootstrapFilesForSession`, `SUBAGENT_BOOTSTRAP_ALLOWLIST`, `CRON_BOOTSTRAP_ALLOWLIST`]:

| Sitzungstyp | Eingespeist |
|---|---|
| Private Direktsitzung (main oder DM pro Peer) | alle, inkl. `MEMORY.md` |
| Gruppe oder Kanal | alle außer `MEMORY.md` (Privatsphäre) |
| Cron/Automation (isoliert) | `AGENTS.md`, `SOUL.md`, `IDENTITY.md`, `USER.md` |
| Sub-Agent | nur `AGENTS.md` |

- **Zurückgezogene Dateien:**
  - `TOOLS.md`: wandert in einen Abschnitt `## Tools` von `AGENTS.md`.
  - `HEARTBEAT.md`: wird zum „Scratch“ des Automation-Jobs in SQLite.
  - Beide Umzüge erledigt Doctor [Code: src/commands/doctor-tools-md-migration.ts, src/commands/doctor-heartbeat-scratch-migration.ts; Docs: docs/reference/templates/TOOLS.md, HEARTBEAT.md].
- **Wann eingespeist wird:** `agents.defaults.contextInjection` steht per Default auf `"always"`, also bei jedem Lauf; dank Cache ist das billig. Alternativen sind `"continuation-skip"` und `"never"` [Code: src/agents/bootstrap-files.ts].
- **Angepinnter Prompt:** Auf den Routen Anthropic-API-Key und OpenAI Responses bleibt der volle System-Prompt pro „Serie“ fest. Änderungen an Skills, Memory oder Datum werden als Instruktionsnachrichten nachgeschoben; eine neue Serie beginnt erst bei Reset oder Komprimierung [Docs: docs/concepts/system-prompt.md].

**Größengrenzen**

| Grenze | Default | Beleg |
|---|---|---|
| Pro Datei (`bootstrapMaxChars`) | 20.000 Zeichen | [Code: src/agents/embedded-agent-helpers/bootstrap-limits.ts] |
| Gesamt (`bootstrapTotalMaxChars`) | 60.000 Zeichen | [Code: src/agents/embedded-agent-helpers/bootstrap.ts] |
| `USER.md` | fix 4.000 Zeichen; Konfiguration darf nur senken | [Code: …/bootstrap.ts] |

**Kürzungsregeln**
- Generische Datei: 75 % Kopf + 25 % Ende, mit Marker „…truncated, read X for full content…“.
- `AGENTS.md`: 45 % Kopf + 35 % „Policy-Digest“ (Zeilen mit must, never, security, secret, commit …) + 15 % Ende.
- `USER.md`: wird bei Überlauf **ganz** weggelassen statt halbiert.
- [Code: src/agents/embedded-agent-helpers/bootstrap.ts]

**`BOOTSTRAP.md`**
- Wird nur in einem frischen Workspace angelegt. Ein neuer Workspace bekommt dabei auch `git init`.
- Im `full`-Modus enthält der Prompt die Anweisung „First visible reply must follow BOOTSTRAP.md; no generic greeting.“
- Das Ritual fragt Name, Vibe und Avatar ab, schreibt `IDENTITY.md` und löscht sich danach selbst.
- Ein Rest von `BOOTSTRAP.md` wird automatisch entfernt, sobald SOUL, IDENTITY oder USER von der Vorlage abweichen.
- [Code: src/agents/bootstrap-mode.ts, src/agents/workspace-git.ts; Docs: docs/reference/templates/BOOTSTRAP.md]

**Skills im Prompt**
- Format: `<available_skills><skill><name/><description/><location/></skill>…</available_skills>`. Das Modell liest die `SKILL.md` bei Bedarf mit `read` oder `skills_read`.
- Budget: höchstens 150 Skills und 18.000 Zeichen. Bei Überlauf werden die Beschreibungen gekürzt (maximal 220 Zeichen) oder weggelassen.
- Was nicht passt, bleibt über `skills_search` auffindbar.
- [Code: src/skills/loading/skill-contract.ts, skill-prompt-limits.ts]

**Prompt-Caching** [Docs: docs/reference/prompt-caching.md]
- `cacheRetention` kennt die Werte `none`, `short` und `long`.
- Anthropic: `short` = 5 min; `long` = `ttl:"1h"`.
- Getrennte Breakpoints für Tool-Präfix und stabilen System-Präfix, dazu bis zu zwei Konversations-Breakpoints.
- OpenAI: `prompt_cache_key` = Session-ID plus Zähler der Reset-/Compaction-Grenzen.
- Warmhalten geschieht nicht mehr über den Heartbeat, sondern über die Automation-Kadenz, z. B. 55 min bei 1 h Retention.

**Session-Pruning (`cache-ttl`)** [Docs: docs/concepts/session-pruning.md; Code: src/agents/embedded-agent-runner/tool-result-truncation.ts]
- Anthropic-API: serverseitiges `clear_tool_uses_20250919` ab `max(50k, 0,3 × Fenster)` Tokens. Die letzten 3 Tool-Nutzungen bleiben erhalten.
- Clientseitig, sonst:
  - nach Ablauf der TTL werden Tool-Ergebnisse über 4.000 Zeichen auf Kopf und Ende mit je 1.500 Zeichen gekürzt;
  - ab 50 % Kontextauslastung werden alte Ergebnisse durch `[Old tool result content cleared]` ersetzt.

### 4.3 Sitzungen, Komprimierung, Memory-Flush

**Sitzungsschlüssel**

| Art | Schlüssel |
|---|---|
| Haupt | `agent:<id>:main` |
| DM nach `dmScope` | `per-peer`: `agent:<id>:direct:<peer>`; `per-channel-peer`: `agent:<id>:<channel>:direct:<peer>`; `per-account-channel-peer` |
| Gruppe | `agent:<id>:<channel>:group:<gid>` |
| Thread | Suffix `:thread:<id>` |
| Telegram-Topic | Suffix `:topic:<n>` |
| Sub-Agent | `agent:<id>:subagent:<uuid>` |
| Cron | neu pro Lauf |

[Code: src/routing/session-key.ts; Docs: docs/concepts/session.md]

**Lebenszyklus**
- **Reset:** Default `mode: "none"`. Optional `daily` (`atHour` 4, lokale Host-Zeit) oder `idle`. Heartbeat- und Cron-Turns verlängern die „Frische“ nicht [Code: src/config/sessions/reset-policy.ts].
- **Wartung:** `pruneAfter` 30 d, `maxEntries` 5.000 [Docs: docs/concepts/session.md].
- **Sitzungs-Tools:** `sessions_list`, `sessions_search`, `sessions_history`, `sessions_send`, `sessions_spawn`, `sessions_yield`, `session_status`. Die Sichtbarkeit `tools.sessions.visibility` steht per Default auf `all` [Docs: docs/concepts/session-tool.md; Code: src/plugin-sdk/session-visibility.ts].
- **Zustandsereignisse:** Ein dauerhaftes Log `session_state_events` (z. B. `goal_changed`, `child_spawned`, `compacted`) speist zusammengefasste Hinweise „Zustand hat sich geändert“ [Docs: docs/concepts/session-state.md].

**Komprimierung** [Code: packages/agent-core/src/harness/compaction/compaction.ts, src/agents/agent-settings.ts, src/agents/agent-compaction-constants.ts]
- Auslöser: `contextTokens > Fenster − reserve`.
- Reserve: `max(16.384, Boden 20.000)`, gedeckelt auf 25 % des Fensters. Bei 200k Fenster greift sie ab ≈ 180k Tokens.
- `keepRecentTokens` 20.000 bleiben wörtlich erhalten. Tool-Call/Result-Paare werden nie getrennt.
- Die Zusammenfassung hat feste Abschnitte: Goal / Constraints / Progress (Done, In Progress, Blocked) / Key Decisions / Next Steps / Critical Context.
- Weitere Regeln:
  - Summary-Modell ist das Sitzungsmodell samt Fallbacks, Thinking `low`.
  - Eingabe höchstens 160.000 Zeichen; lange Tool-Ergebnisse werden auf 2.000 Zeichen projiziert.
  - Ein `qualityGuard` prüft Struktur, offene Fragen und Bezeichner.
  - Höchstens 3 Overflow-Recovery-Versuche.
- Optional (`postCompactionSections`) werden benannte Abschnitte aus `AGENTS.md` nach der Komprimierung wieder eingespielt, höchstens 1.800 Zeichen [Code: src/auto-reply/reply/post-compaction-context.ts].
- **Modus-Default:** Im Code fällt er auf `default` zurück. Beim Materialisieren der Konfiguration wird aber `safeguard` gesetzt; die meisten Installationen landen daher faktisch in `safeguard` [Code: src/config/defaults.ts, src/config/materialize.ts].

**Memory-Flush vor der Komprimierung** [Code: extensions/memory-core/src/flush-plan.ts, src/auto-reply/reply/memory-flush-plan.ts, src/config/sessions/session-entry-state-format.ts]
- **Auslöser:**
  - Start bei `Fenster − reserve − softThresholdTokens`; `softThresholdTokens` = 4.000, gedeckelt auf `(Fenster − reserve)/2`.
  - Zwangs-Flush ab 2 MiB Transkript.
- **Ablauf:** ein stiller, agentischer Turn auf einer **privaten, abgekoppelten Kopie** der Konversation. Seine Prompts landen nicht im Transkript.
- **Prompt im Kern:**
  > „Pre-compaction memory flush. Store durable memories only in memory/YYYY-MM-DD.md … Treat … MEMORY.md, DREAMS.md, SOUL.md, and AGENTS.md as read-only … APPEND new content only … Do NOT create timestamped variant files … If nothing to store, reply with NO_REPLY.“
- **Regeln:**
  - `write` hängt im Flush nur an.
  - Läuft einmal pro Komprimierungszyklus (`memoryFlushCompactionCount`, `memoryFlushContextHash`).
  - Wird übersprungen bei CLI-Backends, Heartbeat-Turns und schreibgeschütztem Workspace.
  - Server-seitige Provider-Komprimierung ist für Flush-Turns aus.

**Context-Engine-Slot**
- `plugins.slots.contextEngine` steht per Default auf `legacy`.
- Ein Plugin implementiert `ingest → assemble → compact → afterTurn`.
- Mit `ownsCompaction: true` ersetzt es die eingebaute Komprimierung (Beispiel `lossless-claw`).
- [Docs: docs/concepts/context-engine.md]

### 4.4 Gedächtnis

**Dateien als Quelle der Wahrheit.** Der SQLite-Index ist abgeleitet: Er lässt sich neu aufbauen, und `memory reset` löscht nur abgeleitete Tabellen [Docs: docs/concepts/memory.md, docs/concepts/memory-builtin.md].

| Datei | Rolle | Automatisch im Prompt? |
|---|---|---|
| `MEMORY.md` | Kuratierte Fakten und Entscheidungen | Ja, nur in privaten Sitzungen; Provenienz-gefiltert |
| `USER.md` | Nutzermodell als Direktiven `<!-- observed: DATUM \| status: active -->` + Imperativ; Änderungen ersetzen den Eintrag statt anzuhängen | Ja, ≤ 4.000 Zeichen |
| `memory/JJJJ-MM-TT.md` (+ `-<slug>.md`) | Episodische Tagesnotizen | Nein. Nur bei `/new` bzw. `/reset` als „Startup Context“: 2 Tage, je ≤ 1.200 Zeichen, gesamt ≤ 2.800 [Code: src/auto-reply/reply/startup-context.ts]. Sonst nur per Tool |
| `DREAMS.md` | Tagebuch der Konsolidierung für Menschen | Nie |
| `memory/imports/{codex,claude-code,hermes}/` | Importierte Fremd-Erinnerungen | Nur indiziert |

[Docs: docs/concepts/memory-architecture.md, docs/concepts/user-model.md]

**Tools, die das Modell sieht** [Code: extensions/memory-core/openclaw.plugin.json, extensions/memory-core/src/memory-tool-contract.ts, packages/memory-host-sdk/src/host/read-file-shared.ts]
- `memory_search(query, maxResults=6, minScore=0.35, corpus=memory|wiki|all|sessions)`
  - Liefert Treffer mit `path`, `startLine`/`endLine`, `score`, `vectorScore`/`textScore` und einem Snippet von höchstens **700 Zeichen**.
  - Im Direktchat optional mit Zitat `Source: path#Lx-Ly`.
- `memory_get(path, from, lines)`: ohne Bereich 120 Zeilen bzw. 12.000 Zeichen, mit Fortsetzungshinweis.
- `intent`: legt Standing Intents an.
- **Es gibt kein eigenes Schreib-Tool.** Der Agent schreibt mit den normalen Datei-Tools, gesteuert durch die Regeln in `AGENTS.md`.

**Index und Suche** (`memory-core`; die Defaults sind im Code fest und nicht mehr konfigurierbar) [Code: src/agents/memory-search.ts, extensions/memory-core/src/memory/{hybrid,mmr,temporal-decay}.ts]
- **Indiziert werden** `MEMORY.md`, `USER.md`, `memory/**/*.md` und `extraPaths`.
- **Chunking:** 400 Tokens mit 80 Tokens Überlappung.
- **Speicher:**
  - SQLite **FTS5** (BM25; Tokenizer `unicode61`, für CJK `trigram`)
  - **sqlite-vec**, falls verfügbar, sonst Cosinus in-process
  - Embedding-Cache mit 50.000 Einträgen
- **Embeddings:**
  - Default-Provider ist **`openai`** (`text-embedding-3-small`).
  - Alternativen: Gemini, Voyage, Mistral, Bedrock, Ollama, LM Studio, OpenAI-kompatibel und `local`. `local` ist ein llama.cpp-Server mit `embeddinggemma-300m`, ca. 0,3 GB.
  - `provider: "none"` ergibt reinen FTS-Betrieb.
- **Ranking:**
  - Hybrid **0,7 Vektor / 0,3 Text**, Kandidaten-Multiplikator 4. BM25 wird pro Anfrage auf die beste Vektorbewertung skaliert.
  - Danach Recency-Decay mit **30 Tagen Halbwertszeit**, nur für datierte Notizen; `MEMORY.md`, `USER.md` und „evergreen“ veralten nicht.
  - Danach ein Importance-Faktor (`<!-- importance: 9 -->`) und anschließend **MMR** (λ 0,7).
- **Keyword-Suche:** Zuerst müssen alle Terme passen; nur bei null Treffern folgt ein Retry mit „irgendein Term“ und Sprachexpansion.
- **Reindexierung:**
  - beim Sitzungsstart, bei der Suche und per File-Watcher mit 1,5 s Debounce
  - zusätzlich Polling alle 30 s und Recovery-Rescan alle 5 min
  - Vollneubau atomar in einer Temp-DB
- **Timeout je Suche:** 30 s; danach gibt es Keyword-Teilergebnisse mit Kennzeichen `partial`.
- **Transkripte:** Auf persönlichen Installationen (`dmScope` main) steht `rememberAcrossConversations` per Default auf **true**. Dann werden auch Sitzungstranskripte indiziert, sobald der Agent 30 s im Leerlauf ist. Cron-, Heartbeat- und Dreaming-Transkripte nie [Code: packages/memory-host-sdk/src/host/config-utils.ts; Docs: docs/concepts/memory-builtin.md].
  - Die Doku nennt das „opt-in“. Das ist irreführend.

**Wer schreibt wann?**
1. **Der Agent**, nach den Regeln der `AGENTS.md`-Vorlage:
   - Tagesnotizen kommen nach `memory/`.
   - Direktiven kommen nach `USER.md`.
   - Fakten kommen nach `MEMORY.md`, und zwar „nur in der Hauptsitzung“.
   - Regel: erst lesen, dann schreiben.
   - [Docs: docs/reference/templates/AGENTS.md]
2. **Der Memory-Flush** vor der Komprimierung (§4.3).
3. **Der Hook `session-memory`.** Bei `/new`, `/reset` und automatischem Reset schreibt er die letzten **15** Nachrichten nach `memory/JJJJ-MM-TT-HHMM.md`. Das Onboarding aktiviert ihn [Code: src/hooks/bundled/session-memory/transcript.ts, src/commands/onboard-hooks.ts].
4. **Dreaming**, per Default **an** [Code: src/memory-host-sdk/dreaming.ts `enabled: parseBoolean(...) ?? true`, `DEFAULT_MEMORY_DREAMING_FREQUENCY = "0 3 * * *"`]:
   - Läuft um 03:00 als isolierter Automation-Turn ohne Zustellung.
   - Phasen light → REM → deep; nur deep schreibt `MEMORY.md`.
   - **Promotions-Hürden:**
     - Score ≥ 0,75
     - ≥ 3 Recalls aus ≥ 3 verschiedenen Anfragen
     - höchstens 10 Promotions pro Durchlauf
     - höchstens 25 % Verlust bisheriger Einträge
   - **Sicherungen:** Schreiben über einen Hash-Recheck und atomisches Rename. Die vorherige `MEMORY.md` liegt in SQLite, der Diff kommt nach `DREAMS.md`.
   - **Taint-Gate:** Kandidaten mit Herkunft `untrusted` oder `system` werden nie promotet.
   - [Docs: docs/concepts/dreaming.md]

**Weitere Bausteine**
- **Provenienz** steht als SQLite-Spalten je Chunk: `originClass` (`owner|agent|untrusted|system`), Sitzungsart, Supersession-Key.
  - Netzwerk-Tool-Ergebnisse „tainten“ den Rest des Turns.
  - `openclaw memory forget --session/--participant` entfernt eine Herkunftslinie.
  - [Docs: docs/concepts/memory-provenance.md]
- **Standing Intents:** ereignisgetriggerte „prospektive Erinnerungen“.
  - Matching über einen FTS-Keyword-Vorfilter **ohne Modellaufruf**.
  - Cooldown 24 h, 3 Auslösungen, Ablauf nach 90 Tagen.
  - [Code: src/standing-intents-model.ts, extensions/memory-core/index.ts]
- **Active Memory** (Plugin): ein blockierender Recall-Sub-Agent, der nur bei Erinnerungsabsicht ohne starken Treffer läuft. Timeout 15 s, Zusammenfassung ≤ 220 Zeichen [Code: extensions/active-memory/config.ts]. **unsicher**, ob er per Default aktiv ist: Das Manifest hat kein `enabledByDefault`, die Doku legt es aber nahe.
- **Memory-Slot:** Es ist immer genau ein Memory-Plugin aktiv (`plugins.slots.memory`, Default `memory-core`).
  - Alternativen: `memory-lancedb` (extern), `knowledge` (FTS + Claims mit Belegen + Git), Honcho (extern, als Dienst).
  - `memory-wiki` läuft als Begleiter neben dem Slot.
  - [Code: src/plugins/slots.ts; Docs: docs/plugins/knowledge.md, docs/concepts/memory-honcho.md]

**Kostenquellen**
- Embedding-Aufrufe, per Default bei OpenAI, auch für Transkripte.
- Ein Flush-Turn pro Komprimierung.
- Nächtliches Dreaming mit zwei Modellaufrufen.
- Active Memory, falls aktiviert.
- Trigger- und Intent-Matching kosten keine Modellaufrufe.

### 4.5 Proaktivität: Heartbeat, Automations, Hooks, Trigger

**Vom Heartbeat zur Automation**
- **Früher:** `agents.defaults.heartbeat` mit Intervall, `HEARTBEAT.md` und `HEARTBEAT_OK`-Token.
- **Im analysierten Stand:** zurückgezogen. Doctor wandelt jeden Heartbeat-Monitor in einen normalen, editierbaren Automation-Job um. Dabei bleiben Run-Historie und Scratch erhalten; `HEARTBEAT.md` wird archiviert und entfernt [Docs: docs/gateway/heartbeat.md].
- **unsicher:** in welchem Release genau. 2026.9.7 erwähnt die Migration „retired heartbeat … settings through 2026.9.5“; spätere Release Notes reden weiter von „heartbeats“ als Lauf-Art [Docs: CHANGELOG/2026.9.7.md, CHANGELOG/2026.10.8.md].

**Default-Job „Proactive check (<agent>)“** [Code: src/cron/default-proactive-job.ts]

| Feld | Wert |
|---|---|
| `schedule` | `{kind:"every", everyMs: 30 min}`; mit Anthropic-OAuth 60 min, mit API-Key 30 min [Code: extensions/anthropic/config-defaults.ts] |
| `sessionTarget` | `session:<Hauptsitzung>`, damit der volle Kontext da ist |
| `idleOnly` | `true` |
| `wakeMode` | `"now"` |
| `payload` | `{kind:"agentTurn", skipIfScratchEmpty:true}` |
| `delivery` | `{mode:"announce", target:"owner"}` |

- **Prompt:** „Review your automation scratch checklist and relevant session context. Do not infer or repeat old tasks from prior conversations. Take useful action only when needed. Use NO_REPLY when there is nothing to tell the user.“
- **Ergebnis-Rückmeldung:** über das Tool `heartbeat_respond(notify=false|true, notificationText)`, mit Fallback `NO_REPLY`. `HEARTBEAT_OK` wird nur noch aus Altdaten entfernt (Ack ≤ 300 Zeichen) [Code: src/auto-reply/heartbeat.ts].
- **Scratch** ersetzt `HEARTBEAT.md`. Er liegt je Job in SQLite (≤ 256 KiB) und wird mit Revisionen geschützt; der Agent pflegt ihn über `scratch_get`/`scratch_set`/`record_result`. Ein explizit leerer Scratch überspringt den Lauf [Docs: docs/automation/cron-jobs/payloads.md].
- **Entfallen:** heartbeat-spezifische Duplikat-Unterdrückung und das Überspringen bei fehlender Route. Aktive Stunden laufen jetzt pro Job über `activeHours` [Docs: docs/gateway/heartbeat.md, docs/automation/cron-jobs/schedules.md].
- **Erstellung:** einmalig durch das Setup. Start und Reload legen den Job nie neu an, Löschen ist dauerhaft.

**Automations-Engine** [Docs: docs/automation/cron-jobs/*.md; Code: src/cron/, src/agents/tools/cron-tool-schema.ts]
- **Speicher:** `state/openclaw.sqlite`, Tabellen `cron_jobs` und `cron_run_receipts`.
- **Zeitpläne:**
  - `at`: ISO oder relativ; ohne Zeitzone in UTC; nach Erfolg gelöscht.
  - `every`.
  - `cron`: 5/6 Felder via croner mit IANA-`tz`; volle Stunden werden um bis zu 5 min gestaffelt.
  - `event`: z. B. MCP-Events.
  - `on-exit`, `stream`.
- **Bedingungs-Trigger:** JS-Gate-Skripte (30 s, 5 Tool-Calls, 16 KB State). Das Modell wird nur aufgerufen, wenn `fire: true`.
- **Payloads:**
  - `systemEvent`: in die Hauptsitzung
  - `agentTurn`
  - `command`: Shell ohne Modell, 10 min
  - `script`: headless Code Mode, 300 s
- **Ziel-Sitzung:**
  - `main`
  - `isolated`: frisch pro Lauf, `cron:<jobId>`
  - `current`: Kind der erstellenden Konversation
  - `session:<key>`: persistent
- **Betrieb:**
  - Watchdog 60 min für isolierte Turns.
  - Backoff bei Fehlschlägen: 30 s/60 s/5 m/15 m/60 m. Auto-Disable nach 10 Fehlschlägen in Folge. Fehleralarme ab 2 Fehlschlägen.
  - Historie 7 Tage, höchstens 2.000 Läufe je Job.
  - Verpasste Läufe werden per Default nachgeholt.
- **Agent-Tool `automations`** (Alias `cron`):
  - Aktionen `add|update|remove|run|runs|list|wake|scratch_get|scratch_set|record_result|next_check`.
  - Vorgabe: Den Zeitplan in Klartext mit dem Nutzer bestätigen und nach dem Anlegen einen Testlauf erzwingen [Docs: docs/automation/cron-jobs/how-it-works.md].
- **Zustellung:**
  - `announce` (Default), `webhook` oder `none`.
  - Ziel `owner` folgt nie dem letzten Gruppenchat.
  - `NO_REPLY` wird als `deliverySuppressionReason` protokolliert.

**Hooks und externe Trigger**
- **Interne Hooks:** `HOOK.md` + Handler.
  - Events: `command:new|reset|stop`, `session:compact:before|after`, `agent:bootstrap`, `gateway:startup`, `message:received|sent` …
  - Gebündelt: `session-memory`, `boot-md` (führt `BOOT.md` beim Gateway-Start aus, per Default aus), `command-logger`, `bootstrap-extra-files`, `compaction-notifier`.
  - Workspace-Hooks brauchen ein explizites Opt-in.
  - [Docs: docs/automation/hooks/*.md]
- **HTTP-Webhooks** (per Default aus):
  - `POST /hooks/wake`: System-Event in die Hauptsitzung.
  - `POST /hooks/agent`: isolierter oder persistenter Lauf mit Zustellung.
  - `POST /hooks/<name>`: Mapping mit Template oder JS-Transform.
  - Token nur im Header; Query-Tokens werden abgewiesen. Externe Inhalte werden umhüllt.
  - [Docs: docs/automation/cron-jobs/webhooks.md, docs/gateway/config-hooks.md]
- **Gmail:** Pub/Sub über `gog gmail watch serve` → Hook. Setup per `openclaw webhooks gmail setup`, per Default über Tailscale Funnel. Empfohlen ist ein eingeschränkter, sandboxed `mail_reader`-Agent [Docs: docs/automation/cron-jobs/gmail.md].
- **IMAP-Plugin** (per Default aus):
  - IDLE oder Polling.
  - Absender-Allowlist + DMARC-Prüfung; Mails älter als 48 h werden verworfen.
  - Jede Mail läuft in einer eigenen isolierten Sitzung.
  - [Docs: docs/automation/imap.md]
- **MCP Events:** HMAC-signierte Callbacks lösen Automations mit `schedule.kind:"event"` aus [Docs: docs/automation/mcp-events.md].
- **„Standing Orders“** sind eine reine Konvention: „Programme“ in `AGENTS.md` plus zugehörige Jobs [Docs: docs/automation/standing-orders.md].

### 4.6 Kanäle

**Adaptervertrag** [Code: src/channels/plugins/types.plugin.ts, src/channels/plugins/outbound.types.ts; Docs: docs/plugins/sdk-channel-plugins.md]
- `ChannelPlugin` verlangt nur `config`. Alles andere ist optional:
  - `pairing`, `security`, `groups`, `mentions`, `allowlist`
  - `outbound`: `sendText`/`sendMedia`/`sendPoll`, `textChunkLimit`, `chunkerMode`
  - `streaming`, `threading`, `gateway`, `status`, `doctor`, `secrets`, `bindings`, `agentPrompt` (Formatierungshinweise)
- Kanäle implementieren **keine** eigenen Sende-Tools. Das Kern-Tool `message` hat ein geschlossenes Vokabular von 57 Aktionen (send, react, edit, poll, pin, thread-create …) [Code: src/channels/plugins/message-action-names.ts].
- Der Kern besitzt Session-Key-Form, Dispatch, Allowlists und Pairing-Speicher. Der Inbound-Pfad im SDK lautet `ingest → classify → preflight → resolve → record → dispatch → finalize` [Docs: docs/plugins/sdk-channel-inbound.md].

**Umfang**
- 28 Produktionskanäle im Repo plus `qa-channel` [Code: src/channels/bundled-channel-ids.generated.ts].
- Der Doku-Katalog führt 33 Einträge, inkl. WebChat und externer Plugins (WeChat, QQ …) [Docs: docs/channels/index.md].
- Die meisten Kanäle sind „official plugins“, die bei Bedarf installiert werden. **unsicher:** welche genau im Paket gebündelt sind; Doku und `bundledDist` widersprechen sich.

| Kanal | Bibliothek / Transport |
|---|---|
| Telegram | grammY 1.46. Long Polling per Default, also **kein öffentlicher Port nötig** |
| WhatsApp | Baileys 7.0.0-rc14 (WhatsApp Web) |
| Discord | eigener REST- und Gateway-Client auf `ws` |
| Slack | Bolt, Socket Mode |
| Signal | `signal-cli`-Daemon (JSON-RPC + SSE) |
| iMessage | `steipete/imsg`; BlueBubbles entfernt |
| Matrix | matrix-js-sdk mit Rust-Crypto |
| Teams | `@microsoft/teams.apps` |
| A2A | Agent-zu-Agent-Protokoll, Linux Foundation |

[Code: extensions/*/package.json; Docs: docs/channels/*.md]

**Eingangsverarbeitung**
- **Umschlag:** `MsgContext` mit `Body`, `BodyForAgent`, `From`, `SessionKey`, `AccountId`, `ReplyToBody`, `InboundHistory` …; Gruppenverlauf wird in Marker eingerahmt [Code: src/auto-reply/templating.ts; Docs: docs/concepts/messages.md].
- **Dedupe:** im Speicher, TTL 20 min, ≤ 5.000 Einträge; Telegram und Discord zusätzlich mit dauerhaftem Ingress [Code: src/auto-reply/reply/inbound-dedupe.ts].
- **Debounce:**
  - global 0, Telegram 300 ms;
  - Texte ≥ 4.000 Zeichen sammeln Fragmente 1.500 ms, Weiterleitungen 1.000 ms;
  - Medien und Befehle umgehen den Debounce.
  - [Code: src/auto-reply/inbound-debounce.ts, extensions/telegram/src/bot-handlers.inbound-buffer.ts]
- **Medien:**
  - Größenlimit je Kanal (Telegram 100 MB, WhatsApp 50, Signal 8 …).
  - Audio wird vor dem Lauf transkribiert und als `[Audio]`-Block mit `{{Transcript}}` eingesetzt. Default ist nur der erste Anhang [Docs: docs/nodes/media-understanding.md].
  - Link-Verständnis ist opt-in: ≤ 3 Links, SSRF-geschützt [Code: src/link-understanding/].

**Zugangskontrolle**
- **DMs:** `dmPolicy` steht per Default auf **`pairing`**; weitere Werte sind `allowlist`, `open` (verlangt `"*"`) und `disabled` [Code: src/security/dm-policy-shared.ts].
- **Pairing-Code:** 8 Zeichen aus einem Alphabet ohne 0/O/1/I. Gültig 1 h, höchstens 3 offene Anfragen pro Kanal-Account [Code: src/pairing/pairing-store-model.ts].
  - Freigabe mit `openclaw pairing approve <ch> <CODE>`. Die erste Freigabe setzt auch `commands.ownerAllowFrom`, damit ist der **Owner** festgelegt [Docs: docs/channels/pairing.md].
- **Gruppen:**
  - `groupPolicy` steht per Default auf `allowlist` [Code: src/config/runtime-group-policy.ts].
  - `requireMention` ist per Default `true` [Code: src/config/group-scope-tree.ts].
  - Dazu Regex-`mentionPatterns`, implizite Erwähnung bei Antwort auf den Bot, ein Gruppen-Verlaufspuffer von 50 Nachrichten [Code: src/auto-reply/reply/history-limit.ts] und Bot-Loop-Schutz (20 Events/60 s).

**Ausgang** [Code: src/auto-reply/chunk.ts; Docs: docs/concepts/streaming.md, docs/concepts/markdown-formatting.md]
- **Chunk-Limits:** generisch 4.000 Zeichen; Discord 2.000 Zeichen bzw. 17 Zeilen; Slack 8.000; IRC 350. Code-Fences werden nie geteilt.
- **Markdown:** einmal in ein Zwischenformat (IR) geparst, dann je Kanal gerendert: Telegram als HTML, Slack als mrkdwn, Signal mit Style-Ranges. Tabellen werden je Kanal `code`, `bullets` oder `block`.
- **Streaming:**
  - Block-Streaming per Default aus.
  - Vorschau-Modi `off|partial|block|progress`; Telegram, Discord und Slack nutzen per Default `progress`, eine live editierte Fortschrittsnachricht.
  - Telegram arbeitet mit `sendMessage` + `editMessageText`.
- **Tippanzeige:** sofort in DMs, Refresh alle 6 s.
- **Ack-Reaktion:** Identitäts-Emoji des Agenten, sonst 👀; per Default nur bei Gruppen-Erwähnungen [Code: src/channels/ack-reactions.ts].
- **Schweigen:** Ein exaktes `NO_REPLY` beendet den Turn ohne Antwort.

**Mehrfachkonten:** `channels.<ch>.accounts.<id>`, unterstützt von 17 Kanälen. Bindings ohne `accountId` gelten nur für den Default-Account [Docs: docs/gateway/config-channels/shared-policies.md].

### 4.7 Multi-Agent

**Mehrere Agenten**
- Jeder Agent hat einen eigenen Workspace, ein eigenes `agentDir` (Auth-Profile) und einen eigenen Session-Store [Docs: docs/concepts/multi-agent.md].
- **Routing über Bindings,** spezifischste Regel zuerst [Code: src/routing/resolve-route.ts]:
  1. Peer
  2. Parent-Peer (Thread)
  3. Peer-Wildcard
  4. Guild + Rollen
  5. Guild
  6. Team
  7. Account
  8. Kanal
  9. Default
- Felder einer Binding gelten mit UND. Bei mehreren Agenten ohne Owner wird nicht stillschweigend der erste genommen; das Routing scheitert mit `AGENT_SELECTION_REQUIRED`.
- Fertiges Preset: `openclaw agents team create` legt Coordinator, Researcher, Writer und Reviewer an.

**Sub-Agents**
- `sessions_spawn(task, agentId?, model?, mode=run|session, context=isolated|fork, sandbox=inherit|require, worktree?, outputSchema?, …)`. Ergebnisse werden eingesammelt mit `sessions_yield` oder `agents_wait` [Code: src/agents/tools/sessions-spawn-tool.ts; Docs: docs/tools/subagents/tool-reference.md].
- **Isolation:**
  - Der Sub-Agent sieht nur `AGENTS.md`.
  - Immer verboten: `gateway`, `automations`, `message`, `sessions_send`, `conversations_*`.
  - An der Tiefengrenze zusätzlich verboten: `sessions_spawn`, `sessions_list` usw.
  - Er erbt das Modell, aber nicht das Konto der Eltern-Sitzung.
  - [Code: src/agents/agent-tools.policy.ts `SUBAGENT_TOOL_DENY_ALWAYS`/`_LEAF`]
- **Grenzen:** Tiefe ≤ 5, aktive Kinder ≤ 5 je Sitzung, gleichzeitige Läufe ≤ 8, Archivierung nach 60 min [Code: src/config/agent-limits.ts].
- **Rückmeldung:**
  - Das Ergebnis wandert stufenweise nach oben.
  - Ein Kind, das `NO_REPLY` liefert, zählt nicht als Ergebnis.
  - `sessions_yield` beendet den Turn; der Nachfolger startet, wenn alle Kinder fertig sind.
  - [Docs: docs/tools/subagents/announce.md, docs/concepts/subagent-yield-handoff.md]

**Agent-zu-Agent und Team-Betrieb**
- **`sessions_send`:** Modi `steer|followup|notify|resume`, eine Antwort pro Sendung. Automatisches Ping-Pong wurde in 2026.10.1 entfernt [Docs: docs/concepts/session-tool.md, docs/releases/2026.10.1.md].
- **Kanäle für fremde Agenten:** A2A 1.0 (`/a2a/v1`, Bearer-Token pro Peer) und Reef, ein Ende-zu-Ende-verschlüsseltes Relay für Agenten verschiedener Eigentümer [Code: extensions/a2a/src/; Docs: docs/channels/reef.md].
- **Team-Betrieb:** Ein Gateway ist eine Vertrauensdomäne; mehrere Menschen pro Sitzung mit `gateway.roles`. Mandanten, die sich nicht vertrauen, brauchen getrennte Gateways [Docs: docs/start/teams.md, docs/concepts/multi-user.md].

### 4.8 Skills und Erweiterungen

**`SKILL.md`** (AgentSkills-kompatibel) [Code: src/skills/types.ts, src/skills/loading/frontmatter.ts; Docs: docs/tools/skills.md]
- **Pflichtfelder:** `name`, `description`.
- **Optionale Felder:**
  - `user-invocable` (Default `true`): wird zum Slash-Command.
  - `disable-model-invocation`.
  - `command-dispatch: tool`: Der Slash-Command ruft direkt ein Tool, ohne Modell.
- **Metadaten** `metadata.openclaw`:
  - `requires.{bins, anyBins, env, config}`
  - `os[]`, `always`, `primaryEnv`
  - `install[]` mit den Arten `brew|node|go|uv|download`; Download höchstens 256 MiB, optional mit sha256.
- Das alte Präfix `metadata.clawdbot` wird nicht mehr gelesen.

**Ladereihenfolge** (höchste Priorität zuerst) [Code: src/skills/loading/workspace-skill-sources.ts]
1. `<workspace>/skills`
2. `<workspace>/.agents/skills`
3. `~/.agents/skills`
4. `~/.openclaw/skills` (managed)
5. Workshop-Skills
6. Gebündelt (+ Custodian)
7. `extraDirs` und Plugin-Skills

- Discovery-Grenzen: ≤ 6 Ebenen tief, ≤ 300 Kandidaten je Wurzel, `SKILL.md` ≤ 256 KB.
- Ein Watcher arbeitet mit Events und zusätzlich einem 30-min-Rescan. Laufende Sitzungen sehen Änderungen beim nächsten Turn.

**Freischaltung (Gating):**
- **Grundregeln:**
  - `bins` müssen im PATH liegen, `env` muss gesetzt sein, `config`-Pfade müssen truthy sein.
  - `os` filtert hart.
  - `always` umgeht die Prüfungen, aber nicht `os`.
- **Hinweise:**
  - Die Prüfung läuft auf dem **Host**. In der Sandbox muss das Binary zusätzlich vorhanden sein.
  - Remote-macOS-Nodes können macOS-Skills auf einem Linux-Gateway freischalten.
  - Per-Agent-Allowlists (`agents.entries.<id>.skills`) sind laut Doku **keine Sicherheitsgrenze**.
- **Env-Injektion:** `skills.entries.<key>.env`/`apiKey` werden nur für den Lauf in `process.env` gesetzt und danach zurückgesetzt [Docs: docs/tools/skills.md, docs/tools/skills-config.md].

**Skill Workshop** (Selbstlernen)
- `skills.workshop.autonomous.mode` steht per Default auf `"auto"`. Nach substanzieller Arbeit legt ein Hintergrund-Review eigene Skills an oder aktualisiert sie.
- Skills, die 30 Tage ungenutzt bleiben, werden archiviert; Obergrenze 40 KB je Skill.
- [Code: src/skills/workshop/config.ts]

**ClawHub**
- **Befehle:** `openclaw skills install @owner/<slug>` (Workspace oder `--global`), `update`, `verify`; Lockfile `.clawhub/lock.json` [Docs: docs/tools/skills.md].
- **Vertrauensprüfung beim Installieren** [Code: src/infra/clawhub-install-trust.ts, src/skills/lifecycle/clawhub-install-core.ts]:
  - Der Server liefert pro Release ein Urteil (`clawhub.skill.verify.v1`).
  - **Blockiert** bei Moderation blocked/quarantined/revoked oder Scan `malicious`.
  - **Warnung** bei offenen oder veralteten Scans.
  - „Suspicious“ verlangt eine getippte Bestätigung.
  - Der sha256 des Archivs wird geprüft.
- **Server-seitig:**
  - statische Muster, LLM-Risikoreview, VirusTotal (wenn konfiguriert)
  - Mindestalter des GitHub-Kontos 14 Tage
  - [Docs: docs/security/THREAT-MODEL-ATLAS.md]
- **Grenzen:**
  - Eine lokale kryptografische Signaturprüfung habe ich **nicht** gefunden; `signature` und `provenance` werden nur durchgereicht [Code: src/infra/clawhub-skills.ts].
  - Laut Doku blockiert OpenClaw beim Installieren selbst keinen gefährlichen Code. Der lokale Scanner läuft nur bei `openclaw security audit --deep` [Docs: docs/gateway/security/tool-permissions.md; Code: src/skills/security/scanner.ts].
  - Das eigene Threat Model stuft das Restrisiko als **„High“** ein [Docs: docs/security/THREAT-MODEL-ATLAS/persistence.md].

**Plugins**
- **Zwei Formen:**
  - **Native Plugins:** `openclaw.plugin.json` + Modul, **in-process** und vollständig vertraut [Docs: SECURITY.md].
  - **Bundles:** Codex-, Claude- oder Cursor-Layouts mit Skills, Hooks, MCP- und LSP-Servern, ohne Code-Import. VISION.md bevorzugt Bundles wegen der „better security boundaries“.
- **Registrierung:** Provider, Kanäle, Tools, Embedding-, Speech- und Websuche-Provider, CLI-Backends, Harnesses.
- **Hooks:** etwa 41 typisierte Hooks, u. a. `before_prompt_build`, `before_tool_call` (umschreiben, blockieren, Freigabe verlangen), `before_message_write`, `before_channel_ingress`, `before/after_compaction`, `subagent_*`, `before_install`.
- Fremde Plugins brauchen für Prompt- und LLM-I/O-Hooks `allowConversationAccess`.
- [Code: src/plugins/hook-types.ts; Docs: docs/plugins/hooks/reference.md]

### 4.9 Werkzeuge

**Katalog**
- 67 konfigurierbare Kern-Tools in Gruppen [Code: src/agents/tool-catalog.ts]:
  - `fs`: `ls`, `read`, `write`, `edit`, `apply_patch`
  - `runtime`: `exec`, `process`, `code_execution`, `secrets`
  - `web`: `web_search`, `web_fetch`, `x_search`
  - `memory`
  - `sessions`
  - `ui`: `browser`, `canvas`, …
  - `messaging`: `message`
  - `automation`: `automations`, `gateway`, `plugins`
  - `nodes`: `nodes`, `computer`, `mobile_ui`
  - `agents`: Goals, `ask_user`, Skills-Tools
  - `media`
- **Profile:**
  - `minimal`
  - `coding`: ohne Browser, Message und Nodes
  - `messaging`
  - `full`
  - Das lokale Onboarding setzt `full`, wenn nichts gesetzt ist [Docs: docs/gateway/config-tools/tool-policy.md].

**Gegen Tool-Bloat**
- **Tool Search:** ein begrenztes Verzeichnis plus `tool_search`/`tool_describe`/`dispatch_action`; automatisch an, wenn nicht konfiguriert [Docs: docs/tools/tool-search.md].
- **Code Mode:** Das Modell sieht nur `exec({code})` und `wait` und ruft Tools als asynchrone JS-Funktionen auf.
  - Aktiv bei Modellen mit `compat.codeMode: "preferred"`.
  - Executor `node:vm`, laut Doku „not a security boundary“. Die gehärtete Alternative ist `quickjs`.
  - [Code: src/agents/code-mode-runtime.ts; Docs: docs/tools/code-mode.md]

**`exec` / `process`** [Code: src/agents/bash-tools.exec-runtime.ts, src/agents/bash-tools.schemas.ts; Docs: docs/tools/exec.md]
- `yieldMs` 10 s (10 ms–120 s): Danach läuft der Befehl im Hintergrund weiter, und das Tool liefert eine `process`-Session.
- `timeoutSeconds` 1.800.
- Ausgabe ≤ 200.000 Zeichen.
- PTY optional.
- `host=auto|sandbox|gateway|node`; `auto` = Sandbox, falls aktiv, sonst Gateway.
- Abschluss-Turns: Hintergrundprozesse lösen beim Ende einen Turn mit „[OpenClaw exec completion]“ aus [Code: src/auto-reply/heartbeat.ts].

**Browser** (gebündeltes Plugin, per Default an) [Docs: docs/tools/browser-control.md; Code: extensions/browser/src/browser-tool-action-facts.ts]
- **Steuerung:** CDP + Playwright (`playwright-core` 1.64.1).
- **Profile:**
  - `openclaw`: verwaltet, isoliert, headless per Default
  - `user`: echtes Chrome über Chrome DevTools MCP
  - `chrome`: über eine Extension
  - Remote-CDP: Browserless o. ä.
- **Snapshots:** `ai` mit Refs wie `f1e12` über Playwright `aria-ref`; `aria` (`axN`); Rollen-Snapshot (`e12`); `[new]`-Deltas.
- **Aktionen** funktionieren **nur über Refs**, nicht über CSS-Selektoren.
- **Umfang:** 25 Tool-Aktionen und 14 `act`-Arten (click, type, fill, drag, evaluate …). SSRF-Schutz blockiert private Netze; die Doku nennt ein Soft-Limit von 8 Tabs.

**Nodes**
- Geräte verbinden sich mit `role:node`. Ein Befehl muss vom Gerät angeboten, freigegeben **und** in der Gateway-Allowlist stehen [Docs: docs/nodes/command-policy.md].
- **Default-Befehle:**
  - iOS: `location.get`, `contacts.search`, `calendar.events`, `reminders.list`, `photos.latest`, `system.notify`
  - Android zusätzlich: Benachrichtigungen, Anrufliste, `mobile.ui.*`
- **Opt-in:** `camera.snap/clip`, `screen.record`, `sms.send`, `calendar.add`.
- **Desktop-Nodes:** `system.run`, `browser.proxy`, `computer.act`.
- **Canvas:** HTML-Widgets in macOS-Panels. A2UI ist zurückgezogen.

**Websuche und Web-Fetch**
- **`web_search`:**
  - Cache 15 min, 5 Ergebnisse [Code: src/agents/tools/web-shared.ts].
  - Auto-Erkennung über vorhandene API-Keys in dieser Reihenfolge: Brave → MiniMax → Gemini → Grok → Kimi → Perplexity → Firecrawl → Exa → Tavily → Parallel → DeepSeek → SearXNG.
  - Ohne Provider **fehlt das Tool ganz**. Ausnahmen: OpenAI-Responses-Modelle nutzen native Websuche, DuckDuckGo nur explizit.
  - [Docs: docs/tools/web.md]
- **`web_fetch`:** lokal, Readability → Markdown, ohne JS. ≤ 20.000 Zeichen, 750 KB, 30 s, 3 Redirects, private Netze gesperrt, Firecrawl als Fallback [Code: src/agents/tools/web-fetch.ts].

**MCP**
- **Client:** `mcp.servers.<name>` mit stdio, SSE oder Streamable HTTP und OAuth. Tools erscheinen als `<server>__<tool>`; Server-Instruktionen ≤ 2.000 Zeichen; Neustart-Backoff 30 s–10 min [Docs: docs/tools/mcp.md; Code: src/agents/agent-bundle-mcp-names.ts].
- **Server:** `openclaw mcp serve` stellt Konversationen für Claude Code und Codex bereit.
- **Relay:** `mcp-relay` öffnet einen Ausgangs-Tunnel zu `mcp.openclaw.ai` [Docs: docs/cli/mcp.md, docs/plugins/mcp-relay.md].

### 4.10 Modelle

**Provider**
- 62 Provider-Plugins und 12 Wire-APIs (OpenAI Completions/Responses, ChatGPT-Responses, Anthropic Messages, Google, Bedrock, Ollama …) [Code: extensions/*/openclaw.plugin.json, packages/llm-core/src/model-data.ts].
- Fallback-Default im Code: `openai/gpt-6-astra` mit 200k Kontext [Code: src/agents/defaults.ts].

**Rollen und Auswahl**
- Rollen: `model.primary`, `model.fallbacks[]`, `imageModel`, `pdfModel`, `utilityModel` (Titel, Zusammenfassungen) und `decisionModel` (typisierte Entscheidungen); dazu `modelPolicy.allow` [Docs: docs/concepts/models.md].
- Eine vom Nutzer per `/model` gewählte Auswahl fällt **nicht** automatisch zurück. Automatische Fallbacks werden markiert, und das Primärmodell wird erneut geprüft.

**Auth-Profile**
- Arten: `api_key`, `oauth`, `token`; gespeichert in SQLite (`auth_profile_store`).
- Rotation in dieser Reihenfolge: OAuth → Token → Key, innerhalb der Art nach ältestem `lastUsed`.
- Sitzungen bleiben aus Cache-Gründen bei einem Profil.
- Cooldowns: transient 30 s → 1 min → 5 min; Billing 10 min mit Verdopplung bis 24 h. Rate-Limits richten sich nach `Retry-After`.
- [Code: src/agents/auth-profiles/usage-failure-state.ts; Docs: docs/concepts/model-failover.md, docs/concepts/oauth.md]

**Abos**
- **OpenAI:** ChatGPT/Codex-OAuth per PKCE, „explicitly supported“.
- **Anthropic:** API-Key, Wiederverwendung der Claude CLI oder `setup-token`.
  - Doku: „Anthropic staff told us this usage is allowed again“; für die Produktion empfiehlt sie trotzdem API-Keys [Docs: docs/concepts/oauth.md].
  - Extern wird berichtet, Anthropic habe ab 04.04.2026 Abo-Nutzung in Drittanbieter-Harnesses gesperrt bzw. separat abgerechnet [Sekundär: heise, 04/2026; MindStudio, 04/2026].
  - **unsicher:** Lage heute. Laut Repo-Doku hat ein Anthropic-Support-Update vom 15.06.2026 ein separates Agent-SDK-Guthaben pausiert; `claude -p` zieht seither aus den Abo-Limits [Docs: docs/providers/anthropic.md].
- **Google:** keine neue Gemini-CLI- oder Antigravity-OAuth-Einrichtung mehr, weil Google den Consumer-Login am 18.06.2026 beendet hat [Docs: docs/providers/google.md].

**Thinking und Usage**
- Thinking-Stufen `off…max/ultra`, je Provider auf das Unterstützte geklemmt.
- Usage: Quoten-Fenster der Provider („X % left“), `/usage`, geschätzte Kosten aus dem Katalog [Docs: docs/tools/thinking.md, docs/concepts/usage-tracking.md].

### 4.11 Sicherheit

**Vertrauensmodell** [Docs: SECURITY.md]
- „Local-first agent infrastructure for trusted operators“, ausdrücklich kein Mandanten-Isolationssystem.
- Wer sich am Gateway authentifiziert, ist vertrauenswürdiger Operator.
- Session-Keys sind Routing, keine Autorisierung.
- Plugins laufen im Prozess und sind vertraut. Das Modell ist **nicht** vertraut.
- **Keine Schwachstelle** sind laut SECURITY.md u. a.: Prompt Injection ohne Umgehung einer Policy-Grenze, bösartige Plugins nach der Installation, öffentliches Exponieren entgegen der Doku und Host-Exec bei ausgeschalteter Sandbox.

**Gateway**

| Thema | Befund | Beleg |
|---|---|---|
| Port | 18789 | [Code: src/config/paths.ts] |
| Bind | Default `loopback`; im Container `auto` → `0.0.0.0` | [Code: src/gateway/net.ts] |
| Auth | Token; fehlt es, wird beim Start ein **flüchtiges** 48-Hex-Token erzeugt | [Code: src/gateway/startup-auth.ts] |
| Fail-closed | Nicht-Loopback ohne Auth wird verweigert; Funnel verlangt Passwort; Control UI außerhalb von Loopback braucht `allowedOrigins` | [Code: src/gateway/server-runtime-config.ts] |
| Rate-Limits | 10 Fehlversuche/60 s → 5 min Sperre; 128 nicht authentifizierte Sockets pro IP | [Code: src/gateway/auth-rate-limit.ts] |
| Scopes | `operator.read/write/admin/pairing/approvals/talk.secrets …` | [Docs: docs/gateway/operator-scopes.md] |
| Shared-Secret-HTTP | Bekommt per Design **alle** Scopes | [Docs: SECURITY.md] |

**Sandbox** [Code: src/agents/sandbox/config.ts, src/agents/sandbox/constants.ts; Docs: docs/gateway/sandboxing/*.md]
- **Defaults:** `mode: "off"`, `scope: "agent"`, `workspaceAccess: "none"`. Mit `non-main` sind alle Sitzungen außer `agent:<id>:main` sandboxed, also auch Gruppen.
- **Docker-Härtung:** `network: none`, read-only Root, `capDrop: ALL`, `pidsLimit` 1024. Das Image `openclaw-sandbox:trixie-slim` wird lokal gebaut.
- **Backends:** docker, podman, ssh, openshell, crabbox.
- **Bleibt auf dem Host:** Gateway, native Plugins und MCP-Tools.
- **„Elevated“:** ein Exec-Notausgang aus der Sandbox auf den Host. Verlangt `tools.elevated.enabled` und eine Absender-Allowlist [Docs: docs/tools/elevated.md].

**Tool-Policy**
- Profil → Provider-Profil → global allow/deny (deny gewinnt) → Provider → Agent → Agent-Provider → Sandbox-Tools → Sub-Agent.
- `toolsBySender` erlaubt Regeln pro Absender.
- Nicht-Owner bekommen nie `cron` oder `gateway`.
- [Docs: docs/tools/multi-agent-sandbox-tools.md, docs/gateway/config-tools/tool-policy.md]

**Exec-Freigaben**
- **Defaults:** `security: "full"`, `ask: "off"`, Fallback `deny`, Approval-Timeout 30 min [Code: src/infra/exec-approvals-config.ts].
- **Modi:** `deny|allowlist|ask|auto|full`; bei `auto` entscheidet ein LLM-Reviewer.
- **Bindung:** Eine Freigabe bindet cwd, argv, env und den aufgelösten Pfad.
- **Weitere Regeln:**
  - `strictInlineEval` (Default false) erzwingt eine Freigabe für `python -c` u. ä.
  - Safe-Bins: `cut uniq head tail tr wc`.
- **Freigabe per Chat:** `/approve <id> allow-once|allow-always|deny` [Docs: docs/tools/exec-approvals.md, exec-approvals-advanced.md].

**Geheimnisse**
- **SecretRefs:** `env|file|exec|store`; als Exec-Quellen 1Password `op`, Bitwarden, Vault, pass, sops [Docs: docs/gateway/secrets/secretref-contract.md].
- **Secret-Store:** in SQLite, **nicht verschlüsselt**; er verlässt sich auf die Rechte 0600/0700.
- **Secret-Egress-Proxy** (opt-in, seit spätestens 2026.8.1) [Docs: docs/gateway/secrets/secret-store-and-egress.md; Code: src/secrets/sentinel.ts]:
  - Subprozesse sehen nur Sentinels der Form `oc-sent-v2.….end`.
  - Ein loopback-MITM-Proxy mit eigener CA setzt das echte Geheimnis nur für die exakt gebundenen Hosts ein.
  - Gilt nicht für Sandbox-Exec und Remote-Nodes.
- **Logs:** Datei-Logs sind roh (0600). Transkripte enthalten Geheimnisse im Klartext [Docs: docs/gateway/security/secrets-and-storage.md].

**Prompt Injection**
- Externe Inhalte (Mail, Webhook, Browser, Websuche, Web-Fetch) werden umhüllt: `<<<EXTERNAL_UNTRUSTED_CONTENT id="<zufällig>">>> … <<<END_EXTERNAL_UNTRUSTED_CONTENT …>>>` plus Quelle.
- Verdächtige Muster werden nur **geloggt**.
- Chat-Template-Spezialtokens werden entfernt.
- [Code: src/security/external-content.ts; Docs: docs/gateway/security/prompt-injection.md]
- Empfehlung der Doku: die stärksten verfügbaren Modelle verwenden und für nicht vertrauenswürdige Inhalte eigene „Reader-Agenten“ einsetzen.

**Prüfwerkzeuge**
- `openclaw security audit [--deep] [--fix]`, mit Prüfungen wie `gateway.bind_no_auth`, `channels.<p>.dm.open`, `tools.exec.security_full_configured`, `plugins.extensions_no_allowlist`, `fs.config.perms_world_readable` [Docs: docs/gateway/security/audit-checks.md].
- `openclaw doctor --fix` migriert Konfiguration und Altlayout, prüft Rechte und Linger und repariert Sandbox-Images [Docs: docs/gateway/doctor.md].

**Vorfälle 2026**

| Datum | Vorfall | Ursache / Folge | Beleg |
|---|---|---|---|
| 27.–30.01. | Umbenennungs-Chaos | Handle-Sniping auf X, Scam-Tokens ($OPENCLAW auf Pump.fun), falsche „Head of Engineering“-Profile | [Docs: docs/start/lore.md] |
| 31.01. | **1-Click-RCE über `gatewayUrl`**: GHSA-g8p2-7wf7-98mq, CVSS 8.8, in der Presse als CVE-2026-25253 | Die Control UI übernahm `gatewayUrl` ungeprüft aus der Query und verband sich automatisch. Dabei wurde das gespeicherte Gateway-Token an den Angreifer gesendet, der danach Sandbox und Tool-Policy umkonfigurieren und Code ausführen konnte. Wirkt **auch bei Loopback-Bind**, weil der Browser die Brücke ist. Betroffen ≤ 2026.1.28; Fix 2026.1.29 (30.01.): neue URL muss in der UI bestätigt werden | [GH-API: GHSA-g8p2-7wf7-98mq; Sekundär: The Hacker News, 02/2026; depthfirst, 02/2026] |
| Ende 01.–03. | **Öffentlich erreichbare Instanzen** | Censys: ≈ 21.600 (31.01.). SecurityScorecard STRIKE: 40.214 Instanzen auf 28.663 IPs (09.02.). Spätere Zahlen 63.000–220.000 sind widersprüchlich. Ursachen: LAN-Bind/Docker-Defaults, Reverse-Proxies, alte Versionen | [Sekundär: Infosecurity Magazine, 09.02.2026; SiliconANGLE, 09.02.2026; cyberdesserts, 2026] |
| Anfang 02. | **ClawHavoc** | Koi Security fand 341 bösartige von 2.857 ClawHub-Skills; 335 aus einer Kampagne. Gefälschte „Prerequisites“ installierten den macOS-Stealer AMOS. Danach Scans mit VirusTotal; Umgehungen noch im Mai 2026 | [Sekundär: The Hacker News, 02/2026; eSecurity Planet, 2026] |
| ≈ 02.02. | **Moltbook-Datenleck** (Wiz) | Der „Agenten-Social-Network“-Dienst, ein Drittprojekt, hatte einen Supabase-Key im Client-JS und kein Row Level Security. Etwa 1,5 Mio. Agent-API-Tokens, ≈ 35.000 E-Mails und private DMs, teils mit OpenAI-Keys, lagen offen. Das Repo stellt klar: „not affiliated with Moltbook“ | [Sekundär: Wiz-Blog, 02/2026; Code: scripts/github/barnacle-auto-response.mjs] |
| 14.02.–31.03. | **Serie kritischer Autorisierungsfehler** (alle Fix-Releases im März) | RCE über Node-Invoke-Approval-Bypass (GHSA-gv46-4xfq-jv58). Pairing- und Device-Token konnten `operator.admin` erlangen (GHSA-4jpw-hj22-2xmc, -hf68-49fm-59cq, -fqw4-mph7-2vr8, -9hjh-fr4f-gxc4, -rqpp-rjj8-7wv8). `.env`-Injektion aus dem CWD (GHSA-8rh7-6779-cjqq). Sandbox-Bypass über Heartbeat-Kontextvererbung (GHSA-g5cg-8x5w-7jpm, extern als CVE-2026-41329, CVSS 9,9). TOCTOU-Sandbox-Escape (GHSA-9p3r-hh9g-5cmg) | [GH-API; Sekundär: beyondmachines, 04/2026] |
| 04.04. | Anthropic sperrt Abo-OAuth für Drittanbieter-Harnesses | Kein Sicherheitsvorfall, aber ein Betriebsrisiko für Abo-Nutzer | [Sekundär: heise, 04/2026] |
| 11.09. | Sammelveröffentlichung von 75 Advisories | U. a.: Exec-Freigaben deckten geänderte Argumente; Cron-Tool erreichte Operator-Command-Jobs; „Dreaming could widen restricted sender authority“; Codex-native Tools ignorierten Chat-Policy | [GH-API] |
| 10.2026 | Kompromittierung der eMudhra-CA | Trust-Bundle des Egress-Proxys bereinigt (v2026.10.8) | [Docs: CHANGELOG/2026.10.8.md] |

**Advisory-Statistik** (veröffentlichte GHSAs, Stand 28.10.2026, 722 insgesamt) [GH-API]:

| Schweregrad | Anzahl |
|---|---|
| critical | 14 |
| high | 249 |
| medium | 390 |
| low | 69 |

| Monat (2026) | Jan | Feb | Mär | Apr | Mai | Jun | Sep |
|---|---|---|---|---|---|---|---|
| Veröffentlicht | 3 | 220 | 204 | 108 | 67 | 45 | 75 |

Die Veröffentlichung erfolgt offenbar gebündelt; im Juli, August und Oktober wurde keine Advisory publiziert.

### 4.12 Betrieb

**Installation**
- **Wege:**
  - `curl … install.sh | bash` (installiert Node, falls nötig)
  - npm: `npm i -g openclaw@latest --allow-scripts=openclaw`
  - Docker: `ghcr.io/openclaw/openclaw`, Varianten `slim` und `-browser`
  - Nix, Desktop-Apps
  - [Docs: docs/install/index.md]
- **Onboarding** (`openclaw onboard`):
  - **Quick Start:** Provider wählen, Live-Completion als Test, Workspace und Konfiguration schreiben, Gateway im Vordergrund starten, Dashboard öffnen. Agent `main`, „full access“.
  - **Custom:** fragt zusätzlich Port, Bind, Auth, Tailscale, Kanäle, Websuche, Skills und Daemon ab.
  - [Docs: docs/start/wizard.md]

**Daemon**
- Linux: systemd-**User**-Unit `openclaw-gateway.service` mit Linger; `Restart=always`, `RestartSec=5`, `OOMPolicy=continue`.
- macOS: LaunchAgent `ai.openclaw.gateway`. Windows: Scheduled Task.
- [Docs: docs/platforms/linux.md, docs/platforms/index.md]

**Updates**
- `openclaw update --channel stable|extended-stable|beta|dev`.
- Auto-Update des Gateways per Default **aus**. Headless-Nodes aktualisieren sich dagegen per Default selbst: stündlicher Check, Aktivierung nur im Leerlauf.
- Update-Checks und anonyme Update-Ergebnisberichte sind per Default an [Docs: docs/install/updating/automatic-updates.md, VISION.md].

**Ressourcen** (1 vCPU / 6 GB ARM64)
- **Dokumentiertes Minimum:** 1 vCPU, 1 GB RAM, ≈ 500 MB Platte; empfohlen 2 GB+ [Docs: docs/help/faq-first-run/providers-and-hosting.md].
- **Gemessen:** Spitzen-RSS 581–811 MB in Smoke-Läufen mit Mock-Provider; das installierte Paket hat ≈ 362 MiB. Die Doku nennt das selbst nur „Trendindiz“ [Docs: docs/reference/release-performance-sweep.md].
- **Heap-Formel:** `min(max(8192, mem/4), max(2048, mem/2), 0,75·mem)`. Bei 6 GiB ergibt das nach meiner Rechnung ≈ 3 GiB Heap [Code: src/daemon/gateway-heap.ts].
- **Oracle:** Es gibt eine eigene Anleitung für Oracle Always Free A1 (Ubuntu 24.04 aarch64, Tailscale Serve). Hinweis darin: Einzelne native Skill-Binaries fehlen für arm64 [Docs: docs/install/oracle.md].
- **Teuer sind:**
  - Chromium
  - die Docker-Sandbox (Images müssen lokal gebaut werden)
  - lokale Embeddings (≈ 0,3 GB Modell)
  - Image-Build aus dem Quellcode (6 GB RAM)
- **Fazit:** Für ein Gateway mit gehosteten Modellen und Messenger-Kanälen ist 1 vCPU / 6 GB **ausreichend**. Mit Browser und Sandbox wird es auf 1 vCPU eng; das ist eine Inferenz, nicht gemessen.

**Fernzugriff**
- Bevorzugt: Tailscale **Serve**, das Gateway bleibt auf Loopback.
- Alternativen: SSH-Tunnel (`ssh -N -L 18789:127.0.0.1:18789`), Funnel nur mit Passwort, Cloudflare Access mit `trusted-proxy`.
- [Docs: docs/gateway/tailscale.md, docs/gateway/remote.md]

**Docker-Compose-Falle**
- Bindet `lan` und veröffentlicht 18789 auf **allen** Host-Interfaces [Code: docker-compose.yml].
- Das ist eine plausible Ursache für viele exponierte Instanzen (Inferenz).

**Begleit-Apps**
- macOS-Menüleisten-App: WebChat, Quick Chat, native Freigabe-Dialoge, Mac als Node.
- iOS (26.2+) und Android sind **reine Nodes** und können kein Gateway hosten: Kamera, Standort, Talk, Voice Wake.
- Tauri-Desktop für Linux und Windows auf Basis des Rust-Crates `openclaw-gateway-client`.
- Rust-Binary `openclaw-node` (`crates/openclaw-node-host`) als headless Node.
- [Docs: docs/platforms/*.md; Code: crates/]

---

## 5. Schwächen, Kritik, Risiken

| Bereich | Befund | Beleg |
|---|---|---|
| Angriffsfläche und Fehlerrate | 722 Advisories in 10 Monaten, viele zu Scope-, Pairing- und Owner-Autorisierung. Ursache ist die Kombination aus vielen Ein- und Ausgängen (Kanäle, Nodes, Webhooks, Plugins, Control UI) und dem Ein-Operator-Modell | [GH-API] |
| Leistung vor Sicherheit in den Defaults | Sandbox `off`, Exec `full`/`ask:off`, lokales Onboarding mit Tool-Profil `full`, `allowCollaborativeSteering: true`, Code Mode in `node:vm`. Die Hauptsitzung führt Shell-Befehle **ohne Rückfrage auf dem Host** aus; die Schutzwirkung hängt an Pairing und Allowlists | [Code: src/agents/sandbox/config.ts, src/infra/exec-approvals-config.ts; Docs: SECURITY.md] |
| Supply Chain | Öffentliche Registry mit Scans, aber ohne lokale Signaturprüfung. Plugins laufen in-process. Das eigene Threat Model nennt das Restrisiko „High“; ClawHavoc hat es real gezeigt | [Docs: docs/security/THREAT-MODEL-ATLAS/persistence.md; Sekundär: The Hacker News, 02/2026] |
| Exponierung | Zehntausende öffentlich erreichbare Instanzen im Februar und März 2026. Begünstigt durch Container-Defaults (`auto` → 0.0.0.0, Compose veröffentlicht auf allen Interfaces) | [Sekundär: Infosecurity Magazine, 09.02.2026; Code: src/gateway/net.ts, docker-compose.yml] |
| Token-Kosten | Großer System-Prompt, 67 Kern-Tools mit Schemas, Bootstrap bis 60k Zeichen, Proaktiv-Läufe, Dreaming, Embeddings. Gegenmittel im Code: Cache-Grenze, Tool Search, Code Mode, `idleOnly`, Trigger ohne Modellaufruf. Nutzerberichte nennen 0,4–0,5 Mio. Tokens pro Aufgabe (anekdotisch) | [Code: src/agents/tool-catalog.ts; Sekundär: DEV Community/Reddit, 02–05/2026] |
| Komplexität und Churn | ≈ 4,3 Mio. Zeilen TS, ≈ 1.400 Doku-Seiten, Releases mit Tausenden PRs. Zahlreiche Doku/Code-Abweichungen (§8). Sekundärliteratur veraltet schnell (Heartbeat, JSONL, `TOOLS.md`) | [Code; Docs: docs/releases/2026.9.5.md] |
| Zuverlässigkeit | Regressionen nach Updates, Abstürze und Fehlbedienungen werden berichtet. Gegenmaßnahmen: „Atomic Updates“ (2026.9.5) und die LTS-Linie `extended-stable` | [Sekundär: Reddit/DEV, 02–05/2026; Docs: docs/releases/2026.9.5.md] |
| Abo-Abhängigkeit | Anthropic hat im April 2026 durchgesetzt, dass Abos nicht in Drittanbieter-Harnesses laufen. Google hat im Juni 2026 den Gemini-CLI-Login für Consumer beendet | [Sekundär: heise, 04/2026; Docs: docs/providers/google.md] |
| Datenschutz | Embedding-Default ist OpenAI. Damit gehen Memory-Inhalte und, auf persönlichen Installationen, auch Transkripte an einen Drittanbieter. Der Secret-Store ist unverschlüsselt, Transkripte enthalten Geheimnisse | [Code: src/agents/memory-search.ts, packages/memory-host-sdk/src/host/config-utils.ts; Docs: docs/gateway/security/secrets-and-storage.md] |
| Gedächtnis-Drift | Dreaming schreibt nächtlich und ohne Review in `MEMORY.md`. Die Gates sind solide, aber ein Advisory im September zeigt Rechteausweitung über Dreaming | [GH-API: GHSA-62qm-6fjj-6g23] |
| Governance | Foundation mit OpenAI als Spender; Steinberger arbeitet bei OpenAI. Die Neutralität wird in Kommentaren bezweifelt | [Sekundär: Kilo-Blog, ≈02/2026; Trending Topics, 07/2026] |

---

## 6. Übertragbare Muster für einen eigenen Rust-Agenten

Die Prioritäten gelten für einen Personal Agent als einzelnes Binary auf kleiner ARM-Hardware. Die Aufwände sind grobe Schätzungen in Personentagen (PT).

### Priorität hoch

**H1. Workspace-Identitätsdateien mit Budget und Filter je Sitzungstyp**
- **Was:**
  - Dateien: `AGENTS.md` (Regeln), `SOUL.md` (Ton), `IDENTITY.md` (Name, Emoji), `USER.md` (Direktiven), `MEMORY.md` (Fakten).
  - Feste Reihenfolge und Grenzen: 20k pro Datei, 60k gesamt, `USER.md` 4k.
  - Kürzung Kopf/Ende. Bei `AGENTS.md` bleibt ein Policy-Digest erhalten; `USER.md` wird ganz weggelassen statt halbiert.
  - Filter: Sub-Agent → nur `AGENTS.md`; Cron → ohne `MEMORY.md`; Gruppe → ohne `MEMORY.md`.
- **Warum es wirkt:** Persönlichkeit und Regeln bleiben über Sitzungen stabil, sind für Nutzer editierbar, und private Fakten gelangen nicht in Gruppenchats.
- **Aufwand:** 2–4 PT.
- **Fallstricke:**
  - Ein Datum oder eine Uhrzeit im System-Prompt zerstört den Cache. Deshalb nur das Datum, in einer eigenen Nachricht.
  - Fehlende Dateien müssen klar markiert werden.
  - Ein `BOOTSTRAP.md`-Ritual braucht eine saubere Abschlusserkennung.

**H2. Stabile Präfix-Grenze für Prompt-Caching**
- **Was:** Persona, Policy und Workspace-Dateien stehen über einem Marker. Tools, Skills und Runtime-Zeile stehen darunter. Anthropic bekommt getrennte `cache_control`-Breakpoints; OpenAI einen `prompt_cache_key` aus Sitzung und Komprimierungszähler.
- **Warum:** Bei „always inject“ wird die Identität praktisch kostenlos mitgeschickt; der proaktive Takt kann den Cache warm halten.
- **Aufwand:** 1–3 PT.
- **Fallstricke:** Jede kleine Änderung im Präfix (Reihenfolge der Skills, Zeitstempel) invalidiert den Cache. Der Präfix sollte deterministisch sortiert und gehasht werden.

**H3. Markdown-Gedächtnis plus SQLite-Index plus Flush**
- **Was:**
  - Dateien: `MEMORY.md` und `memory/JJJJ-MM-TT.md` als Wahrheit.
  - Tools: `memory_search` (Snippet ≤ 700 Zeichen, Zeilenbereich, Score) und `memory_get` (Zeilenfenster).
  - Index: SQLite FTS5 (BM25), optional sqlite-vec; Chunks 400/80 Tokens.
  - Ranking: hybrid 0,7/0,3, 30-Tage-Decay nur für datierte Notizen, MMR.
  - Stiller **Pre-Compaction-Flush** (nur anhängen, `NO_REPLY` wenn nichts zu speichern ist).
  - Reset-Hook schreibt die letzten N Nachrichten.
  - „Startup Context“: die letzten 2 Tage gekürzt nach `/new`.
- **Warum:** Das schließt die größte Lücke von dsh-codex (sitzungsübergreifendes Gedächtnis), bleibt menschenlesbar und lässt sich mit git sichern. FTS5 ist in Rust (rusqlite) trivial und auf 1 vCPU billig.
- **Aufwand:** 5–10 PT; ohne Vektor 3–5.
- **Fallstricke:**
  - Embeddings über externe APIs sind ein Datenschutzthema; FTS-only ist ein guter Default.
  - Indexkonsistenz bei File-Watch: Debounce und atomarer Rebuild.
  - Der Flush braucht eine abgekoppelte Kopie der Konversation, damit er sie nicht verschmutzt.
  - Pro Komprimierungszyklus genau ein Flush.

**H4. Kanaladapter-Vertrag mit Session-Key-Grammatik, Pairing und Mention-Gating**
- **Was:**
  - Ein Trait mit Pflicht-`config`, optionalen `outbound`-, `pairing`-, `groups`- und `threading`-Fähigkeiten.
  - Einheitlicher Inbound-Umschlag; Dedupe (TTL 20 min); Debounce.
  - Session-Keys `agent:<id>:main` / `…:<ch>:direct:<peer>` / `…:<ch>:group:<gid>` / `:thread:`.
  - `dmPolicy=pairing` mit 8-stelligem Code (1 h, ≤ 3 offen); die erste Freigabe macht den Owner.
  - Gruppen per Default auf Allowlist mit Erwähnungspflicht.
  - Markdown → IR → Kanalformat mit Chunk-Limit je Kanal.
  - Ein einziges `message`-Tool statt Tools je Kanal.
- **Warum:** Messenger sind die zweite große Lücke. Der Vertrag hält den Kern klein. **Telegram mit Long Polling** braucht keinen öffentlichen Port und passt damit ideal zum VPS.
- **Aufwand:** Gerüst plus Telegram 6–10 PT; jeder weitere Kanal 2–6 PT. Signal und WhatsApp sind wegen der externen Daemons bzw. Baileys deutlich teurer.
- **Fallstricke:**
  - Nachrichtengrenzen und Formatierung je Kanal.
  - Edit-Streaming hat Rate-Limits.
  - Gruppen-Kontext darf private `MEMORY.md` nicht sehen.
  - Bot-Loop-Schutz.

**H5. Proaktivität als normaler Job**
- **Was:**
  - Ein Default-Job „Proactive check“ alle 30–60 min in der Hauptsitzung: `idleOnly`, ein Scratch-Checklistenfeld, Überspringen bei leerem Scratch.
  - Ergebnis über ein Tool `respond(notify: bool, text)` oder `NO_REPLY`; Zustellung an den Owner; `activeHours`.
  - Cron-Jobs mit `main` vs. `isolated`, Backoff, Auto-Disable, Run-Historie.
  - Bedingungs-Trigger, die ohne Modellaufruf entscheiden.
- **Warum:** Der Agent wirkt dadurch „lebendig“, und die Kosten bleiben kontrollierbar. OpenClaw hat Heartbeat und Cron genau deshalb vereinheitlicht. dsh-codex hat mit `schedule_create` und Goals schon die Basis.
- **Aufwand:** 3–6 PT.
- **Fallstricke:**
  - Nicht im aktiven Gespräch feuern (`idleOnly`).
  - Doppelte Benachrichtigungen vermeiden.
  - Der Prompt muss das „Wiederkäuen alter Aufgaben“ ausdrücklich verbieten (OpenClaw-Formulierung).
  - Zustellroute bei Gruppen: immer an den Owner.

**H6. Steering-Semantik für Nachrichten während eines Laufs**
- **Was:**
  - Standard `steer`: einspielen am nächsten Tool-Übergang. Laufende Tools nie abbrechen; nicht gestartete sequenzielle Tools mit einem synthetischen Ergebnis überspringen.
  - Debounce 500 ms, Cap 20, Überlauf zusammenfassen.
  - Dazu die Modi `followup`, `collect` und `interrupt`.
- **Warum:** Im Messenger schreiben Menschen in Häppchen. Ohne diese Semantik wirkt der Agent stur oder bricht ständig ab.
- **Aufwand:** 2–4 PT.
- **Fallstricke:** Rechte. Bei Gruppen darf die Steer-Nachricht eines Fremden nicht die Autorität des Auftraggebers erben; OpenClaw erlaubt das per Default.

**H7. Umhüllung nicht vertrauenswürdiger Inhalte und Owner-Konzept in der Tool-Policy**
- **Was:**
  - Web-, Mail- und Webhook-Inhalte in `<<<EXTERNAL_UNTRUSTED_CONTENT id=random>>>`-Marker mit Quelle einschließen und Spezialtokens entfernen.
  - Tool-Policy nach Absender und Sitzungstyp: Nicht-Owner bekommen kein exec, cron oder gateway; Gruppen werden sandboxed.
- **Warum:** Billig, und es verhindert die häufigsten Eskalationspfade.
- **Aufwand:** 2–3 PT.
- **Fallstricke:** Marker sind keine Garantie. Die eigentliche Grenze ist die Tool-Policy.

### Priorität mittel

| # | Muster | Warum | Aufwand | Fallstricke |
|---|---|---|---|---|
| M1 | **Websuche-Abstraktion** mit Provider-Auto-Erkennung über vorhandene Keys (Brave, Tavily, SearXNG …), Cache 15 min, `web_fetch` mit Readability, ≤ 20k Zeichen, SSRF-Sperre für private Netze | Schließt die Lücke „Websuche“. SearXNG ist selbst hostbar | 2–4 PT | Provider-Quoten; Redirect-Ketten; DNS-Pinning gegen Rebinding |
| M2 | **Komprimierung nach OpenClaw-Schema**: Reserve-Boden 20k, ≤ 25 % des Fensters; `keepRecentTokens` 20k; feste Summary-Struktur; Tool-Paare nie trennen; optional Policy-Abschnitte nachladen | Robuste lange Sitzungen. dsh-codex komprimiert schon, Struktur und Flush-Kopplung lohnen sich | 2–3 PT | Qualitätsprüfung der Summary; Bezeichner (IDs, Pfade) wörtlich bewahren |
| M3 | **Skills-Katalog mit Budget**: XML-Liste Name, Beschreibung, Pfad; ≤ 150 Skills / 18k Zeichen; `skills_search`/`skills_read` für den Rest; Gating `requires.bins/env/config/os` | dsh-codex lädt bereits Codex/Claude-Skills. Budget und Gating verhindern Prompt-Aufblähung | 1–3 PT | Freischaltung auf dem Host ≠ in der Sandbox |
| M4 | **Auth-Profile mit Rotation, Cooldown-Stufen und Fallback-Kette**; Sitzung bleibt bei einem Profil (Cache) | Ausfallsicherheit für einen Dauerdienst | 3–5 PT | Abo-OAuth ist ToS-riskant (Anthropic 04/2026) |
| M5 | **Sub-Agent-Isolation**: nur `AGENTS.md`, feste Deny-Liste (`message`, `cron`, `gateway`, Senden), Tiefe/Kinder/Parallelität begrenzt, Ergebnis stufenweise nach oben, `NO_REPLY` zählt nicht | dsh-codex hat Subagents und Teams. Die Deny-Liste ist das Übertragenswerte | 1–2 PT | Kontext-Fork vs. isoliert bewusst wählen |
| M6 | **Pre-Run-„Startup Context“ und Reset-Policy** (`daily` 04:00 / `idle`), wobei proaktive Läufe die Frische nicht verlängern | Verhindert endlos wachsende Hauptsitzungen | 1–2 PT | Zeitzonen |
| M7 | **Doctor- und Audit-Befehl**: Bind ohne Auth, offene DMs, Dateirechte, Exec-Policy, Migrationen | Betriebssicherheit; Selbsthilfe | 2–4 PT | Muss mit dem Konfig-Schema mitwachsen |
| M8 | **Gateway-Härtung**: Loopback-Default, Token Pflicht außerhalb von Loopback, Origin-Check, Rate-Limits, Geräte-Pairing mit signierter Nonce; **niemals** Server-URL aus Query-Parametern auto-connecten | Lehren aus GHSA-g8p2 und der März-Serie | 2–4 PT | Mobile App und iOS brauchen ein Pairing-Protokoll |

### Priorität niedrig

| # | Muster | Warum später |
|---|---|---|
| N1 | **Dreaming-Konsolidierung** (nächtliche Promotion mit Recall-Statistik, Verlustgrenze, Diff-Journal) | Teuer und komplex; Risiko von Drift. Vorher reicht ein wöchentlicher, explizit protokollierter Aufräum-Job |
| N2 | **Standing Intents** (Keyword-Trigger ohne Modellaufruf) | Nützlich, aber erst nach H3/H5 |
| N3 | **Geräte-Nodes** (iOS: Standort, Kalender, Benachrichtigungen) | Passt zur iOS-App in Arbeit, braucht aber Pairing-Protokoll und Befehls-Allowlist |
| N4 | **Secret-Egress-Proxy** (Sentinels statt Klartext in Subprozessen) | Starkes Muster, aber MITM-CA-Komplexität |
| N5 | **Code Mode / Tool Search** | Erst relevant, wenn die Tool-Zahl stark wächst |
| N6 | **A2A/ACP** | dsh-codex hat schon Teams und Subagents |

---

## 7. Was man bewusst **nicht** übernehmen sollte

1. **In-Process-Plugins mit vollem Vertrauen.** Für Rust lieber MCP über stdio/HTTP, WASM oder separate Prozesse mit klarer Rechtegrenze. Bundle-artige Erweiterungen (Skills, MCP-Konfig) genügen meist; das sieht selbst OpenClaws VISION.md so.
2. **Power-Defaults ohne Gegengewicht:** Exec `full` ohne Freigaben, Sandbox `off` und Tool-Profil `full` beim Onboarding, solange Messenger-Eingänge existieren. Mindestens Gruppen und Nicht-Owner sollten standardmäßig ohne Shell laufen.
3. **`allowCollaborativeSteering` als Default.** Fremde Nachrichten sollten nie die Autorität des Auftraggebers erben.
4. **Öffentliche, offene Skill-Registry ohne Signaturen.** Allenfalls kuratierte Quellen mit Hash-Pinning; keine „Prerequisite“-Installationsanweisungen in Skills ausführen (ClawHavoc-Muster).
5. **Abo-OAuth-Piggybacking** (Claude-Abo in Drittharness). Rechtlich und betrieblich instabil.
6. **Externe Embeddings als Default** (OpenAI) für private Erinnerungen und Transkripte. Auf kleinem VPS: FTS-only oder lokal.
7. **Container-Defaults, die auf `0.0.0.0` binden**, und Compose-Dateien, die auf allen Interfaces veröffentlichen.
8. **Unbeaufsichtigtes Umschreiben von `MEMORY.md`.** Wenn Konsolidierung, dann mit Diff, Backup und Review.
9. **Konfigurations-Churn ohne Kompatibilität**, kompensiert durch einen immer größeren `doctor`. Für ein kleines Projekt lieber ein stabiles, versioniertes Schema.
10. **Die schiere Breite** (28 Kanäle, 62 Provider, Nodes, Canvas, Voice, Team-Betrieb). Die Qualität als Personal Agent entsteht aus H1–H7, nicht aus der Zahl der Integrationen.
11. **mDNS/Bonjour-Broadcast** und eine Control UI, die URL-Parameter für Verbindungsziele akzeptiert.

---

## 8. Nicht verifiziert oder widersprüchlich

**Release-Zuordnung**
- In welchem Release der Heartbeat-Rückzug und die Ablösung der pi-Pakete genau stattfanden, ist offen. Die 2026.10.x-Release-Notes sprechen weiter von „heartbeats“ als Lauf-Art.

**Doku/Code-Abweichungen**

| Thema | Doku | Code |
|---|---|---|
| Zeit im Prompt | `context.md`: Uhrzeit im System-Prompt | nur das Datum, in einer eigenen Nachricht |
| Einspeisung der Workspace-Dateien | `agent.md`: nur beim ersten Turn | `contextInjection: always` |
| Komprimierungsmodus | Default `safeguard` | Fallback `default`, Materialisierung setzt `safeguard` |
| Geltungsbereich von `MEMORY.md` | Vorlage: „nur Hauptsitzung“ | jede private Sitzung |
| Code-Mode-Default | `tool-policy.md`: aus | `auto` |
| Sub-Agent-Deny-Liste | Doku | Code verbietet zusätzlich `openclaw` und `sessions_search` |
| Transkript-Indexierung | „opt-in“ | per Default an bei `dmScope` main |

**Ungeklärte Defaults und fehlende Prüfungen**
- Ob **Active Memory** und das Trigger-Recall per Default aktiv sind (Manifest vs. Doku).
- Welche Kanäle **im npm-Paket gebündelt** sind.
- **ClawHub-Signaturen:** Keine lokale kryptografische Prüfung gefunden. Server-seitig nicht geprüft; die `/clawhub`-Doku fehlt im Klon.

**Externe Angaben**
- **CVE-Zuordnung:** Die GitHub-API liefert für GHSA-g8p2 und GHSA-g5cg kein `cve_id`. Die CVE-Nummern 2026-25253 und 2026-41329 stammen aus Sekundärquellen.
- **Exponierte Instanzen:** Die Zahlen widersprechen sich stark. Primärveröffentlichungen von Censys und SecurityScorecard habe ich nicht gelesen.
- **Anthropic-Abo-Policy:** Der aktuelle Stand (Oktober 2026) ist nicht verifiziert. Die Repo-Doku widerspricht der Presse vom April.
- **Foundation-Zahlen** („4,5 Mio. Claws/Woche“) sind Eigenangaben.

**Nicht gemessen**
- Laufzeitverhalten, Speicherverbrauch auf ARM64 und Tokenkosten. Alle Ressourcenangaben stammen aus der Doku bzw. sind Inferenzen.

---

## 9. Quellen

Alle abgerufen am 28.10.2026.

**Primärquellen**
- Quellcode und Repo-Doku: https://github.com/openclaw/openclaw (Commit `4757ac8eb6b8bc891f813983de8d02996cc9b61b`). Die zitierten `docs/…`-Pfade entsprechen den Seiten unter https://docs.openclaw.ai/… (z. B. `docs/concepts/memory.md` → https://docs.openclaw.ai/concepts/memory).
- Releases: https://github.com/openclaw/openclaw/releases (v2026.10.8, v2026.9.35, v2026.10.10-beta.4)
- Release Notes v2026.10.8: https://docs.openclaw.ai/releases/2026.10.8
- Security Advisories (722 veröffentlicht): https://github.com/openclaw/openclaw/security/advisories, darunter:
  - GHSA-g8p2-7wf7-98mq: https://github.com/openclaw/openclaw/security/advisories/GHSA-g8p2-7wf7-98mq
  - GHSA-g5cg-8x5w-7jpm: https://github.com/openclaw/openclaw/security/advisories/GHSA-g5cg-8x5w-7jpm
  - GHSA-4jpw-hj22-2xmc: https://github.com/openclaw/openclaw/security/advisories/GHSA-4jpw-hj22-2xmc
- Repo-Statistik: GitHub-API `repos/openclaw/openclaw`
- Architektur: https://docs.openclaw.ai/concepts/architecture
- Gedächtnis: https://docs.openclaw.ai/concepts/memory
- Heartbeat-Migration: https://docs.openclaw.ai/gateway/heartbeat
- Automations: https://docs.openclaw.ai/automation/cron-jobs
- Sicherheit: https://docs.openclaw.ai/gateway/security
- Oracle-Anleitung: https://docs.openclaw.ai/install/oracle
- Lore und Namensgeschichte: https://docs.openclaw.ai/start/lore

**Sekundärquellen** (mit Datum der Veröffentlichung)

*Geschichte und Trägerschaft*
- Business Today: „The lobster sheds its shell for the third time as Clawdbot becomes OpenClaw“, 30.01.2026. https://www.businesstoday.in/technology/news/story/the-lobster-sheds-its-shell-for-the-third-time-as-clawdbot-becomes-openclaw-513650-2026-01-30
- Bloomberg: „OpenAI Hires OpenClaw AI Agent Developer Steinberger“, 15.02.2026. https://www.bloomberg.com/news/articles/2026-02-15/openai-hires-openclaw-ai-agent-developer-peter-steinberg
- Engadget: „OpenAI has hired the developer behind AI agent OpenClaw“, 15.02.2026. https://www.engadget.com/ai/openai-has-hired-the-developer-behind-ai-agent-openclaw-092934041.html
- Trending Topics: „OpenClaw Foundation launches with OpenAI, NVIDIA, Microsoft and Tencent as sponsors“, 09.07.2026. https://www.trendingtopics.eu/opwnclaw-foundation-launches-with-openai-nvidia-microsoft-and-tencent-as-sponsors/
- Kilo Blog: „OpenClaw founder to OpenAI … now what?“, vermutlich 02/2026 (Datum nicht geprüft). https://blog.kilo.ai/p/openclaw-founder-to-openainow-what

*Sicherheitsvorfälle*
- The Hacker News: „OpenClaw Bug Enables One-Click Remote Code Execution via Malicious Link“, 02/2026. https://thehackernews.com/2026/02/openclaw-bug-enables-one-click-remote.html
- depthfirst: „1-Click RCE To Steal Your OpenClaw Data and Keys (CVE-2026-25253)“, 02/2026. https://depthfirst.com/post/1-click-rce-to-steal-your-moltbot-data-and-keys
- The Hacker News: „Researchers Find 341 Malicious ClawHub Skills“, 02/2026. https://thehackernews.com/2026/02/researchers-find-341-malicious-clawhub.html
- eSecurity Planet: „Hundreds of Malicious Skills Found in OpenClaw’s ClawHub“, 2026. https://esecurityplanet.com/threats/hundreds-of-malicious-skills-found-in-openclaws-clawhub
- Antiy CERT: „ClawHavoc: Analysis of Large-Scale Poisoning Campaign …“, 2026. https://www.antiy.net/p/clawhavoc-analysis-of-large-scale-poisoning-campaign-targeting-the-openclaw-skill-market-for-ai-agents/
- Wiz: „Hacking Moltbook: The AI Social Network Any Human Can Control“, 02/2026. https://wiz.io/blog/exposed-moltbook-database-reveals-millions-of-api-keys
- Infosecurity Magazine: „Researchers Find 40,000+ Exposed OpenClaw Instances“, 09.02.2026. https://infosecurity-magazine.com/news/researchers-40000-exposed-openclaw/
- SiliconANGLE: „Tens of thousands of OpenClaw systems exposed …“, 09.02.2026. https://siliconangle.com/2026/02/09/tens-thousands-openclaw-systems-exposed-due-misconfiguration-known-exploits/
- Cyberdesserts: „OpenClaw exposure numbers explained“, 2026. https://blog.cyberdesserts.com/openclaw-exposure-numbers-explained/
- BeyondMachines: „Critical privilege escalation vulnerability in OpenClaw …“, 04/2026. https://beyondmachines.net/event_details/critical-privilege-escalation-vulnerability-in-openclaw-ai-agent-platform-3-0-v-8-v

*Abo-Policy und Kritik*
- heise: „Anthropic is removing OpenClaw from its Claude subscriptions“, 04/2026. https://heise.de/-11246096
- MindStudio: „Anthropic OpenClaw ban: third-party harnesses and Claude subscriptions“, 04/2026. https://mindstudio.ai/blog/anthropic-openclaw-ban-third-party-harnesses-claude-subscriptions
- DEV Community: „I read the 49-comment OpenClaw meltdown …“, 2026. https://dev.to/lars_winstand/i-read-the-49-comment-openclaw-meltdown-and-the-real-problem-isnt-just-openclaw-5ap6
- Reddit-Archiv zu Kosten und Zuverlässigkeit, Snapshots 02–05/2026, z. B. https://reddit.sentinel-team.org/posts/1r7bp8e/snapshots/2026-02-27T15%3A20%3A03.151805Z
