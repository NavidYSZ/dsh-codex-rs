# Fehler seit Aether, Teil A: Server, Agent und Werkzeuge

- **Status:** Umgesetzt
- **Stand:** 2026-11-05 19:20 (Europe/Berlin)
- **Commits:** `a7ff89f`

## Ziel

Der Nutzer will die Fehler seit der Umbenennung zu Aether „einmal alles
aufgedröselt und gefixt“ haben. Dieser Teil behebt A1 bis A8 aus
[regressionen-seit-aether.md](regressionen-seit-aether.md): Server, Agent,
Werkzeuge und Browser. Oberfläche und Mac-App sind Teil B.

## Ausgangslage

- **A1:** `dsh-codex web` (`crates/dsh-codex/src/main.rs` → `web::run`)
  prüft nicht, ob schon ein Server auf demselben Datenverzeichnis
  (`dshx_core::config::home_dir()`, `$DSHX_HOME` oder `~/.dsh-codex`) läuft.
  Die Mac-App startet ihren Server mit `--ready-file` und beendet nur den
  eigenen alten Prozess (`Processes.terminateStale`, PID-Datei der App).
  Ein zweiter Server hält dann Chat-Sperren (`session.lock`,
  `SessionError::Locked`) und fährt Goals allein weiter. Die Tests starten
  ihre Server je mit eigenem temporärem `DSHX_HOME`; `web_inbox.rs` startet
  nacheinander zwei auf einem Home, wartet aber vorher auf das Ende des
  ersten (`kill` + `wait`).
- **A2:** `Agent::dispose` (`crates/dshx-agent/src/agent.rs`) bricht zuerst
  `lifetime` ab. Der Job-Weiterleiter (`crates/dshx-agent/src/jobs.rs`
  `start`) endet daran sofort. Erst danach ruft `dispose`
  `JobRegistry::shutdown` (`crates/dshx-tools/src/jobs.rs`), das die Jobs
  nur zum Stoppen auffordert; deren `job/settled` kommt später und landet
  bei niemandem. Nach dem Neustart kennt die neue `ToolState` die Job-ID
  nicht: `job_output`/`job_kill` antworten „unknown job …“.
- **A3:** `ToolState::observations` (`crates/dshx-tools/src/state.rs`) lebt
  nur im Speicher. Jede neue Agent-Instanz (App-Neustart, wieder geöffneter
  Chat) beginnt leer, also scheitert `edit` an „file has not been read“.
- **A4:** `probe` (`crates/dshx-tools/src/fs/mod.rs`) baut den Dateistand
  als `dev:ino:size:mtime:ctime`. `chmod` ändert nur `ctime`; `write`/`edit`
  (`fs/mutate.rs`) melden danach „file changed since it was read“.
- **A5:** Endet der Modell-Stream ohne `message_stop`, melden
  `Translator::finish` (`crates/dshx-llm/src/translate.rs`) und
  `anthropic/translate.rs` `STREAM_CLOSED`. Die Wiederholungsregel des
  Agenten (`crates/dshx-agent/src/retry.rs`, Liste wie dsh:
  `EMPTY_RESPONSE`, `RATE_LIMIT`, `SERVER`, `TIMEOUT`, `TRANSPORT`) kennt
  den Code nicht, der Turn endet mit Fehler, ein Goal bleibt stehen. Ein
  Leerlauf-Timeout gibt es schon: `stream_idle_timeout` (5 min ohne Bytes,
  auch Heartbeats zählen) endet in `TIMEOUT` und wird wiederholt. Ein
  gescheiterter Versuch wird nie als Antwort übernommen: der Agent schreibt
  nur `assistant/attempt`, die Oberfläche verwirft dann die Live-Deltas.
  Headless (`reasoning` auf stderr) und JSON-/SDK-Clients haben die Deltas
  aber schon ausgegeben.
- **A6:** `summarize_and_commit` (`crates/dshx-agent/src/compaction.rs`)
  gibt der Zusammenfassung als Ausgabegrenze den ganzen Reserveraum
  (`CompactionConfig::headroom_tokens`, 65 536), wenn `max_tokens` nicht
  gesetzt ist. HY4 schrieb so 14k–41k Token in 105–287 s. Wird die Grenze
  erreicht (`FinishReason::MaxTokens`), gilt die Zusammenfassung als
  unvollständig und die Komprimierung scheitert.
