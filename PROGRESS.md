# PROGRESS

## Status

- Plan source: `/work/company/codex-plan263.md`
- Overall status: `IN_PROGRESS`
- Current task: `Task 4 - Eskalationslogik, unresolved counters und deterministische Trigger`
- Current branch: `feat/issue-263-platform-controlplane-completion`
- Hook status: `PreToolUse TaskUpdate + PostToolUse start-enforcer projektlokal registriert`
- Last refresh: `2026-04-05 Europe/Vienna`

## Current findings

- Ausgangsstand war `main = origin/main`; danach wurde der Arbeitsbranch `feat/issue-263-platform-controlplane-completion` frisch von diesem Stand angelegt.
- Im Worktree existiert bereits eine fremde tracked Aenderung an [.gitignore](/work/company/project-sentinel/.gitignore). Sie erweitert lokale Ignore-Regeln fuer `AGENTS.md`, `test-288-verification.md` und `hooks/` und bleibt unangetastet.
- Der bisherige [PROGRESS.md](/work/company/project-sentinel/PROGRESS.md) bezog sich noch auf den abgeschlossenen Security-Block und wurde deshalb als Execution-SSOT fuer `#263` komplett ersetzt.
- `mainrag search "issue 263 platform controlplane" --source claude-conversations --limit 5` ist aktuell nicht nutzbar, weil der lokale MainRag-Source-Endpunkt `http://localhost:3001/api/v1/sources` `Connection refused` liefert. Das ist fuer `#263` derzeit ein Diagnose-Fund, aber kein technischer Blocker.
- Projektregeln, globale Regeln, Workspace-Handover, repo-lokales `.claude/AGENTS.md`, Memory-Index und der komplette `#263`-Plan wurden in dieser Session frisch gelesen.
- Frische Baseline-Evidence fuer `#263` ist als Kommentar `issuecomment-4187899940` im Issue dokumentiert.
- Live auf `10.0.0.240` bestaetigt:
  - `[daemon.platform_controlplane]` steht noch auf den Default-Basiswerten (`cycle_interval_ticks = 60`, `max_projection_lag = 10000`)
  - `GET /operator/platform-state` liefert aktuell `{\"error\":\"Endpoint unbekannt\"}`
  - `SENTINEL_DASHBOARD_API_KEY` ist auf `sentinel-projection` leer
  - `platform_intervention`-Events existieren, aktuell sichtbar aber nur als `event_store_size/system`
  - `sentinel-daemon`, `sentinel-gateway`, `sentinel-projection` und `sentinel-judge` sind `active`
- Frische Repo-Baseline bestaetigt:
  - `agent_stall` ueberspringt weiter alles mit `tick - last_action_tick < 120`
  - `projection_lag` feuert derzeit nur `alert`, keinen Restart
  - `ResourceManager::force_profile()` aendert nur den In-Memory-Zustand, ohne cgroup-Apply und ohne Audit-Event in diesem Pfad
- Task-2-Schemaarbeit ist lokal umgesetzt:
  - [events.rs](/work/company/project-sentinel/crates/sentinel-common/src/events.rs) enthaelt jetzt `PlatformAnalysis` inklusive `trigger`, `severity`, `summary`, `recommendation`, `suggested_action`, `target`, `provider`, `model`, `unresolved_keys` und `parameters`
  - [config.rs](/work/company/project-sentinel/services/sentinel-daemon/src/config.rs) und [daemon.toml](/work/company/project-sentinel/config/daemon.toml) tragen jetzt die benoetigten Platform-CP-Felder fuer Grace-, LLM-, Retry- und Timeout-Steuerung
  - [rules.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/rules.rs) nutzt jetzt `stall_recent_activity_grace_ticks` statt des bisher harten `120`-Tick-Skips
- Remote-Rust-Evidence fuer Task 2 ist vorhanden:
  - `cargo remote -c -- test -p sentinel-common -p sentinel-daemon -p sentinel-projection` lief mit Exit `0` durch; relevante Endzeilen zeigen `142 passed` fuer `sentinel-daemon` sowie gruenen `sentinel_projection`-/Acceptance-Run
  - `cargo remote -c -- clippy -p sentinel-common -p sentinel-daemon -p sentinel-projection --all-targets -- -D warnings` lief ebenfalls mit Exit `0` durch; der Output ist wegen `cargo remote`-Artifact-Transfer sehr rauschig, aber ohne Clippy-Fehler beendet
