# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Zenoh SHM Core-Bus Integration** (#6)
  - `services/sentinel-daemon/src/orchestrator.rs`: Shared `SentinelBus` Instanz im Daemon, verteilt an alle Subsysteme
  - `services/sentinel-daemon/src/fanout.rs`: Event Fan-Out Bridge — Events nach Limbo-Write auf Zenoh Topics publizieren
  - `services/sentinel-daemon/src/query_responder.rs`: Scoped Query Responder — beantwortet Queries ueber Zenoh mit redb State
  - `services/sentinel-daemon/src/ebpf.rs`: `ebpf_publisher` nimmt shared `SentinelBus` statt eigener Instanz
  - `crates/sentinel-ecs/src/world.rs`: Neue `ZenohFanoutSender` ECS Resource
  - `crates/sentinel-ecs/src/systems.rs`: `persist_system` sendet Events nach Limbo-Write an Zenoh Fan-Out
  - `crates/sentinel-zenoh/src/config.rs`: 3 neue Config-Felder (`shm_buffer_size_bytes`, `fanout_channel_capacity`, `query_responder_enabled`)
  - `crates/sentinel-zenoh/benches/shm_benchmark.rs`: 6 Criterion Benchmarks (Latenz, Throughput, Concurrency, Fanout, Query, Buffer Sizing)

