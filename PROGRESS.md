# PROGRESS

## Status

- Plan source: `/work/company/codex-plan314.md`
- Overall status: `TASK_9_DONE_READY_FOR_PR`
- Current task: `Post-task GitHub PR/Merge/Close sequence`
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
- Task 2 ist erledigt:
  - `RequestClass` wurde zentral im Gateway eingefuehrt.
  - `agent_runtime` wird nur bei positiver numerischer `agent_id` und nach Ausschluss von Platform-/Service-/Analysepfaden gesetzt.
  - `agent_runtime_model_policy` setzt in der Control-Config standardmaessig `haiku`.
  - `ResolveModelPolicy` setzt Haiku nur fuer `agent_runtime` ohne explizites Modell.
  - `/v1/messages` bleibt `external_compat` und `PreferredProvider=anthropic-direct`.
  - ungueltige Policy/Provider-Kombinationen failen vor dem Provider-Call mit `model policy rejected`.
  - Zwischenfund: Die Policy wurde nach erstem Testfail aus dem Pre-Synthesis-Pfad in den echten Forward-Pfad verschoben.
  - `go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control` ist gruen.
  - `go build ./cmd/cortex-gateway` ist gruen.
- Task 3 ist erledigt:
  - `traffic-stats` zeigt jetzt `agent_runtime_model_policy` und den letzten Agent-Runtime-Forward mit effektivem Modell, Policy-Source und Provider.
  - `ResponseLogEntry` enthaelt `request_class`, `provider`, `model`, `policy_source`, `agent_id`, `agent_name` ohne Header-/Secret-Felder.
  - Provider-Success, Provider-Error, Stream-Success und Stream-Error loggen Request-Klasse, effektives Modell und Policy-Source.
  - `ResponseLogBuffer` ist jetzt ein bounded circular buffer und vermeidet steady-state O(n)-Kopie beim Append.
  - `go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control` ist gruen.
  - `go build ./cmd/cortex-gateway` ist gruen.
- Task 4 ist erledigt:
  - Policy-Unit-Tests pruefen strikte Request-Klassifikation und Resolver-Reihenfolge.
  - Pipeline-Test prueft, dass Agent-Runtime-Requests auf `haiku` gesetzt werden.
  - Anthropic-Compat-Test prueft, dass `/v1/messages` `external_compat` bleibt und die Request-Override-Policy nutzt.
  - Control-Tests pruefen Default, erlaubte Werte und Rejects fuer `agent_runtime_model_policy`.
  - Response-Log-Test prueft Ring-Overwrite, chronologische Ausgabe und `LastByClass`.
  - `go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control` ist gruen.
  - `go build ./cmd/cortex-gateway` ist gruen.
- Task 5 ist erledigt:
  - Benchmark-Harness fuer `ClassifyRequest`, `ResolveModelPolicy` und `ResponseLogBuffer.Add` wurde ergaenzt.
  - Benchmarks mit `benchmem` laufen gruen:
    - `BenchmarkClassifyRequestAgentRuntime`: `494.9 ns/op`, `16 B/op`, `1 allocs/op`
    - `BenchmarkResolveModelPolicyAgentRuntime`: `38.43 ns/op`, `0 B/op`, `0 allocs/op`
    - `BenchmarkResponseLogBufferAdd`: `3831 ns/op`, `0 B/op`, `0 allocs/op`
  - Zielwerte sind erfuellt: Classify `<1us`, Resolve `<1us`, ResponseLog Add `<10us`.
  - `/usr/bin/time -v`, `vmstat` und `iostat` wurden fuer CPU/RAM/IO-Evidence erfasst.
- Task 6 ist erledigt:
  - `sentinel-gateway` nutzt laut systemd `ExecStart=/opt/sentinel/bin/cortex-gateway`.
  - Altes Binary wurde gesichert unter `/opt/sentinel/bin/cortex-gateway.bak-issue314-20260424062401`.
  - Erster Copy-Versuch traf `Text file busy`; Fix war kontrolliertes `systemctl stop`, Copy, `systemctl start`.
  - Neues Binary `/opt/sentinel/bin/cortex-gateway` ist deployed (`23244031` Bytes, `2026-04-24 06:24 UTC`).
  - `sentinel-gateway` ist `active`.
  - `curl -s localhost:8080/health` liefert `status=ok`.
  - `curl -s localhost:8081/control/traffic-stats` enthaelt `agent_runtime_model_policy: haiku`.
  - PID-basiertes Gateway-Journal seit Restart zeigt Startup-Logs ohne Panic/Fatal.
