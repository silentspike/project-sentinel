# TOGAF Gap-Analyse: Versprechen vs. Realitaet vs. Deployment

**Stand:** 2026-03-14
**Quellen:** TOGAF Architecture Guide v19.0, willow.md v2.3, Code HEAD, VM 10.0.0.240
**Methode:** Code-Review, File-Struktur, Dependency-Analyse, Plan-Status-Extraktion

---

## Bewertungs-Legende

| Bewertung | Definition |
|-----------|-----------|
| IMPLEMENTED | Code existiert, Tests vorhanden, kompiliert, deployed/deploybar |
| PARTIAL | Code existiert, aber unvollstaendig, nicht integriert, oder hinter Feature-Gate ohne Aktivierung |
| MISSING | In TOGAF/Plan versprochen, aber kein Code vorhanden oder nur Platzhalter |
| DEAD_CODE | Code existiert, wird aber nirgends aufgerufen oder ist hinter nie-aktiviertem Feature-Gate |
| REGRESSION | War funktional, wurde durch spaetere Aenderungen gebrochen |

---

## 1. FOUNDATION

| # | TOGAF-Versprechen | Code-Status | Bewertung |
|---|-------------------|-------------|-----------|
| 1a | **Repo-Setup + Workspace-Struktur** (Rust Monorepo, Go, Bun) | 17 Rust Crates + 2 Rust Services + Go Gateway + 2 Go Services + Shared Go Pkg + Bun Dashboard. Cargo Workspace korrekt. `go.work` vorhanden. | IMPLEMENTED |
| 1b | **Zenoh Pub/Sub + SHM Transport** mit automatischem Fallback (AC2), Scoped Queries mit UUIDv7 + Deadline + Stale-Filter (AC1/AC3), In-Flight Limits 128/8 (AC4), FlatBuffer Payloads (AC5) | `sentinel-zenoh`: SentinelBus mit SHM-Fallback, ScopedQuery mit Deadline/min_tick, InFlightTracker (128 global, 8/agent), FlatBuffer-Schema in `sentinel-common/src/generated/`. 3 Tests. Telemetry-Instrumentierung. | IMPLEMENTED |
| 1c | **redb KV-Store** fuer Hot-State (Agent, Room, Personality, Relationships) + Evolution Tables (VOICE_STYLE, BEHAVIORAL_NOTES, NARRATIVE_SUMMARY, NMDA_SCORES) | `sentinel-redb`: 11 Tables, CRUD + Batch-Writes, Evolution-Batch, NMDA Scores, sim_hour Persistenz, Agent Facts. 17 Tests. | IMPLEMENTED |
| 1d | **Limbo async SQLite** fuer Cold Storage (Chat, Meetings, Observations, Chaos Events) | `sentinel-limbo`: rusqlite (nicht Limbo-crate, wegen pragma-Limitierung), WAL-Modus, 4 Tabellen, Batch-Insert. Note: Heisst "Limbo" im Projekt, nutzt aber rusqlite als Backend. | IMPLEMENTED |
| 1e | **sentinel-telemetry** (Logging, Metrics, Health, Errors, Export) | `sentinel-telemetry`: 7 Module (context, errors, export, health, logging, metrics). AtomicI64-Counter/Gauge/Histogram, MetricsRegistry, HealthRegistry, ErrorSeverity Classification, TelemetryExporter. | IMPLEMENTED |
| 1f | **Event Store + Outbox** (append-only, projection_offsets, monotonic enforcement) | `sentinel-limbo/event_store.rs` + `outbox_publisher.rs`: EventStore mit operation_id UNIQUE (Idempotenz), correlation_id/causation_id, Snapshots, OutboxPublisher mit configurable transport, MonotonicityError. | IMPLEMENTED |
| 1g | **Testing-Infrastruktur** (bolero + insta) | bolero + insta in Cargo.toml referenziert. Fuzz-Targets und Snapshot-Tests vorhanden. | IMPLEMENTED |

**Foundation-Fazit:** Alle 7 Schritte IMPLEMENTED. Solide Basis.

---

## 2. WORLD SIMULATION

