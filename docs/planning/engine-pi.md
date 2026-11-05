# Umschaltbare Engine: DSH oder Pi-Agent

- **Status:** `Geplant`
- **Stand:** 2026-11-05 18:55 (Europe/Berlin)
- **Commits:** –

## 1. Ziel

Der Nutzer möchte die Engine wechseln können: Neben DSH (Aethers eigener
Rust-Agent mit Standard, PTC und Minimal) soll der Pi-Agent
(`@earendil-works/pi-coding-agent`) als zweite Engine laufen. Man soll
zwischen DSH und Pi hin und her wechseln können. Pi bekommt in Aether eine
eigene Oberfläche, die genauso gut funktioniert wie bei DSH: Transkript,
Werkzeugzeilen, Live-Text, Warteschlange und Steer, Stopp, Kosten,
Kompaktierung, Statuspunkte, iOS.

## 2. Ausgangslage

### 2.1 Aether

| Baustein | Heute |
|---|---|
| Chat | Sitzungslog v4 (`dshx-session`, `FileLog`). Der Header `agentPreset` wird beim Anlegen gesetzt (`Runtime::create_session`, `crates/dsh-codex/src/runtime.rs`). Der Modus ist fest; ein anderer Modus bedeutet einen neuen Chat (SPEC §5.8a). |
| Laufzeit | `WebBackend` (`crates/dsh-codex/src/web.rs`): `OpenSession { agent: Arc<Agent>, log, cwd, status, tools_version }`. `session_handle` → `start_agent` → `Runtime::new_agent`. `prompt` → `Agent::followup`/`steer_detached`, `cancel` → `cancel_durable`. Warteschlange über `web/composer.rs::update_queue` → `Agent::update_queue`, `/compact` → `Agent::compact_now`. |
| Live | `WebObserver` sendet `StreamMessage::Delta`/`Status`, der Log-Feed `StreamMessage::Event`. `LiveHub` (`live.rs`) ordnet Browser- und Mac-Bilder über `ToolContext.session_id` zu. |
| UI | Sie rendert nur Ereignisse (`ui/src/reduce.js::buildTranscript`): `turn/*`, `step/*`, `user/message`, `assistant/message`, `tool/call`, `tool/result`, `todo/write`, `approval/*`, `llm/retry`, `compaction/*`, `session/title`, `model/selection`, `request/context`, `sandbox/mode`. Die Warteschlange stammt aus `agent/inbox/spliced` (`composer-extras.js::pendingPrompts`). Werkzeugzeilen richten sich nach Namen (`worklog.js::activityOf`) und DSH-Argumenten (`components/tools.js`: `args.file_path`, `old_string`/`new_string`, `meta.path`, `meta.diffs`). Moduswahl: `components/modes.js` (`ModeChip`, `ModeLabel`). |
| iOS | Liest dieselben Ereignisarten (`ios/Aether/Core/Networking/ServerClient.swift`, `Core/Store/ChatModel.swift`). |
| Kosten | `usage.rs::request_of` liest `assistant/message.usage` (`TokenUsage`, camelCase) und bepreist über ein `PriceBook`; Modelle ohne Preis zählen als `unpricedRequests`. |
| Log-Lesen | Unbekannte Ereignisarten werden abgewiesen, außer bei `"ignorable": true` (`dshx-session/src/reader.rs`, `validate.rs`). |
| Node | Die Mac-App bündelt Node 26.13.0 (`desktop/fetch-node.py`, `build.sh`). Auflösung: `DSHX_NODE` → neben dem Programm → `PATH` (`dshx-tools/src/ptc.rs`). |
| Sandbox | `dshx_tools::sandbox::{host_runner, bwrap_profile_args, seatbelt_profile_args}`. Modi `read-only`, `workspace-write`, `danger-full-access`; Freigaberegel `ask`/`never`. |

### 2.2 Pi 1.5.0

Grundlage sind das installierte Paket (README, `docs/rpc*.md`, `json.md`,
`extensions.md`, `security.md`, `sessions.md`, Typen in `dist/`) und eigene
Proben.

