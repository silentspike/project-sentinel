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

### Changed

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