- Task 7 ist erledigt:
  - AC-1 PASS: Interner `/internal/llm` Agent-Runtime-Forward setzt `model=haiku`.
  - AC-2 PASS: Daemon enthaelt kein `AGENT_MODEL_HAIKU`; Agent-Runtime sendet `model: String::new()`.
  - AC-3 PASS: VM-Forward `request_id=49c25b39-466d-4c48-9bc0-04fac9e9b360` lief als `agent_runtime`, Provider `claude-code`, Model `haiku`.
  - AC-4 PASS: `/v1/messages` mit Dummy-Key ging gegen `anthropic-direct`, `request_class=external_compat`, `effective_model=claude-opus-4-6`, `policy_source=request_override`, HTTP `401` wegen absichtlich falschem Key.
  - AC-5 PASS: `traffic-stats`, `traffic-responses` und Journal zeigen redigierte Felder; Secret-Grep fand keine Token/API-Key-Werte.
  - AC-6 PASS: Go-Tests plus VM-Smoke belegen interne Runtime-Default-Policy getrennt vom externen Compatibility-Pfad.
  - Panic/Drift-Grep fuer Gateway/Daemon seit Live-Verifikation ist leer.
- Task 8 ist erledigt:
  - `CHANGELOG.md` enthaelt einen #314-Unreleased-Eintrag.
  - PR-Pflichtsektionen sind fuer `gh pr create` vorbereitet: Summary, Changes, Linked Issues, Test Plan, Benchmarks, Evidence, Checklist.
  - Issue-Close-Sequenz bleibt korrekt nachgelagert: erst PR/CI/Merge, dann `status:verified`, dann Close.
- Task 9 ist erledigt:
  - Plan-Slices 1-9 sind abgeschlossen.
  - `go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control` ist gruen.
  - `go build ./cmd/cortex-gateway` ist gruen.
  - VM-Gateway `/health` ist OK; `anthropic-direct` und `claude-code` Circuit-Breaker sind geschlossen.
  - VM-`traffic-stats` zeigt `agent_runtime_model_policy=haiku`, `last_agent_runtime_effective_model=haiku`, `last_agent_runtime_policy_source=agent_runtime_policy`.
  - GitHub Issue #314 ist offen mit `quality:ready` und `status:in-progress`; `status:verified` wird erst nach PR/Merge gesetzt.

## Blocked items

- Kein technischer Blocker beim Setup.
- `mainrag` ist lokal nicht erreichbar; falls fuer spaetere Architekturfragen relevant, erneut pruefen.

## Commit references