| Thema | Befund |
|---|---|
| Schnittstellen | RPC (`--mode rpc`, JSONL über stdin/stdout, Befehle mit `id`), JSON-Modus (einmalig) und SDK (in-process, Node). |
| RPC-Befehle | `prompt` (mit `images`, während eines Laufs `streamingBehavior: steer\|followUp`), `steer`, `follow_up`, `abort` (wartet, bis Pi untätig ist), `clear_queue`, `get_state`, `get_messages`, `set_model`, `get_available_models`, `set_thinking_level`, `get_available_thinking_levels`, `set_steering_mode`, `compact`, `set_auto_compaction`, `set_auto_retry`, `get_session_stats`, `fork`, `clone`, `switch_session`, `get_entries {since}`, `get_tree`, `set_session_name`, `get_commands`. Eine Werkzeugliste fehlt. |
| Ereignisse | `agent_start`/`agent_end`/`agent_settled {aborted}`, `turn_start`/`turn_end`, `message_start`/`message_update`/`message_end` (Deltas `text_*`, `thinking_*`, `toolcall_start {id, toolName}`/`_delta`/`_end`), `tool_execution_start`/`_update`/`_end {durationMs}`, `queue_update`, `compaction_start {reason}`/`compaction_end`, `auto_retry_start`/`_end`, `session_info_changed`, `thinking_level_changed`, `extension_error`, `extension_ui_request`. Jede Assistenznachricht trägt `usage` mit `cost.total` (USD). |
| Werkzeuge | `read {path, offset, limit}`, `bash {command, timeout}`, `edit {path, edits:[{oldText, newText}]}` (Details: `diff`, `patch`), `write {path, content}`. Abgeschaltet sind `grep`, `find`, `ls`. Eingebaute Erweiterungen: `codemode`, `tool_search`, MCP. Pi führt Werkzeuge parallel aus. Subagenten, To-dos und Plan gibt es nicht. |
| Erweiterungen | TypeScript über jiti. `pi.registerTool`; `pi.on("tool_call")` kann blockieren oder Argumente ändern; `registerProvider` (auch mit `auth.resolve`); `sendMessage(…, {deliverAs: "nextTurn"})`. Dialoge laufen über `extension_ui_request`. Eingebaute Werkzeuge lassen sich ersetzen (`createBashToolDefinition` mit `spawnHook`). |
| Sitzungen | JSONL-Baum; `--session-dir`; `--session-id <id>` legt an oder setzt fort. |
| Zugang | Agent-Verzeichnis `~/.pi/agent` (`PI_CODING_AGENT_DIR`) mit `auth.json` (API-Schlüssel, OAuth, `!command`), `models.json`, `settings.json`, `mcp.json`; außerdem Umgebungsvariablen. Der Provider `deepseek` ist eingebaut, mit `deepseek-flash` und `deepseek-v4-pro` (dieselben IDs wie in Aether). |
| Kontext | `AGENTS.md`/`CLAUDE.md` immer, Skills, Prompt-Vorlagen. Projekt-`.pi/` lädt nur nach Projektvertrauen (`--approve`/`--no-approve`). |
| Sicherheit | Keine Freigaben, keine Sandbox; Schutz nur durch OS-Isolation (`security.md`). |
| Laufzeit | Node ≥ 22.19. |

**Eigene Proben** (Scratch-Ordner, eigenes `PI_CODING_AGENT_DIR`, Faux-Provider
aus `@earendil-works/pi-ai` per Erweiterung, kein Modellaufruf, `~/.pi`
unverändert):

- Die Ereignisfolge oben ist bestätigt. Ein `tool_call`-Gate, das `write` nach
  `/etc/…` blockt, erscheint als `isError`-Ergebnis mit dem Gate-Text.
- **bash erbt die gesamte Prozessumgebung:** Eine `*_API_KEY`-Variable
  erschien in der Befehlsausgabe.
- **Abbruch:** Ein beim `abort` wartendes `steer` wurde danach trotzdem als
  neue Nutzernachricht zugestellt. Ein `followUp` blieb in der Warteschlange.
- `--session-id aether-sess-0001` legt die Datei
  `<zeit>_aether-sess-0001.jsonl` an und setzt sie später fort.
- Start in etwa 0,3 s, im Leerlauf etwa 110 MB RSS.
- Eine eigenständige Laufzeit aus `dist/bundle` + `package.json` +
  `node_modules/jiti` (2.7.0) + `@silvia-odwyer/photon-node` (0.3.4), etwa
  13 MB, startet und lädt TS-Erweiterungen mit Importen aus `pi-ai` und
  `typebox`.
- Ein frisches Agent-Verzeichnis bekommt `auth.json` und `models-store.json`.
  Skills aus `~/.agents/skills` wurden trotz eigenem Agent-Verzeichnis
  gefunden.

## 3. Plan

### 3.1 Überblick

```
UI / iOS ──HTTP/WS──▶ dshx-server ──Backend──▶ WebBackend
                                               ├─ ChatEngine::Dsh(Arc<Agent>)   (unverändert)
                                               └─ ChatEngine::Pi(Arc<PiChat>) ──stdio JSONL──▶ node pi --mode rpc
                                                         ▲                                        │ -e aether-bridge.ts
                                                         └────────── Unix-Socket (Bridge) ◀───────┘
```

Neuer Crate `crates/dshx-pi`:

| Datei | Inhalt |
|---|---|
| `install.rs` | Auflösung von Node und Pi, Versionsprüfung |
| `process.rs` | Start, Umgebung, Prozessgruppe, Sandbox-Hülle, Leerlauf-Ende, Prozessgrenze |
| `rpc.rs` | JSONL-Rahmen (nur an LF trennen, CR abstreifen), `id`-Korrelation, stderr-Ringpuffer (64 KB) |
| `mapper.rs` | Reiner Zustandsautomat: Pi-Ereignis → `Vec<NewEvent>` plus Live-Nachrichten |
| `meta.rs` | Normalisierung der Werkzeugzeilen |
| `inbox.rs` | Warteschlange mit `agent/inbox/spliced` |
| `bridge.rs` | Socket-Server, der Aether-`Tool`s ausführt |
| `extension/aether-bridge.ts` | Mit `include_str!` eingebettet und beim Start nach `$DSHX_HOME/pi/ext/aether-bridge-<sha8>.ts` geschrieben |

`dshx-agent` bleibt unberührt.

### 3.2 Engine pro Chat und Wechsel (Frage 1)

- **Wahl beim Anlegen, fest pro Chat:** `POST /api/sessions {cwd?, preset?,
  engine?}` mit `engine` ∈ `dsh` (Standard) | `pi`. Der Header bekommt
  `engine: "pi"` und `engineVersion: "1.5.0"`. Fehlt `engine`, ist es DSH;
  das gilt für alle bestehenden Chats. `agentPreset` gibt es nur bei DSH.
  `session_handle` prüft zuerst den Header, damit `Runtime::new_agent` nie
  einen Pi-Chat sieht.
- **Globaler Standard:** Config `default_engine`, über `PUT /api/settings
  {defaultEngine}`. `/api/info` liefert zusätzlich `engines: [{id, name,
  available, version?, reason?}]` und `settings.defaultEngine`. `/api/sessions`
  bekommt `engine?` pro Zeile (`SessionSummary.engine` in
  `dshx-session/src/store.rs`, analog zu `preset`).
- **Hin und her = Fortsetzungs-Chat:** `POST /api/sessions/:id/handover
  {engine, preset?, model?}` → `{id}`.
  - Der neue Chat liegt im selben Ordner. Header: `parentSession` und
    `handoverFrom: {session, engine, turn}`.
  - Erstes Ereignis ist ein `user/message` nur für das Modell, mit Quelle
    `{kind: "handover", fromSession, fromEngine}`. Es enthält das
    Übergabepaket aus `crates/dsh-codex/src/handover.rs`.
  - Das Paket wird aus `derive_messages(Quelle)` gebaut: Nutzertexte und
    Antworten wörtlich, Werkzeugaufrufe einzeilig (`bash: <erste Zeile>`,
    `edit: <Pfad>`), dazu die letzten To-dos und der Ordner. Budget etwa
    24 000 Tokens (vier Zeichen pro Token); die Mitte wird mit einem Hinweis
    gekürzt.
  - Ziel Pi: Die Bridge reicht das Paket per `pi.sendMessage({customType:
    "aether-handover", display: false}, {deliverAs: "nextTurn"})` weiter.
  - Ziel DSH: Das Paket kommt über `store.create_seeded` als Seed in den
    Chat.
  - Die UI zeigt die Notiz „Fortgesetzt aus ‹Titel› (Pi)“ mit Link; die
    Quelle zeigt „Weiter in ‹Titel›“. Die Seitenleiste gruppiert Ketten unter
    ihrem Ursprung.
- **Bewusst nicht:** ein Wechsel im selben Log. Gemischte Historie bricht
  DSHs Replay-State und Werkzeugschemata, außerdem gilt der SPEC-Grundsatz
  „fest pro Chat“. Siehe F1.

### 3.3 Prozessmodell (Frage 2)

- **Ein Pi-RPC-Prozess pro aktivem Pi-Chat.** RPC statt SDK-Sidecar, weil:
  - RPC ist Pis Protokoll für eigene Oberflächen;
  - Abbruch und Absturz bleiben auf einen Chat beschränkt;
  - die Sandbox umschließt den ganzen Prozess;
  - es gibt keinen eigenen TS-Host, der SDK-Änderungen folgen müsste.
- **Aufruf** (cwd = Chatordner):

  ```
  <node> <pi>/dist/bundle/cli.js --mode rpc \
    --session-dir $DSHX_HOME/pi/sessions --session-id <aether-id> \
    --model <provider/model> --thinking <stufe> --name <titel> \
    --no-extensions -e $DSHX_HOME/pi/ext/aether-bridge-<sha8>.ts \
    --no-skills [--skill <ordner> …] --no-mcp --no-approve
  ```

- **Umgebung:** `env_clear()` plus Allowlist (`PATH`, `HOME`, `USER`,
  `LANG`, `LC_*`, `TMPDIR`, `SHELL`, `TERM=dumb`). Dazu
  `PI_CODING_AGENT_DIR=$DSHX_HOME/pi/agent`, `PI_TELEMETRY=0`, `PI_OFFLINE=1`,
  `AETHER_BRIDGE=<socket>` und `AETHER_BRIDGE_TOKEN=<32 Byte>`. Die
  Erweiterung löscht beide `AETHER_*`-Variablen beim Laden aus `process.env`.
  Der Prozess läuft in eigener Prozessgruppe; unter Linux zusätzlich
  `--die-with-parent` (bwrap) bzw. `PR_SET_PDEATHSIG`.