- **A7:** `BrowserTool::execute` (`crates/dshx-browser/src/tool.rs`) ruft
  vor jeder Aktion `other.pause_cast()`. Das nimmt die `ops`-Sperre des
  anderen Browsers (`browser.rs` `Browser::pause_cast`) und wartet. Hält
  `connect_user` diese Sperre bis 60 s für „Zulassen“ in Chrome, warten auch
  Hintergrund-Browser-Aufrufe anderer Chats so lange.
- **A8:** `Ctl::visible_tab` (`browser.rs`) prüft bis 40 Tabs nacheinander
  (anhängen bis 30 s, `document.visibilityState` bis 3 s, lösen). Eingefrorene
  Tabs machen den ersten Aufruf minutenlang.

## Plan

### A1 – ein Server pro Datenverzeichnis

- Neues Modul `crates/dsh-codex/src/instance_lock.rs`: `flock(LOCK_EX |
  LOCK_NB)` auf `$DSHX_HOME/run/web.lock` (Ordner 0700, Datei 0600), danach
  die eigene PID hineinschreiben. Die Datei wird ohne Kürzen geöffnet, damit
  ein abgewiesener Start die PID des Halters nicht löscht. Die Sperre gilt
  bis zum Prozessende (der Kernel gibt sie frei, auch nach einem Absturz;
  Kindprozesse erben den Deskriptor wegen `O_CLOEXEC` nicht).
- `main.rs` nimmt die Sperre für `web` vor `Runtime::load` und dem Start
  der MCP-Server. Ist sie belegt: Fehler mit PID und Pfad, Exit-Code 1. Die
  Mac-App zeigt die letzte Logzeile nach ihren Neustartversuchen an.
- Neue Abhängigkeit `rustix` (nur Unix, ist über `dshx-session` schon im
  Lockfile). Tests müssen nicht angepasst werden (eigene Homes,
  nacheinander gestartete Server warten aufeinander).

### A2 – Job-Abschlüsse beim Beenden

- `JobRegistry::shutdown` stoppt die Jobs wie bisher und schließt sie sofort
  selbst ab: Status `killed`, Ursache `teardown`, Detail „stopped when its
  session closed“. Der spätere Abschluss des Prozesses ist dann ein No-op.
  So hängt das Protokoll nicht davon ab, wie schnell der Prozess stirbt, und
  `dispose` bleibt schnell (die Chats werden nacheinander geschlossen).
- Der Weiterleiter hört auf ein eigenes Stopp-Token statt auf `lifetime`
  und teilt sich den Empfänger mit `dispose`. `dispose` stoppt ihn erst nach
  `shutdown`, übernimmt die restlichen Ereignisse und schreibt sie ins
  Protokoll (ohne Abschlussmeldungen an das Modell; die gibt es bei
  `teardown` ohnehin nicht). Nach `lifetime` öffnet der Weiterleiter keine
  Weck-Turns mehr.
- `ToolState::restore(events)` liest beim Erzeugen des Agenten die früheren
  Jobs des Chats aus `job/registered`/`job/settled`. `job_output` und
  `job_kill` antworten für so eine ID, dass der Job mit dem Stopp von Aether
  endete (oder vorher schon fertig war, mit Status), dass die Ausgabe weg
  ist und der Befehl neu laufen muss. Unbekannte IDs bleiben „unknown job“.

### A3 – Lesestand nach dem Neustart

Abwägung: Nur „jetzigen Stand neu messen“ würde nach einem Neustart auch
Dateien freigeben, die jemand inzwischen von außen geändert hat. Stattdessen
schreiben `read`, `write`, `edit` und `read_image` den beobachteten Stand
(`fileVersion`) in die `meta` ihres Ergebnisses, die im Protokoll steht
(`tool/result`, bei PTC `tool/ptc-dispatch`). Beim Wiederherstellen gilt
genau dieser Stand; `write`/`edit` vergleichen ihn wie bisher mit der Datei.
Das ist dieselbe Prüfung wie ohne Neustart: geändert = neu lesen.

- `ToolState::restore` nimmt je Pfad den letzten erfolgreichen Eintrag.
  Ersatzereignisse des Pruners (`surfaceOp: replace`) kopieren alte `meta`
  und werden übergangen.
