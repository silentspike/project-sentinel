# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
  sentinel-daemon + cortex-gateway + sentinel-judge + sentinel-nats-bridge + dashboard
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
  - `gitleaks` scan: 0 leaks across 1055 commits / 12.58 MB
  - `trufflehog` scan: 0 verified + 0 unverified secrets across 56812 chunks / 521 MB
  - `cargo audit` and `govulncheck` clean
  - npm audit clean (dashboard dependencies)
  - 9/9 sandbox breakout tests passing (bwrap + Landlock + cgroups + netns); see
    [docs/security-test-report.md](docs/security-test-report.md)
- 16 CI workflows green at release
  (build, test, CodeQL, OSSF Scorecard, cargo-deny, supply-chain, Renovate, coverage)

[Unreleased]: https://github.com/silentspike/project-sentinel/compare/v0.1.0-alpha...HEAD
[0.1.0-alpha]: https://github.com/silentspike/project-sentinel/releases/tag/v0.1.0-alpha
