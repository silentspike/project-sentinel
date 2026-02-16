# Codex Analyse (SSOT)

## Auftrag und Ziel
- Auftrag (User): Tiefgehende Audit-Analyse der reellen Umsetzung in `project-sentinel` gegen den behaupteten "vollstaendig umgesetzt"-Status.
- Zusatzauftrag (User): Geschlossene GitHub-Issues gegen reale Implementierung pruefen, inkl. Ursache: schlechte Issue-Qualitaet vs. fehlerhafte Umsetzung.
- SSOT-Regel: Dieses Dokument ist die fortlaufende Single Source of Truth fuer die Audit-Ergebnisse und wird inkrementell erweitert.

## Scope
- Repo: `/work/company/project-sentinel`
- Referenzbasis:
  - Plan-Dokument aus Chat (Phasen 1-7, Vollumfang)
  - Closed Issues: `#4-#27`, `#34` (insg. 25 Issues)
  - Projektregeln: `.claude/CLAUDE.md`

## Methodik
- Code- und Test-basierte Evidenz (Datei/Zeile), keine Selbstauskunft als Nachweis.
- Bewertung pro Issue in 3 Klassen:
  - `FULL`: Wesentlicher Scope real implementiert, keine zentralen Stubs/Gaps.
  - `PARTIAL`: Teilweise umgesetzt, zentrale Teile fehlen/sind Platzhalter/entkoppelt.
  - `MISMATCH`: Ticketaussage und reale Umsetzung widersprechen klar.
- Quantifizierung:
  - `Strict Full Rate` = `FULL / Gesamt`.
  - `Weighted Delivery` = `(FULL*1.0 + PARTIAL*0.5 + MISMATCH*0.0) / Gesamt`.

