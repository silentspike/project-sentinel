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

### Fixed

- Clippy `manual_range_contains` warnings in sentinel-common validation
- Clippy `type_complexity` warning in telemetry test mock
- `cargo fmt` formatting across workspace
