# PROGRESS

## Status

- Plan source: `/work/company/codex-plan263.md`
- Overall status: `IN_PROGRESS`
- Current task: `Task 7 - Deploy, Benchmarks sowie AC-1 bis AC-12 inkl. UI-Evidence auf der VM verifizieren`
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
  - `cargo remote -c -- test -p sentinel-common -p sentinel-daemon -p sentinel-projection` lief mit Exit `0` durch; massgebliche Endzeilen zeigen `142 passed` fuer `sentinel-daemon` sowie gruenen `sentinel_projection`-/Acceptance-Run
  - `cargo remote -c -- clippy -p sentinel-common -p sentinel-daemon -p sentinel-projection --all-targets -- -D warnings` lief ebenfalls mit Exit `0` durch; der Output ist wegen `cargo remote`-Artifact-Transfer sehr rauschig, aber ohne Clippy-Fehler beendet
- Task-3-Analyzer ist jetzt als echter daemon-interner Worker vorhanden:
  - neue Datei [llm_analyzer.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs)
  - Modul-Export in [mod.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/mod.rs)
  - Start/Wiring in [orchestrator.rs](/work/company/project-sentinel/services/sentinel-daemon/src/orchestrator.rs)
- Der Analyzer benutzt ausschliesslich den internen Gateway-Vertrag `POST /internal/llm`, baut seinen Kontext aus `PlatformMetrics`, Verify-Ergebnissen, den letzten `PlatformIntervention`-Events und fehlgeschlagenen Interventionen und dispatcht erfolgreiche Antworten jetzt als strukturiertes `PlatformAnalysisCommand` in den gemeinsamen Runtime-Executor.
- Remote-Rust-Evidence fuer Task 3 ist vorhanden:
  - `cargo remote -c -- test -p sentinel-daemon -p sentinel-common` => Exit `0`; massgebliche Endzeilen zeigen `145 passed; 0 failed`
  - `cargo remote -c -- clippy -p sentinel-daemon --all-targets -- -D warnings` => Exit `0`
- Task-4-Trigger- und State-Ebene ist jetzt lokal umgesetzt:
  - [mod.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/mod.rs) fuehrt `PlatformControlCommand`, `PlatformTriggerTestCommand`, `PlatformStateSnapshot`, `PlatformCycleOutput`, unresolved counters, queued trigger und Analyse-Request-Building ein
  - [operator_api.rs](/work/company/project-sentinel/services/sentinel-daemon/src/operator_api.rs) enthaelt jetzt `POST /operator/platform-analyze`, `POST /operator/platform-trigger-test` und `GET /operator/platform-state`
  - [orchestrator.rs](/work/company/project-sentinel/services/sentinel-daemon/src/orchestrator.rs) verbindet `platform_rx`, Analyzer-Queue und den live publizierten Platform-State mit dem ECS-Tick-Loop
- Remote-Rust-Evidence fuer Task 4 ist vorhanden:
  - `cargo remote -c -- test -p sentinel-daemon -- --nocapture` => Exit `0`; Endzeilen zeigen `152 passed; 0 failed`
  - `cargo remote -c -- clippy -p sentinel-daemon --all-targets -- -D warnings` => Exit `0`; Endzeile `Finished 'dev' profile ...`
- Task-5-Executor ist jetzt lokal umgesetzt:
  - [resource_manager.rs](/work/company/project-sentinel/services/sentinel-daemon/src/resource_manager.rs) fuehrt `force_profile_and_apply()` mit echtem cgroup-Resize und `ResourceProfileChanged`-Audit ein
  - [platform_controlplane/mod.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/mod.rs) fuehrt `PlatformAnalysisCommand`, validierte Threshold-Overrides, `persist_platform_analysis_event()` und den gemeinsamen Analysis-Payload-Vertrag ein
  - [operator_api.rs](/work/company/project-sentinel/services/sentinel-daemon/src/operator_api.rs) exponiert jetzt `POST /operator/platform-analysis-test` fuer den lokalen/auth-geschuetzten AC-9-Testpfad
  - [orchestrator.rs](/work/company/project-sentinel/services/sentinel-daemon/src/orchestrator.rs) persistiert und exekutiert `PlatformAnalysisCommand` jetzt zentral fuer LLM-Analysen, Operator-Test-Hooks und deterministische `ForceIdleProfile`-Sideeffects
