# PROGRESS

## Status

- Plan source: `User-Freigabe 2026-04-04: vier echte Runtime-Fixes nach $start umsetzen`
- Overall status: `IN_PROGRESS`
- Current task: `Task 2 - Traffic-Control-Config-Drift vereinheitlichen`
- Current branch: `fix/post-soak-runtime-followups`
- Hook status: `PreToolUse TaskUpdate + PostToolUse start-enforcer projektlokal registriert`
- Last refresh: `2026-04-04 / Task 1 live verifiziert`

## Current findings

- `hallway_encounter_detected` speichert aktuell `location`, aber kein `room_id`; Live-DB zeigt deshalb leeres `$.room_id` trotz korrekter Begegnungs-Location `flur-eg`.
- Die Gateway-Runtime läuft mit `synthesis_enabled=true`, `sequencing_enabled=true`, `tick_sync_enabled=true`; [config/daemon.toml](/work/company/project-sentinel/config/daemon.toml) steht dafür noch auf `false` und ist damit irreführend.
- `/opt/sentinel/config/company-context.md` fehlt auf `10.0.0.240`; der Gateway fällt dadurch auf `defaultCompanyFacts` zurück statt die projektlokale Company-Datei zu laden.
- `/health` zeigt `claude-code:"open"`, obwohl im letzten Soak echte `claude-code`-Completions gelaufen sind; das deutet auf eine Breaker-/Health-Inkonsistenz statt auf einen vollständigen LLM-Ausfall.
- Die Punkte `synthesis_rate` und `capacity live nicht verifiziert` bleiben Beobachtungen, sind aber nicht Teil dieses 4-Task-Fixlaufs.

## Blocked items

- Keine harten Blocker beim Start.

## Commit references

- Noch keine neuen Commits in diesem Ausführungszyklus.

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | Encounter-Event-Payload korrigieren | DONE | `HallwayEncounterDetected` so korrigieren, dass Event-Schema und Live-Payload die Begegnungs-Location konsistent als `room_id`/Ort transportieren; Reader und Tests mitziehen | inspect, command, system |
| 2 | Traffic-Control-Config-Drift vereinheitlichen | IN_PROGRESS | Repo-Defaults und Runtime-Intention für `synthesis_enabled`, `sequencing_enabled`, `tick_sync_enabled` konsistent machen und auf der VM verifizieren | inspect, command, system |
| 3 | `company-context.md` deploybar machen | TODO | Sicherstellen, dass die projektlokale Company-Datei im produktiven Config-Pfad landet und live geladen wird | inspect, command, system |
| 4 | `claude-code` Breaker-/Health-Inkonsistenz beheben | TODO | Health-/Breaker-Zustand so korrigieren, dass erfolgreiche `claude-code`-Nutzung nicht weiter als dauerhaft `open` gemeldet wird | inspect, command, system |
| 5 | Plan-Verifikation | TODO | die vier Fixpunkte gegen Repo- und VM-Endstand vollständig gegenprüfen | inspect, command, system |

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
  - AC-3: relevante Rust-Tests und Clippy sind grün
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
  - AC-3: keine neue Gateway-Regression in den relevanten Kontrollpfaden
- Evidence plan:
  - AC-1 via `/health`
  - AC-2 via Gateway-Journal `pipeline request completed`
  - AC-3 via Go-Tests + VM-Smoke

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
  - AC-3: relevante Tests/Clippy sind grün
- Wie ich jede AC beweise:
  - AC-1 mit frischer `sqlite3`-Abfrage auf `events.db`
  - AC-2 mit Daemon-/Event-Evidence nach provozierter Begegnung
  - AC-3 mit `cargo remote -c -- test` und `cargo remote -c -- clippy`
- Erwartete Dateiänderungen:
  - `crates/sentinel-common/src/events.rs`
  - `crates/sentinel-ecs/src/systems.rs`
  - `crates/sentinel-ecs/src/decision.rs`
  - relevante Tests in `crates/sentinel-ecs` und/oder `crates/sentinel-common`
- Risiken / Abhängigkeiten:
  - Event-Schemaänderung darf alte Reader nicht still brechen
  - Live-Encounter muss sich auf der VM zeitnah provozieren lassen; sonst braucht die Task mehrere kurze Runtime-Anläufe