- `a42ef76` Task [1] Phase 1 - Issue-Body-Repair, Branch und Preflight
- `b953e4a` Task [2] Phase 2 - Gateway Policy-Layer
- `114732d` Task [3] Phase 3 - Observability und Response Log
- `b326a2f` Task [4] Phase 4 - Go-Tests
- `9d2adf5` Task [5] Phase 5 - Benchmarks
- `9495871` Task [6] Phase 6 - Gateway Deploy auf 10.0.0.240
- `TBD` Task [7] Phase 8 - AC-Matrix und Live-Verifikation
- `645553b` Task [8] Dokumentation, PR- und Close-Sequenz
- `TBD` Task [9] Plan-Verifikation

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | Phase 1 - Issue-Body-Repair, Branch und Preflight | DONE | Branch von main, GitHub-Body reparieren, `quality:ready`, Haiku-String fuer claude-code pruefen, Platform-Controlplane out-of-scope bestaetigen | command, inspect, system |
| 2 | Phase 2 - Gateway Policy-Layer | DONE | Request-Klassifikation, Agent-Runtime-Policy, Resolver-Reihenfolge, fail-closed Validation | inspect, command |
| 3 | Phase 3 - Observability und Response Log | DONE | Traffic-Stats, ResponseLogEntry, Journal-Logs fuer Success/Stream/Error, bounded circular buffer | inspect, command |
| 4 | Phase 4 - Go-Tests | DONE | Unit-/Regressionstests fuer Klassen, Policy, `/v1/messages`, Response-Logs und Validation | command |
| 5 | Phase 5 - Benchmarks | DONE | Classify/Resolve/ResponseLog Benchmarks mit Zielwerten und System-Monitoring | command, system |
| 6 | Phase 6 - Gateway Deploy auf 10.0.0.240 | DONE | ExecStart pruefen, Linux-Binary bauen, deployen, Gateway restart, Smoke | command, system |
| 7 | Phase 8 - AC-Matrix und Live-Verifikation | DONE | AC-1 bis AC-6 einzeln auf VM belegen, Config restore, Panic/Error/Secret-Grep | command, system |
| 8 | Dokumentation, PR- und Close-Sequenz | DONE | CHANGELOG, Evidence-Doku, PR mit Pflichtsektionen, Labels, Issue-Close erst nach verified | command, inspect |
| 9 | Plan-Verifikation | DONE | Plan komplett gegen Ergebnis pruefen, Abweichungen fixen oder blocken | inspect, command, system |

## Task 6 - Phase 6: Gateway Deploy auf 10.0.0.240

### Pre-task self-check

- Was muss getan werden:
  - VM-Servicepfad und `ExecStart` fuer `sentinel-gateway` pruefen
  - lokales Linux/amd64 Gateway-Binary bauen
  - Binary nach `/opt/sentinel/bin/cortex-gateway` deployen
  - `sentinel-gateway` restart ausfuehren
  - Smoke-Checks gegen `/health` und `/control/traffic-stats`
  - Gateway-Journal auf offensichtliche Startfehler pruefen
- Welche ACs muessen hier passen:
  - AC-1: Deploy trifft den tatsaechlich von systemd gestarteten Binary-Pfad.
  - AC-2: Service ist nach Restart `active`.
  - AC-3: `/health` liefert OK.
  - AC-4: `/control/traffic-stats` enthaelt `agent_runtime_model_policy`.
  - AC-5: Journal zeigt keinen unmittelbaren Panic-/Fatal-Startfehler.
- Wie wird bewiesen:
  - `ssh ubuntu@10.0.0.240 "systemctl cat sentinel-gateway ..."`
  - `GOOS=linux GOARCH=amd64 go build -o cortex-gateway ./cmd/cortex-gateway/`
  - `scp`, `sudo cp`, `sudo systemctl restart sentinel-gateway`
  - `curl -s localhost:8080/health`
  - `curl -s localhost:8081/control/traffic-stats`
  - `journalctl _PID=$(pgrep cortex-gate) --since '2 min ago' --no-pager`
- Erwartete Dateien:
  - lokales Build-Artefakt `cortex-gateway` untracked/ignored
  - `test-314-verification.md`
  - `PROGRESS.md`
- Risiken:
  - VM kann busy sein; falls Restart fehlschlaegt, sofort Journal lesen und Service nicht kaputt stehen lassen.

### Outcome

- `ExecStart=/opt/sentinel/bin/cortex-gateway` wurde verifiziert.
- Linux/amd64 Gateway-Binary wurde lokal gebaut und auf die VM kopiert.
- Altes Binary wurde vor Austausch gesichert.
- Nach `Text file busy` wurde der Service kontrolliert gestoppt, das Binary ersetzt und wieder gestartet.
- Gateway-Service ist aktiv, Health ist OK, neues Traffic-Stats-Feld ist live sichtbar.

### Evidence

- `test-314-verification.md` enthaelt Task-6 Command/Output-Evidence.
- AC-1 PASS: systemd startet `/opt/sentinel/bin/cortex-gateway`.
- AC-2 PASS: `systemctl is-active sentinel-gateway` liefert `active`.
- AC-3 PASS: `/health` liefert `{"status":"ok",...}`.
- AC-4 PASS: `/control/traffic-stats` enthaelt `"agent_runtime_model_policy": "haiku"`.
- AC-5 PASS: PID-basiertes Journal seit Restart zeigt Startup ohne Panic/Fatal.

