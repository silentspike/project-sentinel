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
  - `FULL`: `7/25` -> `28%` (Strict Full Rate)
  - `PARTIAL`: `18/25` -> `72%`
  - `MISMATCH`: `0/25` (formal, aber mehrere harte Scope-Reduktionen)
  - `Weighted Delivery`: `(7*1 + 18*0.5)/25 = 16/25 = 64%`
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
| #13 | cortex-gateway Vollpipeline | PARTIAL | Pipeline-Komponenten instanziiert aber nicht verdrahtet: `cmd/cortex-gateway/main.go:77`; Handler sendet direkt Provider: `cmd/cortex-gateway/internal/proxy/handler.go:80`; Injection-Ordner leer: `cmd/cortex-gateway/internal/injection/.gitkeep`; Control-Endpoints weichen vom Ticket ab: `cmd/cortex-gateway/internal/control/plane.go:156` | Umsetzungsluecke + AC prueft Module eher isoliert |
| #14 | perception-injection (ECS) | FULL | `generate_perception` + `format_injection` vorhanden: `crates/sentinel-ecs/src/perception.rs:38`, `crates/sentinel-ecs/src/perception.rs:84` | solide Umsetzung |
| #15 | teammate-first runtime | PARTIAL | Orchestrator vorhanden, aber nur in-memory Lifecycle: `crates/sentinel-runtime/src/lib.rs:27` | reduzierte Runtime-Tiefe |
| #16 | sandbox (bwrap+landlock+cgroups) | PARTIAL | bwrap-Args Builder: `crates/sentinel-sandbox/src/bwrap.rs:27`; cgroup-Datenstrukturen: `crates/sentinel-sandbox/src/cgroups.rs:6`; keine echte Landlock-/cgroup-Enforcement-Pipeline in Runtime | Umsetzungsluecke, AC zu struktur-lastig |
| #17 | bitnet + multi-lora + speculative | PARTIAL | BitNet als Subprocess-Wrapper: `crates/sentinel-inference/src/bitnet.rs:18`; vereinfachte speculative Heuristik: `crates/sentinel-inference/src/speculative.rs:54` | AC auf Minimalfunktionen, nicht Produktionsniveau |
| #18 | kv-cache-sharing | PARTIAL | Explizit nur Prompt-Level, kein echter KV-Cache sharing Kernel: `crates/sentinel-inference/src/kv_cache.rs:11` | Scope reduziert/vereinfacht |
| #19 | wasm runtime (wasmtime/extism) | PARTIAL | Native FileRead/FileWrite + sonst Placeholder: `crates/sentinel-wasm/src/runner.rs:79` | starke Abweichung Titel vs. reale Tiefe, AC zu weich |
| #20 | 54 Agenten migrieren | PARTIAL | Tests fordern nur 5 Dateien: `crates/sentinel-common/tests/acceptance_agents.rs:15`; Loader-Test erwartet 5: `crates/sentinel-common/src/agent_config.rs:153`; real nur 5 statt 54 | primar Issue-Qualitaet (Scope-Absenkung im Ticket selbst) |
| #21 | nmda sleep-cycle | PARTIAL | Consolidation explizit TODO: `crates/sentinel-hippocampus/src/sleep.rs:120` | bewusst als Placeholder im Issue zugelassen |
| #22 | fourth-wall detection | PARTIAL | Detection/Judge gut implementiert; aber Proxy-Integration nicht im Live-Handlerpfad (`cmd/cortex-gateway/internal/proxy/handler.go:80`) | Integrationsluecke |
| #23 | hippocampus memory | PARTIAL | Kernmodule vorhanden, aber Backends stark vereinfacht (in-memory FactStore/KV-Tier): `crates/sentinel-hippocampus/src/facts.rs:62`, `crates/sentinel-hippocampus/src/cache_tier.rs:31` | AC auf API-Ebene, nicht auf realen Storage-Backends |
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
- #13 Vollpipeline nicht in HTTP-Request-Pfad verdrahtet.
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
7. CI muss AC hart pruefen (Issue-Quality Gate + AC-Linter + Mindest-Evidence).
8. Non-Functional AC verpflichtend bei Architektur-Issues:
   - Latenz,
   - Speicher,
   - I/O,
   - Isolation/Security.

## Offene Prioritaetsliste fuer Re-Audit/Repair
1. #13 Vollpipeline verdrahten (Injection -> Compiler -> Provider -> Detection/Judge -> Extraction -> Normalize -> Events).
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
- `issue-quality.yml`: lintet neue/edierte Issues auf Pflichtsektionen
- `pr-quality.yml`: verweigert Merge bei fehlender AC-Evidence
- `main-push-guard.yml`: blockt direkte Pushes auf `main`

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
- Methodik: stack-nahe Benchmarks auf der Ziel-VM (`ubuntu@10.0.0.240`) statt Funktions-Simulation.

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
**Binary:** `stack-harness` (Release, gebaut auf 10.0.0.155), ausgefuehrt auf VM 10.0.0.240.
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
- AC5 Verify-Command: `ssh ubuntu@10.0.0.240 './stack-harness 2>/dev/null | grep decision'`

### Cortex Event-Store Benchmark (Issue #13 AC5)

**Kontext:** Issue #13 AC-5 fordert atomare Event+Outbox Writes im cortex-gateway.
**Binary:** `go test -bench` (Go 1.25, modernc.org/sqlite pure-Go), ausgefuehrt auf VM 10.0.0.240.
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
- VM-Verify: `ssh ubuntu@10.0.0.240 "cd ~/project-sentinel/cmd/cortex-gateway && go test -bench=. ./internal/eventstore/"`

### Update-Log
- 2026-02-14: Cortex Event-Store Benchmark (Issue #13 AC5): AppendWithOutbox 1.36ms, IdempotentRetry 193us auf VM 10.0.0.240.
- 2026-02-14: Decision Engine Benchmark (Issue #54 AC5) dokumentiert: 1.02us/tick bei 24 Agents (Schwellenwert <50us, 49x Marge).
- 2026-02-13: VM-Toolchain auf `rustc/cargo 1.93.1` angehoben, 3-Run Stack-Suite auf 1069 durchgefuehrt, Zielwert-Matrix mit PASS/FAIL ergänzt.
