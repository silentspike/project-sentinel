# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- Moved Issue #400 dashboard operator API key out of `sessionStorage` into a shared in-memory module (`dashboard/public/js/api-key.js`), consumed by `control.js`, `floorplan.js`, and `timetravel.js`. This clears the CodeQL `js/clear-text-storage-of-sensitive-data` high alerts and removes the threefold duplication of the `getApiKey`/`authHeaders` helpers. **Honest scope:** this fixes the scanner alert and the duplication — it is **not** real XSS hardening (an in-memory variable is just as readable to same-context XSS as `sessionStorage`); genuine token-theft hardening (keeping the key out of JS-accessible storage via httpOnly cookie / server-side auth) is tracked as a separate follow-up issue. Trade-off: the key no longer survives a full page reload (re-enter once per session).
- Activated Issue #380 eBPF production evidence end to end: daemon/dashboard deploy verified `mode=kernel`, no-CAP `userspace` fallback, TCP request deltas, dashboard eBPF cards without `N/A`, and Deploy-VM overhead of 0.012658% amortized on the i7-3930K VM.
- Finalized Issue #382 NMDA episode-selection policy for hippocampus/nightrun: calibrated threshold `0.25`, shared narrative threshold, max 10 selected episodes, selection-quality payload metrics, and Deploy-VM replay evidence for a `2/3` consolidated Night-Run.
- Moved Issue #278 nightrun/shift evolution LLM calls out of the ECS tick loop into an async background task, with VM evidence for fake-gateway success, gateway-down fail-safe behavior, and redb `EVOLUTION_VERSION` writes.
- Added and optimized Issue #379 live sentinel-fs FUSE/CAS verification paths for storage stats and dedup-hit benchmarking; Deploy-VM evidence shows active FUSE agent homes, 99.22% dedup savings, and same-VM median dedup-hit write p95 improved from 40,411 us to 189 us, while the strict `<100us` target remains unmet on the i7-3930K VM.
- Updated optional `sentinel-wasm` Wasmtime dependencies to 44.0.2 to clear current RustSec advisories.
- Optimized Issue #277 dashboard WebSocket change detection from five per-view polling queries to one Projection-DB `projection_watermarks` lookup per poll cycle, with idle polling skipped when no WebSocket clients are connected; VM benchmark evidence is captured with `dashboard/scripts/measure-ws-polling.sh`.
- Optimized Issue #276 ECS tick-loop hot paths: reusable room-physics and persist workspaces, perception buffer reuse, and batched Limbo event/outbox writes. Deploy-VM benchmarks on Intel i7-3930K show relative improvements of 26.86% physics, 26.34% perception, 52.57% persist e2e, 86.95% persist write-only, and 17.23% full tick.
- Excluded `sentinel-gateway` from daemon platform-controlplane `monitored_services` in the deployment config so benchmark and smoke runs can keep the gateway stopped without self-heal restarts.

### Added
- Added Issue #393 Kani verification baseline: installed/verified Kani+CBMC on the build server, added six proven harnesses across bio, snapshot cursor, and event-store offset/dedup invariants, and documented limits in `docs/security/kani-verification.md`.
- Added Issue #392 unsafe audit baseline: first-party unsafe inventory, local `SAFETY:` justifications, CI-ready baseline checker, and a safe `MaybeUninit` replacement for the nightrun `localtime_r` path with fixed-time shift regression coverage.
- Added Issue #383 component-level READMEs for 25 current Rust/Go component directories, plus `docs/component-readmes.md` and `scripts/check-component-readmes.sh` coverage verification.
- Added Issue #391 prompt-injection defense in the Cortex Gateway: per-agent tool capabilities from agent TOML, server-side action validation, `agent_action_rejected` audit events, and injection/legitimate-action regression coverage without real LLM calls.
- Added Issue #390 security threat model at `docs/security/threat-model.md`, covering compromised-agent, external-attacker, and supply-chain attacker classes with asset inventory and prioritized follow-up gaps.
- Recorded Issue #396 Cluster 12 runtime-contract decision as DEV-006 in the Deviation Register: WASM/WASI on Wasmtime is the default Nano-Container contract, while native code is limited to an explicit Escape-Hatch-Pool.
- Implemented Issue #384 Time-Travel Debugging UI (TOGAF Cluster 11): new dashboard "Zeitreise" view with a visual snapshot timeline (tier badge, timestamp, tick, size), point-in-time world-state preview (active agents from `tick_snapshot`, present agents and per-room occupancy derived from EventStore lifecycle replay up to the snapshot boundary via new `GET /api/control/snapshot-state`), and a hot-swap Restore flow with confirmation dialog. Fixed the broken `snapshot_restored` WebSocket handler (`loadAgents`/`loadRooms` ReferenceError) so the dashboard live-refreshes after a restore. Verified end to end on the Deploy-VM with Playwright (all five ACs).
- Added `ebpf-mode-smoke` for focused kernel-mode/userspace-fallback runtime checks and committed Issue #380 dashboard screenshot evidence at `docs/screenshots/issue380-dashboard-ebpf.png`.
- Added dashboard exposure for Issue #276 benchmark evidence at `/api/metrics/benchmarks`, including hardware scope, relative-only comparison notes, and system-metric log labels.