## Task 7 - Phase 8: AC-Matrix und Live-Verifikation

### Pre-task self-check

- Was muss getan werden:
  - alle 6 GitHub-ACs einzeln mit VM- oder Repo-Evidence belegen
  - kontrollierten `agent_runtime`-Forward ueber `/internal/llm` provozieren
  - externe `/v1/messages`-Compatibility gegen interne Policy trennen
  - `/control/traffic-stats`, `/control/traffic-responses` und PID-Journal fuer Observability pruefen
  - Daemon-Code auf fehlendes hartes `AGENT_MODEL_HAIKU`-Pinning pruefen
  - Panic/Drift/Secret-Grep ausfuehren
- Welche ACs muessen hier passen:
  - AC-1: Interne Agent-Runtime-Requests bekommen effektiv `haiku`.
  - AC-2: Der Daemon pinnt kein Agent-Haiku-Modell.
  - AC-3: VM zeigt mindestens einen echten Agent-Runtime-Forward mit `effective_model=haiku`.
  - AC-4: `/v1/messages` bleibt externer MITM-/Anthropic-Compatibility-Pfad und wird nicht von Agent-Default-Policy erfasst.
  - AC-5: Observability zeigt Request-Klasse, Provider, Policy-Source und effektives Modell ohne Secrets.
  - AC-6: Tests und VM-Smoke belegen die Trennung von internem Runtime-Default und externem Compatibility-Pfad.
- Wie wird bewiesen:
  - VM-`curl` gegen `127.0.0.1:8080/internal/llm`
  - VM-`curl` gegen `127.0.0.1:8080/v1/messages`
  - VM-`curl` gegen `127.0.0.1:8081/control/traffic-stats` und `/control/traffic-responses`
  - PID-basiertes Gateway-Journal
  - `rg` in Daemon-Quellen
  - Panic/Drift/Secret-Grep
- Erwartete Dateien:
  - `test-314-verification.md`
  - `PROGRESS.md`
- Risiken:
  - Claude-Code-Quota/Circuit-Breaker kann den echten Forward blockieren; dann AC-3 bleibt BLOCKED statt durch Tests ersetzt zu werden.

### Outcome

- Alle 6 GitHub-ACs wurden mit Repo- und VM-Evidence belegt.
- Kontrollierter Agent-Runtime-Forward ueber `/internal/llm` lieferte HTTP `200`, Provider `claude-code`, Model `haiku`.
- Externer `/v1/messages`-Pfad wurde mit Dummy-Key getestet und blieb `external_compat`/`anthropic-direct` ohne Agent-Runtime-Policy.
- Observability-Felder sind in `traffic-stats`, `traffic-responses` und Journal sichtbar.
- Secret-Grep auf Stats/Responses/Journal fand keine Token/API-Key-Werte.
- Panic/Drift-Grep blieb leer.

### Evidence

- `test-314-verification.md` enthaelt Task-7 Command/Output-Evidence.
- AC-1 PASS: `/internal/llm` Response enthaelt `"model":"haiku"` und `"provider":"claude-code"`.
- AC-2 PASS: `rg "AGENT_MODEL_HAIKU"` ohne Treffer; `llm_bridge.rs` zeigt `model: String::new()`.
- AC-3 PASS: `traffic-responses` fuer `49c25b39-466d-4c48-9bc0-04fac9e9b360` zeigt `request_class=agent_runtime`, `model=haiku`, `policy_source=agent_runtime_policy`.
- AC-4 PASS: `/v1/messages` Dummy-Key-Test zeigt im Journal `provider=anthropic-direct`, `request_class=external_compat`, `effective_model=claude-opus-4-6`, `policy_source=request_override`.
- AC-5 PASS: Observability- und Secret-Grep erfolgreich.
- AC-6 PASS: Go-Tests plus VM-Smoke decken beide Pfade ab.

