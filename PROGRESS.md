# PROGRESS

## Status

- Plan source: `/work/company/codex-plan314.md`
- Overall status: `TASK_1_DONE_TASK_2_PENDING`
- Current task: `Task 2 - Phase 2: Gateway Policy-Layer`
- Current branch: `feat/issue-314-agent-model-policy`
- Worktree: `/work/company/project-sentinel`
- Base: `origin/main @ 0f1c46c19bfa61d0616b3468834d29b557b3e254`
- Hook status: `PreToolUse TaskUpdate + PostToolUse start-enforcer projektlokal registriert`
- Last refresh: `2026-04-24 Europe/Vienna`

## Current findings

- `$start` wurde fuer #314 aktiviert.
- Hook-Skripte existieren und sind ausfuehrbar:
  `pretooluse-task-checklist-gate.sh`, `pretooluse-start-progress-gate.sh`, `posttooluse-start-enforcer.sh`.
- Projektlokale Hooks wurden in `.claude/settings.json` registriert und die Start-Counter wurden zurueckgesetzt.
- Projektregeln, globale Regeln, Workspace-Handover, `.claude/AGENTS.md` und der komplette #314-Plan wurden in dieser Session frisch gelesen.
- `mainrag search "Issue 314 Haiku Gateway model policy" --source claude-conversations --limit 5` ist aktuell nicht nutzbar, weil `localhost:3001` `Connection refused` liefert. Das ist dokumentiert, aber kein Task-Blocker.
- `git fetch origin` lief, `main` ist synchron mit `origin/main` (`ahead/behind 0/0`), kein Pull war notwendig.
- GitHub-Issue `#314` ist offen und traegt aktuell `quality:needs-spec`, `status:triage`, `status:backlog`.
- Der Issue-Quality-Bot fordert die fehlenden Sektionen `Scope`, `Out of Scope`, `Benchmarks`.
- Umsetzungsscope bleibt Go/Gateway-zentriert; Daemon-Haiku-Pinning ist out-of-scope.
- Task 1 ist erledigt:
  - `issue-314-body.md` wurde als Issue-Body-Artefakt erstellt.
  - GitHub-Issue `#314` wurde aktualisiert und steht jetzt auf `quality:ready` und `status:in-progress`.
  - `quality:needs-spec`, `status:triage` und `status:backlog` wurden entfernt.
  - `ssh ubuntu@10.0.0.240 "/usr/bin/claude -p --model haiku 'Antworte exakt mit PONG.'"` lieferte `PONG`.
  - In Task 1 wurde kein Daemon-Code geaendert.

## Blocked items

- Kein technischer Blocker beim Setup.
- `mainrag` ist lokal nicht erreichbar; falls fuer spaetere Architekturfragen relevant, erneut pruefen.

## Commit references

- `TBD` Task [1] Phase 1 - Issue-Body-Repair, Branch und Preflight
- `TBD` Task [2] Phase 2 - Gateway Policy-Layer
- `TBD` Task [3] Phase 3 - Observability und Response Log
- `TBD` Task [4] Phase 4 - Go-Tests
- `TBD` Task [5] Phase 5 - Benchmarks
- `TBD` Task [6] Phase 6 - Gateway Deploy auf 10.0.0.240
- `TBD` Task [7] Phase 8 - AC-Matrix und Live-Verifikation
- `TBD` Task [8] Dokumentation, PR- und Close-Sequenz
- `TBD` Task [9] Plan-Verifikation

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | Phase 1 - Issue-Body-Repair, Branch und Preflight | DONE | Branch von main, GitHub-Body reparieren, `quality:ready`, Haiku-String fuer claude-code pruefen, Platform-Controlplane out-of-scope bestaetigen | command, inspect, system |
| 2 | Phase 2 - Gateway Policy-Layer | PENDING | Request-Klassifikation, Agent-Runtime-Policy, Resolver-Reihenfolge, fail-closed Validation | inspect, command |
| 3 | Phase 3 - Observability und Response Log | PENDING | Traffic-Stats, ResponseLogEntry, Journal-Logs fuer Success/Stream/Error, ggf. bounded circular buffer | inspect, command |
| 4 | Phase 4 - Go-Tests | PENDING | Unit-/Regressionstests fuer Klassen, Policy, `/v1/messages`, Response-Logs und Validation | command |
| 5 | Phase 5 - Benchmarks | PENDING | Classify/Resolve/ResponseLog Benchmarks mit Zielwerten und System-Monitoring | command, system |
| 6 | Phase 6 - Gateway Deploy auf 10.0.0.240 | PENDING | ExecStart pruefen, Linux-Binary bauen, deployen, Gateway restart, Smoke | command, system |
| 7 | Phase 8 - AC-Matrix und Live-Verifikation | PENDING | AC-1 bis AC-6 einzeln auf VM belegen, Config restore, Panic/Error/Secret-Grep | command, system |
| 8 | Dokumentation, PR- und Close-Sequenz | PENDING | CHANGELOG, Evidence-Doku, PR mit Pflichtsektionen, Labels, Issue-Close erst nach verified | command, inspect |
| 9 | Plan-Verifikation | PENDING | Plan komplett gegen Ergebnis pruefen, Abweichungen fixen oder blocken | inspect, command, system |

## Task 1 - Phase 1: Issue-Body-Repair, Branch und Preflight

### Pre-task self-check