- Remote-Rust-Evidence fuer Task 5 ist vorhanden:
  - `cargo remote -c -- test -p sentinel-daemon -- --nocapture` => Exit `0`; Endzeilen zeigen `156 passed; 0 failed`
  - `cargo remote -c -- clippy -p sentinel-daemon --all-targets -- -D warnings` => Exit `0`; Endzeile `Finished 'dev' profile [unoptimized + debuginfo] target(s) in 5.71s`
- Task-6-Dashboard-/Cockpit-/UI-Ebene ist jetzt lokal umgesetzt:
  - [control.ts](/work/company/project-sentinel/dashboard/src/routes/control.ts) exponiert jetzt `GET /api/control/platform-state`, `GET /api/control/platform-analyses` und `POST /api/control/platform-analyze`
  - [db.ts](/work/company/project-sentinel/dashboard/src/db.ts) liest `platform_analysis`-Events strukturiert aus dem Event Store
  - [cockpit.ts](/work/company/project-sentinel/dashboard/src/routes/cockpit.ts) behandelt `platform_analysis` und `platform_intervention` jetzt als echte Cockpit-Incidents
  - [control.js](/work/company/project-sentinel/dashboard/public/js/control.js) rendert neue UI-Sektionen/Selektoren fuer Analysen und Platform-State
  - [cockpit.js](/work/company/project-sentinel/dashboard/public/js/cockpit.js) setzt `data-incident-type` fuer Playwright-stabile Incident-Selektoren
- Dashboard-Evidence fuer Task 6 ist lokal vorhanden:
  - `cd dashboard && bun test src/__tests__/control.test.ts src/routes/cockpit.test.ts` => Exit `0`; `27 pass`
  - `cd dashboard && bun test` => Exit `0`; `63 pass`
- Offen bleibt fuer Task 7 bewusst:
  - `SENTINEL_DASHBOARD_API_KEY` ist live auf `sentinel-projection` weiter leer und muss fuer die UI-Write-Abnahme auf der VM erst provisioniert werden
  - Playwright-/Screenshot-Evidence und die echte VM-Read/Write-Verifikation folgen erst nach Deploy in Task 7

## Blocked items

- Keine harten Blocker beim Start.
- Beobachtung: Solange `mainrag` lokal nicht verfuegbar ist, kann ich keine frische Conversation-Suche als Zusatzkontext ziehen. Die Ausfuehrung von `#263` selbst ist dadurch aber nicht blockiert.

## Commit references

