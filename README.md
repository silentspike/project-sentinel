# Project Sentinel

Neuro-symbolische Buero-Simulation: deterministische Weltregeln (ECS) kombiniert mit probabilistischen Agent-Entscheidungen (LLM).

## Architektur

```
ECS-Kern (Rust)          Cortex Gateway (Go)         Dashboard (Bun)
  bevy_ecs World    <-->   LLM Proxy + Pipeline   -->  Echtzeit-UI
  Bio/Physics/Mood         Perception Injection        WebSocket
  Event Store (Limbo)      Fourth-Wall Detection       Metriken
  State Store (redb)       Action Extraction
```

- **ECS** berechnet Bio-Zustaende, Physik, Raeume (deterministisch, reproduzierbar)
- **LLM** empfaengt Wahrnehmungs-Texte, entscheidet Aktionen (kreativ, nicht-deterministisch)
- **Agents** wissen nicht, dass sie simuliert werden (Fourth-Wall-Prinzip)

## Verzeichnisstruktur

| Verzeichnis | Inhalt |
|-------------|--------|
| `crates/` | Rust Workspace (ECS, Bio, Physics, Zenoh, redb, Limbo, Sandbox, eBPF, Wasm, Common, Telemetry, Runtime, Hippocampus, Inference) |
| `cmd/cortex-gateway/` | Go LLM-Proxy (7-Step-Pipeline, Provider Registry, Control Plane) |
| `dashboard/` | Bun + Hono Frontend/Backend |
| `schemas/` | FlatBuffer-Definitionen (.fbs) |
| `config/` | Raum-Layout, Agent-Definitionen, Simulations-Parameter ([config/README.md](config/README.md)) |
| `services/` | Standalone Services (sentinel-nightrun) |
| `deploy/` | VM-Konfiguration, systemd, Benchmarks ([deploy/README.md](deploy/README.md)) |

## Prerequisites

| Tool | Version | Zweck |
|------|---------|-------|
| Rust | stable (1.93+) | ECS, Crates |
| Go | 1.23+ | Cortex Gateway |
| Bun | 1.x | Dashboard |
| cargo-remote | latest | Remote-Build auf Build-Server |

## Quick Start

```bash
# Alle Checks (Lint + Tests + cargo deny)
make ci

# Remote Build (auf Build-Server 10.0.0.155)
cargo remote -- build

# Tests
cargo remote -- test

# Go Build
cd cmd/cortex-gateway && go build ./...

# Dashboard
cd dashboard && bun install && bun test
```

## Make-Targets

| Target | Beschreibung |
|--------|-------------|
| `make ci` | Vollstaendiger CI-Lauf (fmt, clippy, test, deny, typos, doc) |
| `make build` | Workspace Build |
| `make test` | Alle Tests |
| `make check` | Quick Lint (fmt + clippy) |
| `make lint-all` | Alle Lints (Rust + Go + Dashboard) |
| `make deny` | Supply-Chain-Check (Licenses, Advisories) |
| `make coverage` | Code Coverage (tarpaulin) |
| `make bench` | Benchmarks |
| `make fuzz` | Fuzzing (bolero, libfuzzer) |

## Konfiguration

Siehe [config/README.md](config/README.md) fuer Details zu Raum-Layout, Agent-Definitionen und Simulations-Parametern.

## Deployment

Siehe [deploy/README.md](deploy/README.md) fuer VM-Setup, systemd-Services und Benchmark-Runner.
