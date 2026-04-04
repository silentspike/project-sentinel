# PROGRESS

## Status

- Plan source: `User-Freigabe 2026-04-04: vier echte Runtime-Fixes nach $start umsetzen`
- Overall status: `DONE`
- Current task: `Completed`
- Current branch: `fix/post-soak-runtime-followups`
- Hook status: `PreToolUse TaskUpdate + PostToolUse start-enforcer projektlokal registriert`
- Last refresh: `2026-04-04 / Task 1 live verifiziert`

## Current findings

- `hallway_encounter_detected` speichert aktuell `location`, aber kein `room_id`; Live-DB zeigt deshalb leeres `$.room_id` trotz korrekter Begegnungs-Location `flur-eg`.
- Die Traffic-Control-Bootstrap-Flags sind jetzt zwischen Repo-`daemon.toml`, VM-`daemon.toml`, Gateway-Journal und `/control/config` konsistent auf `true/true/true` plus `apicp_enabled=true`.
- `company-context.md` wird jetzt aus `config/company-context.md` geladen; die Datei liegt live auf der VM unter `/opt/sentinel/config/company-context.md` und der Gateway loggt genau diesen Pfad.
- Die frühere `claude-code:"open"`-Beobachtung ließ sich nach kontrolliertem Gateway-Restart nicht als reproduzierbarer Code-Bug bestätigen; mit frischen `claude-code`-Erfolgen bleibt `/health` stabil auf `closed`.
- Die Punkte `synthesis_rate` und `capacity live nicht verifiziert` bleiben Beobachtungen, sind aber nicht Teil dieses 4-Task-Fixlaufs.

## Blocked items

- Keine harten Blocker beim Start.

## Commit references

- `3d23084` `Task [1]: Encounter-Event-Payload korrigieren`
- `69b9572` `Task [2]: Traffic-Control-Config-Drift vereinheitlichen`
- `5bc04cf` `Task [3]: company-context Deploy-Pfad korrigieren`
- `5846f21` `Task [4]: Breaker-Health-Status live neu verifizieren`

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | Encounter-Event-Payload korrigieren | DONE | `HallwayEncounterDetected` so korrigieren, dass Event-Schema und Live-Payload die Begegnungs-Location konsistent als `room_id`/Ort transportieren; Reader und Tests mitziehen | inspect, command, system |
| 2 | Traffic-Control-Config-Drift vereinheitlichen | DONE | Repo-Defaults und Runtime-Intention für `synthesis_enabled`, `sequencing_enabled`, `tick_sync_enabled` konsistent machen und auf der VM verifizieren | inspect, command, system |
| 3 | `company-context.md` deploybar machen | DONE | Sicherstellen, dass die projektlokale Company-Datei im produktiven Config-Pfad landet und live geladen wird | inspect, command, system |
| 4 | `claude-code` Breaker-/Health-Inkonsistenz beheben | DONE | Health-/Breaker-Zustand so korrigieren, dass erfolgreiche `claude-code`-Nutzung nicht weiter als dauerhaft `open` gemeldet wird | inspect, command, system |
| 5 | Plan-Verifikation | DONE | die vier Fixpunkte gegen Repo- und VM-Endstand vollständig gegenprüfen | inspect, command, system |

## Task details

### Task 1 - Encounter-Event-Payload korrigieren

- Scope:
  - Event-Typ und Producer/Consumer auf konsistente Location-Felder prüfen
  - Payload so anpassen, dass `hallway_encounter_detected` live mit auswertbarer Raumangabe persistiert
  - Regression-Tests ergänzen
- Checklist:
  - Event-Typ in `sentinel-common` prüfen
  - Producer in `sentinel-ecs` anpassen
  - Consumer/Decision-Pfad anpassen
  - Tests für Payload/Reader ergänzen
  - Daemon deployen und Live-Encounter-Event auf VM prüfen
- Acceptance criteria:
  - AC-1: neues Encounter-Event enthält live eine nicht-leere Raumangabe im erwarteten Feld
  - AC-2: Encounter-Perception für Agents bleibt funktional
  - AC-3: betroffene Rust-Tests und Clippy sind grün