- **Lebenszyklus:** Der Prozess startet beim ersten Prompt. Nach `idle_secs`
  (600) ohne Warteschlange schließt Aether stdin und sendet nach 5 s SIGKILL
  an die Gruppe. `max_processes` (Linux 3, macOS 6) begrenzt die Zahl; darüber
  wird der am längsten untätige Prozess beendet. Sind alle beschäftigt,
  wartet der Prompt sichtbar in der Warteschlange.
- **Stopp:**
  1. `clear_queue`; die zurückgegebenen steer-Texte kommen vorn in die
     Aether-Warteschlange zurück, bleiben also sichtbar.
  2. `abort`. Laufende Bridge-Aufrufe brechen über ihr `CancellationToken`
     sofort ab.
  3. Nach `agent_settled {aborted: true}` schreibt Aether `turn/end
     {reason: {kind: "aborted"}}`.
  4. Antwortet `abort` nicht binnen 10 s: SIGTERM, dann SIGKILL an die
     Gruppe; offene Werkzeugzeilen bekommen den Fehler „Engine gestoppt“.

  Pi-`followUp` wird nie benutzt, deshalb läuft nach einem Stopp nichts von
  selbst weiter (AGENTS.md).
- **Absturz:** Endet der Prozess während eines Turns, schreibt Aether
  synthetische `tool/result` (`isError`) für offene Aufrufe und `turn/end
  {reason: {kind: "error", error: {code: "ENGINE_EXITED", message:
  <stderr-Ende>}}}`; der Status wird `error`. Die Warteschlange bleibt. Der
  nächste Prompt startet Pi neu mit `--session-id`.
- **Neustart von Aether:**
  - Ein offener Turn wird wie bei DSH repariert (`turn/end interrupted`).
  - Nach jedem `turn_end` holt Aether `get_entries {since}` und schreibt
    `pi/cursor {entryId}` (`ignorable: true`). Beim Wiederöffnen werden
    fehlende Einträge nachgespiegelt.
  - Danach geht es wie bei `resume_pending` weiter.
- **Zwei Logs:** Den Modellkontext hält die Pi-JSONL unter
  `$DSHX_HOME/pi/sessions`, die Anzeige das Aether-Log. Beim Löschen eines
  Chats werden beide entfernt.
- **Gepinnt statt Homebrew:**
  - `desktop/fetch-pi.py` (wie `fetch-node.py`) lädt die npm-Tarballs von
    `pi-coding-agent@1.5.0`, `jiti@2.7.0` und `photon-node@0.3.4` mit fest
    eingetragenem SHA-512. Übernommen werden nur `dist/bundle/**`,
    `package.json` und die Lizenzen, nach `Aether.app/Contents/Resources/pi/`.
    Node ist das schon gebündelte 26.13.0.
  - Linux/VPS: dasselbe Paket als `share/dsh-codex/pi/` im Release-Tarball.
    Node kommt über `DSHX_NODE` oder `dsh-codex runtime install` (gepinntes
    linux-arm64-Archiv mit Prüfsumme nach `$DSHX_HOME/runtime/`).
  - Auflösung in `install.rs`: `DSHX_PI` → neben dem Programm →
    `$DSHX_HOME/runtime/pi-1.5.0/`. Ein `pi` aus `PATH` wird nie benutzt.
    Liefert `cli.js --version` nicht `1.5.0`, gilt `available: false` mit
    `reason`. Die Node-Auflösung in `ptc.rs` wird als `pub fn node_binary()`
    geteilt.

### 3.4 Ereignis-Abbildung (Frage 3)

Ein Aether-Turn reicht vom `prompt` bis `agent_settled`; ein Aether-Step
entspricht einem Pi-`turn`. Gespiegelte Nachrichten tragen wie bei DSH
`surfaceOp: "append"`, damit `derive_messages`, `fork_seed` und die Übergabe
funktionieren. Ein Golden-Test prüft das Ergebnis mit `validate_log`.