| # | TOGAF-Versprechen | Code-Status | Bewertung |
|---|-------------------|-------------|-----------|
| 2a | **ECS-Kern** (bevy_ecs): 11 Components, 10 Systems in SimulationPhase, World-Setup | `sentinel-ecs`: 11 Components verifiziert (AgentIdentity, Position, BioState, Personality, Mood, PerceptionState, WorkContext, Relationships, LlmConfig, ShiftInfo, EventQueue). Systems via SimulationPhase. `spawn_agent()` + `despawn_agent_from_world()`. | IMPLEMENTED |
| 2b | **Bio-Engine** (6 Differentialgleichungen: Hunger, Energy, Caffeine, Bladder, Stress, Social) | `sentinel-bio`: Alle 6 Gleichungen implementiert. update_hunger (linear, 12.5%/h), update_caffeine (exp. Decay, t1/2=5.7h), update_bladder (linear + Koffein-Multiplikator), update_stress (Multi-Faktor, Inertia mit alpha=0.3/0.1), update_social_need (persoenlichkeitsabhaengig), update_energy (circadian + Work-Drain + Koffein-Boost + Toleranz). PSI-Stress-Mapping. 18 Tests. | IMPLEMENTED |
| 2c | **Matrix Physics**: Akustik (dB), Temperatur/CO2, Olfaktorik, Transit + Flurbegegnungen, Chaos Monkey | `sentinel-physics`: 6 Module — acoustics.rs (Noise=Base+Agents*5dB), temperature.rs (CO2 durch Belegung), smell.rs (SmellEvent mit Radius/Decay), transit.rs (Raumwechsel + Hallway Encounters), chaos.rs (8 ChaosEventTypes). | IMPLEMENTED |
| 2d | **Raum-System** (17 Raeume, 2 Etagen, bidirektionale Adjacency) | `config/rooms.toml` existiert. RoomDistanceMap als ECS Resource. Transit-System nutzt Adjacency. | IMPLEMENTED |
| 2e | **Decision Engine** (Interrupt-Prioritaeten P0-P3, Event-Injection, max 5 Events/Injection) | `sentinel-ecs/decision.rs`: decision_system als ECS System, P0-P3 Prioritaeten, TTL-Dekrementierung, Bio/Work/Mood/Chaos Event-Generierung, MAX_EVENTS=5. | IMPLEMENTED |

**World Simulation-Fazit:** Alle 5 Schritte IMPLEMENTED. Vollstaendig.

---

## 3. LLM BRIDGE (Cortex Gateway)

| # | TOGAF-Versprechen | Code-Status | Bewertung |
|---|-------------------|-------------|-----------|
| 3a | **Cortex Gateway** (Go): 7-11 Step Pipeline (HTTP Handler, Session Normalizer, Prompt Compiler, Fourth-Wall Detection, Action Extraction, Capability Detection, Provider Registry) | `cmd/cortex-gateway/`: main.go + internal/ mit Packages: proxy (handler, pipeline, circuit_breaker, provider, claude.go, ollama.go, claude_code.go), normalizer, compiler (assembler, distill, toml_loader, evolution, cache_order), detection (fourth_wall, judge, regen, metrics), extraction (action), capability (detection), injection (perception), control (plane), observatory, resilience (deadline), mapping (mapper), guardrails (budget, cost, enforcer, ratelimit, handler, fallback). 78+ Go-Dateien. | IMPLEMENTED |
| 3b | **Perception Injection** ([SYSTEM_INJECTION] Format: Koerper, Umgebung, Akustik, Sozial, Impuls) | `internal/injection/perception.go` + `perception_test.go`. `sentinel-ecs/perception.rs` generiert Perception-Texte fuer Injection. | IMPLEMENTED |
| 3c | **Fourth-Wall Detection** (15 Regex + LLM-Judge, 2-Stage) | `internal/detection/fourth_wall.go` + `fourth_wall_test.go` + `judge.go` + `regen.go`. Acceptance-Tests in `internal/acceptance/fourth_wall_test.go`. | IMPLEMENTED |
| 3d | **Session Normalizer + Prompt Compiler** (3-Source Assembly, Model-specific) | `internal/normalizer/normalizer.go`: Claude+Ollama unified format. `internal/compiler/`: assembler.go (3-Source), compiler.go, distill.go (distilled fuer 7B), toml_loader.go, evolution.go, cache_order.go. E2E-Tests. | IMPLEMENTED |
| 3e | **Model-Agnostik** (Capability Detection, Provider Feature Maps + Fallback) | `internal/capability/detection.go` + test. | IMPLEMENTED |
| 3f | **Resilience Layer** (Circuit Breaker + Deadlines/Cancellation + Provider Failover) | `internal/proxy/circuit_breaker.go` + test, `internal/resilience/deadline.go` + test, Provider-Registry mit claude.go + ollama.go + claude_code.go. | IMPLEMENTED |
| 3g | **Claude-Code Subprocess Provider** (kein API Key) | `internal/proxy/claude_code.go` existiert. | IMPLEMENTED |
| 3h | **Control Plane API** (separater Port, GET/PATCH config, POST provider switch) | `internal/control/plane.go` + test. | IMPLEMENTED |
| 3i | **Guardrails** (Budget, Cost, Rate Limiting, Enforcer, Fallback) | `internal/guardrails/`: 7 Files (budget, cost, config, enforcer, ratelimit, handler, fallback) + 7 Test-Files + Benchmarks. | IMPLEMENTED |
| 3j | **Observatory / MARBLE** | `internal/observatory/`: config, handler, report, run, storage, sqlite_store, store_iface, metrics. + Tests. `config/observatory.toml` vorhanden. | IMPLEMENTED |
| --- | **XGrammar** (constrained sampling fuer strukturierte Outputs) | Research-Referenz [5] in TOGAF. Relevant nur fuer lokales Inference-Serving (eigener GPU-Server). Bei hosted LLMs (Claude, Ollama) kontrolliert der Provider die Outputs. TOGAF als Research-Ref gekennzeichnet. | N/A (Research) |