- Evidence plan:
  - AC-1 via `sqlite3 events.db ... hallway_encounter_detected ...`
  - AC-2 via Daemon-Log/Perception-Flow oder bestehende Encounter-Live-Evidence nach neuem Event
  - AC-3 via `cargo remote -c -- test ...` und `cargo remote -c -- clippy ...`
- Outcome:
  - `DomainEventPayload::HallwayEncounterDetected` persistiert jetzt `room_id`; alte `location`-Payloads bleiben per `serde(alias = "location")` lesbar.
  - Producer in `sentinel-ecs` emitten `room_id`, Reader in `decision.rs` deserialisieren den Event-Typ statt Roh-JSON auszulesen.
  - Regressionstests decken neue Payloads und Legacy-Deserialisierung ab.
- Evidence:
  - AC-1 PASS:
    - VM nach Deploy: `sqlite3 ... hallway_encounter_detected ...`
    - Ergebnis: `8725660|46|50|flur-og|` und `8725665|37|41|flur-eg|`
  - AC-2 PASS:
    - VM-Journal: `Transit pausiert fuer Encounter agent=Ralf Steinbach room=flur-og`
    - VM-Journal: `Transit pausiert fuer Encounter agent=Katharina "Kathi" Wiegand room=flur-og`
    - VM-Journal: `Transit pausiert fuer Encounter agent=Selina Hoffmann room=flur-eg`
    - VM-Journal: `Transit pausiert fuer Encounter agent=Frank Berger room=flur-eg`
  - AC-3 PASS:
    - `cargo remote -c -- test -p sentinel-common -p sentinel-ecs -p sentinel-daemon`
    - `cargo remote -c -- clippy -p sentinel-common -p sentinel-ecs -p sentinel-daemon --all-targets -- -D warnings`
  - Stability:
    - `journalctl -u sentinel-daemon --since '5 min ago' | grep -Ei 'panic|drift'` => kein Treffer

### Task 2 pre-task self-check

- Was jetzt erledigt werden muss:
  - die irreführenden `false`-Defaults in `config/daemon.toml` gegen die produktive Gateway-Runtime prüfen und sauber angleichen
  - Defaulting, Dokumentation und Live-Konfiguration in einen konsistenten Zustand bringen
- Welche ACs jetzt bestehen müssen:
  - AC-1: Repo-Defaults widersprechen der produktiven Gateway-Runtime nicht mehr
  - AC-2: `/control/config` und Repo-Config sind konsistent nachvollziehbar
  - AC-3: betroffene Go-Tests bleiben grün
- Wie ich jede AC beweise:
  - AC-1 mit Source-Inspection der Config-/Default-Pfade
  - AC-2 mit Repo-Config plus VM-`/control/config`
  - AC-3 mit gezielten Go-Tests
- Erwartete Dateiänderungen:
  - `config/daemon.toml`
  - ggf. Gateway-Konfig-/Testdateien
- Risiken / Abhängigkeiten:
  - die Änderung darf nur Dokumentations-/Default-Drift beheben, nicht unbeabsichtigt produktive Runtime-Semantik drehen

### Task 2 - Traffic-Control-Config-Drift vereinheitlichen

- Scope:
  - Repo-Config/Defaults und produktive Gateway-Runtime zusammenziehen
  - irreführende `false`-Defaults entfernen, wenn die gewollte Default-Runtime `true` ist
- Checklist:
  - Default-Pfade in Gateway-Control prüfen
  - `config/daemon.toml` und Default-Parsing angleichen
  - Tests für Default-Import anpassen
  - VM-Runtime auf Konsistenz prüfen
- Acceptance criteria:
  - AC-1: Repo-Defaults widersprechen der produktiven Gateway-Runtime nicht mehr
  - AC-2: `/control/config` und Repo-Config sind konsistent nachvollziehbar
  - AC-3: betroffene Tests bleiben grün
- Evidence plan:
  - AC-1 via Code-Inspection der Default-Pfade
  - AC-2 via VM-`/control/config` plus Config-Datei/Unit-Datei
  - AC-3 via Go-Tests