- Task-3-Analyzer ist jetzt als echter daemon-interner Worker vorhanden:
  - neue Datei [llm_analyzer.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs)
  - Modul-Export in [mod.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/mod.rs)
  - Start/Wiring in [orchestrator.rs](/work/company/project-sentinel/services/sentinel-daemon/src/orchestrator.rs)
- Der Analyzer benutzt ausschliesslich den internen Gateway-Vertrag `POST /internal/llm`, baut seinen Kontext aus `PlatformMetrics`, Verify-Ergebnissen, den letzten `PlatformIntervention`-Events und fehlgeschlagenen Interventionen und persistiert erfolgreiche Antworten als `platform_analysis`.
- Remote-Rust-Evidence fuer Task 3 ist vorhanden:
  - `cargo remote -c -- test -p sentinel-daemon -p sentinel-common` => Exit `0`; relevante Endzeilen zeigen `145 passed; 0 failed`
  - `cargo remote -c -- clippy -p sentinel-daemon --all-targets -- -D warnings` => Exit `0`

## Blocked items

- Keine harten Blocker beim Start.
- Beobachtung: Solange `mainrag` lokal nicht verfuegbar ist, kann ich keine frische Conversation-Suche als Zusatzkontext ziehen. Die Ausfuehrung von `#263` selbst ist dadurch aber nicht blockiert.

## Commit references

- `ae1b5cf` Task [1]
- `de96c37` Task [2]
- `TBD` Task [3]
- `TBD` Task [4]
- `TBD` Task [5]
- `TBD` Task [6]
- `TBD` Task [7]
- `TBD` Task [8]

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | Issue-Hygiene, Branch-Setup und deterministische Baseline fuer AC-1 bis AC-5 / AC-10 bis AC-12 neu setzen | DONE | `#263`-Ist-Zustand gegen Repo/GitHub/VM abgleichen, offene Baseline-Luecken fuer Stall/Projection/Cooldowns/Disable-Flag pruefen, Issue-Text vorbereiten, Branch sauber halten | command, system, inspect |
| 2 | Event- und Config-Schema fuer die LLM-Ebene ergaenzen | DONE | `PlatformAnalysis`-Event, neue Platform-CP-Config-Felder, TOML-Defaults und Typen stabil ergaenzen | inspect, command |
| 3 | LLM-Analyzer als daemon-internes Background-Modul implementieren | DONE | asynchronen Analyzer, Kontext-Assembly, Gateway-Call und Parsing/Persistenz bauen | inspect, command |
| 4 | Eskalationslogik, unresolved counters und deterministische Trigger vervollstaendigen | IN_PROGRESS | scheduled/manual/unresolved Trigger, Counter-State und Test-Hooks fuer `AC-6` vervollstaendigen | inspect, command, system |
| 5 | Suggested-Action-Executor mit force_profile / adjust_threshold / escalate_to_operator implementieren | PENDING | guard-railed Executor inkl. cgroup-Apply, Audit-Trail und Runtime-Overrides bauen | inspect, command, system |
| 6 | Operator-API, Dashboard, Cockpit und Playwright-relevante UI-Surfaces erweitern | PENDING | API-Read/Write-Pfade, Dashboard/Cockpit-Rendering, stabile Selektoren, Projection-Write-Key-Pfad und UI-Verifikation ergaenzen | inspect, command, browser, system |
| 7 | Deploy, Benchmarks sowie AC-1 bis AC-12 inkl. UI-Evidence auf der VM verifizieren | PENDING | Release-Build, Deploy, systemd-Restarts, AC-Matrix, Playwright-Screenshots und Benchmarks mit Systemmetriken abarbeiten | command, system, browser |
| 8 | Plan-Verifikation | PENDING | Gesamtergebnis Zeile fuer Zeile gegen den Plan pruefen, Restluecken sofort fixen oder als Blocker dokumentieren | inspect, command, system, browser |

## Task details

### Task 1 - Issue-Hygiene, Branch-Setup und deterministische Baseline fuer AC-1 bis AC-5 / AC-10 bis AC-12 neu setzen

- Scope:
  - `#263`-Ist-Zustand gegen aktuellen Repo-/VM-Stand neu erfassen
  - Baseline-Facts fuer `AC-1` bis `AC-5` und `AC-10` bis `AC-12` live belegen oder als echte Luecken markieren
  - Branch-/Issue-Hygiene auf den Ausfuehrungsstand ziehen
