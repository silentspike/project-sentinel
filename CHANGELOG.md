# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