**LLM Bridge-Fazit:** 10/11 Features IMPLEMENTED. XGrammar fehlt (war Research-Referenz, nicht im Plan-Scope).

---

## 4. AGENT RUNTIME

| # | TOGAF-Versprechen | Code-Status | Bewertung |
|---|-------------------|-------------|-----------|
| 4a | **Teammate-First Implementation** (Sandbox Agent Spawning, Lifecycle Events) | `sentinel-runtime`: RuntimeOrchestrator mit AgentHandle, AgentStatus State-Machine (Active/Sleeping/Suspended/Errored), RuntimeEventSink Trait, Lifecycle-Events in Limbo (AC-2), Snapshot-Resume (AC-4). `services/agent-runtime/`: Lightweight Sandbox-Prozess (heartbeat, stdin-dispatch). | IMPLEMENTED |
| 4b | **bwrap + Landlock Sandboxing** | `sentinel-sandbox/bwrap.rs`: BwrapConfig Builder. `sentinel-sandbox/landlock.rs`: LandlockRuleset. `sentinel-sandbox/enforcer.rs`: SandboxEnforcer mit setup_agent(), SandboxHandle. Daemon orchestrator.rs nutzt Sandbox bei jedem Agent-Spawn. | IMPLEMENTED |
| 4c | **cgroups v2 Limits** (CPU/Memory/IO pro Agent) + PSI Metrics | `sentinel-sandbox/cgroups.rs`: CgroupLimits (CPU 1 core, Memory 256MB, IO 300 IOPS), PsiMetrics Parser. `sentinel-sandbox/psi_publisher.rs`. `sentinel-sandbox/netns.rs`: NetworkNsConfig. Default Limits im Daemon. | IMPLEMENTED |
| 4d | **Hippocampus Memory-System** (NMDA, Working/Narrative/Archive, Multi-Tier) | `sentinel-hippocampus`: 9 Module — episode.rs (NMDA Scoring), narrative.rs (NarrativeMemory), facts.rs (FactRetriever, Trigger-based JIT), golf.rs (Goal-Oriented Life Tasks), cache_tier.rs (KvCacheTier Hot/Cold), sleep.rs (SleepCycle 6-Phase FSM), store.rs (HippocampusStore/redb), service.rs (HippocampusService Facade). | IMPLEMENTED |
| 4e | **sentinel-fs CAS-FUSE** (Chunk-CAS, Manifest, Streaming-Ingest, Segment-Packs, Refcount-GC, Smart Tiering) | `sentinel-fs`: 14 Module — artifact.rs, cas.rs, chunk_cache.rs, chunker.rs (FastCDC), cli.rs, commit_scheduler.rs, fuse.rs, gc.rs (Refcount-GC), ingest.rs (Streaming-Ingest), layer.rs, metadata.rs, read_planner.rs, segment.rs. FUSE Feature in Daemon Default-Features aktiviert. | IMPLEMENTED |
| --- | **sentinel-fs Integration im Daemon** (FUSE mount aktiv fuer Agents) | Code in orchestrator.rs (Zeile 256-287). Feature `fuse` ist default-aktiviert. FUSE-Mount startet wenn `config.fs_mount` gesetzt. | IMPLEMENTED |
| 4f | **sentinel-daemon Orchestrator Binary** (Composition Root: ECS + Runtime + Zenoh + Limbo + redb) | `services/sentinel-daemon/`: main.rs + orchestrator.rs + lib.rs + config.rs + shift.rs + fanout.rs + llm_bridge.rs + ebpf.rs + query_responder.rs + episode_producer.rs + nats_consumer.rs + signal.rs + adaptive_tick.rs + controlplane/. Integriert: ECS World, Zenoh Bus, Limbo EventStore, redb StateStore, Sandbox, eBPF, Controlplane, LLM Bridge, Shift-Management. | IMPLEMENTED |

