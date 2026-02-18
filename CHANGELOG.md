# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
