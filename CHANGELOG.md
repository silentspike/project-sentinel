# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- Optimized Issue #277 dashboard WebSocket change detection from five per-view polling queries to one global live-view watermark query per poll cycle; VM benchmark evidence is captured with `dashboard/scripts/measure-ws-polling.sh`.
- Optimized Issue #276 ECS tick-loop hot paths: reusable room-physics and persist workspaces, perception buffer reuse, and batched Limbo event/outbox writes. Deploy-VM benchmarks on Intel i7-3930K show relative improvements of 26.86% physics, 26.34% perception, 52.57% persist e2e, 86.95% persist write-only, and 17.23% full tick.
- Excluded `sentinel-gateway` from daemon platform-controlplane `monitored_services` in the deployment config so benchmark and smoke runs can keep the gateway stopped without self-heal restarts.
- Updated optional `sentinel-wasm` Wasmtime dependencies to 44.0.2 to clear current RustSec advisories.

### Added
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