- Checklist:
  - GitHub-Issue `#263` lesen und mit [codex-plan263.md](/work/company/codex-plan263.md) abgleichen
  - relevante Baseline-Implementierung in `platform_controlplane`, `service_health`, `config` und Deploy-Config lesen
  - VM-Checks fuer aktuelle Platform-CP-Defaults und vorhandene Events fahren
  - offene Baseline-Luecken fuer `AC-1`, `AC-4`, `AC-10`, `AC-11`, `AC-12` dokumentieren
  - Issue-Kommentar/Body fuer den echten Stand vorbereiten
- Acceptance criteria:
  - AC-1: Branch und Execution-SSOT sind auf `#263` umgestellt, ohne fremde Worktree-Aenderungen zu verlieren
  - AC-2: deterministische Baseline ist mit frischer Repo-/VM-Evidence beschrieben
  - AC-3: offene Baseline-Luecken sind als echte Implementierungspunkte identifiziert, nicht als still vorausgesetzte PASSs
- Evidence plan:
  - AC-1 via `git status`, Branch-Name, [PROGRESS.md](/work/company/project-sentinel/PROGRESS.md)
  - AC-2 via Code-Lesepfade plus VM-Kommandos fuer Config, Logs und Event-Store
  - AC-3 via dokumentierte Findings und ggf. aktualisiertem Issue-Kommentar
- Pre-task self-check:
  - Was muss getan werden: Issue- und Runtime-Baseline neu ziehen, damit die spaeteren Tasks nicht auf alten Annahmen bauen.
  - Welche ACs muessen hier passen: AC-1/2/3 dieses Tasks; die Issue-ACs selbst werden in Task 7 verifiziert.
  - Wie wird bewiesen: Git/Issue-Stand, Code-Pfade, VM-Config/Logs/Events.
  - Erwartete Dateien: [PROGRESS.md](/work/company/project-sentinel/PROGRESS.md), evtl. Issue-Text/Kommentar; noch kein Feature-Code.
  - Risiken: fremde `.gitignore`-Aenderung, moegliche Drift zwischen Plan und aktuellem VM-Stand.
- Outcome:
  - Arbeitsbranch `feat/issue-263-platform-controlplane-completion` ist angelegt, ohne die fremde `.gitignore`-Aenderung zu verlieren.
  - `#263` traegt jetzt einen frischen Baseline-Kommentar mit Repo-/VM-Facts.
  - Die wesentlichen Startluecken sind belegt und nicht mehr nur Plan-Annahmen:
    - `GET /operator/platform-state` fehlt live
    - `agent_stall` hat weiter den harten `120`-Tick-Skip
    - `projection_lag` ist alert-only
    - `force_profile` ist noch kein echter apply+audit-Pfad
    - Dashboard-Write-Key fuer `sentinel-projection` fehlt live
- Evidence:
  - AC-1 PASS:
    - `git switch -c feat/issue-263-platform-controlplane-completion`
    - `git status --short --branch` => neuer Branch aktiv; nur fremde `.gitignore`-Aenderung und [PROGRESS.md](/work/company/project-sentinel/PROGRESS.md) geaendert
  - AC-2 PASS:
    - `gh issue view 263 --repo silentspike/project-sentinel` => `OPEN`, `status:in-progress`, `quality:needs-spec`, AC-6-9 weiterhin offen
    - `ssh ubuntu@10.0.0.240 "grep -A20 '\\[daemon.platform_controlplane\\]' /opt/sentinel/config/daemon.toml"` => Defaults live bestaetigt
    - `ssh ubuntu@10.0.0.240 "sqlite3 /opt/sentinel/data/events.db \"SELECT event_type, json_extract(payload,'$.rule_name'), json_extract(payload,'$.target') FROM events WHERE event_type='platform_intervention' ORDER BY id DESC LIMIT 10\""` => aktuelle `platform_intervention`-Events vorhanden
    - `ssh ubuntu@10.0.0.240 "systemctl is-active sentinel-daemon sentinel-gateway sentinel-projection sentinel-judge"` => alle `active`
  - AC-3 PASS:
    - `ssh ubuntu@10.0.0.240 "curl -s http://127.0.0.1:8084/operator/platform-state"` => `{\"error\":\"Endpoint unbekannt\"}`
    - [rules.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/rules.rs) bestaetigt `last_action_ticks < 120` und `projection_lag -> alert`
    - [resource_manager.rs](/work/company/project-sentinel/services/sentinel-daemon/src/resource_manager.rs) bestaetigt `force_profile()` ohne apply/audit
    - `systemctl show sentinel-projection --property=Environment --value ...` => leerer `SENTINEL_DASHBOARD_API_KEY`
    - Issue-Kommentar: `issuecomment-4187899940`