- `ae1b5cf` Task [1]
- `de96c37` Task [2]
- `f2bd13a` Task [3]
- `93f2080` Task [4]
- `3355a69` Task [5]
- `TBD` Task [6]
- `TBD` Task [7]
- `TBD` Task [8]

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | Issue-Hygiene, Branch-Setup und deterministische Baseline fuer AC-1 bis AC-5 / AC-10 bis AC-12 neu setzen | DONE | `#263`-Ist-Zustand gegen Repo/GitHub/VM abgleichen, offene Baseline-Luecken fuer Stall/Projection/Cooldowns/Disable-Flag pruefen, Issue-Text vorbereiten, Branch sauber halten | command, system, inspect |
| 2 | Event- und Config-Schema fuer die LLM-Ebene ergaenzen | DONE | `PlatformAnalysis`-Event, neue Platform-CP-Config-Felder, TOML-Defaults und Typen stabil ergaenzen | inspect, command |
| 3 | LLM-Analyzer als daemon-internes Background-Modul implementieren | DONE | asynchronen Analyzer, Kontext-Assembly, Gateway-Call und Parsing/Persistenz bauen | inspect, command |
| 4 | Eskalationslogik, unresolved counters und deterministische Trigger vervollstaendigen | DONE | scheduled/manual/unresolved Trigger, Counter-State und Test-Hooks fuer `AC-6` vervollstaendigen | inspect, command, system |
| 5 | Suggested-Action-Executor mit force_profile / adjust_threshold / escalate_to_operator implementieren | DONE | guard-railed Executor inkl. cgroup-Apply, Audit-Trail und Runtime-Overrides bauen | inspect, command, system |
| 6 | Operator-API, Dashboard, Cockpit und Playwright-stabile UI-Surfaces erweitern | DONE | API-Read/Write-Pfade, Dashboard/Cockpit-Rendering, stabile Selektoren und lokale Testabdeckung ergaenzen | inspect, command, browser |
| 7 | Deploy, Benchmarks sowie AC-1 bis AC-12 inkl. UI-Evidence auf der VM verifizieren | IN_PROGRESS | Release-Build, Deploy, systemd-Restarts, AC-Matrix, Playwright-Screenshots und Benchmarks mit Systemmetriken abarbeiten | command, system, browser |
| 8 | Plan-Verifikation | PENDING | Gesamtergebnis Zeile fuer Zeile gegen den Plan pruefen, Restluecken sofort fixen oder als Blocker dokumentieren | inspect, command, system, browser |

## Task details

### Task 1 - Issue-Hygiene, Branch-Setup und deterministische Baseline fuer AC-1 bis AC-5 / AC-10 bis AC-12 neu setzen

- Scope:
  - `#263`-Ist-Zustand gegen aktuellen Repo-/VM-Stand neu erfassen
  - Baseline-Facts fuer `AC-1` bis `AC-5` und `AC-10` bis `AC-12` live belegen oder als echte Luecken markieren
  - Branch-/Issue-Hygiene auf den Ausfuehrungsstand ziehen
- Checklist:
  - GitHub-Issue `#263` lesen und mit [codex-plan263.md](/work/company/codex-plan263.md) abgleichen
  - massgebliche Baseline-Implementierung in `platform_controlplane`, `service_health`, `config` und Deploy-Config lesen
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
  - Der HTTP-Pfad geht fest gegen `POST /internal/llm`; erfolgreiche Antworten werden als strukturierte `PlatformAnalysisCommand`-Objekte an den gemeinsamen Runtime-Executor weitergereicht.
  - Timeout- und Parse-Fehler bleiben explizit im Fehlerpfad, statt den Daemon zu crashen.
- Evidence:
  - AC-1 PASS:
    - [llm_analyzer.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs) fuehrt `PlatformLlmAnalyzerHandle::spawn()` mit async Worker und Queue ein
    - `handle_enqueue_is_non_blocking_and_worker_persists` belegt den nicht blockierenden Queue-Pfad
    - [orchestrator.rs](/work/company/project-sentinel/services/sentinel-daemon/src/orchestrator.rs) startet den Worker daemon-intern beim Boot
  - AC-2 PASS:
    - `analyzer_dispatches_platform_analysis_command` prueft, dass der Mock-Request auf `/internal/llm` geht
    - [llm_analyzer.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs) sendet ausschliesslich an `format!("{}/internal/llm", ...)`
  - AC-3 PASS:
    - `analyzer_dispatches_platform_analysis_command` prueft die strukturierte `PlatformAnalysisCommand`-Payload inklusive `provider`, `model`, `unresolved_keys` und `parameters`
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
- Pre-task self-check:
  - Was muss getan werden: Die neue LLM-Ebene braucht echte Trigger-Quellen und einen lesbaren Runtime-State, sonst bleiben AC-6 und der spaetere UI-/VM-Pfad nondeterministisch.
  - Welche ACs muessen hier passen: manual, scheduled und unresolved-escalation sind alle ueber denselben Analyzer-Pfad verdrahtet; Operator- und Dashboard-State koennen diesen Zustand lesen.
  - Wie wird bewiesen: zielgerichtete Remote-Rust-Tests fuer Controlplane, Operator-API und anschliessend gruener Gesamt-`sentinel-daemon`-Lauf plus Clippy.
  - Erwartete Dateien: [mod.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/mod.rs), [llm_analyzer.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs), [operator_api.rs](/work/company/project-sentinel/services/sentinel-daemon/src/operator_api.rs), [orchestrator.rs](/work/company/project-sentinel/services/sentinel-daemon/src/orchestrator.rs)
  - Risiken: Borrow-Konflikte im Controlplane-State, zu viele Signaturargumente im Operator-Wiring, `cargo remote`-Output verrauscht die eigentliche Rust-Diagnose.