- Was muss getan werden:
  - frischen Branch von synchronem `main` verwenden
  - GitHub-Issue `#314` mit dem Plan-Body reparieren
  - `quality:needs-spec` entfernen und `quality:ready` setzen
  - kanonischen Haiku-String fuer den aktuellen `claude-code` Provider pruefen
  - Platform-Controlplane fuer #314 explizit out-of-scope halten
- Welche ACs muessen hier passen:
  - AC-1: Branch basiert sauber auf `origin/main`.
  - AC-2: GitHub-Issue-Body enthaelt `Scope`, `Out of Scope`, `Benchmarks` und die 6 ACs.
  - AC-3: Labels zeigen nicht mehr `quality:needs-spec`, sondern `quality:ready`.
  - AC-4: Haiku-Provider-String ist per Live-Preflight geprueft oder als Blocker dokumentiert.
  - AC-5: Keine Daemon-Code-Aenderung in Task 1.
- Wie wird bewiesen:
  - `git status`, `git rev-list`, `gh issue view/edit`, `ssh ubuntu@10.0.0.240 "/usr/bin/claude -p --model haiku ..."`
- Erwartete Dateien:
  - `PROGRESS.md`
  - optional `issue-314-body.md`
  - optional `test-314-verification.md`
- Risiken:
  - `claude -p --model haiku` kann wegen Quota/Auth scheitern; dann Task 1 blockiert nicht den Issue-Body-Repair, aber der kanonische Modellstring muss spaeter vor Deploy final belegt werden.
  - Issue-Body-Repair ist GitHub-Schreibaktion; Ergebnis muss direkt per `gh issue view` gegengeprueft werden.

### Outcome

- Branch `feat/issue-314-agent-model-policy` wurde von synchronem `main` erstellt.
- `issue-314-body.md` enthaelt den reparierten GitHub-Issue-Body.
- `gh issue edit 314` hat den Body aktualisiert, `quality:needs-spec`, `status:triage` und `status:backlog` entfernt sowie `quality:ready` und `status:in-progress` gesetzt.
- `gh issue view 314` bestaetigt die reparierten Labels und Body-Sektionen.
- VM-Preflight bestaetigt `haiku` als akzeptierten `claude-code` Modellalias mit Output `PONG`.

### Evidence

- `test-314-verification.md` enthaelt Task-1 Command/Output-Evidence.
- AC-1 PASS: Branch basiert auf `origin/main @ 0f1c46c19bfa61d0616b3468834d29b557b3e254`, ahead/behind vor Branch-Erstellung `0/0`.
- AC-2 PASS: Issue-Body enthaelt `Kontext`, `Scope`, `Out of Scope`, `Acceptance Criteria`, `Benchmarks`, `Verify-Ideen`.
- AC-3 PASS: Labels enthalten `quality:ready` und `status:in-progress`; alte Spec-/Triage-/Backlog-Labels sind entfernt.
- AC-4 PASS: VM-Befehl `/usr/bin/claude -p --model haiku ...` lieferte `PONG`.
- AC-5 PASS: Task 1 aenderte nur `PROGRESS.md`, `docs/issue-314-body.md` und `test-314-verification.md`.

## Task 2 - Phase 2: Gateway Policy-Layer

### Pre-task self-check

- Was muss getan werden:
  - bestehende Gateway-Pipeline, Provider-Konfig und Control-Plane-Strukturen lesen
  - `request_class` zentral einfuehren
  - `agent_runtime_model_policy` als Gateway-Konfiguration einfuehren
  - Resolver-Reihenfolge implementieren: explizites Modell, Agent-Runtime-Policy, Provider-Fallback
  - Validation fuer unaufloesbare Policy/Provider-Kombinationen fail-closed einbauen
- Welche ACs muessen hier passen:
  - AC-1: `external_compat`, `agent_runtime`, `platform_controlplane`, `service_internal`, `internal_other` sind als klare Klassen modelliert.
  - AC-2: `agent_runtime` erfordert positive numerische `agent_id` und schliesst `platform_analysis`, `request_type`, `sentinel-judge`, `PLATFORM-CONTROLPLANE` aus.
  - AC-3: Leeres Modell wird nur fuer `agent_runtime` zu Haiku resolved.
  - AC-4: Explizites Request-Modell gewinnt.
  - AC-5: `/v1/messages` bleibt bei `PreferredProvider=anthropic-direct` und bekommt keine Agent-Policy.
  - AC-6: Ungueltige Policy/Provider-Kombination wird nicht still auf Opus zurueckfallen.
- Wie wird bewiesen:
  - gezielte Go-Tests in Task 4
  - fuer Task 2 zusaetzlich strukturelle Inspection und `go test` fuer betroffene Packages, sobald Code geaendert ist
- Erwartete Dateien:
  - `cmd/cortex-gateway/internal/proxy/policy.go`
  - `cmd/cortex-gateway/internal/proxy/provider.go`
  - `cmd/cortex-gateway/internal/proxy/pipeline.go`
  - `cmd/cortex-gateway/main.go`
  - ggf. `cmd/cortex-gateway/internal/control/plane.go`
- Risiken:
  - aktuelle Config-Strukturen koennen in `control` statt `proxy` liegen; keine neue Parallel-Konfig bauen, sondern bestehende Patterns nutzen.
  - Response-Observability gehoert erst in Task 3; Task 2 soll Policy-Entscheidung und Request-Klassen sauber schneiden.