- Übergang für ältere Protokolle ohne `fileVersion`: Eine Datei, die der
  Chat erfolgreich gelesen/geschrieben hat, gilt nur dann als gelesen, wenn
  ihre `mtime` nicht nach der Zeit dieses Protokolleintrags liegt; dann wird
  der jetzige Stand genommen. Dateien, die seitdem geändert wurden, müssen
  neu gelesen werden. Restrisiko nur bei Werkzeugen, die alte `mtime`s
  setzen (`cp -p`, `rsync -a`), und nur für Einträge aus der Zeit vor
  diesem Fix; `edit` braucht ohnehin den exakten `old_string`.
- Nur Einträge, die noch nicht im Speicher stehen, werden ergänzt.
- Bewusst ausgelassen: „nicht gefunden“-Beobachtungen (nur Text der
  Fehlermeldung) und ein eigenes Ereignis im Sitzungsformat.

### A4 – `chmod` ändert keinen Dateistand

- `probe` liefert `dev:ino:size:mtime` ohne `ctime`. Echte Änderungen
  bewegen `mtime` (Nanosekunden), meist auch `size`, Ersetzen per Rename
  ändert `ino`. Nicht mehr erkannt: gleiche Größe plus absichtlich
  zurückgesetzte `mtime` (`touch -d`). Kommentare in `state.rs` anpassen.

### A5 – abgebrochener Stream

- `STREAM_CLOSED` wird wiederholt, wenn der gescheiterte Versuch noch nichts
  Sichtbares geliefert hat (kein Text-, Reasoning- oder Tool-Call-Delta an
  die Beobachter). Ist schon etwas ausgegeben, bleibt es beim Fehler: eine
  Wiederholung würde Headless-/SDK-Ausgaben verdoppeln.
- Umsetzung im Agenten: `RetryPolicy::decide_attempt(failure, retries,
  visible)`. Die Liste `retryable_codes` und der `policyKey` bleiben wie
  dsh (Golden-Tests); die Ausnahme gilt nur im Modus `normal` (im Modus
  `always` wird ohnehin alles wiederholt).
- Leerlauf-Timeout: besteht schon (5 min, `TIMEOUT`, wiederholbar). Keine
  zweite Grenze, weil Modelle mit verborgenem Reasoning minutenlang nur
  Heartbeats senden dürfen.

### A6 – kürzere Zusammenfassungen

- Ohne gesetztes `compaction.max_tokens` gilt `min(headroom, 16 384)`
  Token. Erreicht eine Zusammenfassung diese Standardgrenze, folgt ein
  zweiter Versuch mit dem vollen Reserveraum (wie bisher), damit
  ausschweifende Modelle nicht dauerhaft an der Komprimierung scheitern.
  Ein ausdrücklich gesetztes `max_tokens` bleibt die harte Grenze.
- Der Auftrag an das Modell verlangt ausdrücklich Kürze: höchstens etwa
  3 000 Wörter, Code über Pfad und Symbol benennen statt kopieren, nur kurze
  unverzichtbare Ausschnitte.
- Wann komprimiert wird, ändert sich nicht. Der Golden-Test gegen dsh setzt
  die dsh-Grenze (65 536) ausdrücklich.

### A7 – Bildpause ohne Warten

- `Browser::pause_cast` nimmt keine Sperre mehr: ohne Mini-Fenster tut es
  nichts; sonst setzt es `casting = false` im geteilten Zustand und schickt
  `Page.stopScreencast` ohne auf Antwort zu warten, falls die `ops`-Sperre
  gerade frei ist (`try_lock`). Bilder eines Browsers mit `casting = false`
  werden bestätigt, aber nicht mehr ans Mini-Fenster weitergegeben; seine
  nächste Aktion startet das Bild wieder (`ensure`).
- Wartet ein Aufruf in `connect_user` auf „Zulassen“, merkt sich der
  Browser das (`allow_wait`). Ein zweiter Aufruf auf das eigene Chrome
  bekommt dann sofort den bekannten Hinweistext statt bis 60 s zu warten.

### A8 – schnelles erstes Verbinden

- `visible_tab` prüft alle (höchstens 40) Tabs gleichzeitig in eigenen
  Tasks (die sich am Ende selbst lösen) und wartet insgesamt höchstens 3 s.
  Gewählt wird wie bisher der erste sichtbare Tab in Listenreihenfolge,
  sobald alle davor geantwortet haben; sonst nach Ablauf der erste
  sichtbare, sonst wie bisher die erste Seite.