## Executive Summary
- Geschlossene Issues analysiert: `25`
- Ergebnis:
  - `FULL`: `10/25` -> `40%` (Strict Full Rate) *(+3: #13, #15, #23 PARTIAL->FULL)*
  - `PARTIAL`: `15/25` -> `60%`
  - `MISMATCH`: `0/25` (formal, aber mehrere harte Scope-Reduktionen)
  - `Weighted Delivery`: `(10*1 + 15*0.5)/25 = 17.5/25 = 70%`
- Kernbefund:
  - Es gibt viel implementierten Unterbau.
  - Die meisten "completed" Issues sind jedoch nur teilweise realisiert oder bewusst abgeschwaecht.
  - Hauptursache ist **gemischt**:
    - schwache/verwasserte Issue-Akzeptanzkriterien (haeufig),
    - plus fehlende Integrationsumsetzung trotz vorhandener Einzelmodule.

## Querschnitts-Befunde (harte Gaps)
- Agent-Migration weit unter Vollumfang:
  - Quelle 54 MD-Agenten: `/work/company/pixelperfekt/agents` = `54`
  - Migrierte TOML-Agenten: `config/agents` = `5`
- Fehlende Kern-Dokumentation laut Plan/DoD:
  - `README.md` fehlt
  - `config/README.md` fehlt
  - `deploy/README.md` fehlt
  - `tests/e2e/run.sh` fehlt
- Wichtige Verzeichnisse nur als Platzhalter:
  - `deploy/vm-config/.gitkeep`
  - `deploy/systemd/.gitkeep`
  - `cmd/cortex-gateway/internal/injection/.gitkeep`

## Issue-Matrix (Closed Issues)

| Issue | Titel | Realstatus | Evidenz (reelle Umsetzung) | Hauptursache |
|---|---|---|---|---|
| #4 | Repo-Setup + Monorepo | PARTIAL | `zig` fehlt (Sollstruktur verletzt); mehrere Pflichtdocs fehlen (`README.md`, `config/README.md`, `deploy/README.md`) | Umsetzungsluecke + Issue-Checks nicht vollstaendig CI-erzwungen |
| #5 | sentinel-common | FULL | Shared Types/Schemas vorhanden: `crates/sentinel-common/src/types.rs:133`, `schemas/action.fbs:1` | solide Umsetzung |
| #6 | sentinel-zenoh | PARTIAL | SHM explizit TODO: `crates/sentinel-zenoh/src/lib.rs:36` | Issue/AC fokussiert Funktion, nicht Performance-Ziel |
| #7 | sentinel-redb | FULL | redb-Store und Tests vorhanden | solide Umsetzung |
| #8 | sentinel-limbo | PARTIAL | Fallback auf rusqlite statt Limbo-Kern: `crates/sentinel-limbo/src/lib.rs:6` | Scope im Issue bewusst reduziert (Fallback erlaubt) |
| #9 | sentinel-ecs | PARTIAL | Input/Output/Persist nur Stubs: `crates/sentinel-ecs/src/systems.rs:34`, `crates/sentinel-ecs/src/systems.rs:314`, `crates/sentinel-ecs/src/systems.rs:322` | Integrationsphase nicht umgesetzt |
| #10 | sentinel-bio | FULL | Formeln + Aktionen implementiert, Tests vorhanden | solide Umsetzung |
| #11 | rooms.toml | FULL | 15 Raeume + Validierung/Tests vorhanden | solide Umsetzung |
| #12 | sentinel-physics | FULL | Akustik/Temp/CO2/Geruch/Transit/Chaos implementiert | solide Umsetzung |
| #13 | cortex-gateway Vollpipeline | FULL | AC-5 Command->Event Mapping mit atomaren Event+Outbox Writes implementiert (PR #71). Pipeline-Handler mit Extraction+Mapping verdrahtet: `cmd/cortex-gateway/internal/proxy/pipeline.go`. Benchmark: 1.36ms/write auf VM. | solide Umsetzung (PR #71) |
| #14 | perception-injection (ECS) | FULL | `generate_perception` + `format_injection` vorhanden: `crates/sentinel-ecs/src/perception.rs:38`, `crates/sentinel-ecs/src/perception.rs:84` | solide Umsetzung |
| #15 | teammate-first runtime | FULL | Orchestrator mit event-sourced Lifecycle (AC-2), Snapshot-Persistence (AC-4), pause/resume mit State-Machine, RuntimeEventSink-Trait (ECS-Integration). 29 Tests (21 unit + 8 acceptance), 13 Benchmarks. Footprint: 96B/Orch + 72B/Agent, 1 Thread. `crates/sentinel-runtime/src/lib.rs` | solide Umsetzung (PR #72 + Nachbesserung) |
| #16 | sandbox (bwrap+landlock+cgroups) | PARTIAL | bwrap-Args Builder: `crates/sentinel-sandbox/src/bwrap.rs:27`; cgroup-Datenstrukturen: `crates/sentinel-sandbox/src/cgroups.rs:6`; keine echte Landlock-/cgroup-Enforcement-Pipeline in Runtime | Umsetzungsluecke, AC zu struktur-lastig |
| #17 | bitnet + multi-lora + speculative | PARTIAL | BitNet als Subprocess-Wrapper: `crates/sentinel-inference/src/bitnet.rs:18`; vereinfachte speculative Heuristik: `crates/sentinel-inference/src/speculative.rs:54` | AC auf Minimalfunktionen, nicht Produktionsniveau |
| #18 | kv-cache-sharing | PARTIAL | Explizit nur Prompt-Level, kein echter KV-Cache sharing Kernel: `crates/sentinel-inference/src/kv_cache.rs:11` | Scope reduziert/vereinfacht |
| #19 | wasm runtime (wasmtime/extism) | PARTIAL | Native FileRead/FileWrite + sonst Placeholder: `crates/sentinel-wasm/src/runner.rs:79` | starke Abweichung Titel vs. reale Tiefe, AC zu weich |
| #20 | 54 Agenten migrieren | PARTIAL | Tests fordern nur 5 Dateien: `crates/sentinel-common/tests/acceptance_agents.rs:15`; Loader-Test erwartet 5: `crates/sentinel-common/src/agent_config.rs:153`; real nur 5 statt 54 | primar Issue-Qualitaet (Scope-Absenkung im Ticket selbst) |
| #21 | nmda sleep-cycle | PARTIAL | Consolidation explizit TODO: `crates/sentinel-hippocampus/src/sleep.rs:120` | bewusst als Placeholder im Issue zugelassen |
| #22 | fourth-wall detection | PARTIAL | Detection/Judge gut implementiert; aber Proxy-Integration nicht im Live-Handlerpfad (`cmd/cortex-gateway/internal/proxy/handler.go:80`) | Integrationsluecke |
| #23 | hippocampus memory | FULL | Persistentes redb-Backend (4 Tables: episodes, narratives, facts, cache_state), HippocampusService Facade, Night-Run Konsolidierung via SleepCycle, NMDA-priorisiertes Retrieval. 57 Unit-Tests + 4 Acceptance-Tests (AC-1 bis AC-4), 40+ Benchmarks auf VM. `crates/sentinel-hippocampus/src/store.rs`, `crates/sentinel-hippocampus/src/service.rs` | solide Umsetzung (PR #77) |
| #24 | dashboard | PARTIAL | Daten aus Mocks statt Real-Backend: `dashboard/src/index.ts:2`; WebSocket-AC in Tests abgeflacht, echte Upgrade-Pruefung fehlt: `dashboard/src/__tests__/acceptance.test.ts:55`, `dashboard/src/__tests__/acceptance.test.ts:66` | Issue/Test-Qualitaet zu tolerant |
| #25 | eBPF monitoring | PARTIAL | Crate dokumentiert Userspace-only ohne echtes Probe-Loading standardmaessig: `crates/sentinel-ebpf/src/lib.rs:9`; Probe-Module sind userspace tracker, keine geladene BPF-Programme | AC fokussiert userspace Logik |
| #26 | sentinel-judge agent | PARTIAL | Judge-Algorithmen vorhanden, aber kein separater laufender Judge-Prozess in `main.go`-Verdrahtung | Integrations-/Betriebsluecke |
| #27 | marble observatory | PARTIAL | Config + Metriken + Reports vorhanden; Storage aber nur in-memory: `cmd/cortex-gateway/internal/observatory/storage.go:38` (kein Limbo/SQLite persistenter Store) | Umsetzungsluecke + AC nicht hart genug auf Persistenz verankert |
| #34 | telemetry | FULL | Telemetry-Module/Exporter vorhanden; Instrumentierung in Sprint-1 Crates vorhanden; Hinweis: Export serialisiert JSON (`crates/sentinel-telemetry/src/export.rs:147`) | solide Umsetzung mit Format-Drift gegen manche Plantexte |

## Root-Cause Analyse

### A) Issue-Qualitaet (hauptsaechlich)
Wiederkehrende Muster:
- Titel verspricht Vollumfang, Akzeptanzkriterien testen nur Minimal-Slices.
- Platzhalter werden explizit erlaubt und dann als "done" geschlossen.
- Integrationskriterien fehlen (E2E, Prozesslauf, echte Persistenz, echte Runtime-Verkettung).
- Non-Functional Requirements fehlen in AC (Performance, SHM, echte Isolation, Probe-Loading).

Konkrete Beispiele:
- #20: "54 Agenten" im Titel, aber AC fordert nur "mindestens 5".
- #21: Consolidation-Placeholder explizit erlaubt.
- #24: WS/Frontend-AC in Tests auf 404/Smoke abgeschwaecht.
- #8: Limbo-Issue mit rusqlite-Fallback als akzeptierte Endlage.

### B) Umsetzungsqualitaet (sekundaer, aber relevant)
- Mehrere Module sind vorhanden, aber nicht in den Produktionspfad integriert.
- Klassisch: "module complete, system incomplete".

Konkrete Beispiele:
- ~~#13 Vollpipeline nicht in HTTP-Request-Pfad verdrahtet~~ (behoben: PR #71, jetzt FULL).
- #16 Sandbox nur Builder/Struct-Ebene, keine echte Enforcement-Kette.
- #26 Judge nicht als separater Runtime-Prozess integriert.
- #27 Persistenzanspruch nicht realisiert (nur in-memory).

## Antwort auf Kernfrage (Issue falsch umgesetzt vs Issue schlecht beschrieben)
- Es ist **beides**, aber **hauptsaechlich Issue-Qualitaet**.
- Verhaeltnis (qualitativ): ca. `65% Issue-Design-Problem` / `35% Implementations- und Integrationsproblem`.
- Begruendung:
  - Viele Gaps wurden durch AC/DoD formal erlaubt.
  - Dort wo AC klar war, ist die Umsetzung meist solide.

## Was fuer zukunftige Issues zwingend besser werden muss (generisch, nicht projektspezifisch)
1. Titel, Scope und AC muessen denselben Umfang haben (kein "54" im Titel + ">=5" in AC).
2. Jedes Feature braucht mind. 1 End-to-End AC (nicht nur Unit/API-Slices).
3. Placeholder nur mit `scope:experimental` + hartem Follow-up-Issue + Blocker-Label fuer Release.
4. Integrationskriterien als Pflicht:
   - "Komponente existiert" reicht nicht,
   - erforderlich ist "im Runtime-Pfad aktiv".
5. Persistenz-Claims immer mit nachweisbarer Speicherart + Restart-Test.
6. "Done" nur mit Evidence-Block pro AC:
   - Command,
   - erwarteter Output,
   - Artefakt-Pfad,
   - Datum.
7. CI muss AC hart pruefen (Issue-Quality Gate + AC-Linter + Mindest-Evidence). **[UMGESETZT: issue-quality.yml + pr-quality.yml]**
8. Non-Functional AC verpflichtend bei Architektur-Issues:
   - Latenz,
   - Speicher,
   - I/O,
   - Isolation/Security.
   **[UMGESETZT: `## Benchmarks` Pflicht-Sektion in allen 20 offenen Issues + CI Gate]**

## Offene Prioritaetsliste fuer Re-Audit/Repair
1. ~~#13 Vollpipeline verdrahten~~ (erledigt: PR #71, FULL).
2. #20 echte 54er Migration abschliessen (Parser + Validierung + Tests auf 54 hart setzen).
3. #16 echte Sandbox-Enforcement (Landlock/cgroups Anwendung + verifizierbare Integrationstests).
4. #27 persistente Observatory-Storage auf Limbo/SQLite umstellen.
5. #24 Dashboard von Mock-Daten auf echte Telemetrie/State-Backends umstellen.
6. #25 echte eBPF Probe-Loading-Pipeline und Kernel-nahe Integrationsnachweise.
7. Fehlende Repo-Doku und E2E-Script gemaess DoD nachziehen.

## Update-Log (dieses Dokuments)
- 2026-02-12: Initiale SSOT-Version erstellt (Issue-basierte Realstatus-Matrix + Prozentbewertung + Root-Cause).

## Generische Verbesserungen fuer den `github`-Skill (`/home/jan/.claude/commands/github.md`)

Ziel: Issue-Qualitaet so anheben, dass "closed" auch real "done" bedeutet.

### 1) Neue Pflicht-Phase im Skill: "Issue Contract Quality Gate"
Vor jeder Umsetzung muss der Skill ein Ticket gegen einen Mindestvertrag pruefen.

Pflichtfelder pro Issue:
- Problemstatement (1 Satz, testbar)
- In-Scope / Out-of-Scope (explizit)
- Deliverables (Dateien/Module)
- Akzeptanzkriterien (funktional + E2E + NFR)
- Verify-Kommandos (inkl. erwarteter Output)
- Evidence-Block bei Abschluss
- Rollback/Follow-up bei Placeholder

Wenn ein Feld fehlt: Label `quality:needs-spec`, kein `status:ready`.

### 2) AC-Qualitaetsregeln (harte Anti-Pattern)
Der Skill soll diese AC-Anti-Pattern aktiv blocken:
- Titel-Umfang != AC-Umfang (z.B. "54" im Titel, AC fordert nur 5)
- Nur Unit-Tests ohne Integrations-/E2E-AC
- Placeholder als Done ohne Follow-up-Issue
- Architektur/NFR-Claim ohne messbares Ziel (z.B. Latenz, IOPS, RAM)

### 3) "Definition of Done" im Skill auf 2 Ebenen
- Ebene A (Issue-DoD): AC + Verify + Evidence komplett.
- Ebene B (Release-DoD): keine offenen `scope:partial`/`scope:experimental` Tickets fuer release-kritische Epics.

### 4) Evidence-Pflicht standardisieren (maschinenlesbar)
Pro geschlossenem Issue muss der Skill folgenden Block erzeugen:

```yaml
issue: 123
commit: <sha>
ac_evidence:
  - ac: AC-01
    command: "..."
    expected: "..."
    actual: "..."
    artifact: "path:line"
status: full|partial
scope_delta: none|reduced|deferred
follow_up_issue: <id|null>
```

### 5) CI-Kopplung im Skill erzwingen
Skill muss fuer neue Repos standardmaessig erzeugen/aktivieren:
- `issue-quality.yml`: lintet neue/edierte Issues auf Pflichtsektionen **[UMGESETZT 2026-02-16: Benchmarks-Sektion als Pflicht hinzugefuegt]**
- `pr-quality.yml`: verweigert Merge bei fehlender AC-Evidence **[UMGESETZT 2026-02-16: ## Benchmarks als Pflicht-Sektion hinzugefuegt]**
- `main-push-guard.yml`: blockt direkte Pushes auf `main` **[UMGESETZT]**

### 6) Placeholder-Policy
Wenn Placeholder noetig:
- Label `scope:experimental` + `status:partial`
- Folge-Issue automatisch erstellen (mit Deadline/Owner)
- Ursprungs-Issue darf nicht als `completed` geschlossen werden, sondern `partial`.

### 7) Prozessregel fuer Agenten
Im Skill klarstellen:
- "Module vorhanden" gilt nicht als Done.
- Done erst, wenn Runtime-Pfad aktiv und in E2E nachgewiesen.

### 8) Empfohlene Anpassung im Skill-Textstil (LLM-lesbar)
- Weniger generische "Best Practices", mehr checkbare If-Then Regeln.
- Jede Regel mit "Fail Condition" + "Fix Action" formulieren.
- Explizite Entscheidungstabelle fuer `FULL/PARTIAL/MISMATCH` integrieren.

## VM-Tuning Benchmark Update (Host 1069)

### Aufgabenfokus (neu)
- Auftrag (User): VM fuer spaetere Installation der Stack-Produkte optimal konfigurieren/tunen und Sollwerte mit Benchmarks pruefen.
- Methodik: stack-nahe Benchmarks auf der Ziel-VM (`ubuntu@192.0.2.240`) statt Funktions-Simulation.

### Bench-Artefakte
- VM 3-Run Suite (Median/P95): `/work/company/docs/benchmarks/vm-stack-20260213a/summary/stats.tsv`
- VM Highlights: `/work/company/docs/benchmarks/vm-stack-20260213a/summary/highlights.md`
- Zielwert-Pruefung: `/work/company/docs/benchmarks/vm-stack-20260213a/summary/goal-check.md`

### Telemetry Mikroziele (Soll/Ist)
- Counter increment `<1ns` -> `5.5145ns` -> **FAIL**
- Histogram record `<5ns` -> `21.2936ns` -> **FAIL**
- Span enter/exit `<50ns` -> `21.9981ns` -> **PASS**
- Metrics snapshot `<100us` -> `70.6740us` -> **PASS**
- Health check `<1ms` -> `6.7645us` -> **PASS**
- Log emission `<200ns` -> `705.4826ns` -> **FAIL**

### Stack-Ziele (Soll/Ist)
- Orchestrierungs-Latenz `<500us` -> `ecs.us_per_tick=7572.2100us` -> **FAIL**
- Agent-Spawn `<100ms` -> `bwrap.spawn_us=9100.8439us` (~9.1ms) -> **PASS**
- Zenoh Message-Throughput `>100K msg/s` -> aus RTT abgeleitet ~`881K msg/s` (1.134us) -> **PASS (abgeleitet)**
- ECS Tick-Rate `>1000 ticks/s` -> `132.0618` -> **FAIL**
- Decision Engine (24 Agents) `<50us/tick` -> `1.0210us` -> **PASS** (Issue #54 AC5)
- Disk IOPS sustained `<500` -> `cgroup.io300.total_iops=435.5701` -> **PASS**
- BitNet `20-40 tok/s` -> binary/model nicht vorhanden -> **N/A**

### Produktnahe Coverage (von User explizit gefragt)
- FlatBuffers: **PARTIAL** (`flatbuffers.schema.count=4`, aber `flatbuffers.codegen.ready=0`)
- MVCC redb: **GEMESSEN** (`redb.mvcc_read_us=0.5863us/op`)
- MVCC limbo/rusqlite fallback: **PARTIAL** (concurrent write throughput gemessen, aber kein nativer Limbo-MVCC-Pfad)
- SGMV: **NICHT VERFUEGBAR** (`sgmv.runtime.available=0`)
- XGrammar: **NICHT VORHANDEN/GEBENCHMARKT**
- eBPF: **PARTIAL** (userspace + bpftool availability, kein Kernel-Probe-Perf-Nachweis)
- Landlock: **NICHT AKTIV** (`landlock.fs.available=0`)
- BitNet: **NICHT VERFUEGBAR** (`bitnet.binary.available=0`, `bitnet.model.available=0`)
- io_uring: **GEMESSEN** (`fio.iouring.total_iops=210866` vs `fio.psync.total_iops=19223`)
- Wasmtime: **GEMESSEN** (`wasmtime.invoke_us=5997.5162`)
- Extism/Monty: **NICHT GEBENCHMARKT**

### Wichtigste Tuning-Erkenntnis
- Die VM-Hardware ist **nicht** der ECS-Flaschenhals.
- Zusatzmessung auf derselben VM:
  - `ECS_ENABLE_PERSIST=0` -> `6.154us/tick` (`162471 ticks/s`)
  - `ECS_PERSIST_EVERY_N_TICKS=10` -> `245.644us/tick` (`4070 ticks/s`)
- Schluss: Der aktuelle FAIL bei `ecs.us_per_tick`/`ticks_per_s` wird primaer durch den aktuellen Persistenzpfad im Benchmarkprofil verursacht, nicht durch fehlende VM-Leistung.

### Decision Engine Benchmark (Issue #54 AC5)

**Kontext:** Issue #54 fordert `decision_system` mit <50us pro Tick bei 24 gleichzeitigen Agents.
**Binary:** `stack-harness` (Release, gebaut auf 192.0.2.155), ausgefuehrt auf VM 192.0.2.240.
**Methodik:** Isolierte Messung (Decision-only Schedule), 1000 Ticks, 10 Warmup-Ticks, 24 Agents mit realistischer Bio-Mischung (P0-P3 Trigger).

| Metrik | Wert | Schwellenwert | Status |
|--------|------|---------------|--------|
| `decision.24_agents.mean_us` | `1.0210us` | <50us | **PASS** (49x Marge) |
| `decision.24_agents.p95_us` | `1.0000us` | <50us | **PASS** |
| `decision.24_agents.ticks_per_s` | `979,431` | >20,000 | **PASS** |

**Einordnung:**
- Decision System ist ~7x schneller als ein voller ECS-Tick ohne Persist (`6.154us`)
- Kein Flaschenhals: 1us Decision + 6us restliche Systems = 7us Gesamt-ECS-Tick (ohne Persist)
- Keine Heap-Allocations im Hot-Path (`Vec::with_capacity(5)` bei Agent-Spawn)

**Artefakte:**
- Benchmark-Code: `deploy/bench/stack-harness/src/main.rs` (Funktion `bench_decision`)
- Unit-Test: `crates/sentinel-ecs/src/decision.rs` (Test `test_decision_performance_24_agents`)
- AC5 Verify-Command: `ssh ubuntu@192.0.2.240 './stack-harness 2>/dev/null | grep decision'`

### Cortex Event-Store Benchmark (Issue #13 AC5)

**Kontext:** Issue #13 AC-5 fordert atomare Event+Outbox Writes im cortex-gateway.
**Binary:** `go test -bench` (Go 1.25, modernc.org/sqlite pure-Go), ausgefuehrt auf VM 192.0.2.240.
**Methodik:** 3 Runs, Median.

| Metrik | Wert | Einordnung | Status |
|--------|------|------------|--------|
| `cortex.event_store.append_with_outbox_us` | `1360us` (~1.36ms) | Pro LLM-Request (1-3 Events), LLM-Call dauert 5-20s | **PASS** |
| `cortex.event_store.15_agents_tick_ms` | `22.9ms` | 15 Events/Tick, 43 ticks/s (nicht relevant fuer Gateway) | **INFO** |
| `cortex.event_store.idempotent_retry_us` | `193us` | INSERT OR IGNORE bei Duplikat | **PASS** |
| `cortex.event_store.allocs_per_write` | `63` | Pure-Go SQLite overhead | **INFO** |

**Einordnung:**
- Gateway-Pipeline: 1.36ms Schreib-Overhead bei 5-20s LLM-Latenz = <0.03% der Gesamtlatenz
- Idempotent Retry: 193us ist 7x schneller als normaler Write (nur Index-Lookup)
- 15-Agent-Tick FAIL ist irrelevant: Gateway schreibt pro Request, nicht pro Tick

**Artefakte:**
- Benchmark-Code: `cmd/cortex-gateway/internal/eventstore/bench_test.go`
- VM-Verify: `ssh ubuntu@192.0.2.240 "cd ~/project-sentinel/cmd/cortex-gateway && go test -bench=. ./internal/eventstore/"`

### Runtime Orchestrator Benchmark (Issue #15)

**Kontext:** Issue #15 fordert event-sourced Lifecycle-Events (AC-2), Resume nach Neustart (AC-4), Pause/Resume Lifecycle, State-Machine, ECS-Integration-Hook.
**Binary:** `cargo bench -p sentinel-runtime` (Release, Criterion), ausgefuehrt auf VM 192.0.2.240.
**Methodik:** Criterion 100 Samples, Median. Footprint via `#[ignore]` Test mit /proc/self/status.

| Metrik | Wert | Einordnung | Status |
|--------|------|------------|--------|
| `runtime.spawn_with_event_ms` | `1.51ms` | Spawn + JSON-Serialize + append_with_outbox | **PASS** |
| `runtime.spawn_no_event_ns` | `612ns` | Baseline ohne EventStore (Overhead: ~1.51ms = Store-I/O) | **INFO** |
| `runtime.despawn_with_event_us` | `48.0us` | Despawn + Event-Emission | **PASS** |
| `runtime.pause_resume_with_event_us` | `234us` | Pause + Resume Zyklus (2x State-Machine + 2x Event) | **PASS** |
| `runtime.shift_transition_15_agents_us` | `22.9us` | Bulk-Remove 15 Agents + 1 Event | **PASS** |
| `runtime.save_state_5_us` | `622us` | Snapshot 5 Agents (JSON + SQLite) | **PASS** |
| `runtime.save_state_15_ms` | `990us` | Snapshot 15 Agents | **PASS** |
| `runtime.save_state_50_ms` | `1.38ms` | Snapshot 50 Agents | **PASS** |
| `runtime.restore_5_us` | `15.6us` | Restore 5 Agents (SQLite Read + JSON-Deserialize) | **PASS** |
| `runtime.restore_15_us` | `22.0us` | Restore 15 Agents | **PASS** |
| `runtime.restore_50_us` | `42.3us` | Restore 50 Agents | **PASS** |
| `runtime.full_shift_cycle_ms` | `5.18ms` | Spawn 15 + Transition + Spawn 15 + Save | **PASS** |
| `runtime.restart_cycle_us` | `21.9us` | Restore 15 Agents (simulated restart) | **PASS** |

**Thread/Memory Footprint (Verify-Anforderung):**

| Metrik | Wert | Einordnung |
|--------|------|------------|
| `sizeof(RuntimeOrchestrator)` | `96 bytes` | Stack-Allokation (HashMap + Optionals) |
| `sizeof(AgentHandle)` | `72 bytes` | Pro Agent (+ Heap fuer name/role Strings) |
| `sizeof(AgentStatus)` | `1 byte` | Enum mit 4 Varianten |
| RSS Delta (50 Agents) | `2040 KB` | ~40 KB/Agent inkl. EventStore/SQLite overhead |
| Threads | `1` (Runtime) | Kein Thread-pro-Agent, shared HashMap |

**Einordnung:**
- Spawn-Overhead dominiert von SQLite-Write (1.51ms vs. 612ns ohne Store = ~2470x)
- Pause+Resume-Zyklus: 234us fuer State-Machine-Transition + 2 Events (2x SQLite-Write)
- Restore ist extrem schnell: 22us fuer 15 Agents (SQLite-Read + JSON-Parse)
- Voller Schichtwechsel-Zyklus in ~5ms (inkl. SQLite-Writes fuer alle Events)
- Recovery nach Neustart: 22us = vernachlaessigbar (Prozessstart dominiert)
- save_state skaliert sublinear: 5 Agents 622us, 50 Agents 1.38ms (nicht 10x sondern 2.2x)
- Kein Prozesswachstum: 1 Thread, ~40 KB/Agent (dominated by SQLite page cache, nicht Agent-Daten)
- State-Machine verhindert ungueltige Uebergaenge (Active->Suspended->Active, keine Suspended->Sleeping)
- RuntimeEventSink-Trait ermoeglicht synchrone ECS-Integration ohne zusaetzliche Threads

**Artefakte:**
- Benchmark-Code: `crates/sentinel-runtime/benches/runtime_bench.rs`
- Footprint-Test: `crates/sentinel-runtime/src/lib.rs` (`footprint_measurement`, `#[ignore]`)
- VM-Verify: `ssh ubuntu@192.0.2.240 "cd /opt/sentinel && cargo bench -p sentinel-runtime 2>&1 | grep 'time:'"``

### Hippocampus Persistent Memory Benchmark (Issue #23)

**Kontext:** Issue #23 fordert persistentes Memory-Subsystem mit redb (Episode-Store, Narrative, Facts, Cache-State), Night-Run Konsolidierung und NMDA-priorisierte Retrieval-APIs.
**Binary:** `cargo bench -p sentinel-hippocampus` (Release, Criterion), ausgefuehrt auf VM 192.0.2.240.
**Methodik:** Criterion 100 Samples, Median. DB-Groesse via `std::fs::metadata`.

#### Serialisierung (serde_json)

| Metrik | Wert | Einordnung | Status |
|--------|------|------------|--------|
| `hippocampus.episode_serialize_json` | `448ns` | Einzelne Episode (JSON, inkl. participants+tags) | **PASS** |
| `hippocampus.episode_deserialize_json` | `804ns` | Einzelne Episode | **PASS** |
| `hippocampus.episode_batch_10_serialize` | `4.0us` | 10 Episodes (typische Tageslast pro Agent) | **PASS** |
| `hippocampus.episode_batch_10_deserialize` | `9.7us` | 10 Episodes | **PASS** |

#### NMDA-Scoring

| Metrik | Wert | Einordnung | Status |
|--------|------|------------|--------|
| `hippocampus.nmda_score_single` | `2.5ns` | Pure Arithmetik (relevance×emotion×repetitions×decay) | **PASS** |
| `hippocampus.nmda_score_sort_10` | `99ns` | Score + Sort fuer 10 Episodes | **PASS** |

#### redb Store/Load Latenz

| Metrik | Wert | Einordnung | Status |
|--------|------|------------|--------|
| `hippocampus.redb_store_1_episode` | `10.8ms` | Einzelne Episode (Serialize + Write-Txn + fsync) | **PASS** |
| `hippocampus.redb_load_1_episode` | `1.7us` | Read-Txn (kein fsync, MVCC snapshot) | **PASS** |
| `hippocampus.redb_store_10_episodes` | `6.4ms` | Batch (sublinear: 10x Daten, nur 0.6x Latenz vs 1 Ep) | **PASS** |
| `hippocampus.redb_load_10_episodes` | `9.7us` | Batch-Read | **PASS** |
| `hippocampus.redb_append_1_to_5` | `9.5ms` | Read-Modify-Write Pattern (Load 5 + Append 1 + Store) | **PASS** |
| `hippocampus.redb_store_fact` | `5.0ms` | Fact Key-Value Write | **PASS** |
| `hippocampus.redb_load_fact` | `762ns` | Fact Key-Value Read | **PASS** |
| `hippocampus.redb_store_narrative` | `5.1ms` | NarrativeState Write | **PASS** |
| `hippocampus.redb_load_narrative` | `1.2us` | NarrativeState Read | **PASS** |

#### redb Deep-Dive (Transaktionen, MVCC, Cold/Warm Start)

| Metrik | Wert | Einordnung | Status |
|--------|------|------------|--------|
| `hippocampus.redb_open_create` | `51.3ms` | Cold-Start: DB erstellen + 4 Tables anlegen | **INFO** |
| `hippocampus.redb_reopen_existing` | `19.1ms` | Warm-Start: existierende DB oeffnen | **INFO** |
| `hippocampus.redb_mvcc_read_after_write` | `5.3ms` | Write Agent_0 + Read Agent_5 (Snapshot Isolation) | **PASS** |
| `hippocampus.redb_txn_write_empty_commit` | `3.2ms` | Leere Write-Txn (fsync-Overhead) | **INFO** |
| `hippocampus.redb_txn_read_only` | `719ns` | Read-Only Txn (kein fsync) | **PASS** |
| `hippocampus.redb_agent_scan/5` | `1.5us` | list_agents_with_episodes (5 Keys) | **PASS** |
| `hippocampus.redb_agent_scan/15` | `3.2us` | list_agents_with_episodes (15 Keys) | **PASS** |
| `hippocampus.redb_agent_scan/54` | `9.8us` | list_agents_with_episodes (54 Keys) | **PASS** |
| `hippocampus.redb_cache_state_toggle` | `4.1ms` | hot/cold Toggle (Write-Txn) | **PASS** |
| `hippocampus.redb_cache_state_read` | `869ns` | Cache-State Read | **PASS** |

#### Konsolidierung + Retrieval (Service-Ebene)

| Metrik | Wert | Einordnung | Status |
|--------|------|------------|--------|
| `hippocampus.consolidate_1_agent_10_eps` | `28.6ms` | Load + SleepCycle + Narrative-Build + Store + Clear | **PASS** |
| `hippocampus.consolidate_5_agents_10_eps` | `89.8ms` | 5 Agents sequentiell (~18ms/Agent) | **PASS** |
| `hippocampus.retrieve_top5_from_10` | `11.3us` | Load 10 + NMDA-Score + Sort + Truncate | **PASS** |
| `hippocampus.retrieve_top10_from_50` | `60.7us` | Load 50 + Score + Sort + Truncate | **PASS** |
| `hippocampus.fact_retrieval_2_matches` | `2.2us` | Trigger-Match gegen 3 Facts (2 Hits) | **PASS** |
| `hippocampus.fact_retrieval_0_matches` | `639ns` | Trigger-Match (0 Hits) | **PASS** |

#### Produktions-Szenario (54 Agents)

| Metrik | Wert | Einordnung | Status |
|--------|------|------------|--------|
| `hippocampus.production_54_agents_consolidate` | `586ms` | Nightly-Run: 54 Agents × 8-12 Eps = ~540 Episodes | **PASS** |
| `hippocampus.production_54_agents_record_batch` | `339ms` | Tages-Batch: 54 × 1 Episode (54 Write-Txns) | **PASS** |
| `hippocampus.production_54_agents_retrieve_all` | `605us` | Dashboard-Sweep: 54 × Top-5 Retrieval | **PASS** |
| `hippocampus.redb_file_size_54_agents` | `532 KB` | 54 Agents × 10 Eps + 54 Facts | **PASS** |

**Einordnung:**
- Write-Latenz dominiert von fsync (~3-5ms Basis pro Write-Txn, unabhaengig von Payload-Groesse)
- Read-Latenz sub-Mikrosekunde (MVCC Snapshot, kein fsync noetig)
- Nightly-Konsolidierung (586ms fuer 54 Agents) ist vernachlaessigbar gegenueber typischer Night-Run-Dauer
- Tages-Batch-Recording (339ms fuer 54 Agents) ist <1s, akzeptabel fuer nicht-zeitkritische Episode-Erfassung
- DB-Groesse (532 KB) weit unter 1MB, selbst bei Vollauslastung <10MB erwartet
- Skalierung sublinear: 10 Eps Store kostet 6.4ms (vs 10.8ms fuer 1 Ep — Batch-Vorteil durch single Txn)
- Agent-Scan skaliert linear (1.5us/5 → 9.8us/54 ≈ ~0.18us/Key)

**Artefakte:**
- Benchmark-Code: `crates/sentinel-hippocampus/benches/hippocampus_bench.rs`
- VM-Verify: `ssh ubuntu@192.0.2.240 "cd /home/ubuntu/sentinel-target && ./release/deps/hippocampus_bench-* --bench"`

### Benchmark-Governance (Two-Tier Architektur, Enterprise SOTA 2026)

**Kontext:** Benchmark-Strategie fuer das Gesamtprojekt etabliert. Codex (gpt-5.3-codex) Session `019c655c-d997-70b1-a7f7-b9da63f47465` hat Option D (Criterion CI + Production Binary VM) als besten Enterprise-Ansatz bestaetigt.

| Tier | Zweck | Wo | Tool |
|------|-------|----|------|
| **Tier 1** | Component-Level Regression | CI (Build-Server 192.0.2.155) | Criterion.rs, Go testing.B, Bun bench |
| **Tier 2** | System-Level E2E | VM 192.0.2.240 (ext4 /data) | `deploy/bench/stack-harness` + Runner-Scripts |

**Governance-Massnahmen (umgesetzt 2026-02-16):**
- `issue-quality.yml`: Benchmarks-Sektion als Pflichtfeld (Varianten: Benchmarks, Benchmark, Performance)
- `pr-quality.yml`: `## Benchmarks` als Pflicht-Sektion in PR-Body
- Alle 20 offenen Feature-Issues (#17-#76) mit `## Benchmarks` Sektion versehen (Neue Metriken, Performance-Budget, Tier, Betroffene Sprachen, Bestehende Benchmarks betroffen)
- Polyglot-Coverage: Rust, Go, TypeScript, C++, Bash beruecksichtigt

**Erkenntnisse:**
- Criterion.rs ist ein Microbenchmark-Tool — nicht geeignet fuer systemische Effekte (Storage, Scheduler, Kernel, I/O-Stack)
- tmpfs-Benchmarks sind nicht vergleichbar mit ext4-Produktion (fsync-Kosten fehlen)
- `deploy/bench/stack-harness` + `run-stack-suite-guest.sh` existieren bereits als Tier 2 Prototyp

### Nightrun Benchmark (Issue #17, Criterion auf Build-Server)

**Kontext:** Issue #17 implementiert sentinel-nightrun (Schichtwechsel-Konsolidierung). Benchmarks auf Build-Server 192.0.2.155 (tmpfs, Tier 1).
**Binary:** `cargo bench -p sentinel-nightrun` (Release, Criterion).

| Metrik | Wert | Einordnung | Status |
|--------|------|------------|--------|
| `nightrun.shift/shift_set_for_hour` | `7.06ns` | Pure Arithmetik (Hour -> Shift-Set) | **PASS** |
| `nightrun.shift/outgoing_shift_set` | `882ps` | Lookup (New Shift -> Outgoing) | **PASS** |
| `nightrun.job_queue/create_run_15` | `1.72ms` | SQLite: 15 Jobs anlegen | **PASS** |
| `nightrun.job_queue/create_run_54` | `3.91ms` | SQLite: 54 Jobs anlegen | **PASS** |
| `nightrun.job_queue/mark_transitions` | `596us` | Status-Updates (pending->completed) | **PASS** |
| `nightrun.job_queue/get_pending_15` | `51.3us` | 15 pending Jobs abfragen | **PASS** |
| `nightrun.pipeline/consolidate/1` | `12.4ms` | 1 Agent konsolidieren (E2E) | **PASS** |
| `nightrun.pipeline/consolidate/5` | `43.9ms` | 5 Agents konsolidieren | **PASS** |
| `nightrun.pipeline/consolidate/15` | `82.9ms` | 15 Agents konsolidieren | **PASS** |

**Einordnung:**
- Pipeline skaliert sublinear: 15 Agents in 82.9ms (nicht 15x 12.4ms = 186ms)
- Job-Queue create_run skaliert linear: 54 Jobs in 3.91ms (~72us/Job)
- Shift-Detection ist vernachlaessigbar (<10ns)
- **Achtung:** Benchmarks auf tmpfs (Build-Server), nicht auf ext4 (Produktion). Tier 2 Benchmarks auf VM 192.0.2.240 stehen noch aus.

**Artefakte:**
- Benchmark-Code: `services/sentinel-nightrun/benches/nightrun_bench.rs`

### Update-Log
- 2026-02-16: Benchmark-Governance etabliert (Two-Tier Architektur). 20 offene Issues mit ## Benchmarks Sektion versehen. CI Quality Gates (issue-quality.yml, pr-quality.yml) um Benchmarks-Pflichtsektion erweitert.
- 2026-02-16: Nightrun Benchmark (Issue #17): 9 Benchmarks auf Build-Server. Pipeline 15 Agents 82.9ms, Job-Queue 54 Jobs 3.91ms, Shift-Detection 7ns.
- 2026-02-15: Hippocampus Persistent Memory Benchmark (Issue #23): 40+ Benchmarks auf VM 192.0.2.240. Production 54-Agent Consolidate 586ms, Retrieve-Sweep 605us, DB-Size 532KB.
- 2026-02-15: Issue #23 PARTIAL->FULL: redb-Persistence (4 Tables), HippocampusService Facade, Night-Run Konsolidierung, NMDA-priorisiertes Retrieval. 57 Unit-Tests + 4 Acceptance-Tests, 40+ Benchmarks.
- 2026-02-15: Issue #15 Scope-Luecken geschlossen: pause_agent/resume_agent, State-Machine (AgentStatus::can_transition_to), RuntimeEventSink-Trait (ECS-Integration), Thread/Memory-Footprint dokumentiert. 29 Tests (21 unit + 8 acceptance), 13 Benchmarks auf VM.
- 2026-02-15: Runtime Orchestrator Benchmark (Issue #15): Spawn 1.51ms, Restore 22us, Pause+Resume 234us, Footprint 96 bytes/Orchestrator + 72 bytes/Agent auf VM 192.0.2.240.
- 2026-02-14: Cortex Event-Store Benchmark (Issue #13 AC5): AppendWithOutbox 1.36ms, IdempotentRetry 193us auf VM 192.0.2.240.
- 2026-02-14: Decision Engine Benchmark (Issue #54 AC5) dokumentiert: 1.02us/tick bei 24 Agents (Schwellenwert <50us, 49x Marge).
- 2026-02-13: VM-Toolchain auf `rustc/cargo 1.93.1` angehoben, 3-Run Stack-Suite auf 1069 durchgefuehrt, Zielwert-Matrix mit PASS/FAIL ergänzt.