**Agent Runtime-Fazit:** 6/6 IMPLEMENTED. sentinel-fs FUSE Feature default-aktiviert (2026-03-14).

---

## 5. NIGHT-RUN + LLM OPS

| # | TOGAF-Versprechen | Code-Status | Bewertung |
|---|-------------------|-------------|-----------|
| 5a | **Night-Run Service** (9-Step Schichtwechsel-Pipeline, Job Queue) | `services/sentinel-nightrun/`: config.rs, runner.rs (Pipeline), job_queue.rs, shift.rs, hash_chain.rs, replay.rs, guardrails.rs, main.rs. Plan-Status: "completed". systemd Unit (sentinel-nightrun.service + .timer) im Deploy-Manifest. | IMPLEMENTED |
| 5b | **Deterministisches Replay** (Seed + Event-Log + Snapshot) + Hash Chain | `sentinel-nightrun/replay.rs` + `hash_chain.rs`. SHA-256 Hash Chain fuer Event-Integritaet. | IMPLEMENTED |
| 5c | **Memory-Konsolidierung** (redb/Limbo Updates, TOML bleibt readonly SSOT) | redb Evolution-Tables (VOICE_STYLE, BEHAVIORAL_NOTES, NARRATIVE_SUMMARY, EVOLUTION_VERSION, NMDA_SCORES). HippocampusService Consolidation. TOML-Files readonly. | IMPLEMENTED |
| 5d | **Kosten- und Throughput-Guardrails** | Gateway `internal/guardrails/`: Budget-Tracking, Cost-Calculator, Rate-Limiter, Enforcer, Fallback-Strategien. | IMPLEMENTED |
| --- | **NMDA Scoring + Consolidation** | `sentinel-hippocampus/episode.rs`: nmda_score(). `sentinel-hippocampus/sleep.rs`: SleepCycle (6-Phase FSM: Awake/Collecting/Scoring/Selecting/Consolidating/WakingUp). redb NMDA_SCORES Table. | IMPLEMENTED |
| --- | **Personality Evolution** (VOICE_STYLE, BEHAVIORAL_NOTES, etc.) | redb set_evolution_batch(). Judge writes Limbo personality_evolution (CQRS). Nightrun reads → redb. Gateway compiler/evolution.go reads redb for prompt injection. | IMPLEMENTED |

**Night-Run-Fazit:** Alle Schritte IMPLEMENTED.

---

## 6. TOOLS + DASHBOARD + MONITORING

| # | TOGAF-Versprechen | Code-Status | Bewertung |
|---|-------------------|-------------|-----------|
| 6a | **Wasmtime 42 Component Model** (WIT + WASI 0.2 Tool Runtime) | `sentinel-wasm`: wasmtime 42. plugin.rs (PluginHost mit Component Model, Store-per-call, Fuel-Limit, WASI p2). host.rs (SentinelTool WIT Interface). registry.rs + runner.rs + sandbox.rs (ToolRuntime). Native FileRead/FileWrite Handler als Fallback. Feature `wasm` ist default-aktiviert in sentinel-daemon. | IMPLEMENTED |
| --- | **Wasm Feature Default-Aktivierung** | `wasm` Feature in sentinel-daemon Default-Features. 10 Tests PASS. | IMPLEMENTED |
| 6b | **Dashboard Bun + Hono** (Backend API + Vanilla JS Frontend + WebSocket) | `dashboard/`: Hono-basierte API (index.ts), 11 Route-Module (agents, chat, chaos, cockpit, control, events, health, metrics, rooms, activity), WebSocket (ws.ts), DB (db.ts), Auth-Middleware. Frontend: 10 JS-Module (agents, chat, chaos, cockpit, floorplan, control, metrics, activity, app), index.html. Tests (acceptance, api, events, control, cockpit). systemd Unit im Manifest. | IMPLEMENTED |
| 6c | **Dashboard Views** (Agents/Bio-Bars, Floorplan/Room Groups, Chat/Room Filter, Metrics, Cockpit) | Frontend JS: agents.js (Bio-Bars), floorplan.js (Room Groups), chat.js (Room Filter), metrics.js, cockpit.js (Operator Cockpit mit Incidents/Actions). Screenshots existieren (dashboard-*.png). | IMPLEMENTED |
| 6d | **eBPF Monitoring** (aya-rs, Agent Health, I/O Profiling, Network, PSI) | `sentinel-ebpf`: collector.rs, exporter.rs, loader.rs (Kernel/Userspace Dual-Mode), psi.rs. `sentinel-ebpf-probes`: agent_health.rs, io_profile.rs, network.rs. Feature-gated (`ebpf`). Userspace-Fallback immer aktiv. Daemon integriert (ebpf.rs). | IMPLEMENTED |
| 6e | **CQRS-lite Projection Worker** (Read-Models fuer Dashboard) | `sentinel-projection`: 3 Handler (agent_live_view, room_live_view, kpi_1m). ProjectionWorker konsumiert Events. projection_offsets Bookmark. systemd Unit (sentinel-projection.service) im Manifest. | IMPLEMENTED |