### Prüfung (geplant)

Auf Wunsch keine Smoke-Tests, Testsuiten oder Lints: `cargo check` der
berührten Crates, `rustfmt --edition 2024` auf berührte Dateien, höchstens
einzelne gezielte Unit-Tests (A2, A3/A4, A5). Bestehende Tests, die das alte
Verhalten festschreiben, werden angepasst. Kein Chrome, kein Start oder
Stopp der installierten App.

## Umsetzung

- **A1:** Neues Modul `crates/dsh-codex/src/instance_lock.rs` (`acquire`),
  aufgerufen in `main.rs` für `web` vor `Runtime::load`. Die gehaltene Datei
  liegt in einem `static OnceLock` bis zum Prozessende. Fehlermeldung:
  „another dsh-codex web server (PID …) already uses the data directory …
  Stop it first, or give this server its own data directory with
  DSHX_HOME.“ `rustix` als Unix-Abhängigkeit von `dsh-codex`
  (`Cargo.lock`: nur die neue Kante). README: ein Server pro
  Datenverzeichnis.
- **A2:** `dshx-tools/src/jobs.rs`: `JobRegistry::shutdown` schließt die
  gestoppten Jobs sofort ab (`killed`, `teardown`, Detail `TEARDOWN_DETAIL`
  = „stopped when its session closed“); neu `JobRegistry::knows`,
  `EndedJob` (Texte für `job_output`/`job_kill`, `public_json`) und
  `ended_jobs(events)`. `job_output` antwortet z. B. „job bash-… (bash: …)
  ended before this chat was reopened [status: killed, stopped when its
  session closed]; its output is no longer available. Run the command again
  if you still need it.“ bzw. „… was still running when Aether stopped and
  ended with it …“; `job_kill` meldet `already-finished`.
  `dshx-agent/src/jobs.rs`: Der Weiterleiter hält den Empfänger in einem
  geteilten `tokio::sync::Mutex` und endet an einem eigenen Stopp-Token;
  neu `finish`, das `Agent::dispose` nach `shutdown` aufruft (stoppt den
  Weiterleiter, übernimmt den Rest, schreibt nur die Ereignisse, keine
  Meldungen/Weck-Turns). Nach `lifetime` weckt der Weiterleiter nicht mehr.
- **A3:** `read`, `write`, `edit` und `read_image` schreiben
  `meta.fileVersion` (`dshx_tools::fs::VERSION_META_KEY`).
  `fs::restored_observations(events)` und `ToolState::restore(events)`
  (auch für A2) bauen den Stand auf; `Agent::new` ruft `restore` mit den
  Ereignissen des Journals auf, also bei jedem wieder geöffneten Chat und
  jedem Subagenten. Der Werkzeugname kommt aus `tool/call` (`callId` →
  `name`) bzw. aus `tool/ptc-dispatch`. Übergangsregel für Einträge ohne
  Version wie geplant (`mtime` ≤ Zeit des Eintrags).
- **A4:** `probe` ohne `ctime` (`dev:ino:size:mtime`), gemeinsame Hilfe
  `mtime_ns`. Doku von `Observation::Present` angepasst.
- **A5:** `dshx-agent/src/retry.rs`: `SILENT_RETRYABLE_CODES =
  ["STREAM_CLOSED"]`, `RetryPolicy::decide_attempt(failure, retries,
  visible)`; `decide` bleibt (= mit sichtbarer Ausgabe). `agent.rs`:
  `emit_delta` meldet, ob ein Beobachter etwas sah (nicht leerer Text-/
  Reasoning-Delta oder Tool-Name); `step` sammelt das als `visible` und gibt
  es an `request_error`. `policyKey` und `retryable_codes` unverändert.
- **A6:** `compaction.rs`: `DEFAULT_SUMMARY_MAX_TOKENS = 16_384`; ohne
  gesetztes `max_tokens` gilt `min(headroom, 16 384)`, bei
  `FinishReason::MaxTokens` ein zweiter Durchlauf mit dem vollen Reserveraum
  (`compaction/summary.maxTokens` nennt die Grenze des erfolgreichen
  Durchlaufs). Auftrag: neue Regeln „at most about 3,000 words …“ und „Do
  not reproduce file contents, diffs, logs or long tool output …“, Abschnitt
  „Files and Code“ ohne „snippets“. README-Satz dazu. `golden.rs` setzt
  für den dsh-Vergleich `max_tokens = headroom`.