## Task 8 - Dokumentation, PR- und Close-Sequenz

### Pre-task self-check

- Was muss getan werden:
  - `CHANGELOG.md` aktualisieren
  - PR-Pflichtsektionen vorbereiten
  - keine Issue-Schliessung vor PR/Merge/CI
  - Evidence-Datei aktuell halten
- Welche ACs muessen hier passen:
  - AC-1: CHANGELOG enthaelt #314.
  - AC-2: PR-Body kann alle 7 Pflichtsektionen fuellen.
  - AC-3: Close-Sequenz bleibt `status:verified` vor `gh issue close`.
- Wie wird bewiesen:
  - `rg "#314|Gateway Model Policy" CHANGELOG.md`
  - PR-Erstellung nach Task-Commit mit Pflichtsektionen
  - Issue-Label/Close erst nach Merge
- Erwartete Dateien:
  - `CHANGELOG.md`
  - `PROGRESS.md`
  - `test-314-verification.md`
- Risiken:
  - PR/Close vor finaler Plan-Verifikation waere gegen `$start`; deshalb nur vorbereiten, dann Task 9 final pruefen.

### Outcome

- `CHANGELOG.md` hat einen Unreleased-Eintrag fuer #314.
- PR-Body-Sektionierung ist vorbereitet.
- Close-Reihenfolge ist dokumentiert und bleibt bis nach PR/Merge gesperrt.

### Evidence

- `test-314-verification.md` enthaelt Task-8 Command/Output-Evidence.
- AC-1 PASS: CHANGELOG enthaelt #314-Eintrag.
- AC-2 PASS: PR-Pflichtsektionen werden beim `gh pr create` verwendet.
- AC-3 PASS: `status:verified`/Close bleibt nach finaler Verifikation und Merge.

## Task 9 - Plan-Verifikation

### Pre-task self-check

- Was muss getan werden:
  - `codex-plan314.md` gegen den umgesetzten Stand pruefen
  - Issue-ACs, Benchmarks, Deploy, CHANGELOG und Evidence vollstaendig abgleichen
  - finalen Git-Status pruefen
  - keine offenen lokalen Code-/Doku-Aenderungen ausser Task-9-Progress/Evidence hinterlassen
- Welche ACs muessen hier passen:
  - AC-1: alle Plan-Slices sind erledigt oder explizit nicht relevant.
  - AC-2: alle 6 GitHub-ACs sind PASS.
  - AC-3: Benchmarks liegen unter Zielwert.
  - AC-4: VM-Deploy ist live.
  - AC-5: CHANGELOG und PR-Vorbereitung sind vorhanden.
  - AC-6: Git-Status ist vor Push sauber nach Task-9-Commit.
- Wie wird bewiesen:
  - `git status --short`
  - `go test`, `go build`
  - `gh issue view 314`
  - VM `health`, `traffic-stats`, `traffic-responses`
  - Plan-/Evidence-Inspection
- Erwartete Dateien:
  - `PROGRESS.md`
  - `test-314-verification.md`
- Risiken:
  - Nach Task 9 bleiben Push/PR/CI/Merge/Close als GitHub-Sequenz; nicht vor lokal sauberem Stand starten.

### Outcome

- Alle Planphasen wurden gegen Code, Tests, Benchmarks, Deploy, VM-Evidence und CHANGELOG abgeglichen.
- Keine Planabweichung bleibt offen.
- Issue #314 bleibt vor PR/Merge korrekt offen und nicht verfrueht `status:verified`.
- Lokaler Abschluss ist bereit fuer Push/PR/CI/Merge/Close-Sequenz.

### Evidence

- `test-314-verification.md` enthaelt Task-9 Command/Output-Evidence.
- AC-1 PASS: Plan-Slices 1-9 erledigt.
- AC-2 PASS: GitHub-ACs 1-6 sind in Task 7 PASS.
- AC-3 PASS: Benchmark-Zielwerte in Task 5 PASS.
- AC-4 PASS: VM-Deploy in Task 6/7 live belegt.
- AC-5 PASS: CHANGELOG und PR-Vorbereitung in Task 8 PASS.
- AC-6 PASS: finaler Test-/Build-/VM-Smoke ist gruen.