- Outcome:
  - `PlatformControlplane` verwaltet jetzt unresolved counters pro `rule:target`, gescheiterte Interventionen, letzte Analyse-Trigger und scheduled/queued Analyse-Requests.
  - `AnalyzeNow` und deterministische Test-Trigger (`scheduled`, `unresolved_escalation`) laufen alle ueber denselben `PlatformAnalysisRequest`-Pfad wie die spaetere echte Runtime.
  - Die lokale Operator-API exponiert jetzt `POST /operator/platform-analyze`, `POST /operator/platform-trigger-test` und `GET /operator/platform-state`.
  - Der ECS-Tick-Loop drainet Platform-Kommandos, queued Analyse-Requests in den async Analyzer und publiziert pro Tick einen lesbaren `PlatformStateSnapshot`.
- Evidence:
  - AC-1 PASS:
    - [mod.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/mod.rs) fuehrt `PlatformControlCommand::{AnalyzeNow, TriggerTest}`, `PlatformTriggerTestCommand` und `PlatformCycleOutput` ein
    - `test_manual_trigger_creates_analysis_request`, `test_scheduled_analysis_respects_interval` und `test_unresolved_threshold_triggers_escalation_once` laufen gruen
  - AC-2 PASS:
    - [operator_api.rs](/work/company/project-sentinel/services/sentinel-daemon/src/operator_api.rs) fuehrt `POST /operator/platform-trigger-test` und `GET /operator/platform-state` ein
    - `platform_analyze_is_forwarded_to_platform_channel`, `platform_trigger_test_is_forwarded`, `unresolved_trigger_test_requires_rule_and_target` und `platform_state_endpoint_returns_snapshot` laufen gruen
  - AC-3 PASS:
    - [orchestrator.rs](/work/company/project-sentinel/services/sentinel-daemon/src/orchestrator.rs) verdrahtet `platform_rx`, `platform_llm_analyzer.enqueue(...)` und `publish_platform_state_snapshot(...)`
    - `cargo remote -c -- test -p sentinel-daemon -- --nocapture` => Exit `0`; Endzeile `152 passed; 0 failed`
    - `cargo remote -c -- clippy -p sentinel-daemon --all-targets -- -D warnings` => Exit `0`; Endzeile `Finished 'dev' profile [unoptimized + debuginfo] target(s) in 8.29s`

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
- Pre-task self-check:
  - Was muss getan werden: Die LLM-/Operator-Analyse muss in denselben guard-railed Executor laufen wie deterministische Sideeffects, sonst bleibt AC-9 nur scheinbar erfuellt.
  - Welche ACs muessen hier passen: echter cgroup-Apply+Audit fuer `force_profile`, wirksamer Runtime-Override fuer `adjust_threshold`, nachvollziehbare Eskalation fuer `escalate_to_operator`.
  - Wie wird bewiesen: Remote-Rust-Tests/Clippy fuer Wiring und Validierung; VM-Evidence folgt in Task 7.
  - Erwartete Dateien: [resource_manager.rs](/work/company/project-sentinel/services/sentinel-daemon/src/resource_manager.rs), [platform_controlplane/mod.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/mod.rs), [llm_analyzer.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs), [operator_api.rs](/work/company/project-sentinel/services/sentinel-daemon/src/operator_api.rs), [orchestrator.rs](/work/company/project-sentinel/services/sentinel-daemon/src/orchestrator.rs)
  - Risiken: Analyzer/Test-Hook duerfen keinen Parallelpfad aufmachen; `force_profile` darf nicht nur State aendern; Override-State muss spaeter ueber Dashboard/VM lesbar bleiben.