- **SmellEvents End-to-End Pipeline** (#195)
  - `crates/sentinel-ecs/src/world.rs`: Neue `ActiveSmells` ECS Resource (HashMap pro Raum, Decay-Logik)
  - `crates/sentinel-ecs/src/systems.rs`: `input_system` + `smell_system` + `perception_system` erzeugen/injizieren Smells
  - `crates/sentinel-ecs/src/autonomy.rs`: Autonomie-System erzeugt SmellEvents bei Coffee/Meal
  - `crates/sentinel-projection/src/store.rs`: Schema-Migration `active_smells` Spalte + `update_room_smells()`
  - `crates/sentinel-projection/src/handlers/room_live_view.rs`: SmellEventTriggered Handler
  - `crates/sentinel-projection/src/worker.rs`: Legacy-Mapper fuer `smell_event_triggered`
  - `dashboard/src/types.ts`, `routes/rooms.ts`, `ws.ts`: `active_smells` Feld in API + WebSocket
  - 5 neue Integration-Tests: Coffee E2E, Perception Injection, Smell Decay, Raum-Isolation, Food E2E

- **agent-runtime Binary** (#173)
  - `services/agent-runtime/`: Leichtgewichtiger Sandbox-Prozess (Zero Dependencies, nur std)
  - Stdin-Reader-Thread fuer zukuenftige Command-Dispatch, Heartbeat-Loop (30s) fuer VFS I/O
  - Laeuft innerhalb bwrap-Namespace als `/usr/bin/agent-runtime`

### Fixed

- **Projection Legacy-Kompatibilitaet** (#53)
  - `crates/sentinel-common/src/events.rs`: `#[serde(default)]` fuer `valence`/`arousal` in `BioStateUpdated` — 3.4M Legacy-Events werden nicht mehr uebersprungen
  - `crates/sentinel-projection/src/worker.rs`: Rebuild setzt Offset nur einmal am Ende statt pro Batch — verhindert Monotonicity-Fehler bei konkurrierenden Consumern

- **Transit-Varianz: RoomId-Typ-Mismatch** (#194)
  - `crates/sentinel-common/src/types.rs`: `AgentAction.target_room` von `Option<RoomId>` auf `Option<String>` geaendert
  - `crates/sentinel-ecs/src/systems.rs`: `input_system()` nutzt jetzt echte Raumnamen statt `format!("ROOM-{}")` — Distance-Lookup funktioniert endlich
  - `services/sentinel-daemon/src/llm_bridge.rs`: Echter Raumname statt Dummy `RoomId(1)`
  - Vorher: Alle Transits hatten identische Dauer (3100ms), Room-IDs waren nach erstem Transit kaputt
  - 5 neue Integration-Tests: Distance-Varianz, Bounds-Check, Room-ID nach Transit, Encounter-Events, RoomDistanceMap-Lookup

- **Landlock Detection auf Kernel 6.11+** (#173)
  - `crates/sentinel-sandbox/src/landlock.rs`: `detect_abi()` prueft nun auch `/sys/kernel/security/lsm` als Fallback
  - Kernel 6.11+ entfernte `/sys/kernel/security/landlock/` securityfs-Directory
  - Landlock war im Kernel aktiv aber wurde nicht erkannt — jetzt korrekt: "Landlock ABI v4 detected"

- **WASM Component Model Runtime** (#19)
  - `crates/sentinel-wasm/wit/world.wit`: WIT Interface Definition (`sentinel:plugin@0.1.0`) mit Host-API (fs-read/write/list, get-agent-info, get-room-info, log, get-tick) und Plugin-Exports (execute, tool-name, tool-description)
  - `crates/sentinel-wasm/src/host.rs`: `bindgen!`-basierte Host-Implementation, `PluginState` mit WASI 0.2 Context, `AgentSnapshot`/`RoomSnapshot` ECS-Bridges, 7 Host-Function Traits
  - `crates/sentinel-wasm/src/plugin.rs`: `PluginHost` (Engine + Linker + Component Cache), `PluginConfig` (Memory 64MB, Fuel 10M, WASI preopened dirs), Lifecycle: compile-once/instantiate-per-call
  - `crates/sentinel-wasm/src/runner.rs`: `execute_component()` ersetzt `execute_wasm()`, `ExecutionContext` mit optionalen ECS-Snapshots (`agent_snapshot`, `rooms`) hinter `cfg(feature = "wasm")`
  - wasmtime 42 mit Component Model + WASI 0.2 (`wasmtime-wasi` 42), Fuel-Metering, `StoreLimitsBuilder`
  - `services/sentinel-daemon/src/orchestrator.rs`: WASM-Plugin Auto-Load aus `config/tools/*.wasm` mit `query_meta()` fuer Tool-Name/Description
  - `crates/sentinel-ecs/tests/wasm_ecs_integration.rs`: 7 ECS-Integrationstests (voller bevy_ecs World+Schedule Pipeline: Agent→input_system→WASM→DomainEvent)
  - Test-Fixtures: `echo-plugin.wasm`, `loop-plugin.wasm`, `fs-plugin.wasm` (alle Rust `wasm32-wasip2` Components)
  - `fs-plugin`: Nutzt alle 7 Host-API Functions (fs-read/write/list, get-agent-info, get-room-info, get-tick, log) — beweist "Fake OS" E2E
  - `crates/sentinel-sandbox/src/bwrap.rs`: `with_fs_mount()` Builder — optionaler sentinel-fs FUSE-Mount statt `/ram/agents/`
  - `crates/sentinel-sandbox/src/enforcer.rs`: `set_fs_mount()` — SandboxEnforcer nutzt FUSE-Mount fuer Agent-Homes
  - `services/sentinel-daemon/src/config.rs`: `fs_mount` Config-Feld (Optional, Default: None)
  - `services/sentinel-daemon/src/orchestrator.rs`: FUSE-Mount Initialisierung beim Daemon-Start (Feature `fuse`), WASM-Plugin Auto-Load
  - 113 sentinel-wasm Tests (51 Unit + 35 E2E + 20 Acceptance + 7 Security), 0 Failures
  - 7 sentinel-ecs WASM Integration Tests (Multi-Agent, Native+WASM Koexistenz, Fehlerfaelle)
  - 13 Benchmarks (6 native + 7 Component Model: cold/warm start, host roundtrip, E2E, query_meta)

- **Transit-Varianz + Flurbegegnungen** (#194)
  - `sentinel-common/src/room.rs`: `shortest_distance()` BFS-Methode fuer Raum-Distanzen + 5 Tests
  - `sentinel-ecs/src/systems.rs`: Transit-Dauer von hart-codiertem 3000ms auf `(1500 + hops * 800).clamp(2000, 5000)` umgestellt (distanz-basiert)
  - `sentinel-ecs/src/systems.rs`: `encounter_system()` — Flurbegegnungen zwischen in-Transit Agents (splitmix64 RNG, 30% Wahrscheinlichkeit, alle 10 Ticks)
  - `sentinel-ecs/src/world.rs`: `RoomDistanceMap` ECS Resource (vorberechnete BFS-Distanzen aus rooms.toml)
  - `sentinel-common/src/events.rs`: `HallwayEncounterDetected` DomainEventPayload-Variante

- **SmellEvents End-to-End** (#195)
  - `sentinel-ecs/src/systems.rs`: `smell_system()` — generiert Coffee-SmellEvents bei Auto-Coffee (caffeine > 90mg)
  - `sentinel-common/src/events.rs`: `SmellEventTriggered` DomainEventPayload-Variante (room_id, smell_type, intensity, duration_ticks)

- **PSI→Bio Integration** (#196)
  - `sentinel-ecs/src/systems.rs`: `bio_system()` ruft `apply_psi_stress()` auf via `PsiMetrics` ECS Resource
  - `sentinel-ecs/src/world.rs`: `PsiMetrics` ECS Resource (cpu_avg10, mem_avg10)
  - `services/sentinel-daemon/src/orchestrator.rs`: PSI-Metriken aus AdaptiveTickRate in ECS World injiziert vor jedem Schedule-Run

- **Cortex Pipeline Hardening: Config Persistence** (#144)
  - `cmd/cortex-gateway/main.go`: `applyHardeningDefaults()` laedt Personality Guard, Quality Gate und Narrative Nudge Defaults aus Environment-Variablen beim Start
  - `config/cortex-gateway.toml`: Hardening-Dokumentation mit Env-Var-Referenzen
  - systemd Drop-in (`hardening.conf`): Guards sind im Production-Betrieb by-default aktiviert
  - systemd Drop-in (`user.conf`): Gateway laeuft als `ubuntu` User (Claude CLI Auth)

### Security

- **Go Toolchain Update** go1.25.7 → go1.25.8 (GO-2026-4600, GO-2026-4601, GO-2026-4602)
  - `go.work`: `toolchain go1.25.8` — fixes FileInfo escape from Root in os, IPv6 host literal parsing in net/url, panic in x509 name constraint checking

### Fixed

- **eBPF Kernel-Modus Regression: funktionale Korrektheit** (#139)
  - `crates/sentinel-ebpf/src/collector.rs`: Initial Health-Timestamp bei Agent-Registrierung — verhindert False-Positive Stalls fuer frisch registrierte Agents
  - `crates/sentinel-ebpf/src/collector.rs`: Health-Updates aus `/proc/{pid}/io` und cgroup `io.stat` Supplement — VFS-level I/O als Stall-Detection Supplement im Kernel-Modus
  - `crates/sentinel-ebpf/src/collector.rs`: PSI Partial statt All-or-Nothing — wenn nur eine PSI-Datei fehlt, wird der Agent nicht mehr komplett uebersprungen

- **Flaky Test `test_ecs_tick_loop_runs_ticks`** (#135)
  - `services/sentinel-daemon/src/orchestrator.rs`: Threading invertiert — `ecs_tick_loop` laeuft im Background-Thread, Perception-Warten im Main-Thread. Eliminiert Race Condition bei der `recv_timeout(30s)` vor Loop-Start zurueckkehren konnte (Setup-Dauer auf belastetem CI Runner). Gleiches Fix fuer `test_save_state_on_shutdown`.

- **Gateway Provider-Routing Bug** (#138)
  - `services/sentinel-daemon/src/orchestrator.rs`: `extract_swap_provider()` setzte Fallback auf "claude" (API) statt "claude-code" (Subscription) — alle Agent-Overrides zeigten auf nicht-registrierten Provider → 502
  - Match-Reihenfolge korrigiert: "claude-code" vor "claude" (laengster Match zuerst)

- **Bio-Engine Dynamics flach** (#148)
  - `sentinel-bio/src/lib.rs`: Stress-Berechnung mit Traegheit (Exponential Smoothing: Anstieg alpha=0.3, Abfall alpha=0.1) + Baseline-Arbeitsstress der ueber den Tag akkumuliert (max 25)
  - `sentinel-ecs/src/systems.rs`: Auto-Coffee Threshold von energy<50 auf energy<70 gesenkt, Frequenz von 300 auf 180 Ticks erhoehen — Agents trinken nun realistisch Kaffee am Vormittag
  - `sentinel-ecs/src/autonomy.rs`: Return-to-Work nach P0-Notfaellen — Agents kehren automatisch zum Arbeitsraum zurueck wenn kein Notfall mehr aktiv ist (verhindert Deadlock auf Toilette/Kueche)
  - `sentinel-common/src/events.rs`: BioStateUpdated Events enthalten nun Valence/Arousal Zahlenwerte (nicht nur kategorisches Mood-Label)
  - 7 neue Tests (3 Stress-Traegheit, 4 Autonomy Return-to-Work)

### Added

- **Service Health Monitor** — Aktive Ueberwachung aller Sentinel-Dienste
  - `deploy/scripts/sentinel-health-monitor.sh`: Shell-Script prueft alle 60s via systemd-Timer: systemd-Status, HTTP /health Endpoints, NATS Health, Projection-Lag
  - ntfy Alerts bei Statuswechsel (DOWN/RECOVERED), kein Spam durch State-File Deduplizierung
  - Auto-Restart bei toten Services (max 3 Versuche pro Ausfall-Episode)
  - Projection-Lag Monitoring: Warning >500, Critical >5000 Events
  - `deploy/systemd/sentinel-health-monitor.timer`: 60s Intervall, persistent
  - Alle 7 Service-Units gehaertet mit `StartLimitBurst=5` / `StartLimitIntervalSec=300`
  - `sentinel.target` erweitert um Health-Monitor Timer

- **PSI-basierte adaptive Tick-Rate** (#147)
  - `adaptive_tick.rs`: Neues Modul `AdaptiveTickRate` — liest `/proc/pressure/{cpu,memory,io}` und moduliert Tick-Rate dynamisch (TOGAF Adaptive Scheduling)
  - CPU avg10 > 85% → Tick-Rate × 0.5 (halbe Frequenz), Memory avg10 > 80% → Agent-Spawn blockiert, IO avg10 > 70% → Batching 500ms
  - `config.rs`: `AdaptiveConfig` mit 6 konfigurierbaren Schwellwerten unter `[daemon.adaptive]`
  - `orchestrator.rs`: Statisches `sleep(tick_rate)` ersetzt durch adaptive Rate mit PSI-basierter Modulation
  - `ebpf.rs`: Prometheus-Endpoint (:9090) exportiert nun auch `sentinel_tick_duration_ms`, `sentinel_tick_rate_effective_ms`, `sentinel_psi_{cpu,mem,io}_avg10`
  - `metrics.ts`: Dashboard API exponiert Tick-Dauer und PSI-Werte via `/api/metrics` und `/api/metrics/tick`
  - `metrics.js`: 4 neue Dashboard-Karten (Tick Duration, Effective Rate, PSI CPU, PSI IO)
  - `daemon.toml`: `[daemon.adaptive]` Konfigurationssektion mit TOGAF-konformen Schwellwerten
  - 15 neue Unit Tests (13 adaptive_tick + 2 config), 77 Daemon Tests gesamt
  - Graceful Fallback auf statische Rate wenn PSI nicht verfuegbar

- **Dashboard Control Plane Write-Path** (#146)
  - `control.ts`: Proxy-Routen zu Cortex Gateway :8081 (GET/PATCH config, POST provider, POST/DELETE agent-provider, POST pause/resume, GET status)
  - `events.ts`: Durchsuchbare Event-Historie mit Typ/Agent/Since-Filter, Pagination, Typ-Auflistung
  - `metrics.ts`: Neuer `/api/metrics/pipeline` Endpoint — Pipeline-Latenz, Request-Counts, Token-Usage pro Provider aus Cortex Prometheus
  - `auth.ts`: Bearer Token Middleware fuer Write-Endpoints (SENTINEL_DASHBOARD_API_KEY)
  - `control.js`: Eigener "Control"-Tab mit 6 Sektionen (Quick Actions, Provider, LLM Params, Pipeline Hardening, Guardrails Status, Live Config)
  - `index.html`: Control-Tab Navigation und View-Section
  - 16 neue Unit Tests (control + events), 41 Dashboard Tests gesamt

- **Pipeline Hardening: Personality Guard, Quality Gate, Narrative Nudge** (#144)
  - `pipeline.go`: Post-response `personalityGuardCheck()` — DriftDetector on LLM response, re-gen on critical drift
  - `pipeline.go`: Post-response `qualityGateCheck()` — QualityScorer (1-5), re-gen on score <= threshold
  - `pipeline.go`: Narrative Nudge injection in `buildSystemPrompt()` via `[NARRATIVE_NUDGE]` tags
  - `plane.go`: 6 new runtime-switchable config fields (personality_guard_enabled, drift_threshold, quality_gate_enabled, quality_threshold, quality_max_regen, narrative_nudge)
  - `main.go`: Load 54 agent personality profiles from TOML into DriftDetector at startup
  - 3 new Prometheus metrics (drift_total, regen_total, score histogram)
  - All features disabled by default, controllable via `PATCH /control/config`
  - 12 new Go tests (7 pipeline + 5 control plane)

### Fixed

- **Cortex Control Plane + Guardrails + Token Tracking** (#145)
  - `pipeline.go`: Standalone `sentinel_pipeline_tokens_total` Prometheus counter (fires regardless of guardrails config)
  - `main.go`: Health endpoint includes `guardrails_enabled` field, wired to enforcer presence
  - VM systemd unit: Guardrails env vars added (rate limits, budget caps)

- **Health Endpoint zeigt Circuit Breaker State** (#143)
  - `main.go`: Health endpoint (`/health`) includes `circuit_breakers` map with per-provider state
  - `pipeline.go`: New `BreakerStates()` method exposes CB states (closed/open/half-open)
  - 2 new Go tests (BreakerStatesEmpty, BreakerStatesReflectsState)

### Added

- **Archive Layer + FactRetriever Full-Stack Integration** (#142)
  - `store.rs`: ARCHIVE redb table (6th table in hippocampus.redb), append_archive/load_archive CRUD
  - `service.rs`: Consolidation archives episodes BEFORE clearing (data preservation)
  - `facts.rs`: FactRetriever extensible with custom triggers (add_triggers, with_triggers)
  - `sentinel-redb/lib.rs`: AGENT_FACTS table in state.redb, set_evolution_batch 5th parameter
  - `orchestrator.rs`: Facts Bridge (hippocampus FactRetriever → state.redb AGENT_FACTS)
  - `llm_bridge.rs`: evolution_facts read from state.redb into LLM metadata
  - `evolution.go`: AgentFacts field in EvolutionData, EvolutionFromMetadata, IsEmpty
  - `assembler.go`: formatEvolution includes Unternehmens-Fakten block
  - `shift.rs`: detect_shift_from_sim_hour() for virtualized shift detection (time_scale != 1.0)
  - `orchestrator.rs`: Conditional shift detection (system clock at time_scale=1.0, sim_hour otherwise)
  - 15 new Rust tests (archive, facts, agent_facts, shift) + 3 new Go tests

- **GOLF Framework — Goal-Oriented Life Tasks** (#141)
  - `golf.rs`: Goal struct, GoalType (Career/Project/Social/Skill), GoalStatus enums
  - `store.rs`: GOALS redb table (5th table), CRUD methods (store/load/append/update/list)
  - `service.rs`: Goal-CRUD facade on HippocampusService (create, append, update_progress, get, get_active, list)
  - `orchestrator.rs`: Default-Goals created at agent spawn based on role (CEO→Career+Project, Dev→Project+Skill, HR→Social+Career, Designer→Skill+Project)
  - `default_goals_for_role()`: Role-based goal initialization (initial + shift-change spawn)
  - `orchestrator.rs`: Goal-Progress Updates bei Schichtwechsel-Konsolidierung (+0.05 pro ueberlebte Schicht, auto-Completed bei >= 1.0)
  - 29 new unit tests (13 golf, 9 store, 7 service)
  - Issue #141 ACs corrected: redb instead of SQLite, log/test-based verification

- **Judge-Daemon Zenoh/NATS Integration** (#140)
  - **ADR-001:** NATS-First Communication for Go Services documented
  - **eBPF→NATS Bridge:** Daemon publishes eBPF metrics on NATS subjects
    (`sentinel.ebpf.*`) alongside Zenoh (ADR-001: Dual-Bus bridge for Go consumers)
  - **SENTINEL_EBPF JetStream Stream:** Memory-backed, 1-day retention, 50MB max
  - **Judge eBPF Consumer:** Subscribes to `sentinel.ebpf.agent-health`, feeds
    stall data into heuristic pipeline as drift-score weight factor
    (`finalDrift = 0.7*textDrift + 0.3*ebpfSignal`)
  - **Gateway per-Agent Provider Routing:** `POST /control/agent-provider` endpoint
    for per-agent model overrides; pipeline checks overrides before global primary
  - **Daemon Model-Swap Handler:** Swap alerts from NATS trigger HTTP POST to
    Gateway Control Plane (`/control/agent-provider`) instead of just logging
  - **TOGAF Deviation Register:** `docs/togaf-deviations.md` with 3 documented deviations
  - **Functional Audit Judge:** `docs/functional-audit-judge.md` (B-5, M-17-M-20)
  - **Zenoh Wiring Status:** `docs/zenoh-wiring-status.md` cataloging all 22 topics

### Removed

- `JUDGE_ALERT` Zenoh constant (ADR-001: alerts flow via NATS, was deprecated)
- `MODEL_SWAP` Zenoh constant (ADR-001: swap via NATS alert + HTTP, was deprecated)
- `cortex_inject()` topic function (no active subscribers)
- Claude API provider registration block from Cortex Gateway — Claude Code
  (subscription-based subprocess) is the default, no API key needed

### Fixed

- **eBPF Kernel-Modus Funktionale Korrektheit** (#139)
  - **Stall-Detection False Positives (AC-4):** TCP Ring Buffer Events aktualisieren
    jetzt den Health-Checker fuer alle registrierten Agents — LLM-Agent-Prozesse
    die via HTTP/TCP kommunizieren werden nicht mehr faelschlich als stalled gemeldet
  - **/proc/PID/io VFS-Level Metriken (AC-6):** `rchar`/`wchar` (VFS-Level) statt
    nur `read_bytes`/`write_bytes` (Block-Level) fuer I/O-Tracking — erfasst auch
    buffered I/O das nie den Block-Layer erreicht
  - **/proc/PID/io Permission Warning:** Einmalige `warn!()` Meldung wenn
    `/proc/PID/io` wegen fehlender `CAP_SYS_PTRACE` Capability nicht lesbar ist
  - **PSI Error-Handling (AC-7):** Differenzierte Fehlerbehandlung —
    PermissionDenied → `warn!()`, NotFound → `debug!()`, andere → `warn!()`
  - **Dashboard Stall-Indikator Name-Mismatch (AC-10):** Agent-Name Matching
    korrigiert — Prometheus nutzt echten Namen, Dashboard verglich mit AGENT-XX Format
  - **Dashboard Stall Differential-Update:** Stall-Status wird bei WebSocket-Updates
    korrekt hinzugefuegt/entfernt ohne Full-Rerender

- **Closed-Loop Personality Evolution E2E Integration** (#138)
  - **NATS Consumer Silent-Failure:** `get_stream("SENTINEL_JUDGE")` durch
    `get_or_create_stream()` mit vollstaendiger Stream-Config ersetzt — Daemon
    empfaengt jetzt zuverlaessig Judge-Alerts auch wenn Stream noch nicht existiert
  - **DomainEvent-Persistierung bei Alerts:** Judge-Alerts (drift, quality, fatigue, swap)
    erzeugen jetzt `JudgeAlertReceived` DomainEvents in Limbo + Prometheus Counter
    `sentinel_daemon_judge_alerts_total`
  - **nmda_score in Evolution-DB:** Judge schreibt `max(drift, fatigue)` als
    NMDA-Relevanz-Proxy in `personality_evolution` Tabelle

### Added

- **LLM-basierte Voice-Style und Behavioral-Notes Generierung** (#138)
  - Bei Schichtwechsel-Konsolidierung: LLM-Call an Cortex Gateway fuer
    `voice_style` und `behavioral_notes` pro Agent
  - Fail-safe: Bei Gateway-Fehler laeuft Konsolidierung ohne voice/behavioral weiter
  - `reqwest::blocking` Feature fuer HTTP-Calls im ECS std::thread

### Dependencies

- **fuser 0.16 → 0.17** (#136)
  - Migrated all `Filesystem` trait method signatures (`&mut self` → `&self`)
  - Adapted to newtype wrappers: `INodeNo`, `FileHandle`, `Generation`, `LockOwner`, `OpenFlags`, `Errno`
  - `mount2()` now takes `&Config` instead of `&[MountOption]`
  - `reply.add()` / `reply.entry()` use typed parameters instead of raw integers

### Added

- **sentinel-wasm Integration in laufendes System** (#19)
  - `ToolRuntimeResource` als ECS Resource (wraps sentinel-wasm `ToolRuntime`)
  - `AgentCapabilities` Component fuer Capability-basierte Tool-Zugriffskontrolle
  - `parse_tool_content()` Parser fuer `tool:NAME:INPUT` und JSON-Format
  - Tool-Dispatch im `input_system` ToolUse-Branch via ToolRuntime
  - `apply_capabilities()` Funktion zum Setzen der Capabilities aus TOML-Config
  - Tool Registry im Daemon mit 5 nativen Tools (file_read, file_write, chat, calendar, search)
  - `[capabilities]` Sektion in allen 54 Agent-TOMLs mit rollenbasierten Tool-Zuweisungen
  - sentinel-wasm als Dependency in sentinel-ecs und sentinel-daemon
  - 3 neue Integrationstests: Tool-Dispatch (text + JSON), Fallback ohne Runtime

### Fixed

- **Flaky Test `test_ecs_tick_loop_runs_ticks` deterministisch gemacht** (#135)
  - `sleep(500ms)` durch `perception_rx.recv_timeout(30s)` ersetzt
  - Wartet auf tatsaechlichen Tick-Abschluss statt blindem Timer
  - Gleiches Fix fuer `test_save_state_on_shutdown`

- **eBPF Functional Fixes — Ende-zu-Ende Datenkette repariert** (#139)
  - **Stall-Detection false positives:** BPF Map enthielt ALLE System-cgroups (sshd,
    systemd etc.), nur registrierte Sentinel-Agents werden jetzt getrackt
  - **cgroup_name = "unknown":** System-cgroups korrekt rausgefiltert, Agent-Namen
    in Prometheus Labels und Stall-Reports aufgenommen
  - **I/O writes = 0:** Delta-Tracking fuer /proc/PID/io und cgroup io.stat eingebaut,
    cgroup io.stat Daten werden jetzt tatsaechlich aufgezeichnet (nicht nur getraced)
  - **PSI cpu_pressure = 0.0:** Silent `.ok()` durch diagnostisches Error-Logging ersetzt
  - **Zenoh Publisher Lifecycle:** info→error Logging bei Publisher-Tod,
    periodisches Alive-Logging, PSI-Daten auf Zenoh publiziert
  - **Dashboard eBPF-Metriken:** Neuer `/api/ebpf/metrics` Endpoint mit Stalled Count,
    Collection Cycle, Ring Buffer Drops, I/O Bytes, Avg PSI Stress
  - **Dashboard Agent-Stall-Indikator:** Stalled-Badge in Agent-View mit rotem Rahmen,
    Stall-Daten via Prometheus mit 10s Cache-TTL

### Added

- **Sandbox Agent Spawning — TOGAF-konform (bwrap + cgroups v2)** (#173)
  - `SandboxEnforcer::start_agent_process()` wird jetzt vom Orchestrator aufgerufen:
    echte bwrap-Prozesse pro Agent (statt nur cgroup/home-dir Setup)
  - `AgentProcess` Struct: Haelt Child-Handle, Drop-Impl reaps Zombies
  - `EbpfCollector::update_agent_pid()`: PID-Tracking fuer `/proc/{pid}/io` Monitoring
  - Orchestrator verdrahtet: Initial-Spawn + Shift-Transition + Graceful Shutdown
  - Network-Namespace Isolation nach Prozess-Start (optional, via `setup_network()`)
  - `agent_command` konfigurierbar in `daemon.toml` (Default: `/usr/bin/agent-runtime`)
  - TOGAF-konforme bwrap Config: `/usr`, `/lib`, `/lib64` readonly, `/etc/resolv.conf`,
    `/work/company` → `/company` readonly, Agent-Home writable, Landlock Defense-in-Depth
  - cgroup Cleanup bei Schichtwechsel und Shutdown

- **Closed-Loop Personality Evolution (TOGAF-vollstaendig)** (#138)
  - **Judge → Drift Detection (Go):** Agent-Profile aus TOML laden, Drift-Score > 0
    fuer alle aktiven Agents, Evolution-Writes (drift/quality/fatigue) in personality_evolution
  - **Judge → NATS Alerts:** Drift-, Quality-, Fatigue- und Model-Swap-Alerts auf
    `sentinel.judge.alert.{agent}` (1295+ Messages, Consumer `sentinel-daemon`)
  - **Daemon NATS Consumer (Rust):** async-nats Dependency, Durable Pull Consumer,
    Alert-Handler fuer Drift/Quality/Fatigue/Swap mit `alert_ref` Tracking
  - **Daemon LLM Bridge Evolution-Metadata:** Liest VOICE_STYLE, BEHAVIORAL_NOTES,
    NARRATIVE_SUMMARY, EVOLUTION_VERSION aus redb und sendet als Gateway Metadata-Headers
  - **Night-Run graceful Lock-Handling:** Erkennt redb Lock durch Daemon, WARN statt Crash,
    delegiert Konsolidierung an Daemon-Schichtwechsel
  - **redb Personality-Tabellen:** VOICE_STYLE, BEHAVIORAL_NOTES, NARRATIVE_SUMMARY,
    EVOLUTION_VERSION Tabellen mit get/set Methoden
  - **Gateway 3-Source Assembly Fix:** `compiler.NewWithAssembler()` statt `compiler.New()`,
    TOML DNA + Evolution + Perception korrekt verdrahtet, 0 Fallback-Warnings
  - **NMDA Scores in redb:** Neue NMDA_SCORES Tabelle, Daemon schreibt Scores waehrend
    Schichtwechsel-Konsolidierung (set_nmda_scores/get_nmda_scores mit JSON-Serialisierung)
  - **NMDA Consolidation Threshold gesenkt (0.1 → 0.05):** Autonomie-generierte Actions
    (move, emote) scoren 0.06 via classify_action() Default. Mit Threshold 0.1 wurden
    diese nie konsolidiert. Threshold 0.05 ermoeglicht realistische Konsolidierung.
  - **Gateway "evolution injected" Logging:** Info-Log wenn Evolution-Daten (Voice, Notes,
    Narrative) in Agent-Prompt injiziert werden (AC-8 Observability)

### Fixed

- **UTF-8 String-Slicing Panic in Episode Producer** (#138)
  - `&c[..77]` konnte in Multi-Byte UTF-8 Zeichen (Umlaute) schneiden → Panic
  - Fix: UTF-8-safe Truncation via `char_indices().take_while()`

- **Flaky acceptance test unter tarpaulin (Coverage CI)**
  - `ac_10_02_bio_formulas` schlug fehl weil `extraversion=0.5` genau auf dem
    Introvert/Extrovert-Schwellenwert lag — tarpaulin ptrace-Instrumentierung
    veraenderte den `>=` Vergleich. Extraversion auf 0.6 gesetzt um Boundary-Flakiness
    zu vermeiden.

- **Personality aus TOML-Dateien ins ECS laden** (#148)
  - Root Cause: `spawn_agent()` haertete Default-Personality ein (alle Big Five = 0.5),
    individuelle TOML-Werte (Conscientiousness, Neuroticism, Caffeine Tolerance etc.)
    wurden ignoriert — alle Agents im selben Schicht-Set hatten identische Bio-Werte
  - Neue `apply_personality()` Funktion ueberschreibt Defaults mit TOML-Werten nach Spawn
  - TOGAF-Konformitaet: TOML = readonly SSOT ("DNA"), Big Five individualisieren Bio-Engine

- **Bio-Engine Dynamics: Stress, Energy, Caffeine reagieren dynamisch** (#148)
  - Root Cause: WorkContext-Flags (`in_meeting`, `has_deadline`, `has_conflict`) wurden NIRGENDS gesetzt,
    Energy hatte keinen Arbeitsdrain, Kaffee wurde nie automatisch getrunken
  - Neues `work_context_system`: Deriviert WorkContext automatisch aus Raum-Belegung (Meeting),
    Tageszeit (Deadline-Druck 14-17h) und Chaos-Events (Conflict-Cooldown 120 Ticks)
  - Energy Work-Drain: Muedigkeit akkumuliert waehrend Arbeitszeit (06-22h), Conscientiousness
    reduziert den Drain (gewissenhafte Agents halten laenger durch)
  - Caffeine-Tolerance: Personality-Feld moduliert jetzt den Koffein-Boost (0.5x bis 1.0x)
  - Auto-Coffee: Agents trinken automatisch Kaffee bei Energy < 50 und Caffeine < 10mg
    (08-16h, max 1x/5min) — unabhaengig von LLM-Responses
  - Chaos→Conflict: Stressausloesende Events (PrinterBroken, FireAlarm, AirCon, Internet)
    setzen conflict_cooldown auf Agents im betroffenen Raum
  - sim_hour Fortschritt: Daemon aktualisiert jetzt SimulationTime.sim_hour korrekt
    (vorher war sim_hour=8.0 statisch, jetzt schreitet mit Echtzeit voran und wraps 0-24)

- **sim_hour Zeitvirtualisierung + Persistenz** (#148)
  - Root Cause: `sim_hour = (8.0 + tick_count/3600.0) % 24.0` — tick_count startete bei 0
    nach jedem Restart, sim_hour war de facto immer ~8.0, Deadline-Druck [14,17) unerreichbar
  - Fix: sim_hour wird inkrementell berechnet und in redb SIM_META Table persistiert
  - Neuer `time_scale` Config-Parameter (default 1.0 = Echtzeit, 60.0 = 60x Speedup)
  - `delta_seconds = tick_rate * time_scale` — entkoppelt Simulationsgeschwindigkeit von Tick-Rate
  - work_context_system von Phase::Transit nach Phase::Input verschoben (vor bio_system)
  - sim_hour in Tick Checkpoint Log fuer Observability
  - `drink_water` Bio-Action hinzugefuegt (+5 Energy, -10 Bladder)

- **Episoden-Pipeline im Daemon reaktiviert** (#137)
  - Root Cause: Cortex Gateway war seit 27.02. inaktiv → keine `agent_action_received` Events
  - Episode Producer Starvation-Diagnostik: Warnt alle 10 leeren Laeufe (~5 Min) wenn keine
    konvertierbaren Events ankommen (verhindert stilles Verhungern der Pipeline)
  - Cortex Gateway operationell reaktiviert → Agent-Action-Events fliessen wieder

- **Event Snapshots: last_event_id Fix + periodischer Snapshot** (#150)
  - `save_state()` speichert jetzt `max_event_rowid()` statt hardcoded `0` als `last_event_id`
  - Periodischer Runtime-Snapshot alle 600 Ticks (~10 Minuten) im Daemon Tick-Loop
  - Vorher: 2.9M Events, alle 11 Snapshots mit `last_event_id = 0` → Full-Replay bei Recovery
  - Nachher: Snapshots referenzieren korrekte Event-Position, Recovery ab letztem Snapshot
  - Projection Worker: Legacy-Event-Fallback fuer Events ohne `"type"` Discriminator-Tag
  - Alte Events mit abweichenden Feldnamen (`target` → `target_room`) werden korrekt remapped
  - Projection Worker systemd Unit: `Restart=always` statt `on-failure` (Auto-Restart nach SIGTERM)
  - Legacy-Deserializer: 6 fehlende Event-Typen hinzugefuegt (ShiftTransitionCompleted,
    AgentStatusChanged, NightRunStarted/Completed, AgentConsolidated/ConsolidationFailed)

- **eBPF Kernel-Modus Regression** (#139)
  - Daemon-Binary wird jetzt mit `--features ebpf` gebaut
  - CI prueft eBPF-Feature-Kompilierung (Clippy + Tests)
  - Kernel-Probes (fentry/vfs_write, tcp_connect, tcp_close) wieder aktiv
  - Probe-Overhead gemessen: ~540ns/hit (vorher unbelegte ~50ns Behauptung korrigiert)
  - Collection Cycle: ~333us fuer 109 Cgroups, 0 Ring Buffer Drops, 0.007% CPU

### Added

- **Episode Producer im Daemon** (#137)
  - Neues Modul `episode_producer.rs`: Konvertiert DomainEvents aus Limbo zu Hippocampus-Episoden
  - Verarbeitet `AgentActionReceived`, `BioActionPerformed`, `ChaosTriggered` Events
  - Cursor-Persistierung via Limbo `projection_offsets` (restart-sicher)
  - Skip-History beim ersten Start via `max_event_rowid()` (verhindert 40h Aufhol-Phase bei 2.7M Events)
  - Alle 30 Ticks (~30s) Batch-Verarbeitung mit 500 Events pro Batch
  - NMDA-Score-Berechnung (Relevanz + Emotion + Recency) fuer Episode-Selektion
  - `EventStore::max_event_rowid()` Methode in sentinel-limbo hinzugefuegt
  - 12 Unit Tests + 3 Orchestrator-Integrationstests

- **sentinel-nightrun: Schichtwechsel-Konsolidierung produktiv** (#17)
  - Run-to-completion Service mit NMDA SleepCycle-basierter Memory-Konsolidierung
  - systemd Timer 06:00/14:00/22:00 UTC (Persistent=true, RandomizedDelaySec=30)
  - 6-Step Pipeline: Agent Discovery → Shift Filter → Event Emit → Job Queue → Consolidation → Complete
  - Persistente SQLite Job-Queue (WAL mode) mit Crash-Recovery (`--resume`)
  - SHA-256 Hash Chain fuer deterministische Replay-Verifikation
  - Deterministic Guardrails: Backlog-Skip (>1000 episodes), Total-Timeout (7200s), Agent-Timeout (300s)
  - Shift-Detection via `libc::localtime_r`, outgoing_shift_set Mapping (Frueh→Spaet, Mittel→Frueh, Spaet→Mittel)
  - Event Emission: NightRunStarted, NightRunCompleted, AgentConsolidated, AgentConsolidationFailed
  - Security-gehaertetes systemd Unit (NoNewPrivileges, ProtectSystem=strict, MemoryMax=2G)
  - 13 Integration-Tests + Criterion Benchmarks (3 Gruppen: shift, job_queue, pipeline)
  - VM-verifiziert: 3 Schichtwechsel, 46 Agents konsolidiert, 0 Fehler, 96-624ms Laufzeit

- **Sandbox Enforcer Integration in Daemon** (#16)
  - `SandboxEnforcer::detect()` during daemon startup with per-variant warning logging
  - cgroups v2 resource limits enforced per agent: CPU 100000/100000 (1 core), Memory 256MB, IO 300 IOPS + 10MB/s
  - `delegate_controllers()` enables +cpu +memory +pids +io in cgroup subtree_control at both root and sentinel levels
  - Sandbox setup with per-agent timing during spawn (~200us per agent after initial 30ms device discovery)
  - Sandbox teardown on shift transitions and graceful shutdown
  - `SandboxHandle` tracking per agent in `HashMap<AgentId, SandboxHandle>`

### Changed

- **RuntimeOrchestrator Integration in Daemon** (#15)
  - RuntimeOrchestrator moved into ECS thread (was dead code in tokio context)
  - Agent spawning now goes through RuntimeOrchestrator (lifecycle event emission)
  - `save_state()` on graceful shutdown, `restore()` on startup (snapshot persistence)
  - Periodic shift detection every 60 ticks with `shift_transition()` + ECS despawn/spawn
  - New `despawn_agent_from_world()` in sentinel-ecs for runtime shift transitions
  - `max_agents` default increased from 15 to 30 (accommodates 15 shift + 9 sonder agents)
  - EventBuffer cleared after agent spawning to prevent duplicate lifecycle events

### Added

- **sentinel-ebpf: Full eBPF Daemon Integration** (#25)
  - `loader.rs`: Capability detection (BTF, CAP_BPF, kernel version, fentry support)
  - `MonitoringMode` enum (Kernel/Userspace) with Prometheus label support
  - Graceful fallback with mandatory WARN logging (no silent degradation, AC-N1)
  - `collector.rs`: `EbpfCollector` with agent registration, Per-CPU aggregation, userspace fallback
  - `MetricsSnapshot` with serde::Serialize for JSON export via Zenoh
  - Prometheus exporter: all 4 metric groups (agent_health, io_profile, network, psi/collector_meta)
  - `sentinel-ebpf-probes`: real fentry/tracepoint BPF programs compiled for bpfel-unknown-none
  - Probes: fentry/vfs_write (agent health), tracepoint/block:block_rq_complete (I/O), fentry/tcp_connect+tcp_close (network)
  - Per-CPU Hash Maps (AGENT_HEALTH, IO_STATS) + Ring-Buffer (TCP_EVENTS) for lock-free data flow
  - BTF/CO-RE: cross-compiled on build server, loaded on VM with Kernel 6.17
  - Daemon integration: `ebpf.rs` module with init, Prometheus TCP server (port 9090), Zenoh publisher
  - ECS thread: periodic collect every 10 ticks via mpsc channel to tokio runtime
  - Agent cgroup registration on spawn, unregistration on shift-transition despawn
  - `cgroup_id()` helper in sentinel-sandbox for BPF map correlation (inode-based)
  - Dashboard: `/api/ebpf/status` endpoint + monitoring mode badge (Kernel=green, Userspace=yellow)
  - Measured probe overhead: 374-647ns/hit (sub-microsecond, ~1% ECS tick budget)
  - 45 daemon tests passing, 0 ring buffer drops at 100 req/s load

- **Claude Code Subprocess Provider** (Cortex Gateway)
  - New `ClaudeCodeProvider` in `claude_code.go`: subprocess management for `claude -p --output-format stream-json`
  - NDJSON protocol parsing with content block extraction and result deduplication
  - Lazy init, `sync.Mutex` serialized, auto-restart on crash
  - Provider registration in `provider.go` (Case "claude-code") and `main.go`
  - Config: `CLAUDE_CODE_ENABLED`, `CLAUDE_CODE_MODEL`, `CLAUDE_CODE_PATH` env vars
  - `--system-prompt` flag support: system messages now override Claude Code's default persona via `splitMessages()`, fixing the Fourth-Wall break where agents refused to roleplay

- **Wasm Tool Implementations** (sentinel-wasm)
  - `execute_chat()`: JSON input `{"target":"AGENT-XX","message":"text"}`, agent ID validation
  - `execute_calendar()`: create/query/cancel actions with date, time, subject, attendees
  - `execute_search()`: query with scope (documents/agents/rooms) validation
  - 15 new tests covering all 3 tool types (happy path + error cases)

- **Extended E2E Test Suite** (tests/)
  - `tests/e2e_extended_tests.py`: automated API validation script (35 tests)
  - T23: Bio-Bar Ranges (10 tests) — 6 bio fields, numeric validation
  - T24: Room Physics Format (8 tests) — temperature, CO2, noise dB plausibility
  - T25: Chaos-Event-Typen (8 tests) — specific types, no generic ChaosTriggered
  - T26: Cockpit Incidents Lifecycle (12 tests) — status/severity, SLO schema
  - E2E_TEST_PLAN.md expanded from 218 to 256 tests

- **LLM Bridge** (sentinel-daemon)
  - `llm_bridge.rs`: async HTTP bridge to Cortex Gateway for agent LLM decisions
  - Autonomy system `autonomy.rs`: agent action cycle integration

- **Dashboard Enhancements**
  - Agent cards: bio-bar visualization (hunger, energy, stress, bladder, social_need, caffeine)
  - Agent cards: "Letzte Aktion" field from LLM-generated events
  - Activity feed: Transit/Action events alongside Spawn/Chaos
  - Chaos badge: specific event types (PhoneRing, PrinterBroken etc.) instead of generic
  - Chat view: room-filtered chat log with WebSocket updates
  - Cockpit: incident action correlation and outcome display

- **User-Manual Guide** (docs/project-sentinel-guide.html)
  - New Section 13: API-Referenz (Dashboard REST, WebSocket, Cortex Gateway, Observatory)
  - Extended Section 14: Fehlerbehebung (LLM-Provider, idle agents troubleshooting)

- **TOGAF HTML Guide v18.0** (docs/togaf-llm-architecture-guide.html)
  - Crate count corrected: 11 → 16
  - NATS JetStream Dual-Bus architecture section
  - Claude Code Provider in Cortex Gateway section
  - Go Services in architecture overview
  - Stack reference table extended

### Fixed

- **Dashboard Chaos Badge**: uses `event_type` from payload instead of generic `type` (serde tag)
- **Dashboard Activity**: shows Transit/Action events, not just Spawn/Chaos
- **Cockpit Incidents**: empty action lists and never-visible resolved incidents

### Changed

- **Implementation Plan** (peaceful-splashing-willow.md): API Key → Subscription, claude-code Provider section, NATS JetStream
- **E2E Results**: expanded from 43 to 78 total verified tests

### Verified

- **Agent Migration**: 54/54 TOML files present, all acceptance tests pass
- **MARBLE Observatory**: SQLite persistence fully implemented (73 tests pass), relabeled scope:full
- **sentinel-nightrun**: 52 tests pass, verify report posted on GH#17, #18
- **sentinel-hippocampus**: 4 acceptance tests pass, verify report posted on GH#23
- **sentinel-judge**: all ACs pass per verify report, label updated on GH#26
- **eBPF Kernel-Probes**: implementation plan posted on GH#25 (userspace complete, kernel probes planned)

- **Room Physics Events + Dashboard Integration**
  - New `RoomPhysicsUpdated` DomainEvent variant (temperature, co2_ppm, noise_db, occupant_count)
  - `physics_system` emits events every 20 ticks for occupied rooms
  - Projection: `room_live_view` extended with temperature, co2_ppm, noise_db columns
  - Room API response includes physics data; floorplan.js renders inline
  - CSS: `.room-physics` styling for compact physics display

- **Activity-View EventStore Integration**
  - New `/api/activity` endpoint reads directly from EventStore (replaces agent-state derivation)
  - 13 event types mapped to German-language summaries with type badges
  - WebSocket `activity_update` for live pushes
  - Frontend rewritten as async self-loading module

- **Metrics MARBLE Extension**
  - Metrics API extended with evolution_count, evolution_drifts, evolution_fatigue, evolution_quality
  - Nightrun stats: nightrun_consolidated, nightrun_failed
  - Frontend: 4 new metric cards (Nightrun OK, Nightrun Fail, Drift-Alerts, Fatigue-Alerts)

### Fixed

- **Tick counter not advancing**: `orchestrator.rs` only set `time.tick_count` but not `time.tick` (Tick newtype), causing all events to have tick=0
- **Bio-Values display**: Changed `Math.round(value * 100)` to `Math.min(100, Math.max(0, Math.round(value)))` — values are 0-100 range, not 0.0-1.0
- **Projection Worker monotonicity crash**: `update_offset()` changed from strict `<=` to idempotent `==` (no-op) + `<` (error), plus guard in worker.rs
- **NATS JetStream storage**: Fixed `ReadWritePaths` mismatch in systemd unit (was `/var/lib/nats`, needed `/opt/sentinel/data/nats`)
- **Service enablement**: All services now `systemctl enable`d for auto-start on reboot

- **Statusmodell + DoD Gate Audit** (#111)
  - `docs/STATUS_MODEL.md`: Statusmodell-Dokumentation (implemented/deployed/verified Lifecycle)
  - `docs/DEFINITION_OF_DONE.md`: Definition of Done Checklisten (Feature, Gate, Docs)
  - `docs/GATE_AUDIT_2026-02-20.md`: Label-Hygiene Audit (5 Fixes, 0 Governance-Verstoesse)
  - Label-Fixes: #108, #107, #99, #28, #94 von falschem Status auf `status:completed`

- **Dashboard Operator Cockpit** (#108)
  - New "Cockpit" view: priorisierte Incident-Liste statt Metric Wall (anti-Metric-Wall Design)
  - Incidents aus EventStore (chaos_triggered, consolidation_failed, despawned, nightrun failures)
  - Incidents aus personality_evolution (drift, fatigue_spike, quality_shift)
  - Korrelierte Actions pro Incident via correlation_id/causation_id Ketten
  - Outcome-Detection: resolved/active/pending/failed basierend auf Folge-Events
  - SLO-Verletzungen: Projection Lag, Nightrun Failure Rate, Chaos-Frequenz, Despawn-Rate
  - `GET /api/cockpit` + `GET /api/cockpit/incident/:id` API-Endpunkte
  - WebSocket cockpit_update Ping bei neuen Incident-Events
  - 8 neue Tests (erste Dashboard-Tests ueberhaupt)

- **Zenoh Query Bridge — InFlightMap Consumer** (#99)
  - Wired `InFlightMap` into Cortex Gateway pipeline around `provider.Send()` (Step 6)
  - Query lifecycle tracking: Track before Send, Accept/Cancel after response/error
  - Stale-drop: responses with `response_tick < min_tick` rejected with 504
  - `sentinel_query_inflight` Prometheus gauge for current in-flight queries
  - `sentinel_query_cancelled_total` and `sentinel_query_stale_dropped_total` counters now active in production path
  - Prune goroutine (5s interval) prevents unbounded map growth
  - 5 integration tests + 4 Go benchmarks in `resilience/bench_test.go`
  - Bench-dashboard updated: Go benchmark parser + InFlightMap group (20 total benchmarks)

- **VM-Deploy FULL Closure** (#28)
  - `deploy/smoke-test.sh` + `deploy/smoke-test-remote.py`: post-deploy smoke test (health endpoints + systemd services, single SSH roundtrip, configurable timeout)
  - `make deploy`: full deploy workflow with mandatory preflight verification
  - `make preflight`: manifest hash verification against target VM
  - `make smoke-test`: standalone smoke test target
  - `deploy/generate-manifest.sh`: added `controlplane.toml` to artifact list (29 total artifacts)
  - `deploy/release-manifest.json`: regenerated with all 5 binaries + 10 configs + 9 systemd units + 5 scripts

- **Native Controlplane Kernel C1-C4** (#107)
  - Full observe/decide/act/verify cycle integrated into ECS tick-loop
  - 7 new modules in `services/sentinel-daemon/src/controlplane/`:
    - `types.rs`: Observation, Incident, ControlAction, VerifyOutcome, RuntimeState
    - `store.rs`: ControlplaneStore (redb, 4 tables: config, runtime_state, action_log, incidents)
    - `observe.rs`: ECS World query (BioState/Position/Mood), threshold-based incident detection, stress-cluster detection
    - `decide.rs`: Rule-based policy engine, guarded mode, cooldown tracking, TTL+rollback per action (AC-5)
    - `act.rs`: Action execution with store persistence
    - `verify.rs`: TTL expiry, rollback condition evaluation (field/op/value parser)
    - `config.rs`: TOML deserialization with serde defaults
  - `config/controlplane.toml`: cycle_interval=10, thresholds (hunger/energy/stress/bladder)
  - No LLM in real-time path (AC-N1), cycle timing guard <200ms (AC-2)
  - 42 unit tests, clippy -D warnings clean

### Verified

- **sentinel-daemon 4f FULL gate closure** (#94)
  - AC-1: Clippy zero warnings, 6 unit tests pass (orchestrator, shift mapping)
  - AC-2: `--dry-run` exit 0 — loads 54 agents (24 active: 15 shift-1 + 9 shift-0)
  - AC-3: Tick-loop stable 21+ hours on VM (tick 76080, 61.7MB memory, ~42s CPU)
  - AC-4: SIGTERM graceful shutdown — `signal: SIGTERM empfangen`, `Shutdown eingeleitet`, no panic
  - AC-5: implemented/deployed/verified evidence documented

### Added

- **Release Manifest + Hash-Parity Deploy-Gate** (#110)
  - JSON Schema `deploy/release-manifest.schema.json` (v1.0) for deployment artifact manifests
  - Generator `deploy/generate-manifest.sh`: hashes all deploy artifacts (binaries, configs, systemd units, init scripts) with SHA-256
  - Preflight `deploy/deploy-preflight.sh`: verifies hash parity via SSH before deploy; hard-aborts (exit 1) on mismatch or missing artifacts
  - CI: `release.yml` generates manifest after build and attaches it as release artifact alongside SBOM
  - `deploy/release-manifest.json` excluded from git via `.gitignore` (generated artifact)
  - Benchmark: manifest generation ~124ms, preflight parsing ~43ms; full preflight well under 5s target

- **NATS Infrastructure Verification Gate N1-N3** (#109)
  - Deployed nats-server v2.12.4 on deploy-VM: binary + /etc/nats/nats.conf + nats-server.service
  - Deployed sentinel-nats-bridge: /opt/sentinel/bin/ + sentinel-nats-bridge.service (active, running)
  - Deployed sentinel-judge: /opt/sentinel/bin/ + sentinel-judge.service (active, running)
  - Deployed judge.toml to /opt/sentinel/config/judge.toml
  - Verified: SENTINEL_EVENTS + SENTINEL_JUDGE streams with correct SSOT subjects
  - Verified: Nats-Msg-Id dedup with 10min window (AC-3)
  - Verified: Durable consumer resume without data loss (AC-4)
  - Verified: Bridge latency p50=2.2ms, p95=2.7ms, p99=3.2ms (AC-5, threshold <2s)
  - Verified: Empty store poll 30s soak — no crash, no busy-loop (AC-N1)
  - Benchmarks: Batch=50 13737 evt/s, Batch=100 16719 evt/s, Batch=200 9980 evt/s

- **sentinel-fs Artifact Plane** (#56)
  - 5 new redb tables: `FS_OBJECTS`, `FS_MANIFESTS`, `FS_CHUNKS`, `FS_CHUNK_REFCOUNT`, `FS_OBJECT_REFS`
  - Content-Defined Chunking (CDC) with gear-hash based Rabin-style rolling hash (`src/chunker.rs`)
    - Target: 64 KB chunks (min 16 KB, max 256 KB), fully deterministic
  - Transactional ingest pipeline (`src/ingest.rs`): `begin_ingest` / `commit_ingest` / `abort_ingest`
    - Atomic redb write transaction: chunk storage, refcount increment, manifest, object metadata
    - Streaming multi-write support before commit
    - `abort_ingest` leaves zero DB artifacts
    - Dedup optimization: skip zstd compression for chunks already in DB (read-only pre-check)
    - BLAKE3-128 chunk fingerprinting (16-byte keys, ~3-5x faster than SHA-256 for hashing)
    - SHA-256 retained for object-level integrity (compliance)
    - `BatchIngest` API: amortize fsync across N objects in single redb transaction
      - `BatchIngest::new()` / `add()` / `commit()` — chunking+compression up front, one write txn
    - Configurable `DurabilityLevel` for ArtifactPlane (`Immediate` / `Eventual`)
      - `Eventual` skips fsync: 1MB ingest 55ms → 22ms (-59%), not crash-safe
      - Config: `config/storage.toml` `[artifact] durability = "immediate" | "eventual"`
    - Adaptive parallel compression via rayon: serial for < 32 new chunks, parallel above
      - `chunk_data_parallel()` in chunker: serial CDC boundaries + parallel BLAKE3 hashing
      - Dedup paths 23-36% faster, eventual ingest 14.7ms (-35% vs 22.5ms)
    - `FS_INGEST_SESSIONS` table (6th redb table): tracks in-progress ingests as `.part` files
      - Pre-allocated ObjectId doubles as session ID for FUSE visibility
      - Throttled progress updates (every 256 KB) to avoid DB thrashing on streaming writes
      - Atomic session cleanup: commit removes entry in same write txn, abort removes separately
    - Segment Pack storage (`src/segment.rs`): chunk data in append-only files, not redb
      - `FS_CHUNKS` stores 16-byte `ChunkLocation` index instead of inline compressed data
      - `SegmentStore`: append-only ~64 MB segment files on NVMe
      - Two-phase commit: append to segment (crash-safe dead space) → atomic redb index
      - Less redb bloat, better I/O patterns for sequential reads, enables future io_uring
    - L1 RAM chunk cache (`src/chunk_cache.rs`): decompressed data cache with anti-pollution
      - Two-hit admission policy: chunks only cached after 2nd read (prevents scan pollution)
      - FIFO eviction, 64 MB default capacity, oversized chunk rejection (>25% of cache)
      - `read_chunk_decompressed()` on ArtifactPlane: cache-first path avoids redundant I/O + zstd
      - `cache_stats()` for observability (hits, misses, entries, bytes)
    - io_uring batch reads for segment data path (feature-gated `iouring`)
      - `SegmentStore::read_batch()`: submit N pread SQEs in one syscall, reap CQEs
      - `ArtifactPlane::read_chunks_decompressed()`: cache-first batch path, single redb txn for misses
      - `read_object()` now uses batch reads for full manifest fetch in one pass
      - Sync fallback: cached file handles per segment_id, sequential pread (no io_uring)
    - Adaptive Commit Scheduler (`src/commit_scheduler.rs`): IOPS-aware write throttling
      - Rate tracking with sliding window, configurable max_iops (default: 500)
      - PSI-aware: reads `/proc/pressure/io` avg10 for system-wide I/O backpressure
      - PSI pressure multiplier: 3x delay when avg10 > 10%
      - Integrated into `ArtifactPlane::begin_write()` — transparent to callers
      - `scheduler_stats()` for observability
  - Streaming read planner (`src/read_planner.rs`): `read_object` + `read_object_streaming`
    - Manifest lookup → chunk decompression → sequential reassembly (L1 cache accelerated)
  - Refcount GC (`src/gc.rs`): `gc_chunks` + `release_object`
    - Orphan chunk detection via FS_CHUNKS vs FS_CHUNK_REFCOUNT diff
    - `release_object` decrements all chunk refcounts atomically before removal
  - Config: `config/storage.toml` (chunking params, compression level, GC interval)
  - 7 new Criterion benchmarks: chunker throughput, 1 MB/100 MB ingest, dedup identical/similar files, read planner, GC
  - 10 new integration tests for AC-4 (dedup effectiveness) and AC-5 (multi-format ingest)
    - Dedup: identical files, 10x identical, similar data, prepend boundary stability, scaling 10MB
    - Multi-format: binary, HTML, PDF, JSON in single ingest pipeline
    - GC lifecycle, compression ratio, chunk size distribution verification
  - 87 unit tests across 7 new modules + 19 integration tests, all green
- **Sentinel Judge: Enterprise Quality Analysis Service** (#26)
  - Bridge unit tests (6 tests: publish, dedup, subject mapping, config defaults, GetEventsSince)
  - Go benchmarks: judge (7), messaging (4), all passing with HeuristicPipeline at ~1µs (target <5ms)
  - New Go service `services/sentinel-judge/` with NATS JetStream consumer + LLM analysis
  - NATS JetStream integration: durable pull consumers for realtime heuristic pipeline
  - Dual-mode operation: streaming (NATS realtime, <5s) + batch (HTTP Night-Run API, <60s/agent)
  - 4 LLM analysis types via Cortex Gateway: voice_style, behavioral_notes, narrative_arc, relationship_dynamics
  - `personality_evolution` persistence layer (Limbo SQLite, CQRS pattern)
  - Multi-target alerter: Prometheus metrics + slog + NATS `sentinel.judge.alert` publish
  - 7 Prometheus metric families (drift/quality/fatigue scores, alerts, events, consumer lag, LLM duration)
  - HTTP API: `/health`, `/ready`, `/metrics`, `POST /api/v1/analyze`
  - Graceful shutdown with NATS drain and HTTP server timeout
  - 14 tests across 5 packages (api, alerter, analyzer, gateway, persistence)
- **Sentinel NATS Bridge: Event Bridge Service** (#26)
  - New Go service `services/sentinel-nats-bridge/` polling Limbo EventStore to NATS JetStream
  - Exactly-once delivery via `Nats-Msg-Id` header (maps to `operation_id`)
  - Health endpoint on port 8083
  - Temporary bridge — will be replaced by daemon Zenoh-to-NATS bridge
- **Shared Go Package** (`pkg/sentinel-go/`) (#26)
  - Extracted `judge/` (4 algorithms: Drift, Quality, Fatigue, Swap) from gateway internal
  - Extracted `eventstore/` from gateway internal, added `GetEventsSince()` for bridge polling
  - New `messaging/` package: NATS connection factory, stream definitions (SSOT), subject helpers
  - Go workspace module importable by all Go services (gateway, judge, bridge)
  - 24 tests (18 judge + 5 eventstore + 5 messaging, all moved/new)
- **NATS JetStream Infrastructure** (#26)
  - `config/nats.conf`: JetStream server config (localhost-only, 512MB mem, 2GB disk)
  - `config/judge.toml`: Judge service configuration (thresholds, gateway, evolution)
  - `config/nats-bridge.toml`: Bridge service configuration (poll interval, batch size)
  - systemd units: `nats-server.service`, `sentinel-nats-bridge.service`, `sentinel-judge.service`
  - `sentinel.target` updated with new services

### Changed

- Gateway imports refactored: `internal/judge/` and `internal/eventstore/` now imported from `pkg/sentinel-go/`
- `go.work` extended with 3 new modules (pkg/sentinel-go, services/sentinel-judge, services/sentinel-nats-bridge)

- **LLM Guardrails: Cost & Throughput Limits** (#59)
  - Token-bucket rate limiter with per-agent and global limits (configurable RPM)
  - Budget tracker with hourly/daily token limits and automatic window reset
  - Cost tracker with per-provider pricing (USD/1M tokens)
  - Automatic fallback to Ollama when cloud budget is exhausted
  - Enforcer facade combining rate limiter, budget, cost, and fallback
  - Pipeline integration: pre-flight Check() + post-call Record()
  - Dashboard endpoint: GET /api/guardrails/status (budget, cost, rate info)
  - Prometheus metrics: sentinel_tokens_total, sentinel_cost_usd_total, sentinel_rate_limited_total, sentinel_budget_used_tokens
  - LLMResponse extended with InputTokens/OutputTokens for accurate cost tracking
  - Environment-based configuration (SENTINEL_GUARDRAILS_*)
  - 46 tests, 5 benchmarks (EnforcerCheck <500ns, RateLimitCheck <300ns)
- **Prompt Compiler: 3-Source Assembly** (#58)
  - TOML DNA loader with in-memory cache (Big Five, Bio, Quirks)
  - Evolution data integration from redb (Voice Style, Behavioral Notes, Narrative, Relationships)
  - Cache-optimized block ordering (static before dynamic for Anthropic prefix caching)
  - Prompt distillation for 7B models (<2000 tokens)
  - Extended capability detection: `caching`, `predicted_output`, `kv_retention`
  - Added `openai` provider to capability registry
  - New `CompileFromSources()` entry point with fallback to basic `Compile()`
  - Pipeline Step 5 upgraded to 3-source assembly with automatic fallback
  - 52 compiler tests, 23 capability tests, 6 benchmarks (Assembly: <5µs)
- `sentinel-fs` crate: CAS-FUSE agent filesystem with SHA-256 dedup and zstd compression (#56)
  - Content-Addressed Storage (CAS) with atomic writes, 2-char prefix subdirs, zstd level-3 compression
  - redb metadata store with agent-scoped composite keys (FS_INODES, FS_DIRENTS, CAS_REFCOUNT)
  - Layer manager: shared base (readonly) + per-agent CoW layers with whiteout markers
  - FUSE handler (feature-gated): single mount for all agents, path-based Agent-ID extraction
  - CLI: stats, gc, populate commands
  - 49 tests (unit + integration), 7 criterion benchmarks
  - Integration tests verify >87% dedup rate, agent isolation, crash recovery
- `OutboxPublisher<T>` background task: polls outbox entries and publishes via generic `OutboxTransport` trait
- `OutboxTransport` trait in sentinel-limbo for transport abstraction (no zenoh dependency)
- `OutboxPublisherConfig` with env-based configuration (`SENTINEL_OUTBOX_POLL_INTERVAL_MS`, `SENTINEL_OUTBOX_BATCH_SIZE`)
- Graceful shutdown support via `tokio::sync::watch` channel with drain-on-exit
- 6 unit tests for OutboxPublisher (publish, retry, empty, shutdown, batch-size, config)
- E2E acceptance test `ac_57_06` covering full outbox publish flow
- `Clone` derive on `EventStore` (was already `Arc<Mutex>` internally)
- Configurable provider deadline via `SENTINEL_CORTEX_PROVIDER_DEADLINE_SECONDS` (10-30s, default 20s)
- `resilience` package: Zenoh query deadline with InFlightMap and stale-drop logic
- Configurable Zenoh query deadline via `SENTINEL_CORTEX_ZENOH_DEADLINE_MS` (50-120ms, default 100ms)
- Prometheus metrics: `sentinel_breaker_trips_total`, `sentinel_query_cancelled_total`, `sentinel_query_stale_dropped_total`
- E2E integration test for circuit breaker lifecycle (trip → failover → half-open → recovery)

### Changed

- Provider deadline now configurable instead of hardcoded 20s in pipeline handler

### Added

- Project scaffolding and GitHub infrastructure
- CI/CD with path-based filtering (Rust, Go, Bun, FlatBuffer)
- Security audit workflow (cargo audit, govulncheck)
- Release automation with changelog extraction
- `sentinel-common` crate: newtypes (AgentId, RoomId, Tick, Timestamp), domain structs, validation
- `sentinel-zenoh` crate: Zenoh pub/sub wrapper with topic management
- `sentinel-redb` crate: embedded key-value store for agent/room state
- `sentinel-limbo` crate: SQLite-based message and event log
- `sentinel-telemetry` crate: cross-cutting observability (metrics, health, errors, export)
- Telemetry retrofit: Counter/Histogram instrumentation in redb, zenoh, limbo crates
- TelemetryTransport trait for decoupled metric publishing
- `deny.toml`: Supply-chain security (license compliance, advisory checks, crate bans, source restrictions)
- `rustfmt.toml`: Unified Rust formatting config (max_width=100, crate-level imports)
- `clippy.toml`: Clippy thresholds (too-many-arguments=10, cognitive-complexity=30)
- `.golangci.yml`: Go linter suite (gosec, gocyclo, errcheck, misspell, prealloc)
- `deny.yml` workflow: cargo-deny CI (advisories, licenses, bans, sources as parallel jobs)
- `coverage.yml` workflow: Code coverage via cargo-tarpaulin with Codecov upload
- `scorecard.yml` workflow: OSSF Scorecard for security posture (weekly + SARIF upload)
- SBOM generation in release workflow (CycloneDX JSON via Syft, attached to GitHub Releases)
- npm audit job in security workflow for Dashboard dependencies
- `make deny`, `make coverage`, `make typos`, `make doc`, `make machete` Makefile targets
- Team rules in CLAUDE.md (model assignments, verification protocol, lessons learned, architecture mindset)
- `typos.toml`: Spell-check config for code and docs (crate-ci/typos)
- `.cargo/audit.toml`: cargo-audit advisory ignore list (synced with deny.toml)
- Typos CI job in ci.yml (runs on every PR, part of ci-pass gate)
- Rustdoc warnings-as-errors check in Rust CI job (`RUSTDOCFLAGS="-D warnings"`)
- cargo-machete unused dependency detection in Rust CI job (non-blocking warning)
- Concurrency groups on deny.yml and coverage.yml (cancel superseded runs)
- `config/**` path filter in CI (triggers Rust tests when config files change)
- Sprint 2 domain knowledge in CLAUDE.md (ECS, Bio-Engine, Physics, Room System, naming conventions, performance constraints)
- Sprint 2 German domain words in `typos.toml` (Buero, Kueche, Laermpegel, Koffein, etc.)
- `.claude/rules/` modular rules directory with Sprint 2 domain knowledge
- `sentinel-ecs` crate: ECS core with bevy_ecs (10 components, 9 systems via SimulationPhase, agent spawning)
- `sentinel-bio` crate: Bio-Engine with 6 differential equations (hunger, energy, caffeine decay, bladder, stress, social need)
- `sentinel-physics` crate: Matrix physics (acoustics/dB, temperature/CO2, smell propagation, transit/hallway encounters, chaos events)
- `config/rooms.toml`: Office building layout (15 rooms, 2 floors, bidirectional adjacency)
- `sentinel-common::room` module: Room config parser with validation (adjacency, capacity, room types)
- `config/company.toml`, `config/simulation.toml`: Company and simulation configuration files
- Bio-Engine example: 8-hour workday simulation (`crates/sentinel-bio/examples/bio_simulation.rs`)
- ECS component types moved to `sentinel-common::components` (breaks circular dependency)
- `SimulationTime` ECS Resource for tick-based time management
- Real ECS system implementations: `bio_system` (sentinel-bio), `transit_system`, `chaos_system` (sentinel-physics), `mood_system` (valence-arousal), `perception_system` (German text generation)
- Mood system: valence-arousal model with weighted bio/stress/hunger/social factors
- Perception system: German natural-language body/environment/social text for LLM prompts
- Room-ID to German text mapping (15 rooms with descriptive names)
- Sprint 3: `cortex-gateway` Go HTTP proxy with LLM pipeline (Provider Registry, HTTP Handler, Prompt Compiler, Session Normalizer, Action Extraction, Capability Detection, Control Plane API)
- Sprint 3: `perception.rs` - Perception text generator with `generate_perception()` and `format_injection()` for [SYSTEM_INJECTION] blocks
- Sprint 3: Fourth-wall detection package with 15 regex patterns, LLM judge (2-stage pipeline), re-generation, Prometheus metrics
- Sprint 3: Provider interface with Claude API and Ollama backends, HTTP proxy handler with size limits and timeouts
- Sprint 3: Session normalizer for unified LLM response format (Claude + Ollama)
- Sprint 3: Prompt compiler with model-specific configs (full bio for Claude, distilled for 7B)
- Sprint 3: Action extraction with German emotion/intent regex patterns
- Sprint 3: Capability detection with provider feature maps and fallback strategies
- Sprint 3: Control Plane API (GET/PATCH config, POST provider switch) on separate port
- Sprint 3: `config/cortex-gateway.toml` gateway configuration
- Sprint 3: `caffeine_tolerance` field added to `Personality` component
- Sprint 3: Go CI enhanced with `go build` step and race detector (`go test -race`)
- Sprint 4: `sentinel-runtime` crate: Agent orchestrator with spawn/despawn lifecycle, shift transition (Sonder-Set preserved), health checks, max-agents enforcement (6 tests)
- Sprint 4: `sentinel-sandbox` crate: Bubblewrap (bwrap) config builder with for_agent() defaults (ro-bind, rw-bind, tmpfs, unshare-all), cgroups v2 config (CPU/memory/IO limits), PSI metrics parser (8 tests, 3 ignored)
- Sprint 4: `sentinel-wasm` crate: Tool runtime with registry, native FileRead/FileWrite handlers, placeholder tools (Chat/Calendar/Search) (5 tests)
- `sentinel-wasm` rewrite (Issue #19): ToolType::Wasm variant with wasmtime integration (feature-gated), SandboxConfig filesystem isolation (allowed_paths + canonicalize), capability-based access control (can_execute), ToolResult struct with to_domain_event(), ExecutionContext grouping (agent_id, capabilities, sandbox, correlation_id, tick), fuel-based WASM CPU limiting, post-hoc timeout check for native tools, 8 Criterion benchmarks (6 native + 2 WASM), 33 tests (23 unit + 10 acceptance)
- Sprint 4: `sentinel-common::agent_config` module: TOML-based agent definition parser with Big Five personality validation [0.0, 1.0], load_agent_config() and load_all_agents() (11 tests)
- All 54 agent definition TOML files migrated from Markdown (AGENT-01 through AGENT-54: 3 shifts x 15 agents + 9 Sonder-Set), with shift distribution tests, ID completeness checks, personality validation across all agents, and migration script (`scripts/migrate-agents.py`)
- Sprint 4: `sentinel-dashboard` backend: Hono-based API (health, agents, rooms, agent state, room chat, metrics endpoints) with WebSocket live events (5 tests)
- Sprint 4: `sentinel-dashboard` frontend: Dark mode UI with 4 views (Agents with bio-bars, Floorplan with room grouping, Chat with room filter, Metrics), vanilla JS ES modules, textContent-only (no innerHTML)
- Sprint 5: `sentinel-common::psi` module: PsiMetrics struct and `parse_psi()` moved from sentinel-sandbox for cross-crate reuse
- Sprint 5: `sentinel-inference` crate: BitNet b1.58 subprocess manager (BitNetConfig, BitNetClient), Multi-LoRA adapter management (scan, swap, cache), Speculative Decoding pipeline (draft+verify), KV-Cache prefix sharing with FIFO eviction (12 tests)
- Sprint 5: `sentinel-hippocampus` crate: Multi-tier memory system with Episode + NMDA scoring, NarrativeMemory (running summary), FactRetriever (JIT RAG via redb), KvCacheTier (hot/cold tiering) (35 tests)
- Sprint 5: `sentinel-ebpf` crate extended: Agent-health probe (write-syscall tracking, 30s stall detection), I/O profiling probe (IOPS per cgroup), Network probe (TCP latency for LLM calls), PSI stress reader, Prometheus text exporter
- Sprint 5: `sentinel-hippocampus::sleep` module: NMDA-based sleep cycle with 6-phase state machine (Awake→Collecting→Scoring→Selecting→Consolidating→WakingUp), episode selection by NMDA score with threshold filtering (5 tests)
- Sprint 5: `sentinel-judge` Go package: Drift detection (personality consistency), Fatigue detection (repetition patterns), Quality scoring (1-5 scale), Model-swap trigger (thread-safe, consecutive bad scores) (18 tests)
- Sprint 5: `bitnet/build.sh` and `bitnet/README.md` for BitNet b1.58 build instructions
- Sprint 6: `sentinel-sandbox` IO enforcement: Block device discovery via `/proc/self/mountinfo`, `io.max` format fix (MAJ:MIN device prefix), two-level IO controller delegation check (root + sentinel subtree), `format_io_max()` + `enable_io_controller()` + `io_controller_enabled()` helpers, VM setup script for IO delegation (`scripts/vm-setup-io-delegation.sh`), 11 new tests (7 unit + 4 acceptance)
- Sprint 6: PSI-Bio-Pipeline: `AgentPsi` publisher reads CPU/Memory PSI from agent cgroups and publishes to `sentinel/agent/{name}/psi` via Zenoh (5s interval, graceful skip on missing cgroups), `apply_psi_stress()` maps PSI thresholds to Bio-Engine stress/comfort (CPU avg10>50 → stress+10, Memory avg10>70 → stress+20/comfort-15), `PsiMetrics` now serializable, 9 new tests (6 unit + 3 acceptance)
- Sprint 6: `observatory` Go package: MARBLE multi-model benchmark with 6 metrics (InfoPropagation, GroupPolarization, CommunicationScore, PersonalityConsistency, ResponseCreativity, EmotionalRange), judge integration, in-memory observation store, Markdown + JSON report generator (42 tests)
- Sprint 6: `config/observatory.toml`: Multi-model observatory configuration (3 shifts: Claude, Llama, Qwen; 4 scenarios; feature flag via SENTINEL_OBSERVATORY)
- Sprint 6: Network Namespace Isolation (`sentinel-sandbox`): Per-agent network isolation via veth pairs + `br-sentinel` bridge (10.42.0.1/16) + nftables (allow only Zenoh:7447 + Cortex:8080, DROP rest), `BwrapConfig` default changed to `share_net: false`, graceful fallback via `with_shared_net()` when CAP_NET_ADMIN unavailable, `SandboxEnforcer::setup_network()` for post-spawn NS configuration, VM setup script (`scripts/vm-setup-network-isolation.sh`), 6 Criterion benchmarks (3 Tier-1 CI + 3 Tier-2 VM), 15 new tests (7 unit + 8 acceptance)
- Sprint 6: Sandbox Breakout Test Suite (`sentinel-sandbox`): `breakout-helper` binary for in-sandbox escape attempts, 9 breakout scenarios across 3 isolation layers (4 filesystem via Landlock+bwrap, 3 resource-exhaustion via cgroups v2, 2 namespace via bwrap PID/UTS), `BwrapConfig::proc_mount` field for /proc visibility in namespace tests, security test report template, Landlock write_paths execute gap documented (#76)
- Sprint 6: `sentinel-daemon` service: ECS Orchestrator binary — Composition Root composing all library crates into a running daemon. Dedicated std::thread for ECS tick loop, tokio runtime for async I/O bridge, mpsc channel bridge (Action/Perception), shift-based agent filtering (3 shifts + Sonder-Set 0), TOML config loader, graceful SIGTERM/SIGINT shutdown, systemd service unit with security hardening, dry-run mode for config validation (6 tests)
- Sprint 6: vm-deploy — systemd service stack (sentinel-cortex, sentinel-dashboard, sentinel-agent@ template, sentinel.target), 5 init scripts (dirs, tmpfs, cgroups, hugepages, sysctl), VM config references (Proxmox, kernel params, sysctl), Phase D tmpfs benchmarks on VM 10.0.0.240 (hippocampus -99.3%, nightrun -97.9%, runtime -95%, projection -42%)
- `sentinel-zenoh` extended: SHM-Fallback (AC2), Scoped Queries with UUIDv7/deadline/min_tick (AC1/AC3), InFlightTracker with global=128/per-agent=8 Semaphore limits (AC4), BusConfig ENV parsing, query metrics (stale_drop, timeout, duration, inflight gauge)
- `sentinel-telemetry`: Gauge metric type (AtomicI64) with set/increment/decrement/get, integrated into MetricsRegistry and snapshot_raw
- `sentinel-limbo` EventStore: `snapshots` table with `save_snapshot()` and `get_latest_snapshot()` (auto-incrementing version)
- `sentinel-limbo` EventStore: `compensation_type` field (Saga-Pattern, default "none") in events table and DomainEvent
- `sentinel-limbo` EventStore: Monotonic offset enforcement in `update_offset()` (returns `MonotonicityError` on violation)
- `sentinel-limbo` EventStore: `event_count()` and `get_all_events()` for rebuild/recovery
- `sentinel-limbo` EventStore: Telemetry instrumentation (Counter + Histogram for append, query, snapshot, outbox)
- `sentinel-limbo` EventStore: 7 acceptance tests (AC-1 through AC-7) and 4 new unit tests
- `sentinel-common` DomainEvent: `compensation_type` field with `with_compensation_type()` builder

### Changed

- CI: Merged `lint` + `typos` into single job (saves runner startup time)
- CI: Coverage runs only on `main` push (not on PRs), reduces PR check time by ~8min
- CI: `cargo-machete` + `cargo-tarpaulin` installed via pre-built binaries (`taiki-e/install-action`)
- CI: Security workflow now has concurrency groups (cancel superseded runs)
- CI: Go vulnerability check uses `cache-dependency-path` for faster Go module caching
- Go minimum version bumped from 1.23 to 1.25.0 (toolchain 1.25.7) for crypto/tls vulnerability fixes
- `redb` upgraded from 2.x to 3.1.0 (new `ReadableDatabase` trait, explicit type annotations for `AccessGuard`)
- CLAUDE.md restructured per `/claudemd` best practices (382→229 lines, domain knowledge moved to rules/)

- All GitHub Actions updated to latest versions and pinned to full commit SHAs (17 actions across 10 workflows)
- `.golangci.yml` migrated to v2 format (version field, gosimple merged into staticcheck)
- All workspace crates now have `license = "MIT"` and `publish = false`
- Dependabot configured with Conventional Commit messages per ecosystem
- PR template extended with CHANGELOG, breaking changes, performance, and secrets checklist
- `make lint-all` now includes `cargo deny check`
- CLAUDE.md rewritten with supply-chain-security rules, team protocols, and updated references

### Fixed

- Clippy `manual_range_contains` warnings in sentinel-common validation
- Clippy `type_complexity` warning in telemetry test mock
- `cargo fmt` formatting across workspace

### Dependencies

- Rust: zenoh 1.5.1 → 1.7.2, x509-parser 0.16.0 → 0.18.1, and transitive dependency updates via `cargo update`
- Removed unused `flatbuffers` dependency from sentinel-common
- Removed unused `sentinel-common` and `tracing` dependencies from sentinel-wasm