## Task 5 - Phase 5: Benchmarks

### Pre-task self-check

- Was muss getan werden:
  - Benchmark-Harness fuer `ClassifyRequest`, `ResolveModelPolicy` und `ResponseLogBuffer.Add` ergaenzen oder vorhandene Benchmarks erweitern
  - Benchmarks mit Zielwerten aus dem Plan ausfuehren
  - System-Monitoring parallel dokumentieren: CPU, RAM, Disk/IOPS soweit lokal sinnvoll messbar
  - Benchmark-Evidence in `test-314-verification.md` festhalten
- Welche ACs muessen hier passen:
  - AC-1: `ClassifyRequest` bleibt unter `1us/op`.
  - AC-2: `ResolveModelPolicy` bleibt unter `1us/op`.
  - AC-3: `ResponseLogBuffer.Add` bleibt unter `10us/op`.
  - AC-4: Benchmarks laufen mit `allocs/op` sichtbar.
  - AC-5: System-Monitoring zeigt keine auffaellige lokale Lastspitze.
- Wie wird bewiesen:
  - `go test ./cmd/cortex-gateway/internal/proxy -bench 'Benchmark(ClassifyRequest|ResolveModelPolicy|ResponseLogBufferAdd)' -benchmem -run '^$'`
  - Sidecar-Monitoring per `ps`, `vmstat`, optional `iostat` falls vorhanden
- Erwartete Dateien:
  - `cmd/cortex-gateway/internal/proxy/bench_test.go`
  - `test-314-verification.md`
  - `PROGRESS.md`
- Risiken:
  - Go-Benchmarkzeiten variieren lokal; Zielwerte muessen mit ausreichender Marge gegen ns/op liegen, nicht aus Einzellaeufen ueberinterpretiert werden.

### Outcome

- `bench_test.go` enthaelt jetzt Microbenchmarks fuer `ClassifyRequest`, `ResolveModelPolicy` und `ResponseLogBuffer.Add`.
- Alle drei Benchmarks liegen deutlich unter den Zielwerten.
- `benchmem` zeigt Allokationen explizit.
- Lokales System-Monitoring wurde parallel bzw. ergaenzend per `/usr/bin/time -v`, `vmstat` und `iostat` dokumentiert.

### Evidence

- `test-314-verification.md` enthaelt Task-5 Command/Output-Evidence.
- AC-1 PASS: `ClassifyRequest` `494.9 ns/op` < `1us/op`.
- AC-2 PASS: `ResolveModelPolicy` `38.43 ns/op` < `1us/op`.
- AC-3 PASS: `ResponseLogBuffer.Add` `3831 ns/op` < `10us/op`.
- AC-4 PASS: `benchmem` zeigt `B/op` und `allocs/op`.
- AC-5 PASS: `/usr/bin/time -v`, `vmstat` und `iostat` erfasst; keine Benchmark-Blockade oder Swap-Fehler.

## Task 4 - Phase 4: Go-Tests

### Pre-task self-check

- Was muss getan werden:
  - gezielte Unit-/Regressionstests fuer Request-Klassifikation und Policy-Resolver ergaenzen
  - Tests fuer `/v1/messages` als externe Compatibility-Klasse absichern
  - Tests fuer Control-Config-Default und Validation von `agent_runtime_model_policy` ergaenzen
  - Tests fuer Response-Log-Felder und Ringbuffer-Chronologie ergaenzen
  - komplette betroffene Gateway-Testpakete laufen lassen
- Welche ACs muessen hier passen:
  - AC-1: Agent-Runtime wird nur fuer positive numerische Agent-ID nach Ausschluss von Platform-/Service-Pfaden klassifiziert.
  - AC-2: `/v1/messages` bleibt `external_compat` und bekommt keine Agent-Policy.
  - AC-3: leeres Modell wird fuer Agent-Runtime zu `haiku`, explizites Modell gewinnt.
  - AC-4: ungueltige Provider-/Policy-Kombination failt deterministisch.
  - AC-5: Control-Config setzt standardmaessig `haiku` und akzeptiert nur erlaubte Werte.
  - AC-6: ResponseLogBuffer liefert chronologische Eintraege nach Ring-Overwrite und `LastByClass` findet den letzten Runtime-Eintrag.