## [0.1.0-alpha] - 2026-04-26

First public release.

This is the first **public** release. The project was developed privately
prior to this release. `v0.1.0-alpha` marks the boundary between private
development and public visibility — not the beginning of the project itself.

### Added
- Complete TOGAF v22.1 architecture guide in `docs/architecture/`
- `docs/governance.md` mapping governance mechanisms to code paths (3 controlplanes,
  policy-as-code locations, event-store as audit trail, sandbox boundaries,
  16 CI workflows)
- `docs/togaf-gap-v22.md` — per-cluster implementation status across 12 TOGAF v22.1 clusters
- `docs/togaf-deviations-v22.md` — five deliberate deviations (DEV-001..005) with
  what / why / revisit-when for each
- `docs/glossary.md` — PixelPerfekt narrative and 60-LLM + 5-services agent-layer terminology
- Run-able 10-minute demo via `docker-compose.demo.yml` (`make demo`) bringing up NATS +
  sentinel-daemon + cortex-gateway + sentinel-judge + sentinel-nats-bridge + sentinel-projection + dashboard
- 3-tier `make demo-binaries` recipe (release-fetch / cargo-remote / local cargo build) plus
  `scripts/fetch-demo-binaries.sh` so the demo onboard is ~60 MB instead of a 20-min cargo build
- Pre-built Linux x86_64 release artifacts: `sentinel-daemon`, `sentinel-nightrun`,
  `sentinel-projection` attached to the v0.1.0-alpha release
- Demo dashboard recording in `docs/images/sentinel-demo.gif`
- 4-tier IP / path configuration strategy: `.env.example`, `.make.local.example`,
  `deploy/systemd/sentinel-env.example` and `Makefile` `-include .make.local`
- CodeQL configuration `.github/codeql/codeql-config.yml` with explicit path scoping;
  Go uses `build-mode: manual`, dashboard uses `build-mode: none`

### Changed
- Go module path migrated from `github.com/obtFusi/project-sentinel` to
  `github.com/silentspike/project-sentinel` (4 modules, 38 source files)
- README restructured around the runtime-and-sandbox research positioning and the
  60 LLM-persona + 5 background-services framing
- `llms.txt` rewritten with the same agent-layer framing and explicit cross-links
  into the new architecture docs

### Security
- Pre-release security review:
  - `gitleaks` scan: 0 leaks across 1063 commits / 12.58 MB
  - `trufflehog` scan: 0 verified + 0 unverified secrets across 56851 chunks / 521.76 MB
  - `cargo audit`, `govulncheck`, `npm audit`: clean
  - 9/9 sandbox breakout tests passing (bwrap + Landlock + cgroups + netns); see
    [docs/security-test-report.md](docs/security-test-report.md)
- CI workflow status at release: ci, lint, coverage, supply-chain (cargo-deny, npm-audit,
  go-vuln, rust-audit), conventional-commits, dependency-freshness — green on main.
  CodeQL scanning depends on GitHub Advanced Security, which is automatically enabled
  for public repositories; the workflow files (`.github/codeql/codeql-config.yml`,
  `.github/workflows/codeql.yml`) ship configured so the SAST pipeline goes green
  on the first scheduled run after the public-flip.

### Known limitations at this release
- The docker compose demo intentionally exercises only the deterministic + LLM layers
  (ECS world, bio-engine, gateway pipeline, dashboard). It does **not** exercise
  the kernel-bound sandbox primitives (bwrap, Landlock, cgroups v2, netns, eBPF,
  sentinel-fs FUSE) — those need user namespaces and CAP_BPF / CAP_SYS_ADMIN that
  a plain unprivileged container does not have. The full stack is documented in
  `deploy/systemd/*.service` for VM deployment. See README "What the docker demo
  shows — and what it does not" and [docs/known-limitations.md](docs/known-limitations.md).
- The signed v0.1.0-alpha tag will display "Unverified" in the GitHub UI until the
  maintainer's SSH signing key is registered as a Signing Key on GitHub. The tag
  itself carries a valid Ed25519 signature (verifiable locally with
  `git tag -v v0.1.0-alpha`).

[Unreleased]: https://github.com/silentspike/project-sentinel/compare/v0.1.0-alpha...HEAD
[0.1.0-alpha]: https://github.com/silentspike/project-sentinel/releases/tag/v0.1.0-alpha