- **A7:** `Browser::pause_cast` ohne Warten (wie geplant);
  `Events::on_event` gibt Bilder nur bei `casting` weiter. Neu
  `Browser::waiting_for_allow`, `AllowWait` (Drop-Wächter um das Warten in
  `connect_user`) und `allow_prompt` (der bisherige Text); `tool.rs` `run`
  antwortet damit vor `browser.lock()`.
- **A8:** `visible_tab` mit `tab_visible` je Tab in eigenen Tasks,
  `VISIBLE_TAB_BUDGET = 3 s`, Auswahl wie geplant.
- Angepasste bestehende Tests: `dshx-tools/tests/fs.rs` (vier exakte
  `meta`-Vergleiche ohne `fileVersion`, mit Formatprüfung),
  `dshx-agent/tests/real_tools.rs` (Vergleich mit der dsh-Referenz ohne
  `fileVersion`), `dshx-agent/tests/golden.rs` (dsh-Grenze).

Abweichungen vom Plan: keine inhaltlichen. Zusätzlich ein kleiner Test für
A1, weil das Nicht-Löschen der Halter-PID leicht kaputtgeht.

## Prüfung

Am 2026-11-05 geprüft:

- `cargo check -p dshx-tools -p dshx-agent -p dshx-browser -p dsh-codex
  --tests` ohne Warnungen; `rustfmt --edition 2024` auf alle berührten
  Dateien.
- Neue gezielte Tests, einzeln ausgeführt, alle grün:
  `instance_lock::tests::a_second_server_is_refused_and_named` (A1),
  `jobs::tests::shutdown_settles_at_once_and_a_restored_log_knows_the_job`
  (A2, Registry), `dispose_logs_the_teardown_of_running_jobs`
  (`dshx-agent/tests/real_tools.rs`, A2 mit echtem `bash`-Hintergrundjob),
  `fs::tests::chmod_keeps_the_version_and_a_restart_restores_what_was_seen`
  (A3/A4), `retry::tests::a_closed_stream_is_retried_only_before_visible_output`
  (A5).
- Angepasste bestehende Tests einzeln ausgeführt, grün:
  `read_formats_like_dsh`, `write_requires_a_read_of_existing_files`,
  `edit_rules_and_messages`, `read_image_returns_an_image_part`,
  `real_tools_answer_dsh_reference_calls_identically`,
  `matches_dsh_resume_and_overflow_compaction`, `matches_dsh_retries`,
  `shutdown_tears_down_without_notices`.

Nicht geprüft (auf Wunsch bzw. Vorgabe): keine Testsuiten, kein `clippy`,
kein Smoke-Test, kein Start von `dsh-codex web`, kein Chrome (A7/A8 nur
kompiliert), keine Prüfung mit echten Modellen (A5/A6) und keine Installation.

## Offen

- Zusammenführen mit Teil B, Changelog, `cargo fmt/clippy/test` vor dem
  Release, Build und Installation. Die Übersicht in
  [README.md](README.md) steht für diese Datei noch auf „In Arbeit“ (beim
  Zusammenführen anpassen; hier nicht geändert, um Konflikte mit Teil B zu
  vermeiden).
- A1: Die Mac-App zeigt bei belegtem Datenverzeichnis nach drei Versuchen
  nur die Fehlerzeile. Sie könnte anbieten, den genannten Prozess zu
  beenden (Teil B / später).
- A3: Für Dateien, die vor diesem Stand gelesen wurden, gilt die
  `mtime`-Übergangsregel; ab jetzt ist der Stand exakt.
- A5: Bricht ein Stream nach sichtbarer Ausgabe ab, endet der Turn weiter
  mit Fehler und ein Goal hält an. Eine Wiederaufnahme danach wäre eine
  Entscheidung der Goal-Steuerung, nicht der Wiederholungsregel.
- A7: Ist der andere Browser gerade beschäftigt, streamt dessen Chrome bis
  zu seiner nächsten Aktion weiter (Bilder werden nur verworfen).