- Wie wird bewiesen:
  - `go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control`
  - `go build ./cmd/cortex-gateway`
- Erwartete Dateien:
  - `cmd/cortex-gateway/internal/proxy/policy_test.go`
  - `cmd/cortex-gateway/internal/proxy/response_log_test.go`
  - vorhandene Tests in `cmd/cortex-gateway/internal/control`
- Risiken:
  - bestehende Tests koennen alte ResponseLogBuffer-Signatur erwarten; nicht per Compatibility-Wrapper kaschieren, sondern Tests auf neue Struktur aktualisieren.

### Outcome

- `policy_test.go` deckt strikte Klassifikation und Resolver-Reihenfolge ab.
- `pipeline_test.go` deckt Agent-Runtime-Haiku-Anwendung und externe `/v1/messages`-Trennung ab.
- `plane_test.go` deckt Default, erlaubte Werte und invaliden `agent_runtime_model_policy` ab.
- `response_log_test.go` deckt Ringbuffer-Overwrite, chronologische Ausgabe und `LastByClass` ab.

### Evidence

- `test-314-verification.md` enthaelt Task-4 Command/Output-Evidence.
- AC-1 PASS: Tests fuer numerische Agent-ID und Ausschluss von Platform-/Service-Pfaden.
- AC-2 PASS: Pipeline-Test bestaetigt `/v1/messages` als `external_compat`.
- AC-3 PASS: Policy- und Pipeline-Tests bestaetigen Haiku fuer Agent-Runtime und explizites Modell als Override.
- AC-4 PASS: Policy-Test prueft unsupported Provider und unknown Policy als Fehler.
- AC-5 PASS: Control-Test prueft Default `haiku`, leeres Disable und Rejects.
- AC-6 PASS: ResponseLogBuffer-Test prueft Ring-Overwrite und `LastByClass`.
- Command PASS: `go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control`.
- Command PASS: `go build ./cmd/cortex-gateway`.

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

### Outcome

- `cmd/cortex-gateway/internal/proxy/policy.go` neu eingefuehrt.
- `LLMRequest` traegt jetzt `RequestClass`, `EffectiveModel` und `PolicySource` als interne Felder.
- `control.ConfigSnapshot` und `control.Config` enthalten `AgentRuntimeModelPolicy`.
- Default fuer `agent_runtime_model_policy` ist `haiku`.
- `pipeline.ServeHTTP` klassifiziert Requests frueh, wendet die Modellpolicy aber erst im echten Forward-Pfad vor Streaming/Provider.Send an.
- Die erste Testiteration zeigte eine Pre-Synthesis-Blockade; diese wurde durch Verschieben der Policy-Anwendung behoben.
- Testprovider `mock` mappt `haiku` fuer bestehende Gateway-Tests; unbekannte Provider bleiben fail-closed.

### Evidence

- `test-314-verification.md` enthaelt Task-2 Command/Output-Evidence.
- AC-1 PASS: `RequestClassExternalCompat`, `RequestClassAgentRuntime`, `RequestClassPlatformControlplane`, `RequestClassServiceInternal`, `RequestClassInternalOther` in `policy.go`.
- AC-2 PASS: `ClassifyRequest()` prueft `/v1/messages`, `platform_analysis`, `request_type`, Service-Identitaeten und erst danach numerische Agent-ID.
- AC-3 PASS: `ResolveModelPolicy()` setzt Haiku nur fuer `RequestClassAgentRuntime` ohne explizites Modell.
- AC-4 PASS: explizites Request-Modell gewinnt mit `PolicySourceRequestOverride`.
- AC-5 PASS: `/v1/messages` bleibt `PreferredProvider=anthropic-direct` und `RequestClassExternalCompat`.
- AC-6 PASS: nicht unterstuetzte Provider liefern `model policy rejected`, kein stiller Opus-Fallback.
- Command PASS: `go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control`.
- Command PASS: `go build ./cmd/cortex-gateway`.

