# dsh-codex: UX-Überarbeitung – Analyse und Lösungsplan

Stand 22.10.2026, Branch `codex-app` (`6e06d32`). Grundlage:

- **dsh-codex:** der Code in diesem Repo.
- **Laufende Oberfläche:** angesehen mit Testdaten (`?fixture=coding`, `hero`) und mit dem lokalen Dev-Server gegen das Ersatz-Modell (Plugins, Skills, MCP, Einstellungen).
- **DeepSeek Harness 0.2.0-rc.2:** das installierte npm-Paket `@deepseek-ai/dsh` (MIT-Lizenz, Code und Texte dürfen übernommen werden) und die DSH-Konfiguration des Nutzers in `~/.dsh/profiles/desktop/cordis.patch.yml`.
- **Codex-App:** die offizielle Doku (Befehle und Tastenkürzel, MCP).
- **Preise:** die offizielle DeepSeek-Preisseite, abgerufen am 22.10.2026.

Alle Punkte des Auftrags sind abgedeckt. Kapitel 9 sammelt die übrigen Lücken gegenüber DSH und Codex, Kapitel 10 legt Qualitätsmaßstab und Reihenfolge fest.

## Entscheidungen des Nutzers (22.10.2026)

- **Modus** heißt DSH-Agent-Modus: **Standard** und **Minimal** (Kapitel 5). Die Darstellung (Kapitel 4) kommt trotzdem.
- **Kosten** nur in **USD**.
- **Anbieter vorerst:** DeepSeek, Anthropic und **eigene API** (Anthropic-Messages- oder OpenAI-Chat-Completions-kompatibel). OpenAI Responses, Gemini und die übrigen Katalog-Anbieter kommen später.

## Umsetzung: Pakete und Zuständigkeit

| Welle | Paket | Kapitel | Branch (Worktree) |
|---|---|---|---|
| 1 | A: Arbeitsschritte-Ansicht, Statuszeile, Leiste mit Kontext, Cache und Kosten, Verbrauchsseite | 4, 6 | eigener Worktree |
| 1 | B: Ordnerauswahl, Instanzwechsel (Mac-App und Server) | 2, 7 | eigener Worktree |
| 1 | D: Anbieter DeepSeek, Anthropic, eigene API; Einstellungsseite „Modelle“, Modell-Wähler | 3 | eigener Worktree |
| 1 | E: Modi Standard und Minimal | 5 (ohne Plan-Modus) | eigener Worktree |
| 2 | C: `/` und `@`, Skills und MCP auf Abruf, Skills- und MCP-Seiten, Composer-Punkte der Paritätsliste | 1, 8, 9 | nach Welle 1 |
| 2 | F: übrige P1-Punkte der Paritätsliste (Nachrichtenaktionen, Fehltexte, Tastenkürzel, Bestätigung „Voller Zugriff“) | 9 | nach Welle 1 |

Der Orchestrator führt die Branches nacheinander in `codex-app` zusammen (A, E, D, B), prüft die Oberfläche selbst und startet danach Welle 2.

**Gemeinsame Dateien** wie `composer.js`, `settings.js`, `i18n.js`, `app.css`, `api.js`, `state.js`, `web.rs` und `dshx-server/src/lib.rs` werden nur minimal geändert. Neue Oberfläche kommt in neue Komponenten- und CSS-Dateien, neue Texte in einen markierten Block je Paket am Ende von EN und DE.

---

## 0. Überblick

| Kapitel | Wunsch | Heute | Lösung in einem Satz |
|---|---|---|---|
| 1 | Skills per `/`, MCP per `@`, nichts vorab im Kontext | Skill-Katalog und **alle** MCP-Tools samt Schemas stehen in jeder Anfrage; Composer kennt weder `/` noch `@` | Composer mit Vorschlagsmenü. Skills kommen nur über `/name` in den Kontext. MCP-Server werden erst durch `@server` per `tool_addition` (deferred, cachefreundlich) zugeschaltet. |
| 2 | Projektordner per macOS-Auswahl | Textfeld „Pfad eingeben“ | Mac-App: `NSOpenPanel` über die Brücke. VPS/Browser: Ordner-Browser vom Server. Dazu Drag & Drop und ⌘O. |
| 3 | Weitere Anbieter wie in DSH | Nur DeepSeek (ein `base_url`, fester Katalog) | `[providers.*]` mit Protokoll-Adaptern (Anthropic Messages, OpenAI Chat Completions, OpenAI Responses), Einstellungsseite „Modelle“ wie in DSH, Import aus der DSH-Konfiguration |
| 4 | Statuszeile statt Reasoning- und Tool-Flut | Live-Reasoning wird voll aufgeklappt, jede Tool-Zeile einzeln, Diffs offen | DSH-Ansichten „Kompakt / Standard / Detailliert / Ausführlich“, Standard als Vorgabe. Eine Statuszeile je Arbeitsschritt mit Live-Detail, nach dem Turn eingeklappt. |
| 10 | Durchdachte UX | Funktional, aber an vielen Stellen roh | Qualitätsmaßstab und Abnahmeliste |
| 5 | Modus wechseln | Es gibt keinen Modus-Schalter | Klarstellung: Der heutige Stand ist DSH **Standard**, nicht Minimal. Dazu kommen ein Modus-Wähler beim Chatstart (Standard, Minimal), der Plan-Modus als Schalter (`/plan`) und die Ansicht aus Punkt 4. |
| 6 | Kontext, Cache-Rate und Preis unten in der Leiste | Eine kaum lesbare graue Zeile ohne Interaktion | Leiste mit drei Schaltflächen und Popover: Kontextring, Cache und Kosten; Kosten pro Turn; Verbrauchsseite. DeepSeek-Preise mit Spitzen- und Nebenzeit. |
| 9 | Mindestens so gut wie DSH | Viele Kleinigkeiten fehlen | Paritätsliste mit Prioritäten |
| 9 | Mehr von DSH und Codex übernehmen | – | DSH ist MIT-lizenziert: Textbausteine, Zustandslogik und Ansichtsregeln direkt übernehmen. Von Codex Tastenkürzel und Bedienmuster. |
| 7 | Instanzwechsel ohne grauen Bildschirm | Verbindung startet erst beim Umschalten; bis die Seite steht, verdeckt eine fensterfüllende graue `NSVisualEffectView` alles | Verbindungen beim App-Start vorwärmen, alte Seite bis zur Bereitschaft stehen lassen, Schnappschuss plus kleine Fortschrittspille, Überblendung |
| 8 | Skills-Seite moderner, MCP-UX besser, keine Eingabezeile | Monospace-Liste ohne Suche und Schalter; MCP nur als Zeile „Name + Befehl“ | Skills: Liste mit Suche, Schaltern und Detailansicht in Systemschrift. MCP: Karten, Detailansicht mit Tools, Logs und Anmeldung, Dialog „Server hinzufügen“ mit Formular, Verbindungstest und JSON-Import. |

---

## 1. Skills per `/` und MCP per `@`, ohne Vorladen in den Kontext

### Ist