### Task 2 - Event- und Config-Schema fuer die LLM-Ebene ergaenzen

- Scope:
  - `PlatformAnalysis`-Eventtyp und Platform-CP-Konfiguration um alle benoetigten Felder erweitern
  - TOML-/Serde-/Type-Pfade stabil und deploybar halten
- Checklist:
  - [events.rs](/work/company/project-sentinel/crates/sentinel-common/src/events.rs) erweitern
  - [config.rs](/work/company/project-sentinel/services/sentinel-daemon/src/config.rs) und [daemon.toml](/work/company/project-sentinel/config/daemon.toml) aktualisieren
  - Typ-/Parser-Tests nachziehen
- Acceptance criteria:
  - AC-1: `PlatformAnalysis` enthaelt den geplanten Payload-Vertrag
  - AC-2: `daemon.toml` und Runtime-Config tragen alle benoetigten LLM-/Grace-/Retry-Felder
  - AC-3: Build/Test fuer die Schema-Aenderungen ist gruen
- Evidence plan:
  - AC-1 via Code-Inspection + Tests
  - AC-2 via Code-Inspection + Config-Parse-Test
  - AC-3 via `cargo remote -c -- test ...` / `clippy`
- Pre-task self-check:
  - Was muss getan werden: Basistypen und Config muessen zuerst stabil stehen, bevor der Analyzer oder Operator-Pfade darauf aufbauen koennen.
  - Welche ACs muessen hier passen: Eventschema, Config-Felder, gruene Remote-Rust-Checks.
  - Wie wird bewiesen: Rust-Unit-Tests, Config-Parse-Tests, `cargo remote` fuer Test und Clippy.
  - Erwartete Dateien: [events.rs](/work/company/project-sentinel/crates/sentinel-common/src/events.rs), [config.rs](/work/company/project-sentinel/services/sentinel-daemon/src/config.rs), [daemon.toml](/work/company/project-sentinel/config/daemon.toml), [rules.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/rules.rs)
  - Risiken: exhaustive Event-Matches in Folgemodulen, lauter `cargo remote`-Output, keine lokale Rust-Ausfuehrung erlaubt.
- Outcome:
  - `PlatformAnalysis` ist als neues Domain-Event mit dem geplanten Analyse-/Recommendation-Vertrag vorhanden.
  - Platform-Controlplane-Konfiguration enthaelt jetzt explizite Felder fuer `stall_recent_activity_grace_ticks`, `llm_enabled`, `llm_analysis_interval_secs`, `llm_retry_delay_secs`, `llm_gateway_timeout_ms`, `llm_prompt_template`, `llm_max_context_events` und `llm_max_failed_interventions`.
  - Die bisher harte Stall-Grace von `120` Ticks ist aus der Regel herausgezogen und nun konfigurierbar.
  - Event-/Config-Tests und Remote-Rust-Checks laufen gruen.
- Evidence:
  - AC-1 PASS:
    - [events.rs](/work/company/project-sentinel/crates/sentinel-common/src/events.rs) fuehrt `DomainEventPayload::PlatformAnalysis` und `event_type_str() = "platform_analysis"` ein
    - `platform_analysis_serializes_all_required_fields` prueft JSON-Shape inkl. `suggested_action`, `provider`, `model`, `unresolved_keys` und `parameters`
  - AC-2 PASS:
    - [config.rs](/work/company/project-sentinel/services/sentinel-daemon/src/config.rs) erweitert `PlatformControlplaneConfig` samt Defaults und Parse-Test `test_platform_controlplane_custom`
    - [daemon.toml](/work/company/project-sentinel/config/daemon.toml) traegt die neuen Platform-CP-Felder im Default-Profil
    - [rules.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/rules.rs) nutzt `config.stall_recent_activity_grace_ticks`
  - AC-3 PASS:
    - `cargo remote -c -- test -p sentinel-common -p sentinel-daemon -p sentinel-projection` => Exit `0`; Endzeilen: `142 passed; 0 failed` fuer `sentinel-daemon`, `4 passed` fuer `sentinel_projection`, `6 passed` in `tests/acceptance.rs`
    - `cargo remote -c -- clippy -p sentinel-common -p sentinel-daemon -p sentinel-projection --all-targets -- -D warnings` => Exit `0`

### Task 3 - LLM-Analyzer als daemon-internes Background-Modul implementieren

- Scope:
  - Analyzer-Module, Kontext-Assembly, Gateway-Call, Parsing und `PlatformAnalysis`-Persistenz