## Task 3 - Phase 3: Observability und Response Log

### Pre-task self-check

- Was muss getan werden:
  - `traffic-stats` um Agent-Runtime-Policy und letzten effektiven Runtime-Forward erweitern
  - `ResponseLogEntry` um `request_class`, `model`, `policy_source`, `agent_id`, `agent_name` erweitern
  - Journal-Logs fuer Success-, Stream- und Provider-Error-Pfade mit Request-Klasse und Policy-Feldern anreichern
  - pruefen, ob `ResponseLogBuffer` wegen Hot-Path-Kopieren auf bounded circular buffer umgebaut werden muss
- Welche ACs muessen hier passen:
  - AC-1: Traffic-Stats zeigen `agent_runtime_model_policy`, `last_agent_runtime_effective_model`, `last_agent_runtime_policy_source`.
  - AC-2: Traffic-Responses zeigen redigiert `request_class`, `provider`, `model`, `policy_source`, `agent_id`, `agent_name`.
  - AC-3: Success-, Stream- und Error-Logs enthalten die neuen Felder.
  - AC-4: keine Header, Tokens oder Secrets werden in Response-Log/Stats aufgenommen.
  - AC-5: Response-Log-Append bleibt bounded und ohne O(n)-Kopie im steady state, falls Benchmarks das erzwingen.
- Wie wird bewiesen:
  - Go-Tests/Build nach Edit
  - strukturelle Inspection fuer Secret-Freiheit
  - Benchmarks folgen in Task 5
- Erwartete Dateien:
  - `cmd/cortex-gateway/internal/proxy/response_log.go`
  - `cmd/cortex-gateway/internal/proxy/pipeline.go`
  - `cmd/cortex-gateway/main.go`
  - Tests in `cmd/cortex-gateway/internal/proxy` und ggf. `cmd/cortex-gateway/internal/control`
- Risiken:
  - `traffic-stats` lebt in `main.go` und muss ohne zusaetzliche globale State-Duplikation an Gateway-Daten kommen.
  - Error-Logs muessen AC-4 belegen koennen, auch wenn `/v1/messages` mit Dummy-Key fehlschlaegt.

### Outcome

- `traffic-stats` liest die Control-Konfiguration einmal pro Request und exportiert `agent_runtime_model_policy`.
- Wenn ein Agent-Runtime-Forward im Response-Log existiert, exportiert `traffic-stats` zusaetzlich `last_agent_runtime_effective_model`, `last_agent_runtime_policy_source` und `last_agent_runtime_provider`.
- `ResponseLogEntry` wurde um `request_class`, `model`, `policy_source`, `agent_id` und `agent_name` erweitert; Header, Tokens und Secrets werden nicht gespeichert.
- Provider-Success-, Provider-Error-, Stream-Success- und Stream-Error-Logs tragen jetzt Request-Klasse, effektives Modell, Policy-Source und Agent-Metadaten.
- `ResponseLogBuffer` wurde auf einen bounded circular buffer umgestellt; `Add()` ueberschreibt bei vollem Buffer ohne Slice-Restkopie.

### Evidence

- `test-314-verification.md` enthaelt Task-3 Command/Output-Evidence.
- AC-1 PASS: `main.go` exportiert `agent_runtime_model_policy` und letzte Agent-Runtime-Policy-Felder.
- AC-2 PASS: `ResponseLogEntry` enthaelt redigierte Policy-/Agent-Felder.
- AC-3 PASS: Success-, Stream- und Error-Logs enthalten Request-Klasse, effektives Modell und Policy-Source.
- AC-4 PASS: Neue Response-Log-/Stats-Felder enthalten keine Header, API-Keys oder Tokenwerte.
- AC-5 PASS: `ResponseLogBuffer.Add()` bleibt bounded und hat im steady state keine O(n)-Kopie.
- Command PASS: `go test ./cmd/cortex-gateway/internal/proxy ./cmd/cortex-gateway/internal/control`.
- Command PASS: `go build ./cmd/cortex-gateway`.