| Pi | Aether |
|---|---|
| Aether sendet `prompt` an untätiges Pi | `agent/inbox/spliced` (Entnahme `next-turn`), `turn/start {turn}`, Status `running` |
| `turn_start` / `turn_end` | `step/start` / `step/end {turn, step}`, danach `pi/cursor` |
| `message_end` role `user` | `user/message`: Aethers eigene Nachricht (`messageId`, Bild-Anhänge als Referenz, Quelle `user`); bei Steer Entnahme aus `next-step` (FIFO-Abgleich, abgesichert über `queue_update`) |
| `message_*` role `system` | nicht gespiegelt |
| `text_*` / `thinking_*` | `Delta {turn, step, index: contentIndex, blockType: text\|reasoning}` |
| `toolcall_start` | `Delta {blockType: "tool-call", text: toolName}` |
| `message_end` role `assistant` | bei Wechsel `request/context {provider, model, contextWindow}`; dann `assistant/message {turn, step, message: {content: [reasoning\|text\|tool-call{id, name, arguments: JSON-String}], source: {kind: "model", provider, model}}, usage}`. `stopReason: aborted` → `interrupted: true`; `length` → `turn/end max-tokens`; `error` → Fehler für `turn/end` |
| `tool_execution_start` | `tool/call {turn, step, callId, name, arguments}` |
| `tool_execution_end` und `message_end` role `toolResult` | `tool/result {message: {role: "tool", toolCallId, content, isError}, meta}`; Bilder kommen in den `AttachmentStore` |
| `todo_write` über die Bridge | zusätzlich `todo/write {turn, todos}` aus `value` (wie `scheduler.rs`) |
| `tool_execution_update` | MVP: verworfen; Phase 3: Live-Ausgabe |
| `auto_retry_start` / `_end` | `llm/retry {turn, retry, maxRetries, delayMs, failure}`; `finalError` geht in `turn/end` |
| `compaction_start` / `_end` | `compaction/start {compactionId, turn\|null}`, `compaction/summary {summary: [{type: "text", text}], provider, model, usage}`, `compaction/end {error?}` |
| `agent_settled {aborted}` | `turn/end {reason: {kind: completed\|aborted\|error\|max-tokens}}`, Status `idle`, danach der nächste `next-turn`-Eintrag |
| `session_info_changed` / `thinking_level_changed` | `session/title` / `model/selection` |
| `extension_ui_request` (Dialog) | MVP: Antwort `cancelled`; Phase 4: Freigaben |

**Usage:** `input → inputTokens`, `output → outputTokens`,
`cacheRead/cacheWrite → cacheRead/WriteTokens`, `reasoning → reasoningTokens`,
dazu neu `costUsd = cost.total`. `usage.rs` nimmt `costUsd` für Modelle ohne
Aether-Preis. `session_stats` liest für Pi-Chats das Kontextfenster aus
`request/context` statt aus `runtime.route`.

**Werkzeugzeilen** (`meta.rs` und kleine UI-Aliase):

| Pi | `meta` | UI |
|---|---|---|
| `read` | `{path}` (absolut) | `ReadRow`: `args.file_path ?? args.path` |
| `edit` | `{path, diffs: [{path, oldText, newText}]}` aus `args.edits` | `EditRow` liest `meta.diffs` schon |
| `write` | `{path, operation}` | `editPath`: `args.path` |
| `bash` | `{exitCode, truncated, fullOutputPath}` aus `structuredContent` | `ShellRow`: `meta.exitCode` vor dem `[exit code: N]`-Parsing |
| Bridge | Aethers eigenes `ToolOutput.meta` | – |

**Neue UI-Teile:**

- `EngineChip` auf der Startseite (`modes.js`, neben `ModeChip`; bei Pi
  verschwindet die Moduswahl).
- `ModeLabel` zeigt „Pi“ mit den Aktionen „In DSH fortsetzen“ bzw. „In Pi
  fortsetzen“.
- Einstellungen: Standard-Engine sowie Pi-Status (Version, verfügbar, Grund).
- Composer: Modelle nach Engine gefiltert. MCP, Ziel und Modus sind bei Pi im
  MVP ausgeblendet; Slash-Befehle `compact`, `model`, `status`, `export`.
- `reduce.js`: `engine` aus dem Header, Notiz für die Quelle `handover`.
- `state.js` und `api.js`: `composer.engine`, `createSession(cwd, {preset,
  engine})`.
- Fixture `ui/fixtures/pi.json`, Prüfung `ui/dev/engine_check.mjs`,
  Texte in `i18n.js`.
- iOS liest alles ab MVP, weil die Ereignisse gleich bleiben.

### 3.5 Werkzeuge (Frage 4)

- **Pis eigene Werkzeuge** `read`, `bash`, `edit`, `write` bleiben mit Pis
  Schema und Prompt. `bash` ersetzt die Erweiterung durch eine gleichnamige
  Variante (`createBashToolDefinition` mit `spawnHook`). Diese entfernt aus
  der Umgebung alle Variablen mit `KEY`, `TOKEN`, `SECRET` oder `PASSWORD`
  sowie alle `AETHER_*`.