- Checklist:
  - neues `llm_analyzer.rs` anlegen
  - Trigger/Queue-Anbindung in Daemon herstellen
  - Parser-/Timeout-Tests schreiben
- Acceptance criteria:
  - AC-1: Analyzer laeuft asynchron und blockiert den Tick-Loop nicht
  - AC-2: Gateway-Call geht ueber den internen Vertrag
  - AC-3: erfolgreiche Antworten werden als `platform_analysis` persistiert
- Evidence plan:
  - AC-1 via Code + Tests + ggf. Tick-Overhead-Mikrobench
  - AC-2 via Tests und spaetere VM-Logs
  - AC-3 via Event-Store-Test und spaetere VM-Evidence
- Pre-task self-check:
  - Was muss getan werden: Ein echter Worker fuer LLM-Analysen muss stehen, bevor Trigger, Operator-Hooks oder Executor darauf zeigen koennen.
  - Welche ACs muessen hier passen: async Worker vorhanden, interner Gateway-Pfad, persistierte `platform_analysis`-Events im Test.
  - Wie wird bewiesen: zielgerichtete Remote-Rust-Tests, Mock-Gateway-Tests, Clippy.
  - Erwartete Dateien: [llm_analyzer.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs), [mod.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/mod.rs), [orchestrator.rs](/work/company/project-sentinel/services/sentinel-daemon/src/orchestrator.rs)
  - Risiken: Queue muss spaeter aus dem sync ECS-Thread ansteuerbar sein, keine Direct-API-Abkuerzung, Tests duerfen nicht an instabilen HTTP-Mocks haengen.
- Outcome:
  - Der neue `PlatformLlmAnalyzerHandle` kapselt einen async Worker mit unbounded Queue und laeuft daemon-intern als Hintergrundtask.
  - Der Worker assembliert seinen Prompt-Kontext aus `PlatformMetrics`, `verify_results`, den letzten `PlatformIntervention`-Events und den letzten fehlgeschlagenen Interventionen.
  - Der HTTP-Pfad geht fest gegen `POST /internal/llm`; erfolgreiche Antworten werden als `DomainEventPayload::PlatformAnalysis` in Limbo persistiert.
  - Timeout- und Parse-Fehler bleiben explizit im Fehlerpfad, statt den Daemon zu crashen.
- Evidence:
  - AC-1 PASS:
    - [llm_analyzer.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs) fuehrt `PlatformLlmAnalyzerHandle::spawn()` mit async Worker und Queue ein
    - `handle_enqueue_is_non_blocking_and_worker_persists` belegt den nicht blockierenden Queue-Pfad
    - [orchestrator.rs](/work/company/project-sentinel/services/sentinel-daemon/src/orchestrator.rs) startet den Worker daemon-intern beim Boot
  - AC-2 PASS:
    - `analyzer_persists_platform_analysis_event` prueft, dass der Mock-Request auf `/internal/llm` geht
    - [llm_analyzer.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs) sendet ausschliesslich an `format!("{}/internal/llm", ...)`
  - AC-3 PASS:
    - `analyzer_persists_platform_analysis_event` prueft die persistierte `PlatformAnalysis`-Payload inklusive `provider`, `model`, `unresolved_keys` und `parameters`
    - `analyzer_handles_gateway_timeout` prueft den expliziten Timeout-Fehlerpfad ohne Event-Persistenz
    - `cargo remote -c -- test -p sentinel-daemon -p sentinel-common` => Exit `0`; `145 passed; 0 failed`
    - `cargo remote -c -- clippy -p sentinel-daemon --all-targets -- -D warnings` => Exit `0`

### Task 4 - Eskalationslogik, unresolved counters und deterministische Trigger vervollstaendigen

- Scope:
  - scheduled/manual/unresolved Trigger und deren deterministische Testpfade herstellen
- Checklist:
  - unresolved counter je `rule:target` einfuehren
  - scheduled trigger und manual trigger anbinden
  - `POST /operator/platform-trigger-test` implementieren
- Acceptance criteria:
  - AC-1: alle drei Triggerpfade existieren
  - AC-2: scheduled und unresolved sind deterministisch provozierbar
  - AC-3: Trigger benutzen denselben Analyzer-Pfad wie die echte Runtime
- Evidence plan:
  - AC-1/2 via Tests und spaetere VM-Commands
  - AC-3 via Code + Integrationstest

### Task 5 - Suggested-Action-Executor mit force_profile / adjust_threshold / escalate_to_operator implementieren