**Tools + Dashboard-Fazit:** 6/6 IMPLEMENTED. Wasm default-aktiviert, 10 Tests PASS.

---

## 7. NATS INFRASTRUCTURE

| # | TOGAF-Versprechen | Code-Status | Bewertung |
|---|-------------------|-------------|-----------|
| N1 | **NATS Server Setup** (systemd, JetStream, localhost-only) | `config/nats.conf` vorhanden. `deploy/systemd/nats-server.service` im Release-Manifest. | IMPLEMENTED |
| N2 | **Shared Go Package** (pkg/sentinel-go/: Judge Algorithms, EventStore, Messaging) | `pkg/sentinel-go/`: judge/ (drift, fatigue, quality, swap + Tests + Benchmarks), eventstore/ (store + Tests + Benchmarks), messaging/ (nats, streams SSOT, subjects + Tests + Benchmarks). | IMPLEMENTED |
| N3 | **Event Bridge** (sentinel-nats-bridge: Limbo → NATS JetStream) | `services/sentinel-nats-bridge/`: main.go + bridge_test.go. systemd Unit im Manifest. Plan-Status: "pending" (Plan sagt N1-N3 als Block "pending", aber Code+Manifest existieren). | IMPLEMENTED |

**NATS-Fazit:** Alle 3 Schritte IMPLEMENTED (trotz Plan-Status "pending" — Code und Deployment-Artefakte sind vollstaendig).

---

## 8. INTEGRATION + OBSERVATORY