- **Skills:**
  - `SkillsHook` (`crates/dsh-codex/src/runtime.rs:367`) hängt vor jedem Schritt den Katalog `<available_skills>` an (`crates/dshx-tools/src/skill/catalog.rs:152`), sobald das Tool `skill` angeboten wird. Das betrifft jeden Chat.
  - `/name` in einer Nutzernachricht wird bereits erkannt: `invocation_messages` hängt dann `<skill_content>` an (`catalog.rs:311–360`, DSH-identisch). Es fehlt nur die Bedienung im Composer.
- **MCP:**
  - `Runtime::tools()` (`runtime.rs:252`) registriert **alle** Tools aller verbundenen Server, also jedes Schema in jeder Anfrage.
  - `prompt_sections()` (`runtime.rs:177`) fügt zusätzlich die Server-Anweisungen in den System-Prompt ein.
- **Composer:** `composer.js:125` ist ein einfaches `<textarea>` ohne Auslöser für `/` oder `@`.

### Grundlage, die wir schon haben (wichtig)

Die DeepSeek-Route `deepseek-flash` kann Tools **aufgeschoben** aktivieren:

- `ToolUpdate::AdditionOnly` (`crates/dshx-llm/src/models.rs:25`).
- Kommt ein Tool mitten in einer Serie hinzu, schreibt der Agent eine `developer/message` mit `tool-addition` (`crates/dshx-agent/src/agent.rs:1226–1270`).
- Der Serializer deklariert dieses Tool dann mit `defer_loading: true` (`crates/dshx-llm/src/serialize.rs:481`) und schickt `tool_addition` im Verlauf mit (`serialize.rs:249`).

So wird ein Tool **an der Stelle im Verlauf** freigeschaltet, an der der Nutzer es erwähnt. Der gecachte Präfix davor bleibt gültig. Das ist der Hebel für `@`.

### Lösung: Backend

1. **Skills standardmäßig nur manuell**
   - Neue Einstellung `[skills] catalog = "manual" | "auto"`, Vorgabe `manual`, dazu je Skill `[skills.entries."<name>"] enabled = true|false, model = true|false`.
   - `SkillSummary.model_invocable` ergibt sich dann aus Frontmatter (`disable-model-invocation`) **und** Einstellung. Bei `manual` ist kein Skill modell-aufrufbar. `apply_catalog` (`catalog.rs:267`) veröffentlicht dann keinen Katalog bzw. zieht einen alten zurück, und das Tool `skill` wird nicht angeboten (`runtime.rs:275` nur registrieren, wenn mindestens ein Skill auf `model = true` steht).
   - `/name` funktioniert unverändert: Der volle Skill-Inhalt kommt genau in diesem Turn als `user/message` (`skill-invocation`), also nach dem Cache-Präfix.
   - Abschaltbare Skills (`enabled = false`) erscheinen weder im Menü noch werden sie geladen.