- Outcome:
  - `PlatformAnalysisCommand` ist jetzt der gemeinsame Payload fuer echte LLM-Analysen und den lokalen Operator-Test-Hook.
  - Der Analyzer dispatcht nicht mehr direkt in den Event-Store, sondern uebergibt erfolgreiche Antworten in den gemeinsamen Runtime-Executor.
  - Der Orchestrator persistiert `platform_analysis` zentral, fuehrt `force_profile` ueber echten cgroup-Resize plus `ResourceProfileChanged`-Audit aus, setzt `adjust_threshold` als validierten Runtime-Override und loggt `escalate_to_operator` sichtbar.
  - Der bestehende deterministische `ForceIdleProfile`-Sideeffect nutzt denselben echten Apply-Pfad und laeuft damit nicht mehr nur auf In-Memory-State.
- Evidence:
  - AC-1 PASS:
    - [resource_manager.rs](/work/company/project-sentinel/services/sentinel-daemon/src/resource_manager.rs) fuehrt `force_profile_and_apply()` ein
    - [orchestrator.rs](/work/company/project-sentinel/services/sentinel-daemon/src/orchestrator.rs) ruft fuer `ApplyAnalysis(force_profile)` und `PlatformSideEffect::ForceIdleProfile` denselben Apply-Pfad
    - [operator_api.rs](/work/company/project-sentinel/services/sentinel-daemon/src/operator_api.rs) validiert `force_profile`-Payloads via `POST /operator/platform-analysis-test`
  - AC-2 PASS:
    - [platform_controlplane/mod.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/mod.rs) fuehrt validierte `threshold_overrides` samt `effective_config()` ein
    - `test_apply_threshold_override_updates_effective_config` prueft, dass der Override die effektive Regelauswertung veraendert
    - [orchestrator.rs](/work/company/project-sentinel/services/sentinel-daemon/src/orchestrator.rs) publiziert `threshold_overrides` und `resource_profiles` in den live `PlatformStateSnapshot`
  - AC-3 PASS:
    - [platform_controlplane/mod.rs](/work/company/project-sentinel/services/sentinel-daemon/src/platform_controlplane/mod.rs) fuehrt `persist_platform_analysis_event()` ein
    - `test_persist_platform_analysis_event_normalizes_empty_target` prueft den zentralen Persistenzpfad
    - `platform_analysis_test_is_forwarded`, `platform_analysis_test_rejects_missing_force_profile_parameters` und `analyzer_dispatches_platform_analysis_command` belegen den gemeinsamen Executor-Pfad fuer Operator-Test und Analyzer
    - `cargo remote -c -- test -p sentinel-daemon -- --nocapture` => Exit `0`; Endzeile `156 passed; 0 failed`
    - `cargo remote -c -- clippy -p sentinel-daemon --all-targets -- -D warnings` => Exit `0`; Endzeile `Finished 'dev' profile [unoptimized + debuginfo] target(s) in 5.71s`