- **Bridge-Protokoll** (JSON-Zeilen, Socket `$DSHX_HOME/pi/run/<id>.sock`,
  Ordner `0700`):

  | Nachricht | Richtung | Inhalt |
  |---|---|---|
  | `hello {token, pi}` → `config` | Erweiterung → Aether → Erweiterung | `config {session, cwd, sandbox, tools: [{name, description, parameters}], providers: [...]}` |
  | `gate {id, callId, tool, input}` → `{allow}` \| `{allow: false, reason}` | Erweiterung → Aether | Aus `pi.on("tool_call")` für `write`, `edit` und Bridge-Werkzeuge |
  | `call {id, callId, tool, args}` → `progress*`, `result {content, isError, meta}` | Erweiterung → Aether | Aether führt `Arc<dyn Tool>` mit `ToolContext {session_id: <Chat>, cwd, sandbox, cancel, attachments, approver, caller}` aus; Bilder gehen als base64 an Pi |
  | `cancel {id}` | Erweiterung → Aether | Aus dem `signal` des Werkzeugs |
  | `tools`, `providers`, `handover` | Aether → Erweiterung | Push: MCP umgeschaltet, Schlüssel geändert, Übergabepaket |

- **Welche Aether-Werkzeuge:**
  - MVP: `todo_write` (Planfeld).
  - Phase 3: `web_fetch`, `web_search`, `browser`, `app`/`computer` (macOS;
    die Live-Ansicht funktioniert, weil `session_id` der Pi-Chat ist),
    MCP-Werkzeuge (`mcp__<server>__<tool>`, gleiche Benennung wie Pi) mit
    Aethers Connector-Auswahl.
  - Skills: Aether-Skills mit `model = true` über `--skill`. `/name` expandiert
    Aether selbst wie bei DSH.
  - Phase 4: Ziele, optional DSH-Subagenten (F5).
- **Nicht über die Bridge:** Aethers `bash`, `read`, `edit`, `write`, `glob`,
  `grep` (doppelt), `run_code` (Pi hat `codemode`) und `job_*`.
- **DSH-Verträge unverändert:** `Preset` (Standard, PTC, Minimal),
  Werkzeugschemata, `run_code` und die Goldens bleiben, wie sie sind. Pi ist
  kein Preset. Regression: `crates/dsh-codex/tests/presets.rs`,
  `dshx-agent/tests/golden.rs`.

### 3.6 Modelle und Zugang (Frage 5)

- **Modellauswahl im Pi-Chat:**
  - Aether-Provider: Zeilen aus `/api/info.models` bekommen `engines: ["dsh",
    "pi"]`.
  - Zuordnung: `deepseek-official` → Pis eingebautes `deepseek` (gleiche
    IDs). `[providers.<id>]` → `pi.registerProvider(id, {baseUrl, api,
    headers, models, apiKey})` (`anthropic-messages` bzw.
    `openai-completions`).
  - Später kommen Pi-eigene Konten dazu (F3).
- **Schlüssel ohne Kopien:**
  - Aether liest sie in Rust (`credentials.toml`/Umgebung) und gibt sie nur
    in der `config`-Nachricht der Bridge weiter; in Pi liegen sie nur im
    Speicher, nie in Umgebung oder Datei.
  - Escaping: Ein `!` am Anfang wird zu `$!`, ein `$` zu `$$`.
  - Ändert sich ein Schlüssel, schickt Aether `providers`, und die
    Erweiterung registriert neu.
- **Aufwand ↔ Denkstufe:** `off`/`low`/`medium`/`high`/`max` direkt;
  `xhigh` nur, wenn das Modell es anbietet. Nach `set_model` wird mit
  `get_available_thinking_levels` begrenzt und `model/selection` geschrieben.
  Ein Modellwechsel mitten im Chat läuft vor dem nächsten Prompt über
  `set_model` + `set_thinking_level`.
- **Titel:** Ersatztitel aus dem ersten Prompt (`session/title`) und
  `set_session_name`; ein LLM-Titel folgt in Phase 3.

### 3.7 Sicherheit (Frage 6)

| Ebene | Was Aether erzwingt |
|---|---|
| Prozess-Sandbox | Der ganze Pi-Prozess läuft unter `host_runner()` im Modus des Chats. Schreibbar: der Chatordner (`workspace-write`), `$DSHX_HOME/pi/{sessions,agent,run}` und ein eigenes `tmp`. `read-only`: nur diese Pi-Ordner. `danger-full-access`: keine Hülle. Ohne Runner gibt es einmal eine Warnung wie bei DSH; Status in `/api/engines`. Moduswechsel im Composer → Pi startet vor dem nächsten Prompt neu (setzt fort, etwa 0,3 s) und `sandbox/mode` wird geschrieben. |
| Gate | `tool_call` → Bridge `gate` → Aethers Pfadregeln aus `dshx-tools`; geblockt mit DSHs Text `[sandbox: file access denied under … mode]`. |
| Umgebung | `env_clear` + Allowlist, `spawnHook`-Filter, Bridge-Token aus `process.env` gelöscht; Socket `0700`, Token im Constant-Time-Vergleich. |
| Start | `--no-extensions` (nur die Bridge), `--no-approve` (kein Projekt-`.pi`-Code), `--no-mcp`, `--no-skills` + explizit, eigenes Agent-Verzeichnis, Telemetrie aus. |
| Freigaben | MVP: keine Einzel-Eskalation. Die UI sagt „Zugriff gemäß Modus“. Phase 4: Ein geblockter Aufruf → Freigabekarte (`approval/asked`/`approval/decided`, `/api/approvals/:callId`); bei „einmal erlauben“ führt Aether den Aufruf selbst mit weiterem Modus aus und gibt das Ergebnis über die Bridge zurück. |