- Outcome:
  - `config/daemon.toml` bootstrapped die Traffic-Control-Flags jetzt auf denselben Stand wie die produktive Gateway-Runtime.
  - Der Drift betrifft neben den drei gemeldeten Flags auch `apicp_enabled`; dieser Bootstrap-Wert wurde im selben Schritt mitgezogen.
  - Nach Gateway-Restart werden die Dateiwerte unverändert geloggt und in `/control/config` sichtbar.
- Evidence:
  - AC-1 PASS:
    - Repo-`config/daemon.toml`: `synthesis_enabled = true`, `sequencing_enabled = true`, `tick_sync_enabled = true`, `apicp_enabled = true`
  - AC-2 PASS:
    - VM-Datei: `grep -n ... /opt/sentinel/config/daemon.toml` => Zeilen 35-38 alle `true`
    - VM-Journal: `traffic control defaults applied ... updates:{..., sequencing_enabled:true, synthesis_enabled:true, tick_sync_enabled:true, apicp_enabled:true}`
    - VM-`/control/config`: `synthesis_enabled:true`, `sequencing_enabled:true`, `tick_sync_enabled:true`, `apicp_enabled:true`
  - AC-3 PASS:
    - `go test ./cmd/cortex-gateway/internal/control`
    - `go test ./cmd/cortex-gateway`

### Task 3 pre-task self-check

- Was jetzt erledigt werden muss:
  - den Company-Context-Lookup auf den Repo-/Deploy-Pfad `config/company-context.md` ausrichten
  - den produktiven Copy-/Restart-Pfad so anpassen, dass die Datei auf der VM dort liegt und aus diesem Pfad geladen wird
- Welche ACs jetzt bestehen müssen:
  - AC-1: `company-context.md` liegt live unter `/opt/sentinel/config/company-context.md`
  - AC-2: Gateway lädt den Company-Context aus dem Config-Pfad statt aus dem Workdir-Root
  - AC-3: Company-Context bleibt nach Restart aktiv, ohne auf Fallback/Disabled zu fallen
- Wie ich jede AC beweise:
  - AC-1 mit `ls -l /opt/sentinel/config/company-context.md`
  - AC-2 mit Gateway-Journal `company context loaded` plus Pfad
  - AC-3 mit Journal ohne `company context disabled` und mit erfolgreichem Gateway-Start
- Erwartete Dateiänderungen:
  - `cmd/cortex-gateway/internal/compiler/assembler.go`
  - ggf. Tests im Compiler
  - ggf. Deploy-Artefakt bzw. VM-Datei
- Risiken / Abhängigkeiten:
  - der Lookup-Pfad darf bestehende Agent-DNA-/Rooms-Pfade nicht versehentlich mit umbiegen

### Task 3 - `company-context.md` deploybar machen

- Scope:
  - den produktiven Company-Context-Dateipfad absichern
  - fehlende Deployment-Stufe ergänzen
- Checklist:
  - Source-Datei im Repo prüfen
  - Deploy-/Copy-Pfad ergänzen oder dokumentiert automatisieren
  - Datei auf VM ablegen
  - Gateway-Neustart und Lade-Log prüfen
- Acceptance criteria:
  - AC-1: `company-context.md` liegt auf der VM im erwarteten Pfad
  - AC-2: Gateway loggt `company context loaded`
  - AC-3: Fallback-only-Betrieb ist nicht mehr der aktive Produktionspfad
- Evidence plan:
  - AC-1 via `ls -l /opt/sentinel/config/company-context.md`
  - AC-2 via Gateway-Journal
  - AC-3 via Journal + Dateipfad
- Outcome:
  - Der Compiler leitet den Company-Context-Lookup jetzt aus dem `config`-Sibling der Agent-TOMLs ab statt aus dem Workdir-Root.
  - Die Repo-Quelle `config/company-context.md` wird damit auf dem produktiven Layout korrekt auf `/opt/sentinel/config/company-context.md` gemappt.
  - Gateway-Binary und Company-Context-Datei sind auf die VM deployed und nach Restart live verifiziert.