- Scope:
  - kontrollierten Executor fuer die drei whitelisted Actions bauen
- Checklist:
  - cgroup-Apply fuer `force_profile`
  - Runtime-Override-State fuer `adjust_threshold`
  - Event/Log/UI-Sichtbarkeit fuer `escalate_to_operator`
  - deterministischen Operator-Testpfad mit dem echten Executor verbinden
- Acceptance criteria:
  - AC-1: `force_profile` resized cgroups und emittiert Audit-Event
  - AC-2: `adjust_threshold` ist im Runtime-State sichtbar
  - AC-3: `escalate_to_operator` ist als Event/Log nachvollziehbar
- Evidence plan:
  - AC-1 via Tests + spaetere VM-cgroup/DB-Evidence
  - AC-2 via Tests + spaetere `platform-state`-Readbacks
  - AC-3 via Tests + spaetere Event-/Log-Evidence

### Task 6 - Operator-API, Dashboard, Cockpit und Playwright-relevante UI-Surfaces erweitern

- Scope:
  - Operator-Read/Write-Pfade, Dashboard-Readmodelle, Cockpit-Mappings und stabile UI-Selektoren
- Checklist:
  - `operator_api`-Routen ergaenzen
  - Dashboard-Routen, DB-Layer und Typen erweitern
  - Control/Cockpit-Frontend und Playwright-stabile IDs/Data-Attributes einbauen
  - produktiven `SENTINEL_DASHBOARD_API_KEY`-Pfad fuer `sentinel-projection` beruecksichtigen
- Acceptance criteria:
  - AC-1: API liefert `platform-analyses` und `platform-state`
  - AC-2: Control/Cockpit rendern `platform_analysis` und `platform_intervention` sichtbar und brauchbar
  - AC-3: UI ist per `playwright-cli` robust pruefbar; Write-Pfade sind nicht 403-blockiert, wenn `#263` sie fuer AC-8/9 beansprucht
- Evidence plan:
  - AC-1 via Dashboard-Tests
  - AC-2 via Dashboard/Cockpit-Tests und spaetere Screenshots
  - AC-3 via Playwright-Flow und Projection-Service-Konfiguration

### Task 7 - Deploy, Benchmarks sowie AC-1 bis AC-12 inkl. UI-Evidence auf der VM verifizieren

- Scope:
  - Release-Build, Deploy, systemd-Restarts, AC-Matrix, Playwright-Evidence und Benchmarks komplett abarbeiten
- Checklist:
  - Remote-Rust/Go/Dashboard-Tests gruen
  - `sentinel-daemon`, ggf. `sentinel-gateway` und `sentinel-projection` deployen
  - AC-1 bis AC-12 einzeln gegen die VM fahren
  - Playwright-Control/Cockpit-Screenshots aufnehmen
  - Benchmarks + Systemmetriken dokumentieren
- Acceptance criteria:
  - AC-1: alle Issue-ACs `AC-1` bis `AC-12` sind mit Command+Output oder Browser-Evidence frisch belegt
  - AC-2: Benchmarks liegen innerhalb der Zielwerte und haben Sidecar-Systemmetriken
  - AC-3: keine Panic-/Drift-Regressionssignale im relevanten Messfenster
- Evidence plan:
  - AC-1 via vollstaendige VM-AC-Matrix
  - AC-2 via Bench-Commands und Systemmetriken
  - AC-3 via Journal und Stabilitaetschecks

### Task 8 - Plan-Verifikation

- Scope:
  - Plan und Ergebnis Zeile fuer Zeile abgleichen, verbleibende Luecken sofort schliessen oder blockieren
- Checklist:
  - kompletten Plan neu lesen
  - Implementierung/Tests/Deploy/Evidence gegen den Plan abgleichen
  - `CHANGELOG.md`, PR-Sektionen, Labels und Close-Pfade pruefen
- Acceptance criteria:
  - AC-1: keine unbelegte Zeile aus dem Plan bleibt still offen
  - AC-2: [PROGRESS.md](/work/company/project-sentinel/PROGRESS.md) endet auf `COMPLETE` oder sauberen Blockern
  - AC-3: die Schlusslage ist issue-/PR-/VM-seitig close-ready
- Evidence plan:
  - AC-1 via Plan-Diff gegen Ergebnis
  - AC-2 via finaler [PROGRESS.md](/work/company/project-sentinel/PROGRESS.md)-Stand
  - AC-3 via GitHub-/VM-/PR-Pruefung