### 3.8 Phasen (Frage 7)

| Phase | Inhalt | Dateien | Prüfung | Aufwand |
|---|---|---|---|---|
| 0 Pin und Spike | `fetch-pi.py`, `install.rs`, `node_binary()`, Aufnahme echter Ereignisströme mit dem Faux-Provider als Fixtures; mit Erlaubnis des Nutzers **ein** echter Request zur Usage-Semantik (`input` mit oder ohne Cache) | `desktop/fetch-pi.py`, `crates/dshx-pi/src/install.rs`, `dshx-tools/src/ptc.rs`, `crates/dshx-pi/tests/fixtures/*.jsonl` | `cli.js --version`, Prüfsummen, Fixture-Replay | 1–1,5 T |
| **1 MVP** | Engine-Wahl, Header, Liste; Prozessmodell mit Stopp, Absturz, Neustart; Abbildung, Live-Deltas, Status, Warteschlange mit Steer; Aether-Provider über die Bridge; Pi-Werkzeuge + `todo_write`; Sandbox, Gate, Filter; Kosten; Kompaktierung (Pis Druck-Kompaktierung + Aethers Turn-Ende-Modus über `compact` nach `agent_settled`; `/compact` → `compact`) | neuer Crate `crates/dshx-pi/*`, `Cargo.toml`, `dshx-core/src/config.rs` (`default_engine`, `[codex.pi]`: `max_processes`, `idle_secs`), `dshx-session/src/store.rs`, `dsh-codex/src/{runtime.rs, web.rs, web/composer.rs, usage.rs}`, `dshx-server/src/lib.rs` (`engine` beim Anlegen, `GET /api/engines`), UI (`api.js`, `state.js`, `reduce.js`, `modes.js`, `composer.js`, `tools.js`, `i18n.js`), `desktop/build.sh`, `THIRD_PARTY_NOTICES.md`, SPEC §6, README, CHANGELOG | Mapper-Goldens + `validate_log`; Live-Test mit gepinntem Pi und Faux-Erweiterung (`DSHX_PI_TEST=1`): Antwort, Werkzeuge, Steer, Stopp mit Warteschlange, Absturz (`kill`), Neustart; `env` in bash ohne Schlüssel; Gate-Text; `ui/dev/*_check.mjs` inkl. `engine_check.mjs`; `cargo fmt`, `clippy -D warnings`, `cargo test --locked --workspace` (ohne `dshx-browser`); Mac-App mit gebündelter Laufzeit und leerem `DSHX_HOME` | 7–9 T |
| 2 Wechseln | `handover`-Route, `handover.rs`, Notizen, Kettengruppe in der Seitenleiste, Aktionen in `ModeLabel` | `dsh-codex/src/{handover.rs, web.rs}`, `dshx-server/src/lib.rs`, `ui/src/components/{modes.js, sidebar.js}`, `reduce.js` | DSH→Pi→DSH: Kontext kommt an (Faux prüft die Übergabenachricht), Seed-Log besteht `validate_log` | 2–3 T |
| 3 Werkzeugbrücke | `web_*`, Browser, App/Computer, MCP mit Auswahl, Skills, Bild-Ergebnisse, Live-bash-Ausgabe, LLM-Titel, Pi-Modelle in `/api/engines/pi/models` | `dshx-pi/src/bridge.rs`, `aether-bridge.ts`, `runtime.rs`, `web/composer.rs`, `composer-extras.js` | Faux ruft jedes Brückenwerkzeug; Live-Bilder mit richtiger Chat-ID; MCP an/aus mitten im Chat; keine Chrome-Tests nach Nutzerwunsch, nur Bridge-Attrappe | 5–7 T |
| 4 Automatik und Freigaben | Ziele und Zeitpläne für Pi (`SessionResolver` → Engine-Enum), Eskalation mit Freigabekarte, Fork (Aether-Turn → Pi-`fork`/`clone` über Entry-IDs), optionale DSH-Subagenten | `automation.rs`, `dshx-schedule`, `web.rs::fork_session`, `bridge.rs` | Zielrunde, Erinnerung, Freigabe erlauben/ablehnen, Fork an Turn N | 4–6 T |
| 5 iOS und VPS | Engine-Abzeichen und -Wahl in iOS; Laufzeit auf VPS 1/2 (linux-arm64, bwrap); Doku | `ios/Aether/Core/Models/API.swift`, `ServerClient.swift`, Chat-Liste; Release-Paket; OracleVPS `state.md`/`log.md` | Simulator; VPS: Pi-Chat über SSH-Transport, Speicher bei drei Prozessen | 2–3 T |