### Task 6 - Operator-API, Dashboard, Cockpit und Playwright-stabile UI-Surfaces erweitern

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
- Pre-task self-check:
  - Was muss getan werden: Die bestehenden Dashboard-/Cockpit-Pfade muessen erweitert werden, damit Platform-State und Analysen nicht nur im Backend existieren, sondern auch fuer Operator und spaetere Playwright-Abnahme lesbar und steuerbar werden.
  - Welche ACs muessen hier passen: neue Read-/Write-Endpunkte im Dashboard, sichtbare `platform_analysis`-/`platform_intervention`-Incidents, stabile Selektoren fuer Control/Cockpit.
  - Wie wird bewiesen: Bun-Route-/UI-Tests lokal; Live-/Screenshot-Evidence folgt in Task 7 auf der VM.
  - Erwartete Dateien: [control.ts](/work/company/project-sentinel/dashboard/src/routes/control.ts), [db.ts](/work/company/project-sentinel/dashboard/src/db.ts), [types.ts](/work/company/project-sentinel/dashboard/src/types.ts), [cockpit.ts](/work/company/project-sentinel/dashboard/src/routes/cockpit.ts), [control.js](/work/company/project-sentinel/dashboard/public/js/control.js), [cockpit.js](/work/company/project-sentinel/dashboard/public/js/cockpit.js), [control.test.ts](/work/company/project-sentinel/dashboard/src/__tests__/control.test.ts), [cockpit.test.ts](/work/company/project-sentinel/dashboard/src/routes/cockpit.test.ts)
  - Risiken: Write-Pfade duerfen keinen Parallelpfad zur Operator-API aufmachen; Cockpit darf bestehende Incident-Logik nicht regressieren; VM-Write-Abnahme bleibt ohne `SENTINEL_DASHBOARD_API_KEY` bewusst blockiert.
- Outcome:
  - Das Dashboard erweitert jetzt bestehende statt neue Parallelpfade: `GET /api/control/platform-state`, `GET /api/control/platform-analyses` und `POST /api/control/platform-analyze` haengen an den vorhandenen Control-Routen.
  - `platform_analysis`-Events werden aus dem Event Store strukturiert gelesen und im Control-View mit stabilen Selektoren (`#platform-analyze-btn`, `#platform-analysis-list`, `data-trigger`, `data-severity`, `data-suggested-action`) gerendert.
  - Der Platform-State wird im Control-View ueber `#platform-state-section`, `#platform-state-table`, `#platform-threshold-overrides` und `data-platform-agent-id` sichtbar.
  - Das Cockpit behandelt `platform_analysis` und `platform_intervention` jetzt als echte Incident-Typen; die Frontend-Items tragen `data-incident-type`.
  - Die lokale Testabdeckung umfasst sowohl die neuen Control-Routen als auch Cockpit-Mapping und bleibt ueber den kompletten Dashboard-Testlauf gruen.
- Evidence:
  - AC-1 PASS:
    - [control.ts](/work/company/project-sentinel/dashboard/src/routes/control.ts) fuehrt `GET /control/platform-state`, `GET /control/platform-analyses` und `POST /control/platform-analyze` ein
    - [db.ts](/work/company/project-sentinel/dashboard/src/db.ts) fuehrt `getRecentPlatformAnalyses()` ein
    - `cd dashboard && bun test src/__tests__/control.test.ts src/routes/cockpit.test.ts` => Exit `0`; `27 pass`
  - AC-2 PASS:
    - [cockpit.ts](/work/company/project-sentinel/dashboard/src/routes/cockpit.ts) mapped `platform_analysis` und `platform_intervention` auf Severity/Summary/Incident-Felder
    - [control.js](/work/company/project-sentinel/dashboard/public/js/control.js) und [cockpit.js](/work/company/project-sentinel/dashboard/public/js/cockpit.js) rendern die neuen Control-/Cockpit-Surfaces mit stabilen IDs/Data-Attributes
    - `cd dashboard && bun test` => Exit `0`; `63 pass`
  - AC-3 PASS fuer den lokalen Build-/Selektor-Teil:
    - `#platform-analyze-btn`, `#platform-analyses-section`, `#platform-analysis-list`, `#platform-state-section`, `#platform-state-table`, `#platform-threshold-overrides` und `.cockpit-incident-item[data-incident-type=...]` sind jetzt im Frontend verankert
    - Der produktive `SENTINEL_DASHBOARD_API_KEY`-Pfad bleibt fuer die echte VM-Write-Abnahme absichtlich in Task 7 offen; ohne diesen Key wird `#263` nicht geschlossen

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