- Evidence:
  - AC-1 PASS:
    - VM: `ls -l /opt/sentinel/config/company-context.md`
    - Ergebnis: `-rw-r--r-- 1 root root 1242 ... /opt/sentinel/config/company-context.md`
  - AC-2 PASS:
    - VM-Journal: `company context loaded` mit `path:"config/company-context.md"`
  - AC-3 PASS:
    - kein `company context disabled`
    - Gateway nach Restart `active (running)` und `/control/config` weiterhin erreichbar
  - Test-Evidence:
    - `go test ./cmd/cortex-gateway/internal/compiler`
    - `go test ./cmd/cortex-gateway`

### Task 4 pre-task self-check

- Was jetzt erledigt werden muss:
  - die Diskrepanz zwischen erfolgreichem `claude-code`-Betrieb und `/health`-Status `open` auf den konkreten Breaker-/Health-Pfad zurückführen
  - den Status so korrigieren, dass Erfolg den Providerzustand wieder sichtbar schließt
- Welche ACs jetzt bestehen müssen:
  - AC-1: `/health` meldet `claude-code` nach erfolgreichen Requests nicht weiter fälschlich `open`
  - AC-2: erfolgreiche `claude-code`-Completions bleiben möglich
  - AC-3: betroffene Gateway-Tests bleiben grün
- Wie ich jede AC beweise:
  - AC-1 mit VM-`/health` vor/nach reproduzierter Completion
  - AC-2 mit Gateway-Journal `pipeline request completed` über `provider:"claude-code"`
  - AC-3 mit gezielten Go-Tests
- Erwartete Dateiänderungen:
  - Breaker-/Health-Code im Gateway
  - betroffene Tests
- Risiken / Abhängigkeiten:
  - echte `claude-code`-Completions auf der VM hängen von verfügbarem Quota und lebendem Gateway-/Daemon-Pfad ab

### Task 4 - `claude-code` Breaker-/Health-Inkonsistenz beheben

- Scope:
  - Ursache für `claude-code:"open"` trotz erfolgreicher Requests identifizieren
  - Health-/Breaker-Sicht korrigieren
- Checklist:
  - Breaker-/Health-Code prüfen
  - Reproduktionspfad gegen Live-Logs abgleichen
  - Fix implementieren
  - Gateway deployen
  - Health- und Live-Completion erneut prüfen
- Acceptance criteria:
  - AC-1: `/health` meldet `claude-code` nach erfolgreichem Betrieb nicht fälschlich dauerhaft `open`
  - AC-2: erfolgreiche `claude-code`-Completions bleiben möglich
  - AC-3: keine neue Gateway-Regression in den betroffenen Kontrollpfaden
- Evidence plan:
  - AC-1 via `/health`
  - AC-2 via Gateway-Journal `pipeline request completed`
  - AC-3 via Go-Tests + VM-Smoke
- Outcome:
  - Der gemeldete `open`-Zustand ließ sich auf dem aktuellen Stand nicht als verbleibender Codefehler reproduzieren.
  - Nach kontrolliertem Gateway-Restart und frischen `claude-code`-Requests bleibt `/health` konsistent `closed`.
  - Die vorhandenen Breaker-Tests decken den betroffenen Open→Half-Open→Closed-Pfad bereits ab; ein zusätzlicher Code-Patch war nicht nötig.
- Evidence:
  - AC-1 PASS:
    - VM-`/health` vor Repro: `{"circuit_breakers":{"claude-code":"closed"}}`
    - VM-`/health` nach frischem Operator-Chat und weiteren Erfolgen: unverändert `{"circuit_breakers":{"claude-code":"closed"}}`
  - AC-2 PASS:
    - VM-Journal `sentinel-gateway`: mehrere `pipeline request completed` mit `provider":"claude-code"`, z. B. für `agent_id:"48"`, `50`, `42`, `47`, `40`, `35`, `36`, `39`, `33`, `60`, `37`, `38`, `46`
    - VM-Journal `sentinel-daemon`: `Operator-Chat empfangen`, `heard_text gefunden`, `LLM call triggered ... has_heard=true`, dazu `URGENT LLM Response erhalten`
  - AC-3 PASS:
    - `go test ./cmd/cortex-gateway/internal/proxy -run 'TestHalfOpenSuccessCloses|TestCircuitBreakerE2E|TestBreakerStatesReflectsState'`