Gesamt etwa 21–30 Personentage. Das **MVP** (Phase 0 und 1) liefert schon einen
vollwertigen Pi-Chat in Web, Mac-App und iOS (lesend und schreibend über
dieselbe API). Wechseln geht dort vorerst über „neuer Chat mit Engine Pi“;
Phase 2 holt die Übergabe direkt nach.

**Risiken:**

| Risiko | Gegenmaßnahme |
|---|---|
| Pi ändert RPC oder Ereignisse | Pin, Goldens aus aufgezeichneten Strömen, Update nur mit Live-Test |
| Die beiden Logs laufen auseinander | `pi/cursor` + `get_entries`-Nachspiegeln; die Pi-Datei bestimmt den Kontext |
| Schlüssel-Leck über bash | Allowlist, `spawnHook`, Test |
| Rennen zwischen Steer und Stopp | `clear_queue` vor `abort`, Aether hält Folgeprompts, Faux-Test |
| Kein bwrap auf dem VPS | Status anzeigen; Gate wirkt trotzdem; bwrap installieren und in OracleVPS dokumentieren |
| Speicher auf dem VPS (etwa 110 MB pro Prozess) | `max_processes` 3, Leerlauf-Ende |
| UI nimmt DSH-Argumente an | Aliase + `pi.json`-Fixture |
| Lizenzen | Pi und jiti MIT, photon Apache-2.0 → `THIRD_PARTY_NOTICES.md` |

**Bewusst ausgelassen:** Wechsel im selben Log, Pis TUI-Funktionen,
Baum-Navigation, `codemode`/`tool_search`, Pi-OAuth-Anmeldung im MVP,
Windows.

## 4. Umsetzung

Noch nicht gebaut.

## 5. Prüfung

Beim Planen geprüft (2026-11-05, nur Scratch-Ordner): Pi-Doku und Typen
gelesen; RPC-Proben mit Faux-Provider ohne Modellaufruf (Ereignisfolge,
Gate, bash-Umgebung, Abbruch mit steer/followUp, `--session-id` anlegen und
fortsetzen, Start und Speicher, eigenständige 13-MB-Laufzeit mit
TS-Erweiterung). Nicht geprüft: echte Provider-Requests, Usage-Semantik
echter Modelle, Sandbox um den Pi-Prozess, Linux ARM64, Verhalten bei
geschlossenem stdin während eines Laufs. Am Code wurde nichts geändert.

## 6. Offen – Fragen an den Nutzer

| # | Frage | Empfehlung |
|---|---|---|
| **F1** | Wechsel als verknüpfter Fortsetzungs-Chat oder Wechsel im selben Chat? | Fortsetzungs-Chat mit Übergabe, Ketten in der Seitenleiste gruppiert. Den Wechsel im selben Chat nur angehen, wenn sich die Kette nicht bewährt. |
| **F2** | Aether-eigenes Pi-Verzeichnis (`$DSHX_HOME/pi/agent`) oder dein `~/.pi/agent` mit deinen Logins, `models.json` und Skills? | Aether-eigen als Standard (sicher, VPS-tauglich). Optional ein Experten-Schalter „`~/.pi` verwenden“, ohne Gewähr für eigene Erweiterungen. |
| **F3** | Pi-eigene Anmeldungen (OAuth-Abos) in Aether? | Später. Im MVP nur Aether-Provider (DeepSeek, Anthropic, eigene Endpunkte wie Z.ai über die Modellseite). |
| **F4** | Reicht im MVP „Zugriffsmodus ohne Einzel-Freigaben“? | Ja; Eskalation mit Freigabekarte in Phase 4. |
| **F5** | Welche Aether-Werkzeuge bekommt Pi, und sollen DSH-Subagenten dazugehören? | `todo_write` im MVP, dann Web, Browser, App/Computer, MCP. Subagenten vorerst nicht. |
| **F6** | Ziele und Zeitpläne auch für Pi-Chats? | Ja, Phase 4. |
| **F7** | Pis eigene Druck-Kompaktierung behalten und Aethers Turn-Ende-Modus ergänzen? | Ja. |
| **F8** | Projekt-`.pi`-Ressourcen (Erweiterungen aus Repos) laden? | Nein (`--no-approve`). `AGENTS.md` wirkt trotzdem. |
| **F9** | Pi fest pinnen und nur bewusst mit Regressionstest aktualisieren? | Ja (`fetch-pi.py`, Version im Header jedes Pi-Chats). |