2. **MCP-Server pro Chat aktivieren**
   - `Runtime::tools()` bekommt die Session-ID und registriert nur Tools von Servern, die in **diesem Chat** aktiv sind, plus Servern mit `[mcp_servers.x] always = true` (neue Option, Vorgabe `false`).
   - **Aktiv-Status merken:** keine neue Ereignisart. Die vorhandenen `developer/message`-Ereignisse mit `tool-addition`/`tool-removal` sind die Quelle der Wahrheit; der Journal-Fold liefert die Menge der aktiven `mcp__<server>__*`-Tools. Das bleibt DSH-kompatibel und überlebt Neustarts.
   - **Erwähnung:** `@server` (bzw. `@plugin`) im Prompttext erkennt ein neuer `PreStepHook` analog zu `SkillsHook`. Er schaltet den Server für die Session ein. Der Agent schreibt dann über den bestehenden Mechanismus die `tool-addition`-Nachricht, gefolgt von einer kurzen `user/message` mit Quelle `{kind: "mcp-activation", server}`: „@linear: Tools mcp__linear__* sind ab jetzt verfügbar.“ Danach folgen die Server-Anweisungen, die damit aus `prompt_sections()` heraus wandern und erst bei Aktivierung im Verlauf stehen.
   - **Deaktivieren:** Klick auf ✕ an der Chip-Leiste über dem Composer. Bei `ToolUpdate::InHistory` wird `tool_removal` geschrieben; bei `AdditionOnly` (Flash) bleibt das Tool deklariert, wird aber nicht mehr angeboten. Der Agent kann es dann nicht mehr ausführen (Fehlerantwort „vom Nutzer deaktiviert“).
   - **Routen ohne Tool-Updates** (Pro, andere Anbieter): Dort ändert sich die Tool-Liste. Das kostet genau einen Cache-Fehlschlag beim Aktivieren. Das Menü zeigt dann den Hinweis „Cache wird einmal neu aufgebaut“.
   - `@plugin` schaltet alle MCP-Server des Plugins ein und listet seine Skills als „per / verfügbar“ (wie Codex' `@plugin`-Erwähnung).
3. **Vorschlagsdaten:** `GET /api/composer/suggest?kind=slash|mention&q=&cwd=` liefert:
   - **Befehle:** `compact`, `goal`, `plan`, `model`, `permission` (Zugriff), `status` (Kontext und Kosten), `export`, `view` (Ansicht).
   - **Skills:** Name, Kurzbeschreibung, Quelle, Plugin.
   - **MCP-Server und Konnektoren:** Status, Anzahl Tools, ob eine Anmeldung nötig ist.
   - **Dateien und Ordner** aus dem Projekt: schnelle Suche mit Ignore-Regeln wie `glob`; Tab öffnet einen Ordner wie in DSH.
   - **Chats** als Referenz, in einer zweiten Ausbaustufe.

### Lösung: Composer (UI)

- **Auslöser:** `/` am Textanfang oder nach einem Leerzeichen. `@` überall nach einem Leer- oder Satzzeichen. Pfade wie `/usr/bin` oder E-Mail-Adressen dürfen nichts auslösen: `/` zählt nur, wenn danach Buchstaben folgen und davor kein Zeichen außer einem Leerzeichen steht.
- **Menü:** über dem Composer, 360 px breit, maximal 8 Zeilen, Abschnitte wie in DSH: „Befehle“, „Skills“ bzw. „Apps & MCP“, „Dateien & Ordner“.
  - Jede Zeile hat Symbol, Name (13 px, Gewicht 500), Beschreibung (12 px grau, einzeilig) und rechts die Quelle bzw. den Status (Punkt grün/grau, „Anmelden“).
  - Unscharfe Suche (Präfix vor Teilstring vor Fuzzy), Treffer fett.
  - Bedienung: ↑ ↓ bewegen, ↩ oder Tab wählt, Tab bei Ordnern öffnet den Ordner, Esc schließt, Maus-Hover markiert.
  - Leerer Zustand: „Keine Skills gefunden – Skills verwalten“ als Link zur Skills-Seite.
- **Chips:** Eine Wahl wird zur **Pille im Text** (`/visual-brand-foundation`, `@Linear` mit Logo). Rückschritt löscht die ganze Pille. Technisch bleibt der gesendete Text reiner Text (`/name`, `@server`), damit die Backend-Erkennung und DSH-Kompatibilität bleiben. Umsetzung als `contenteditable` mit Pillen-`<span contenteditable=false>`. Alternativ ein Overlay über dem `<textarea>`; das ist robuster für IME, Undo und Paste.
- **In der gesendeten Nachricht** erscheinen dieselben Pillen in der Nutzerblase. Ein Klick öffnet den Skill bzw. den Server in der Detailansicht.
- **Leiste aktiver Server** über dem Composer, nur wenn welche aktiv sind: `Aktiv: [Linear ✕] [allintitle-gsc ✕]`.
- **Hinweis im Platzhalter** wie in DSH: „Nachricht oder Aufgabe, / für Befehle und Skills, @ für Apps und Dateien“.
- **Codex-Extra:** `$name` als Alias für Skills, weil Codex-Nutzer das gewohnt sind.

### Abnahme

- Wire-Dump-Test mit dem Ersatz-Modell: Eine Anfrage ohne `/` und `@` enthält keinen `<available_skills>`-Block, kein `mcp__`-Tool und keine MCP-Anweisungen.
- Nach `@linear` enthält die nächste Anfrage `tool_addition` für genau dessen Tools.
- **Ein echter Cache-Test mit DeepSeek** (zwei bis drei Anfragen, Kosten im Cent-Bereich, nur nach OK): `cache_read_input_tokens` bleibt nach `@`-Aktivierung hoch. Erst dann gilt „cachefreundlich“ als bewiesen.

---

## 2. Projektordner über die native macOS-Ordnerauswahl

### Ist

`WorkspaceMenu` (`ui/src/components/composer.js:81`) und `AddProject` in der Sidebar (`sidebar.js:157`) bieten nur ein Textfeld „Pfad“.

### Lösung

1. **Mac-App, lokale Instanz**
   - Neue Brückenmethode `window.dshxDesktop.pickFolder({title, prompt, defaultPath})` → `Promise<string|null>`.
   - In Swift: `NSOpenPanel` als **Sheet** am Hauptfenster mit `canChooseDirectories = true`, `canChooseFiles = false`, `canCreateDirectories = true`, `allowsMultipleSelection = false`, `prompt = "Projekt öffnen"`, Startordner zuletzt benutzt bzw. `~`.
   - Gilt nur für Seiten mit dem Origin der eigenen lokalen Instanz (wie `revealInFinder`).
   - Ergebnis → `addWorkspace(path)` → Projekt erscheint oben in der Sidebar und ist sofort ausgewählt.
2. **Mac-App mit VPS-Instanz und reiner Browser**
   - Ordner-Browser als Dialog, gespeist vom Server, wie DSH-„Select Workspace Directory“.
   - Routen: `GET /api/fs/dirs?path=` (nur Unterordner, sortiert, versteckte optional, Limit 500 mit Hinweis „zu viele Ordner“) und `POST /api/fs/mkdir`.
   - Erlaubt sind nur Wurzeln aus der Konfiguration: Home und `/srv/deepseek-workspaces` auf dem VPS.
   - Dialog: Brotkrumen, „Home“, Ordnerliste mit Git-Symbol bei Repos, „Neuer Ordner“, „Pfad bearbeiten“ (das bisherige Textfeld als Rückfall), „Öffnen“. Zuletzt benutzte Ordner und Git-Repos unter der Arbeitswurzel erscheinen oben als Vorschläge.
3. **Browser auf dem Mac mit lokalem Server:** `POST /api/fs/pick` startet `osascript -e 'choose folder'`, wie DSH `host-directory-picker-native`. Nur bei Loopback-Anfrage und nur auf macOS mit grafischer Sitzung, sonst Dialog wie in 2.
4. **Drag & Drop:** Ein Ordner aus dem Finder auf Sidebar oder Startseite fügt das Projekt hinzu. WKWebView liefert keinen Pfad, darum nimmt die Mac-App den Drop im Container-View an (`registerForDraggedTypes([.fileURL])`) und meldet ihn per Ereignis `dshx-desktop-drop {paths}` an die Seite.
5. **Bedienung:**
   - ⌘O „Ordner öffnen“ wie in Codex.
   - Menüeintrag „Ablage → Projekt öffnen …“.
   - Auf der Startseite ersetzt der Projekt-Chip die Textzeile durch „Ordner wählen …“ (öffnet je nach Umgebung 1, 2 oder 3) plus die zuletzt benutzten Projekte.
6. **Fehlerfälle:** Ordner existiert nicht mehr („Ordner nicht gefunden – erneut wählen“), Projekt schon vorhanden (dann nur auswählen, keine Fehlermeldung), Zugriff verweigert. Bei `~/Documents`/`~/Desktop` fragt macOS einmal nach Freigabe für dsh-codex; das muss im ersten Lauf geprüft werden.

---

## 3. Weitere Anbieter (wie DeepSeek Harness)

### Ist

- `Config` hat genau einen `base_url` (`crates/dshx-core/src/config.rs:22`).
- `dshx-llm` kennt nur den DeepSeek-Katalog (`models.rs:60`) und das Anthropic-Wire-Format von DeepSeek.
- Einstellungen „Anbieter“ zeigen nur den DeepSeek-Schlüssel.

### Vorbild DSH

- Provider über `dsh-llm-pi-ai` mit Protokollen `anthropic-messages`, `openai-completions`, `openai-responses` (dazu Bedrock, Mistral, Azure).
- Je Provider `apiKeyEnv` bzw. ein gespeicherter Schlüssel, `baseURL` und eine Modellliste (`contextWindow`, `maxTokens`, `input`, `reasoningEfforts`, `compat`).
- Einstellungsseite „Models“:
  - „Add model provider“ mit zwei Wegen: „Third-party model provider“ aus dem Katalog oder „Custom model API“ mit Basis-URL, Protokoll und Provider-ID.
  - „Fetch available models“ ruft die Modelle beim Anbieter ab; man wählt sie per Haken.
  - Modellzeilen mit ID, Anzeigename, Kontextfenster, maximaler Ausgabe und Eingabearten.
- Deine DSH-Konfiguration hat bereits opencode-go, xiaomi, zai, command-code und anthropic.

### Lösung

1. **Konfiguration**

   ```toml
   [providers.anthropic]
   api = "anthropic-messages"          # anthropic-messages | openai-completions | openai-responses
   base_url = "https://api.anthropic.com"
   api_key_env = "ANTHROPIC_API_KEY"   # alternativ gespeichert in credentials.toml [providers.anthropic]
   [[providers.anthropic.models]]
   id = "claude-sonnet-5-5"
   name = "Claude Sonnet 5.5"
   context_window = 1_000_000
   max_tokens = 64_000
   input = ["text", "image"]
   reasoning_efforts = ["low", "medium", "high", "max"]
   cost = { input = 3.0, output = 15.0, cache_read = 0.3, cache_write = 3.75 }  # USD je 1 Mio.
   ```

   `deepseek-official` bleibt eingebaut und unverändert. Modellwahl wird `provider/model`; die Session schreibt `provider` schon heute in `request/context` (`agent.rs:1281`).
2. **`dshx-llm`: Adapter hinter einem Trait** `ProviderAdapter { serialize(GenerateRequest) -> WireRequest; stream(SSE) -> StreamChunk; usage(); errors() }`
   - **DeepSeek:** der heutige Serializer, Byte für Byte unverändert (Cache-Parität mit DSH bleibt).
   - **Anthropic Messages, generisch:** für Anthropic selbst und Anthropic-kompatible Endpunkte wie Kimi, MiniMax oder Z.ai. `cache_control`-Breakpoints auf System, Tools und die letzte Nutzernachricht; Thinking mit Signatur-Replay nur für dieselbe Route.
   - **OpenAI Chat Completions:** OpenRouter, Z.ai, Moonshot, opencode-go, command-code, xiaomi, Groq, Ollama, LM Studio. Dazu gehören `reasoning_content`/`reasoning` im Delta, `tool_calls`, Bilder als `image_url`-Data-URI und `usage.prompt_tokens_details.cached_tokens`.
   - **OpenAI Responses:** GPT-Modelle mit `store=false`, verschlüsselte Reasoning-Items als Replay.
   - **Gemini:** zunächst über den OpenAI-kompatiblen Endpunkt.
3. **Fähigkeiten je Route** (heute DeepSeek-fest in `RouteCapabilities`):
   - System-Prompt im Verlauf: nur DeepSeek; sonst oben und neue Serie.
   - Tool-Updates: nur DeepSeek; sonst volle Liste.
   - Bilder ja/nein und Bildbudget: Anthropic etwa 5 MB je Bild, OpenAI eigene Grenzen.
   - Effort-Abbildung und Ausgabegrenze.
   - Beim Anbieterwechsel mitten im Chat fremde Reasoning-Signaturen verwerfen (`project_messages` tut das schon für fremde Replay-Daten).
4. **Einstellungen → „Modelle“** (ersetzt „Anbieter“), Aufbau wie in DSH:
   - Karten je Provider mit Punkt „Schlüssel gesetzt/fehlt“, Modellanzahl, Bearbeiten und Löschen.
   - „Anbieter hinzufügen“ → Katalog mit Logo und Suche: Anthropic, OpenAI, OpenRouter, Z.ai, Moonshot (Kimi), xAI, Gemini, Mistral, Groq, Ollama und LM Studio (lokal, ohne Schlüssel), oder „Eigene API“ mit Basis-URL, Protokoll, ID und Anzeigename.
   - „Modelle abrufen“ (`GET {base}/models`) → Dialog mit Haken und Suche.
   - Schlüssel liegt in `credentials.toml` (0600) bzw. auf dem VPS. Ist eine Umgebungsvariable gesetzt, zeigt das Feld „aus der Umgebung“ und ist schreibgeschützt.
   - **„Aus DeepSeek Harness übernehmen“**: liest `~/.dsh/profiles/*/cordis.patch.yml` (`llm-pi-ai.providers`, nur Namen der Umgebungsvariablen, keine Schlüssel) und legt die Provider an.
5. **Modell-Wähler** (Composer):
   - Gruppiert nach Anbieter, mit Suche (⌃⇧M wie Codex) und „Zuletzt benutzt“ oben.
   - Je Modell: Kontextgröße, Bild ja/nein, Preis je 1 Mio. Tokens.
   - Effort-Leiste nur, wenn das Modell Stufen kennt.
6. **Subagenten und Teams** dürfen andere Modelle nutzen (DSH `allowedModels`).
7. **Abnahme:** Wire-Goldens je Adapter gegen die Ersatz-Server, ein echter kurzer Turn je Anbieter mit deinem Schlüssel (nur nach OK), Wechsel des Anbieters mitten im Chat ohne Fehler.

---

## 4. Statuszeile statt Reasoning- und Tool-Flut

### Ist (`ui/src/components/transcript.js`)

- `Reasoning` ist live immer aufgeklappt (`transcript.js:27`: `show = live || open`). Der ganze Denktext läuft sichtbar durch.
- Jedes Tool ist eine eigene Zeile. Bearbeitungen öffnen den Diff standardmäßig (`tools.js:152`: `useOpen(!failed)`). Lange Turns werden zu langen Listen.
- Erst nach dem Turn gibt es „N s gearbeitet“ zum Einklappen.

### Vorbild DSH

Einstellung „Work details“ mit vier Stufen (`dsh-client-ui-chat`, Tabelle `POLICIES`). Die Desktop-App nutzt **Standard**.

| Ansicht | Abgeschlossene Turns einklappen | Schritte gruppieren | Live-Detail in der Statuszeile | Reasoning-Vorschau (eine Zeile) |
|---|---|---|---|---|
| Kompakt | ja | eingeklappt | nein | nein |
| **Standard** (Vorgabe) | ja | eingeklappt | ja | ja |
| Detailliert | ja | Verlauf offen, solange der Turn läuft | ja | ja |
| Ausführlich | nein | keine | nein | ja |

### Lösung (DSH-Logik übernehmen)

1. **Arbeitsgruppe** = alle aufeinanderfolgenden Reasoning- und Tool-Einträge zwischen zwei Antworttexten. Sie zeigt **eine** Kopfzeile:
   - **Live:** Symbol + Tätigkeit + „·“ + Live-Detail, mit Schimmer. Beispiele: „Führt Befehle aus · npm test“, „Bearbeitet Dateien · src/app.ts“.
   - Ein Titelwechsel bleibt mindestens **150 ms** stehen (DSH `PROCESS_TITLE_MINIMUM_MS`), damit nichts flackert.
   - Solange ein Tool-Aufruf noch gestreamt wird: „Bereitet vor, Befehle auszuführen“.
   - **Abgeschlossen:** Zusammenfassung der häufigsten Tätigkeiten, z. B. „Dateien gelesen und Befehle ausgeführt“ oder „Code durchsucht, Dateien bearbeitet, Befehle ausgeführt usw.“ (DSH `processTitle`).
   - Ein Klick klappt die einzelnen Tool-Zeilen auf (die heutigen Zeilen bleiben als Inhalt).
2. **Reasoning** zeigt live nur **eine Zeile**: die erste Zeile des zuletzt fertigen Absatzes, ohne `**`, mit Schimmer (DSH `latestCompletedParagraphFirstLine`). Ein Klick zeigt den ganzen Text. In der Ansicht Ausführlich ist er wie heute offen.
3. **Nach dem Turn:** Die gesamte Arbeit klappt zu einer Zeile zusammen: „Fertig in 1 min 20 s · 12 Tool-Aufrufe · 3 Nachrichten · 0,4 ct“ (Kosten aus Kapitel 6). Die Antwort und die Karte „N Dateien geändert“ bleiben sichtbar.
4. **Bleibt immer offen:** Freigabe-Anfragen, Rückfragen, Fehler, Abbruch und Unterbrechung (DSH `alwaysOpen`). Ebenso eine laufende Browser- oder Computer-Aktion als kleines Live-Vorschaubild in der Statuszeile; das Seitenpanel bleibt die große Ansicht.
5. **Diffs** stehen in Standard und Kompakt nicht mehr offen im Verlauf. Sie sind über „N Dateien geändert → Ansehen“ und das Review-Panel erreichbar; in Detailliert und Ausführlich bleiben sie wie heute offen.
6. **Wortschatz (deutsch, nach DSH `message.stepProcess.*`)**

   | Art | Live | Vorbereitung | Fertig |
   |---|---|---|---|
   | thinking | Analysiert die Anfrage | – | Analyse abgeschlossen |
   | read | Liest Dateien | Bereitet Lesen vor | Dateien gelesen |
   | readImage | Liest Bilder | … | Bilder gelesen |
   | search (glob, grep) | Durchsucht Code | … | Code durchsucht |
   | edit | Bearbeitet Dateien | … | Dateien bearbeitet |
   | write | Schreibt Dateien | … | Dateien geschrieben |
   | commands (bash, job_*) | Führt Befehle aus | … | Befehle ausgeführt |
   | webSearch, webFetch | Sucht im Web / Besucht Webseiten | … | Im Web gesucht / Webseiten besucht |
   | subagents (subagent*, team) | Koordiniert Subagenten | … | Subagenten koordiniert |
   | plan (todo_write) | Aktualisiert den Plan | … | Plan aktualisiert |
   | browser (neu) | Bedient den Browser | … | Browser bedient |
   | computer, app (neu) | Steuert den Mac | … | Mac gesteuert |
   | mcp (neu) | Nutzt Linear | … | Linear genutzt |
   | skill | Lädt Skill | … | Skill geladen |
   | questions | Wartet auf dich | … | Rückfrage gestellt |

7. **Einstellung:**
   - Einstellungen → Darstellung → „Arbeitsschritte: Kompakt | Standard | Detailliert | Ausführlich“.
   - Schnellwechsel im „…“-Menü der Kopfzeile und per `/view`.
   - Gespeichert serverseitig (`ui-state.json`), damit es auch für VPS-Instanz und App-Neustart gilt.
8. **Leistung:** Die Gruppierung wird in `reduce.js` einmal pro Ereignis berechnet, nicht pro Render. Lange Chats laden ältere Turns erst bei Bedarf („Frühere laden“ wie in DSH).

---

## 5. Modus wechseln

### Klarstellung

dsh-codex entspricht heute dem DSH-Modus **Standard**, nicht Minimal:

- Gleiche Tools: `bash`, `read`, `edit`, `write`, `glob`, `grep`, Jobs, `todo_write`, `web_fetch`, `skill`, Subagenten, Goals.
- Gleicher System-Prompt, Kontext-Komprimierung und Laufzeitkontext.
- Dazu kommen `browser`, `app` und `computer`.
- Gegenüber Standard fehlen: Plan-Modus (`exit_plan_mode`), `workflow`/PTC, Rückfragen (`ask_user`) und „Dateien übergeben“ (`present`).
- DSH **Minimal** hat nur **ein dauerhaftes Shell-Tool und einen festen System-Prompt**, ohne Skills, Planung, Komprimierung und Laufzeitkontext. Gedacht ist das als Vergleichsbasis für Experimente.

Wahrscheinlich wirkt der Verlauf „minimal“, weil er roh aussieht. Das löst die Ansicht aus Kapitel 4. Ein echter Modus-Schalter fehlt trotzdem.

### Lösung

1. **Modus beim Chatstart** (DSH-Muster: wird für eine neue Aufgabe gewählt):
   - Chip im Composer der Startseite: „Standard ▾“ → Standard, Minimal, später „Eigener Modus“.
   - Jede Zeile hat eine Erklärung und „Wann wählen?“ (Texte aus DSH `guideStandard*`/`guideMinimal*` übersetzen).
   - Gespeichert im Session-Header (`extra.preset`). Ein Wechsel mitten im Chat ist nicht möglich (Prompt und Tool-Liste würden den Cache brechen); stattdessen „Neuen Chat mit Modus … starten“.
   - Minimal so bauen wie DSH: den exakten Prompt und das Tool-Schema per Wire-Dump aus dem installierten DSH holen (`tools/dev/mock_deepseek.py` mit DUMP_DIR, siehe SPEC §2).
2. **Plan-Modus als Schalter innerhalb des Chats** (DSH `dsh-plan-mode`):
   - `/plan` oder ein Chip „Plan“ im Composer. Der Agent arbeitet nur lesend und legt einen Plan vor (`exit_plan_mode`).
   - Karte „Plan prüfen“: Annehmen, Änderungen wünschen, Ablehnen. Der Plan öffnet sich im Seitenpanel.
3. **Rückfragen** (`ask_user`): Karte mit Optionen, „Empfohlen“-Markierung und eigener Antwort; Countdown „Weiter in 30 s“, pausierbar. Das sind DSH-Texte aus `dsh-client-ui-user-questions`.
4. **Zugriffsstufe** (Nur lesen, Auto, Voller Zugriff) bleibt ein eigener Chip. „Voller Zugriff“ bekommt wie in DSH einen Bestätigungsdialog mit „Ich verstehe das Risiko“.

---

## 6. Kontext, Cache und Preis in der Leiste

### Ist

`Stats` (`composer.js:292`) zeigt eine graue 11-px-Zeile: „deepseek-flash · 68k Tokens · 86 % aus dem Cache · 1 % vom Kontext“. Sie ist nicht klickbar und hat keine Aufschlüsselung.

**Vorhandene Daten:**

- Jede `assistant/message` trägt `usage` mit `inputTokens` (nur ungecacht), `cacheReadTokens`, `cacheWriteTokens`, `outputTokens` und `reasoningTokens` (`crates/dshx-core/src/llm.rs:39`).
- Jedes Ereignis hat `time` in ms.
- `stream` enthält Zeitstempel pro Chunk (`dshx-agent/src/stream.rs:170`).

Daraus lassen sich Kosten, TTFT, Token/s, LLM-Zeit und Tool-Zeit exakt berechnen.

### Preise DeepSeek (offizielle Seite, abgerufen 22.10.2026, USD je 1 Mio. Tokens)

| Modell | Eingabe Cache-Treffer | Eingabe ohne Cache | Ausgabe |
|---|---|---|---|
| `deepseek-flash` (V4.1-Flash-1017) | 0,006 (Nebenzeit 0,003) | 0,30 (0,15) | 1,20 (0,60) |
| `deepseek-v4-pro` (V4-Pro-0813) | 0,044 (0,022) | 1,32 (0,66) | 3,96 (1,98) |

- **Spitzenzeit:** Montag bis Freitag 01:00–04:00 und 06:00–10:00 UTC, außer an chinesischen Feiertagen. Alle anderen Zeiten sind Nebenzeit mit halbem Preis.
- **Search und Fetch API:** 0,75 USD je 1.000 Aufrufe. Über Harness-Integrationen bis 01.11.2026 kostenlos.

### Lösung

1. **Berechnung im Backend** als einzige Quelle: `GET /api/sessions/{id}/stats`.
   - Formel je Anfrage: `kosten = (input × ohne_cache + cache_read × treffer + cache_write × schreiben + output × ausgabe) / 1e6`.
   - Der Tarif hängt vom Zeitstempel der Anfrage ab (Spitze oder Nebenzeit, UTC).
   - Chinesische Feiertage stehen als kleine Tabelle je Jahr im Code. Fehlt das Jahr, wird Spitzenzeit angenommen und die Zahl mit „≈“ markiert.
   - Preise anderer Anbieter kommen aus `cost` in `[providers.*]`. Ohne Preis werden nur Tokens gezeigt.
   - Die Preistabelle trägt Quelle und Prüfdatum im Code.
2. **Leiste unter dem Composer** (ersetzt `Stats`), drei Schaltflächen im Stil der Composer-Chips, rechts ausgerichtet:
   - **Kontextring** (16 px, Füllgrad) „23 %“. Popover:
     - „23 % von 1 Mio. Tokens belegt“.
     - Aufschlüsselung System-Prompt / Tool-Definitionen / Nachrichten (DSH `context.*`; Schätzung aus der letzten Anfrage und den Header-Längen).
     - Hinweis „Automatisch komprimiert ab 80 %“ und Knopf „Jetzt komprimieren“ (`/compact`).
     - Der Ring wird ab 70 % gelb und ab 90 % rot.
   - **Cache „94 %“.** Popover:
     - Trefferquote des Chats und der letzten Anfrage.
     - Gecachte und ungecachte Eingabe, Cache-Schreibvorgänge.
     - „Durch den Cache gespart: 0,31 $“.
   - **Kosten „0,042 $“.** Popover:
     - Chat gesamt und letzter Turn, Aufteilung nach Eingabe (Cache), Eingabe (ohne Cache) und Ausgabe (davon Reasoning).
     - Anteil Nebenzeit.
     - Leistung: TTFT, Token/s, LLM-Zeit und Tool-Zeit (DSH `stats.dialog.*`).
     - Link „Verbrauch aller Chats“.
3. **Pro Turn:** In der eingeklappten Turn-Zeile „Fertig in … · 0,4 ct“. Hover oder Klick öffnet die Turn-Nutzung mit Anbieter, Modell, Cache-Treffer, Eingabe, Ausgabe und Token/s (DSH `message.turnUsage.*`).
4. **Verbrauchsseite** (Einstellungen → Verbrauch):
   - Kosten und Tokens pro Tag und Modell für 7 bzw. 30 Tage, die teuersten Chats, Cache-Quote im Verlauf.
   - `GET /api/usage?since=` summiert über alle Session-Logs; ein Index-Cache verhindert, dass jedes Mal alles gelesen wird.
5. **Einstellungen:**
   - „Leistung & Verbrauch: Kompakt | Detailliert“ (DSH): Kompakt zeigt nur Ring und Kosten.
   - Währung USD; optional EUR mit fest eingetragenem Kurs. Es gibt keinen Online-Abruf, weil SPEC §1.5 sagt: nichts Zusätzliches senden.
6. **Sidebar:** Beim Überfahren eines Chats erscheinen Kosten und Kontext im Tooltip.
7. **`web_search`** wird heute ausgeblendet (`runtime.rs:257`, „noch kein Anbieter“). Es kann jetzt an die DeepSeek Search/Fetch API angebunden werden, so wie DSH `web-search-deepseek`. Die Kosten pro Aufruf fließen in die Rechnung ein.

---

## 7. Instanzwechsel ohne grauen Zwischenbildschirm

### Ist (`desktop/Sources`)

- `InstanceManager.connection(id)` (`InstanceManager.swift:32`) startet eine Verbindung erst beim ersten `activate`. Der Wechsel zu VPS 2 beginnt also mit SSH-Aufbau, Token-Abruf und Seitenladen (3–5 s).
- Bis die neue Seite geladen ist, zeigt `updateOverlay` (`MainWindow.swift:173`) die fensterfüllende `StatusView` (`MainWindow.swift:547`, `NSVisualEffectView` `.underWindowBackground`, also die große graue Fläche) mit Symbol, Titel und Spinner.

### Lösung

1. **Vorwärmen:** Beim App-Start werden **alle** Instanzen im Hintergrund verbunden, die aktuelle zuerst, die anderen 2 s später. Dazu gehören Tunnel, Token und ein **unsichtbar vorgeladenes** WKWebView. Der Wechsel ist dann sofort da; die Seiten bleiben ohnehin im Hintergrund geladen. Pro Instanz gibt es die Option „Beim Start verbinden“ (Vorgabe an). Beim Hover über eine Instanz im Wechsler wird zusätzlich vorgewärmt.
2. **Kein leerer Zwischenzustand:**
   - Ist die Zielinstanz noch nicht bereit, bleibt die **bisherige Seite stehen**.
   - Im Instanzwechsler oben links erscheint neben dem Zielnamen ein Spinner mit „Verbinde mit VPS 2 …“, dazu eine 2 px hohe Fortschrittslinie unter der Titelleiste.
   - Erst wenn die Zielseite gerendert ist, wird überblendet (150 ms Crossfade über zwei Layer). „Gerendert“ heißt: Die Web-UI meldet `dshxDesktop.ready()` nach dem ersten vollständigen Render von Sidebar und Chat, nicht schon `didFinish`.
   - War die Instanz schon einmal offen, wird sofort ihr **letzter Schnappschuss** gezeigt (`WKWebView.takeSnapshot` beim Verlassen) und leicht abgedunkelt mit einer Pille „Verbinde …“. Sobald sie live ist, wird überblendet.
3. **Hintergrundfarbe:** Fenster- und WebView-Hintergrund bekommen die Theme-Farbe der UI (`#FFFFFF`/`#181818`) statt der Material-Grautöne. Die Web-UI meldet ihr Theme über die Brücke. So blitzt nichts.
4. **Fehler nicht vollflächig:** Schnappschuss oder alte Seite bleiben sichtbar. Darüber erscheint ein Banner oben mit Grund („SSH: Host nicht erreichbar“), „Erneut versuchen“ und „Instanzen bearbeiten“. Die Vollflächen-`StatusView` bleibt nur für „noch nie verbunden und Fehler“.
5. **Schneller verbinden:**
   - `ControlPersist=10m` statt `no`, damit Token-Abruf und Neuaufbau über die bestehende Verbindung laufen.
   - Token zwischenspeichern und erst bei 401 neu holen (Ablauf existiert schon).
   - Auf dem VPS den Token zusätzlich in eine Datei mit Rechten 0600 schreiben (`--ready-file` gibt es bereits). Dann reicht `cat` statt `journalctl`, und es ist nur ein SSH-Aufruf nötig.
6. **Wechsler in der Web-UI** (`sidebar.js:220`): Statuspunkte (grün bereit, grau animiert verbindet, rot Fehler), ungelesene Chats je Instanz als Zahl, letzte Verbindungszeit, Tastenkürzel neben jedem Eintrag.
7. **Tastenkürzel an Codex angleichen:** Codex nutzt ⌘1–9 für Chats und ⌃1–3 für Bereiche. Darum Instanzen auf **⌃1–⌃9** legen, ⌘1–9 für Chats freigeben.

---

## 8. Skills- und MCP-Seiten

### Ist (`ui/src/components/plugins.js`)

- **Skills** (`plugins.js:224`): reine Liste, Namen fett in Monospace, keine Suche, keine Schalter, keine Detailansicht, keine Gruppierung.
- **MCP** (`plugins.js:277`): Liste, darunter eine Zeile „Name + Befehl/URL“ (`serverConfig` zerlegt den Text) und „Server hinzufügen“. Es gibt keine Umgebungsvariablen, keine Header, keinen Test und keine Logs.

### Lösung: Skills

- **Schrift:** Systemschrift (Inter bzw. SF Pro), Name 14 px Gewicht 600 als lesbarer Titel („Visual Brand Foundation“ aus `name` bzw. Frontmatter `title`), Kurzname nur als kleiner grauer Zusatz `/visual-brand-foundation` in 12 px Mono. Beschreibung 13 px, zwei Zeilen, dann „…“.
- **Kopf:** Suche („Skills durchsuchen“), Filter-Pillen „Alle · Projekt · Persönlich · Plugins · System“, rechts „Neuer Skill“.
- **Zeile:** Initialen-Symbol bzw. Plugin-Logo, Titel, Beschreibung, Quelle als Pille, rechts zwei Bedienelemente: Schalter „Aktiv“ und Menü „Vom Modell selbst laden lassen“ (an/aus, Vorgabe aus; siehe Kapitel 1).
- **Detail-Schublade rechts** (Klick auf eine Zeile):
  - Gerenderte `SKILL.md` und Dateien des Skill-Ordners.
  - Pfad, „Im Finder zeigen“, „Im Editor öffnen“ (nur lokal).
  - „Im Chat verwenden“ fügt `/name` in den Composer ein.
- **„Neuer Skill“:** zwei Wege. „Mit dem Agent erstellen“ öffnet einen neuen Chat mit dem Skill `skill-creator`. „Leere Vorlage“ legt einen Ordner mit `SKILL.md`-Gerüst unter `~/.agents/skills` bzw. im Projekt an.
- **Leere und Fehlerzustände:** „Keine Skills in diesem Projekt – Skills liegen in .agents/skills“ mit Link. Fehlerhafte Frontmatter erscheint als gelber Hinweis in der Zeile.

### Lösung: MCP

- **Karten** wie bei Konnektoren:
  - Logo bzw. Initiale, Name, Typ-Pille „Lokal“ (stdio) bzw. „Remote“ (HTTP), Status mit Punkt und Text, „12 Tools“.
  - Schalter „Aktiv“ und Schalter „Immer laden“ (Vorgabe aus, siehe Kapitel 1). Menü mit Neu starten, Abmelden und Entfernen.
- **Detail-Schublade** mit Reitern:
  - **Tools:** Liste mit Beschreibung, Schalter je Tool (neue Option `disabled_tools = [...]`), Schema aufklappbar.
  - **Einstellungen:** dasselbe Formular wie beim Hinzufügen.
  - **Protokoll:** die letzten 200 Zeilen stderr bzw. HTTP-Fehler mit Zeit.
  - **Anmeldung:** OAuth-Status, Konto und Ablauf, „Neu anmelden“.
- **„Server hinzufügen“ als Dialog** (statt Eingabezeile), nach Codex „Settings → Integrations → MCP servers“:
  1. **Auswahl:** „Aus dem Katalog“ (die heutigen Konnektoren mit Suche) · „Eigener Server“ · „JSON einfügen“.
  2. **Eigener Server:** Name; Typ als Umschalter **Lokal (STDIO) | Remote (HTTP)**.
     - Lokal: Befehl, Argumente als einzelne Felder bzw. Chips, Umgebungsvariablen als Tabelle mit Schlüssel und Wert (Werte maskiert, gespeichert in `credentials.toml`), „Variablen aus der Umgebung durchreichen“, Arbeitsordner mit Ordnerauswahl aus Kapitel 2.
     - Remote: URL; Anmeldung **Keine | Bearer-Token | OAuth (automatisch erkannt) | Eigene Header** (Tabelle).
  3. **„Verbindung testen“** führt vor dem Speichern Handshake und `tools/list` aus und zeigt „Verbunden – 12 Tools“ bzw. den Fehler mit stderr-Auszug. Speichern geht auch ohne Test, dann mit Warnung.
  4. **„JSON einfügen“:** `{"mcpServers": {...}}` im Format von Claude, Cursor oder VS Code bzw. der Schnipsel aus einer README wird erkannt, ins Formular übernommen und bei mehreren Servern als Liste mit Haken angeboten.
- **Tab-Aufteilung wie Codex:** „Plugins · Apps (Konnektoren) · Skills · MCP-Server“. Apps und MCP-Server teilen dieselbe Detail-Schublade.

---

## 9. Weitere Lücken gegenüber DSH und Codex (Paritätsliste)

Priorität P1 = mit dem ersten Paket, P2 = danach, P3 = bei Gelegenheit.

| Bereich | Fehlt heute | Vorbild | Prio |
|---|---|---|---|
| Composer | ↑ im leeren Feld holt den letzten Prompt | Codex | P1 |
| Composer | Beliebige Dateien anhängen (nicht nur Bilder), Ordner per Drag & Drop als Referenz | DSH `file.*`, `attachment.*` | P1 |
| Composer | Eingereihte Nachrichten sichtbar, bearbeitbar, löschbar; ⌘↩ „alle sofort einbringen“ | DSH `steerQueue` | P1 |
| Composer | Klare Fehltexte bei Bildgrenzen (Anzahl, Größe, Modell ohne Bilder) | DSH `image.*` | P2 |
| Verlauf | Nachricht kopieren, „Neu senden“, „Bearbeiten und neu senden“, „Ab hier als neuen Chat abzweigen“ | DSH `message.branch`, Codex | P1 |
| Verlauf | Turn-Navigation (Sprungmarken am Rand), „Frühere laden“ bei langen Chats | DSH `chat.turnNavigation`, `chat.loadOlder` | P2 |
| Verlauf | Suche im Chat (⌘F) | Codex | P2 |
| Verlauf | Komprimierung lesbar: „N Einträge komprimiert (~X Tokens)“, Zusammenfassung aufklappbar | DSH `message.compaction.*` | P2 |
| Verlauf | Fehlertexte je Code (Schlüssel ungültig, Guthaben leer, Kontext voll) mit passender Aktion | DSH `message.failure.*` | P1 |
| Verlauf | „Ausgabegrenze erreicht – ‚weiter‘ senden“ mit Knopf | DSH `message.maxTokens.hint` | P2 |
| Werkzeuge | Rückfragen (`ask_user`), Plan-Modus, Dateien übergeben (`present`) mit Karten | DSH | P2 |
| Werkzeuge | `web_search` über die DeepSeek Search API | DSH | P1 |
| Sidebar | Gruppieren nach Projekt / Baum / flach, Sortierung, Archiv-Filter, Volltextsuche über Chats | DSH `dsh-client-ui-workspace` | P2 |
| Sidebar | Tastenkürzel: ⌘⇧A archivieren, ⌘⌥P anheften, ⌘⌥R umbenennen, ⇧Esc alles gelesen, ⌘⌥A nächster Chat, der dich braucht | Codex | P1 |
| Seitenpanel | Dateibaum (⌘⇧E), Dateisuche (⌘P), Terminal (⌃`) | Codex, DSH | P2 |
| Öffnen in | Liste erkannter Apps (Cursor, VS Code, Zed, Xcode, JetBrains, Terminal, iTerm, Ghostty …) statt eines Editors | DSH `open-in-app` | P2 |
| Einstellungen | Darstellung (Arbeitsschritte, Leistung & Verbrauch, Links im Seitenpanel oder Browser, Schriftgröße) | DSH `ui-chat` | P1 |
| Einstellungen | Tastenkürzel-Übersicht (⌘/) | Codex | P2 |
| Freigaben | Bestätigung beim Einschalten von „Voller Zugriff“ | DSH `permission-presets` | P1 |
| Allgemein | Befehlspalette ⌘K mit allen Aktionen (heute nur Chatsuche) | Codex | P2 |
| Allgemein | Rückmeldung „gut/schlecht“ mit Kategorien (nur lokal gespeichert, kein Versand) | DSH `message-feedback` | P3 |

---

## 10. Qualitätsmaßstab und Vorgehen

**Abnahme für jede Funktion** (gilt für alle Kapitel):

1. **Zustände:** leer, lädt, Fehler, viele Daten (200 Skills, 50 MCP-Tools, 500 Turns). Jeder Zustand hat einen sinnvollen Text und eine nächste Aktion.
2. **Bedienung:** komplett per Tastatur, sichtbarer Fokus, Esc schließt, ↩ bestätigt. Menüs folgen dem Muster von `Menu` in `common.js`.
3. **Darstellung:** hell, dunkel und Handybreite (390 px), Deutsch und Englisch. Kein Layoutsprung beim Laden; Animationen 120–200 ms, nur `opacity` und `transform`.
4. **Vergleich:** Für jede neue Ansicht ein Screenshot neben DSH bzw. Codex in `ui/screenshots/compare/`. Gleich gut oder besser ist Pflicht für die Abnahme.
5. **Tests:** Fixture je neuem UI-Zustand (`ui/fixtures`); Wire-Dump-Tests für den Kontext (Kapitel 1); Rust-Tests für Kostenrechnung (Spitzen- und Nebenzeit, Feiertag, fehlender Preis) und Provider-Adapter.
6. **Echte Läufe:** gegen DeepSeek bzw. andere Anbieter nur nach ausdrücklichem OK, mit Angabe der erwarteten Kosten.

**Reihenfolge** (jedes Paket endet lauffähig mit Commit; VPS-Rollout nur nach OK):

| Paket | Inhalt | Warum zuerst | Grob |
|---|---|---|---|
| A | Arbeitsschritte-Ansicht mit Statuszeile (4), Leiste mit Kontext, Cache und Kosten (6), Darstellungs-Einstellungen | sofort spürbar, nur UI und eine Stats-Route | 2–3 Tage |
| B | Ordnerauswahl (2), Instanzwechsel (7) | klein, betrifft Mac-App und Server getrennt | 1–2 Tage |
| C | `/` und `@` mit Skills und MCP auf Abruf (1), Skills- und MCP-Seiten (8) | Backend-Änderung am Kontext, braucht den Cache-Test | 3–4 Tage |
| D | Provider (3) | größter Umbau in `dshx-llm` | 4–6 Tage |
| E | Modi und Plan-Modus (5), restliche Paritätsliste (9) | baut auf A–D auf | fortlaufend |

**Parallel möglich:** B (Swift plus eine Server-Route) neben A (UI). D in eigenem Worktree, weil fast nur `dshx-llm` und `dshx-core/config` betroffen sind. C erst nach A, weil beide den Composer und den Verlauf anfassen.

**Offene Entscheidungen:**

1. Meintest du mit „Mode“ die Darstellung (Kapitel 4) oder die Agent-Ausstattung (Kapitel 5)? Der Plan deckt beides ab.
2. Kosten in USD (so rechnet DeepSeek ab) oder zusätzlich EUR mit festem Kurs?
3. Welche Anbieter zuerst? Vorschlag: Anthropic, OpenRouter, Z.ai, Kimi und Ollama, weil sie zu deiner DSH-Konfiguration passen.

---

## Stand nach der Umsetzung (22.10.2026, Branch `codex-app` mit Paket H)

**Umgesetzt** (Einzelheiten im CHANGELOG unter „Unreleased“):

- **A:** Arbeitsschritte-Ansicht mit Statuszeile (Kompakt, Standard, Detailliert, Ausführlich), eingeklappte Turns mit Dauer, Tool-Aufrufen und Kosten, Leiste mit Kontext, Cache und Kosten, Verbrauchsseite mit DeepSeek-Spitzen- und Nebenzeit.
- **B:** Ordnerauswahl (Mac-Sheet, macOS-Dialog des Servers, Ordner-Browser), Instanzwechsel ohne grauen Bildschirm, ⌃1–⌃9.
- **C:** `/` und `@` im Composer mit Pillen, Skills und MCP-Server nur auf Abruf (cachefreundlich per `tool-addition`), Warteschlange, Anhänge, `/compact`, `/export`.
- **D:** Anbieter DeepSeek, Anthropic und eigene API (Anthropic Messages, OpenAI Chat Completions), Einstellungsseite „Modelle“, Modell-Wähler.
- **E:** Modi Standard und Minimal (ohne Plan-Modus).
- **F:** Nachrichtenaktionen, Fehltexte je Code, lesbare Komprimierung, lange Chats mit Sprungmarken, Suche im Chat, Tastenkürzel, Bestätigung für „Voller Zugriff“, Ansichtsoptionen der Seitenleiste.
- **G:** Skills- und MCP-Seiten mit Schublade, Dialog „Server hinzufügen“ mit Verbindungstest und JSON-Import, Protokoll und Anmeldung.
- **H (Feinschliff):** DeepSeek-Modelle mit Anzeigenamen und Listenpreisen (Spitzenzeit, Hinweis auf Nebenzeit); Einstellungen in Bereichen mit Navigation (`#/settings/<bereich>`, Plugins-Reiter unter `#/plugins/<reiter>`); Plan-Leiste über dem Composer wie die DSH-To-dos; Dialog „Marktplatz hinzufügen“; Pillen öffnen Skill, Server oder Plugin; einheitliche Begriffe („Tools“, „Ziel“, „Denkstufe“, „Chat“); Rückfragen im App-Dialog statt `confirm()`; `config.toml` wird nur an den geänderten Schlüsseln bearbeitet; Kosten behalten den Preis ihrer Zeit; Entwurf je Chat; Fokus kehrt nach Menüs und Dialogen zurück; `web` wartet höchstens 3 s auf MCP-Server; ein gestoppter Bash-Befehl führt keinen weiteren Befehl mehr aus; alle QA-Befunde zu `3caa9e3` (Q1–Q19) bearbeitet; Tests auf dem Mac grün.

**Offen:**

- Plan-Modus (`/plan`, `exit_plan_mode`), Rückfragen (`ask_user`) und „Dateien übergeben“ (`present`), Kapitel 5 und 9.
- Anbieter OpenAI Responses, Gemini und die übrigen Katalog-Anbieter; „Aus DeepSeek Harness übernehmen“.
- `web_search` über die DeepSeek Search API.
- Echter Cache-Test mit DeepSeek nach `@`-Aktivierung (Kapitel 1, nur nach OK, Kosten im Cent-Bereich); dabei auch die Reihenfolge aufgeschobener Tools im `tools`-Feld prüfen.
- Vergleichs-Screenshots neben DSH und Codex in `ui/screenshots/compare/` (Kapitel 10).
- P2/P3 der Paritätsliste: Dateibaum, Dateisuche und Terminal im Seitenpanel, „Öffnen in“ mit erkannten Apps, Volltextsuche über Chats, Rückmeldung gut/schlecht, Chats als `@`-Referenz.
- Server-Fehlermeldungen (MCP, Git) kommen weiterhin englisch; bekannte Fälle übersetzt die Oberfläche.