### Task 5 - Plan-Verifikation

- Scope:
  - alle vier Punkte gegen Repo und VM-Endstand prüfen
- Checklist:
  - Task-1- bis Task-4-Evidence gegen aktuellen Stand rereaden
  - prüfen, ob noch offene Drift-/Deploy-Reste bestehen
  - Abschlussstand in `PROGRESS.md` fixieren
- Acceptance criteria:
  - AC-1: alle vier Tasks mit frischer Repo- und VM-Evidence verifiziert oder sauber blockiert
  - AC-2: `PROGRESS.md` Abschlussstand korrekt
- Evidence plan:
  - AC-1 via kombinierte Command-/System-Evidence
  - AC-2 via finale `PROGRESS.md`-Inspektion
- Outcome:
  - Alle vier Task-Fixpunkte sind im Repo und auf der VM auf dem aktuellen Stand gegengeprüft.
  - Es blieb kein verbleibender Blocker zurück; nur die bereits vorhandenen untracked Dateien im Repo wurden bewusst unangetastet gelassen.
- Evidence:
  - AC-1 PASS:
    - Task 1 Endstand: neue `hallway_encounter_detected`-Events zeigen weiter `room_id`, z. B. `8726303|37|50|flur-eg|`
    - Task 2 Endstand: VM-`daemon.toml` und `/control/config` zeigen weiter `synthesis/sequencing/tick_sync/apicp = true`
    - Task 3 Endstand: `/opt/sentinel/config/company-context.md` existiert, aktuelles Gateway-Journal zeigt `path:"config/company-context.md"`
    - Task 4 Endstand: `/health` zeigt `{"circuit_breakers":{"claude-code":"closed"}}`
    - Stabilität: `journalctl -u sentinel-daemon --since '10 min ago' | grep -Ei 'panic|drift'` => kein Treffer
  - AC-2 PASS:
    - `PROGRESS.md` auf Abschlussstand aktualisiert
    - Repo-Worktree enthält nur die bekannten untracked Dateien `AGENTS.md`, `hooks/`, `test-288-verification.md`

### Task 5 - Plan-Verifikation

- Scope:
  - alle vier Punkte gegen Repo und VM-Endstand prüfen
- Checklist:
  - Plan rereaden
  - jede AC erneut gegen Evidence spiegeln
  - Restbefunde dokumentieren
- Acceptance criteria:
  - AC-1: alle vier Tasks mit frischer Repo- und VM-Evidence verifiziert oder sauber blockiert
  - AC-2: `PROGRESS.md` Abschlussstand korrekt
- Evidence plan:
  - AC-1 via kombinierte Command-/System-Evidence
  - AC-2 via finale `PROGRESS.md`-Inspektion

## Task 1 pre-task self-check

- Was jetzt erledigt werden muss:
  - `HallwayEncounterDetected` von inkonsistentem `location`-only Payload auf ein live auswertbares Raumfeld bringen
  - Producer, Consumer und Tests in einem Zug synchron halten
- Welche ACs jetzt bestehen müssen:
  - AC-1: Encounter-Events haben auf der VM eine nicht-leere Raumangabe
  - AC-2: Encounter-Perception bleibt für beteiligte Agents intakt
  - AC-3: betroffene Tests/Clippy sind grün
- Wie ich jede AC beweise:
  - AC-1 mit frischer `sqlite3`-Abfrage auf `events.db`
  - AC-2 mit Daemon-/Event-Evidence nach provozierter Begegnung
  - AC-3 mit `cargo remote -c -- test` und `cargo remote -c -- clippy`
- Erwartete Dateiänderungen:
  - `crates/sentinel-common/src/events.rs`
  - `crates/sentinel-ecs/src/systems.rs`
  - `crates/sentinel-ecs/src/decision.rs`
  - betroffene Tests in `crates/sentinel-ecs` und/oder `crates/sentinel-common`
- Risiken / Abhängigkeiten:
  - Event-Schemaänderung darf alte Reader nicht still brechen
  - Live-Encounter muss sich auf der VM zeitnah provozieren lassen; sonst braucht die Task mehrere kurze Runtime-Anläufe