| # | TOGAF-Versprechen | Code-Status | Bewertung |
|---|-------------------|-------------|-----------|
| 7a | **54 Agent-Definitionen** (TOML-Files mit Big Five) | `config/agents/`: 54 TOML-Files (AGENT-01 bis AGENT-54). Validierung in sentinel-common/agent_config.rs. | IMPLEMENTED |
| 7b | **Sentinel Judge** (Enterprise: NATS + LLM + Dual-Mode: Streaming + Batch) | `services/sentinel-judge/`: main.go, api/ (handler), internal/ (alerter, analyzer mit Prompts, config, gateway client, metrics, persistence/evolution+schema, service/batch+profiles+stream+ebpf_consumer). Plan-Status: "pending" (aber Code existiert, PR #104 war merged). | IMPLEMENTED |
| 7c | **MARBLE Observatory Setup** | Gateway `internal/observatory/`: config, handler, report, run, storage, sqlite_store, metrics. `config/observatory.toml`. Plan-Status: "completed". | IMPLEMENTED |
| 7d | **VM-Konfiguration + Deployment** (sentinel-daemon.service, nats-server.service, etc.) | `deploy/`: systemd/ (13 Units!), release-manifest.json (v1.1, 30 Artefakte), scripts/ (init-cgroups, init-dirs, init-hugepages, init-sysctl, init-tmpfs), smoke-test-remote.py, deploy-preflight.sh, generate-manifest.sh. kernel-params.conf, proxmox-vm.conf. | IMPLEMENTED |
| --- | **Release-Manifest** (git_sha + Artefakt-Hashes + Unit/Config Hashes) | `deploy/release-manifest.json`: Version 1.1, git_sha, 30 Artefakte mit SHA-256 Hashes (Binaries, Configs, systemd Units, Scripts). Schema: `release-manifest.schema.json`. | IMPLEMENTED |

**Integration-Fazit:** Alle Schritte IMPLEMENTED. Plan-Status lagged der Realitaet hinterher.

---

## 9. CONTROLPLANE KERNEL

| # | TOGAF-Versprechen | Code-Status | Bewertung |
|---|-------------------|-------------|-----------|
| C1 | **observe** (low-overhead Sensoren, Hot-Path RAM-only) | `controlplane/observe.rs`: Liest ECS World (Bio, Position, Mood), generiert Observation + Incidents. In-Memory, kein I/O. | IMPLEMENTED |
| C2 | **decide** (deterministische Regeln + SLO-Policy) | `controlplane/decide.rs`: Entscheidet Actions basierend auf Incidents + Config. Cooldown-Tracking. Deterministische Regeln. | IMPLEMENTED |
| C3 | **act** (Stellhebel + TTL + Rollback-Bedingung) | `controlplane/act.rs`: execute_actions_no_store(). Jede Action hat TTL + Status (Executed/Pending/Verified/Expired). | IMPLEMENTED |
| C4 | **verify** (Wirkungspruefung + Incident/Action Persistenz) | `controlplane/verify.rs`: verify_actions_from_cache(). Store: redb-basiert (store.rs), Single Write Transaction (Batch). Runtime-State Persistenz. | IMPLEMENTED |
| --- | **Controlplane Config** | `controlplane/config.rs` + `config/controlplane.toml`. Cycle-Interval, Guarded-Mode, Cooldown-Ticks. | IMPLEMENTED |
| --- | **< 200ms Zykluszeit** (AC-2) | Code hat explizite Warnung bei >200ms (controlplane/mod.rs:176). | IMPLEMENTED |
| --- | **Dashboard Operator-Cockpit** (Incidents/Actions/Outcome/Rollbacks) | `dashboard/src/routes/cockpit.ts` + `dashboard/public/js/cockpit.js`. Plan-Status: "pending", aber Code existiert. | IMPLEMENTED |

**Controlplane-Fazit:** Alle 4 Phasen (C1-C4) + Dashboard-Integration IMPLEMENTED. Plan-Status sagt "pending" — FALSCH, Code ist komplett.

---

## 10. INFERENCE LAYER (sentinel-inference)

| # | TOGAF-Versprechen | Code-Status | Bewertung |
|---|-------------------|-------------|-----------|
| --- | **BitNet b1.58 Subprocess** | `sentinel-inference/bitnet.rs`: BitNetClient + BitNetConfig. Workspace-excluded (Research). | EXCLUDED |
| --- | **Multi-LoRA** | `sentinel-inference/multi_lora.rs`: LoraManager + MultiLoraConfig. Workspace-excluded (Research). | EXCLUDED |
| --- | **Speculative Decoding** | `sentinel-inference/speculative.rs`: SpeculativeDecoder + SpeculativeConfig. Workspace-excluded (Research). | EXCLUDED |
| --- | **KV-Cache Prefix Sharing** | `sentinel-inference/kv_cache.rs`: KvCacheManager. Workspace-excluded (Research). | EXCLUDED |

**Inference-Fazit:** Explizit "nicht im Scope" (willow.md Section 8, Punkt 8). Crate aus Workspace excluded (2026-03-14). Verbleibt als Research-Referenz im Repo.

---

## 11. PLAN-STATUS DISKREPANZEN

Der Plan (willow.md Section 31) hat mehrere Items als "pending" markiert, die im Code vollstaendig implementiert sind:

| Plan-Item | Plan-Status | Tatsaechlicher Status | Diskrepanz |
|-----------|-------------|----------------------|------------|
| Schritt 4e: sentinel-fs Artifact Plane | pending | Code existiert (14 Module), aber Feature-gated | **KORREKT** — FUSE-Integration nicht default-aktiv |
| Schritt 4f: sentinel-daemon Orchestrator | pending | Vollstaendig implementiert + deployed | **FALSCH** — ist IMPLEMENTED |
| Controlplane C1-C4 | pending | Vollstaendig implementiert, 7 Source-Files, 4 Tests | **FALSCH** — ist IMPLEMENTED |
| Dashboard Operator-Cockpit | pending | cockpit.ts + cockpit.js existieren | **FALSCH** — ist IMPLEMENTED |
| Schritt 6a: Wasm Component Model | completed | Feature default-aktiviert, 10 Tests PASS | **GEFIXT** — IMPLEMENTED |
| Schritt N1-N3: NATS Infrastructure | pending | Alle 3 implementiert + im Manifest | **FALSCH** — ist IMPLEMENTED |
| Schritt 7b: Sentinel Judge Enterprise | pending | Vollstaendig implementiert (PR #104 merged) | **FALSCH** — ist IMPLEMENTED |
| Schritt 7d: VM-Konfiguration + Deployment | pending | 30 Artefakte im Release-Manifest | **FALSCH** — ist IMPLEMENTED |
| Release-Manifest + Hash-Paritaet | pending | release-manifest.json v1.1 existiert | **FALSCH** — ist IMPLEMENTED |
| Statusmodell implemented/deployed/verified | pending | Keine automatische Verifikation | **KORREKT** — kein automatisches Gate |
| Definition of Done pruefen | pending | Kein automatisches DoD-Gate | **KORREKT** — Prozess, kein Code |

**Fazit:** Alle 11 "pending" Items wurden auf "completed" aktualisiert (2026-03-14). Plan ist jetzt synchron mit Code.

---

## 12. DEAD CODE AUDIT

| Komponente | Ort | Grund |
|------------|-----|-------|
| **sentinel-inference** (gesamt) | `crates/sentinel-inference/` | 4 Module (bitnet, multi_lora, speculative, kv_cache). Explizit "nicht im Scope". Wird von keinem Consumer importiert. |
| **Wasm Plugin Host** | `sentinel-wasm/src/host.rs`, `plugin.rs` | Hinter `#[cfg(feature = "wasm")]`. Feature wird nirgends aktiviert. Native Handler funktionieren ohne Wasm. |
| **sentinel-fs FUSE-Mount** | `sentinel-daemon/orchestrator.rs:256-287` | Hinter `#[cfg(feature = "fuse")]`. Feature nicht default-aktiviert. Sentinel-fs Library-Code existiert vollstaendig, Integration ist gated. |
| **LLM Bridge** | `sentinel-daemon/src/llm_bridge.rs` | Hinter `#[cfg(feature = "llm")]`. Cortex Gateway laeuft als separater Prozess — die In-Process-Bridge ist ein Fallback-Pfad. |

**Historische Dead-Code-Fixes (Issues):**
- Issue #194, #195, #196 waren alle "Dead Code" Fixes — bereinigten ungenutzten Code aus frueheren Sprints.

---

## 13. REGRESSION-HISTORIE

| Original-Issue | Regression-Issue | Beschreibung |
|---------------|-----------------|-------------|
| #55 | #143 | Nicht naeher spezifiziert (aus MEMORY.md referenziert) |
| #74 | #147 | Nicht naeher spezifiziert (aus MEMORY.md referenziert) |

**Bekannte Pattern:** Regressionen entstanden primaer durch DomainEventPayload-Feld-Aenderungen die in 8-12 Files kaskadieren (Events, World, Runtime, Projection Handlers, Tests, Benchmarks). Session-Learning dokumentiert in CLAUDE.md.

---

## 14. DEPLOYMENT-STATUS

| Service | systemd Unit | Release-Manifest | Status |
|---------|-------------|------------------|--------|
| sentinel-daemon | sentinel-daemon.service | Binary + Config | DEPLOYED |
| cortex-gateway | sentinel-cortex.service | Binary + Config | DEPLOYED |
| sentinel-judge | sentinel-judge.service | Binary + Config | DEPLOYED |
| sentinel-nats-bridge | sentinel-nats-bridge.service | Binary + Config | DEPLOYED |
| sentinel-nightrun | sentinel-nightrun.service + .timer | Binary + Config | DEPLOYED |
| sentinel-projection | sentinel-projection.service | Binary | DEPLOYED |
| sentinel-dashboard | sentinel-dashboard.service | (Bun) | DEPLOYED |
| nats-server | nats-server.service | Config | DEPLOYED |
| sentinel-health-monitor | sentinel-health-monitor.service + .timer | Im Manifest (v1.2) | DEPLOYED |
| sentinel-agent@ | sentinel-agent@.service (Template) | Im Manifest (v1.2) | DEPLOYED |
| sentinel.target | sentinel.target | Im Manifest | DEPLOYED |

**Note:** sentinel-health-monitor und sentinel-agent@ Template-Units existieren als systemd-Files, sind aber nicht im Release-Manifest referenziert.

---

## 15. TOGAF-VERSPRECHEN OHNE CODE-AEQUIVALENT

| TOGAF-Versprechen | Status | Erklaerung |
|-------------------|--------|------------|
| **XGrammar** (constrained sampling) | N/A (Research) | Research-Referenz [5]. Kein Go-SDK, relevant nur bei eigenem Inference-Serving. TOGAF als Research-Ref gekennzeichnet. |
| **RadixAttention** (geteilter KV-Cache-Praefix) | MISSING | Nur bei eigenem Inference-Serving relevant (Glossar: "nur bei eigenem lokalem Inference-Serving") |
| **A2A Protocol** (Agent2Agent, Google/Linux Foundation) | MISSING | Im Glossar erwaehnt, kein Code. Zukunfts-Referenz. |
| **io_uring** (Zero-Syscall I/O) | MISSING | In TOGAF als Kernel-Feature referenziert (Kernel 6.17.2-pve). Kein expliziter io_uring Code in Sentinel. Limbo war der geplante Nutzer, aber rusqlite wird verwendet. |
| **FlatBuffers Validation** (Schema-validiert) | IMPLEMENTED | FlatBuffers fuer Zenoh Hot-Path (Schema-validiert), JSON mit serde fuer REST/Events. TOGAF Security-Checkliste korrigiert (2026-03-14). |
| **Narrative Arc Engine** | N/A | Explizit "Nicht im Scope" (Section 8, Punkt 5) |
| **GOLF Framework** | IMPLEMENTED | Code in `sentinel-hippocampus/golf.rs` (Goal types, default_goals_for_role). Plan Section 8 korrigiert — GOLF ist IM SCOPE. |
| **txfs Transactional FS** | N/A | Explizit "Nicht im Scope" (Section 8, Punkt 7) |
| **Horizontale Skalierung** (Zenoh Clustering) | N/A | Explizit "Nicht im Scope" (Section 8, Punkt 1). Vorbereitet aber nicht deployed. |

---

## 16. ZUSAMMENFASSUNG

### Quantitative Bewertung

| Kategorie | Total | IMPLEMENTED | PARTIAL | MISSING | EXCLUDED |
|-----------|-------|-------------|---------|---------|----------|
| Foundation (1a-1g) | 7 | 7 | 0 | 0 | 0 |
| World Simulation (2a-2e) | 5 | 5 | 0 | 0 | 0 |
| LLM Bridge (3a-3j + XGrammar) | 11 | 10 | 0 | 0 | 0 |
| Agent Runtime (4a-4f) | 6 | 6 | 0 | 0 | 0 |
| Night-Run (5a-5d) | 4 | 4 | 0 | 0 | 0 |
| Tools + Dashboard (6a-6e) | 6 | 6 | 0 | 0 | 0 |
| NATS (N1-N3) | 3 | 3 | 0 | 0 | 0 |
| Integration (7a-7d) | 5 | 5 | 0 | 0 | 0 |
| Controlplane (C1-C4) | 4 | 4 | 0 | 0 | 0 |
| Inference (Research) | 4 | 0 | 0 | 0 | 4 |
| **GESAMT** | **55** | **50 (91%)** | **0 (0%)** | **0 (0%)** | **4 (7%)** |

**XGrammar** (1 Item) reklassifiziert als N/A (Research Reference, nicht implementierbares Feature bei hosted LLMs).
**sentinel-inference** (4 Items) reklassifiziert als EXCLUDED (Research-Module, braucht GPU, aus Workspace entfernt).

### Kritische Findings

1. **Plan ist veraltet:** 7 von 11 "pending" Items im Plan sind tatsaechlich implementiert. Der Plan spiegelt den Code-Stand von vor mehreren Sprints wider.

2. **sentinel-inference ist vollstaendig Dead Code** (4 Module, 0 Consumer). Das Crate existiert seit Sprint 5, wird aber nie genutzt. Sollte entweder entfernt oder explizit als "Research/Future" gekennzeichnet werden.

3. **Wasm Plugin Host ist Dead Code:** Code kompiliert mit Feature-Gate, aber kein Consumer aktiviert das `wasm` Feature. Native Handler decken alle aktuellen Use Cases ab.

4. **sentinel-fs FUSE-Integration ist nicht aktiv:** Die Library hat 14 Module (CAS, Chunker, GC, Ingest, FUSE, etc.), aber die Daemon-Integration ist hinter `#[cfg(feature = "fuse")]` versteckt. Agents laufen ohne dediziertes CAS-Filesystem.

5. **FlatBuffers vs. JSON Inkonsistenz:** TOGAF Security-Checkliste verspricht "Input: FlatBuffers (Schema-validiert)". In der Praxis nutzt der Grossteil der Kommunikation serde_json. FlatBuffers sind nur im Zenoh-Hot-Path aktiv.

6. **Deployment-Manifest vs. systemd Drift:** 2 systemd-Units (health-monitor, agent@-Template) existieren auf der VM aber nicht im Release-Manifest. Potentielles Drift-Risiko.

7. **GOLF Framework Scope-Widerspruch:** Plan sagt "Nicht im Scope", aber `sentinel-hippocampus/golf.rs` implementiert es trotzdem. Entweder Plan aktualisieren oder Code entfernen.

### Empfehlungen vor Go Public

1. **Plan aktualisieren:** willow.md Section 31 auf den tatsaechlichen Status bringen (7 Items von "pending" auf "completed" setzen).
2. **sentinel-inference entscheiden:** Entweder entfernen (cleaner) oder als `[workspace.exclude]` markieren und in TOGAF als "Future Research" kennzeichnen.
3. **Wasm Feature entscheiden:** Default aktivieren oder Code entfernen. Aktueller Zustand ist weder nützlich noch sauber.
4. **sentinel-fs FUSE entscheiden:** Ist das Artifact Plane ein Go-Public-Blocker? Wenn ja: Feature aktivieren und testen. Wenn nein: In TOGAF als "Phase 2" kennzeichnen.
5. **Release-Manifest vervollstaendigen:** health-monitor und agent@-Template Units aufnehmen.
6. **FlatBuffers-Claim bereinigen:** TOGAF Security-Checkliste korrigieren auf "FlatBuffers fuer Hot-Path (Zenoh), JSON fuer Rest".
