# Project Sentinel

[![CI](https://github.com/silentspike/project-sentinel/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/silentspike/project-sentinel/actions/workflows/ci.yml)
[![Coverage](https://github.com/silentspike/project-sentinel/actions/workflows/coverage.yml/badge.svg?branch=main)](https://github.com/silentspike/project-sentinel/actions/workflows/coverage.yml)
[![CodeQL](https://github.com/silentspike/project-sentinel/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/silentspike/project-sentinel/actions/workflows/codeql.yml)
[![Supply Chain](https://github.com/silentspike/project-sentinel/actions/workflows/deny.yml/badge.svg?branch=main)](https://github.com/silentspike/project-sentinel/actions/workflows/deny.yml)
[![OSSF Scorecard](https://github.com/silentspike/project-sentinel/actions/workflows/scorecard.yml/badge.svg?branch=main)](https://github.com/silentspike/project-sentinel/actions/workflows/scorecard.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/silentspike/project-sentinel?include_prereleases&label=release)](https://github.com/silentspike/project-sentinel/releases)
[![Rust 1.93+](https://img.shields.io/badge/rust-1.93%2B-orange.svg)](https://www.rust-lang.org)
[![Go 1.26+](https://img.shields.io/badge/go-1.26%2B-blue.svg)](https://go.dev)

**A research testbed for runtime hardening, controlplane design, and LLM agent
boundary detection.** Sixty LLM-persona agents live inside a fictional web
design agency under strict sandbox isolation. The simulated office is the
*evaluation context*; the platform underneath is the work.

## What It Is

A testbed environment combining two agent layers:

**60 LLM-persona agents** — autonomous entities with distinct personalities
(Big Five profiles), roles (developers, designers, management, works council,
medical staff), and bio-driven behavior (hunger, caffeine, stress, social
need). Staffed as **51 on a 3-shift rotation (17 per shift)** + **9 always-on
duty staff** (works council, occupational psychologist, occupational
physician). Approximately 26 agents are active at any given moment.

**5 background service agents** — Rust/Go services running the platform itself:

- `sentinel-daemon` — ECS world, tick loop, persistence
- `cortex-gateway` — LLM proxy, synthesis engine, controlplane
- `sentinel-judge` — quality + drift monitoring, NATS streaming
- `sentinel-nightrun` — nightly batch consolidation, deterministic replay
- `sentinel-nats-bridge` — eBPF metrics dual-publish

Both layers run under sandbox isolation (bwrap + Landlock + cgroups v2 +
netns). The simulated office is the evaluation context for stress-testing
runtime hardening primitives, agent control loops, and boundary detection.

See the [TOGAF Architecture Guide](docs/architecture/togaf-architecture-guide.html)
(v22.1) for cluster-level detail.

## Why It Exists

Three things are hard to study without a believable, persistent, multi-agent
environment:

1. **Sandbox primitives at scale.** What does bwrap + Landlock + cgroups
   v2 + netns actually cost when 26 agents tick simultaneously? Where do
   the breakouts come from when nobody is looking? The
   [security test report](docs/security-test-report.md) records 9/9
   breakout tests passing.
2. **Controlplane design.** Three independent observe / decide / act /
   verify loops (Agent CP, Platform CP, API CP) co-exist. Each owns one
   decision domain, none reach across. See
   [docs/governance.md](docs/governance.md).
3. **Boundary detection.** When does an LLM agent realize it is an LLM?
   The Cortex Gateway's fourth-wall detector (15 regex + LLM judge)
   measures it; the synthesis engine intercepts ~70% of routine
   perceptions before they reach a real LLM call.

PixelPerfekt GmbH (the fictional employer) is a Truman-Show framing — see
[docs/glossary.md](docs/glossary.md) for the narrative convention.

## Architecture at a Glance

```
Deterministic (ECS)            Probabilistic (LLM)
┌───────────────────┐          ┌────────────────────┐
│ bevy_ecs World    │          │ Cortex Gateway     │
│ Bio / Physics     │ ───────> │ 7-step pipeline    │
│ 60 agent slots    │ <─────── │ Synthesis engine   │
│ Event Store       │          │ Fourth-wall guard  │
└───────────────────┘          └────────────────────┘
         │                              │
         └────── Event Sourcing ────────┘
            (sentinel-limbo, append-only)
```

| Layer            | Tech                                      |
|------------------|-------------------------------------------|
| World simulation | Rust workspace (15 crates), `bevy_ecs`    |
| LLM gateway      | Go (`cmd/cortex-gateway`)                 |
| Quality monitor  | Go (`services/sentinel-judge`)            |
| Dashboard        | Bun + Hono + vanilla-JS (`dashboard/`)    |
| Pub/Sub          | Zenoh (Rust SHM <10 µs) + NATS JetStream  |
| Storage          | redb (state) + Limbo SQLite (events)      |

For per-cluster implementation status see
[docs/togaf-gap-v22.md](docs/togaf-gap-v22.md).
For deliberate deviations from the spec see
[docs/togaf-deviations-v22.md](docs/togaf-deviations-v22.md).

## Quick Start

### Prerequisites

| Tool        | Version  | Purpose                       |
|-------------|----------|-------------------------------|
| Rust        | 1.93+    | ECS world, all Rust crates    |
| Go          | 1.23+    | Gateway, judge, nats-bridge   |
| Bun         | 1.x      | Dashboard                     |
| cargo-remote (optional) | latest | Remote build server  |
| Docker + Compose | 24+ | Demo stack                    |

### Configure

Sentinel takes deployment-specific values from a single local file. Copy
the templates and fill in your own values:

```bash
cp .env.example .env
cp .make.local.example .make.local
```

The `.env` file holds runtime values (NATS URL, dashboard port). The
`.make.local` file holds build values (cargo remote server address, deploy
target). Neither file is committed.

### Build

```bash
make ci          # full: fmt + clippy + test + cargo-deny + typos
make build       # workspace build
make test        # all tests
```

If you have cargo-remote configured for offload builds, those targets
transparently use it.

### Demo (one command)

![Sentinel demo dashboard](docs/images/sentinel-demo.gif)

```bash
make demo                                 # build binaries + image, then run
# or, step by step:
make demo-binaries                        # build sentinel-daemon + sentinel-nightrun
make demo-image                           # docker build
./scripts/demo.sh                         # run + open dashboard, tear down after 10 min
```

The Rust workspace is heavy. `make demo-binaries` uses `cargo-remote`
against a build server if `.cargo-remote.toml` is present, otherwise
falls back to a local `cargo build --release` (~8 GB RAM, ~20 min on
a developer laptop). See [CONTRIBUTING.md](CONTRIBUTING.md) for
cargo-remote setup if you want to offload the Rust compile.

Runs five agents through a 10-minute morning shift with a default
PixelPerfekt configuration. Dashboard: http://localhost:18000 (host port
18000 is used because 8000 is commonly bound by local nginx/dev servers;
adjust in `docker-compose.demo.yml` if you have 8000 free).

#### What the docker demo shows — and what it does not

The compose stack is deliberately a **behavioral demo**, not a full
production deployment. It is meant to give a recruiter or curious reader
a working dashboard in one command, not to reproduce the full sandbox
story.

| Feature                                 | Demo container | VM deploy |
|-----------------------------------------|----------------|-----------|
| ECS world, Bio-Engine, Physics          | yes            | yes       |
| Event sourcing + projections + dashboard| yes            | yes       |
| Cortex Gateway pipeline + synthesis     | yes            | yes       |
| NATS JetStream + sentinel-judge         | yes            | yes       |
| **bwrap + Landlock per-agent isolation**| no (warned)    | yes       |
| **cgroups v2 per-agent resource caps**  | no (warned)    | yes       |
| **netns + nftables agent network**      | no (warned)    | yes       |
| **eBPF probes (aya-rs)**                | no (warned)    | yes       |
| **sentinel-fs CAS-FUSE**                | no (warned)    | yes       |
| Zenoh SHM transport                     | no (TCP only)  | yes       |

These kernel-bound features need user namespaces, `CAP_BPF`,
`CAP_SYS_ADMIN`, `CAP_NET_ADMIN`, and a writeable bpf-fs / `/dev/fuse`.
A plain unprivileged container has none of those. The
`SandboxEnforcer` (`crates/sentinel-sandbox/src/enforcer.rs`) detects
the absence at boot and degrades gracefully — warnings in the daemon
log are the expected demo signal.

For the full stack with sandbox enforcement see
`deploy/systemd/*.service` and the deployment notes in
[docs/governance.md](docs/governance.md).

## Status — what works in this alpha, what doesn't yet

| Area | Status |
|------|--------|
| ECS world (bevy_ecs), bio + physics + room sim                   | ✅ implemented + exercised in demo |
| Event sourcing (Limbo SQLite, idempotent, replayable)            | ✅ implemented + exercised in demo |
| Cortex Gateway 7-step pipeline + 10-rule synthesis engine        | ✅ implemented + exercised in demo |
| Dashboard (Bun + Hono + WebSocket)                               | ✅ implemented + exercised in demo |
| sentinel-judge quality + drift monitoring (NATS streaming)       | ✅ implemented + exercised in demo |
| sentinel-projection CQRS read-models                             | ✅ implemented + exercised in demo |
| sentinel-nightrun batch consolidation, deterministic replay      | ✅ implemented, manual trigger only |
| 60 LLM-persona agents (`config/agents/AGENT-*.toml`)             | ✅ defined; demo runs a 5-agent subset |
| **bwrap + Landlock per-agent isolation**                         | ✅ implemented (`crates/sentinel-sandbox/`); 9/9 breakout tests pass on a privileged host; **not exercised in the docker demo** |
| **cgroups v2 per-agent caps + netns + nftables**                 | ✅ implemented; same caveat |
| **eBPF probes (aya-rs)** + **sentinel-fs CAS-FUSE**              | ✅ implemented; same caveat |
| TOGAF v22.1 architecture guide + per-cluster gap report          | ✅ shipped in `docs/architecture/` |
| Pre-built demo binaries (linux-x86_64) on every release          | ✅ since v0.1.0-alpha |
| Tag verified-badge on GitHub                                     | ⏳ pending maintainer's SSH signing-key registration; tag itself carries valid Ed25519 signature |
| CodeQL pipeline live status                                      | ⏳ green only after first scheduled run post-public-flip (GHAS gating) |
| Demo binaries for arm64 / Apple Silicon                          | ⏳ planned (currently linux-x86_64 only) |
| Multi-tenant company configs ("Gaia firmen-konfigurator")        | ⏳ tracked as roadmap issue |

See [docs/known-limitations.md](docs/known-limitations.md) for the full
caveat list.

## Repository Layout

| Path                         | Contents                                                    |
|------------------------------|-------------------------------------------------------------|
| `crates/`                    | 15 Rust crates (ECS, bio, physics, sandbox, eBPF, …)        |
| `services/sentinel-daemon/`  | Daemon + controlplane                                       |
| `services/sentinel-judge/`   | Quality / drift monitor (Go)                                |
| `services/sentinel-nightrun/`| Nightly consolidation (Rust)                                |
| `services/sentinel-nats-bridge/` | NATS event bridge (Go)                                  |
| `cmd/cortex-gateway/`        | LLM proxy + synthesis (Go)                                  |
| `dashboard/`                 | Bun + Hono real-time UI                                     |
| `pkg/sentinel-go/`           | Shared Go package (judge heuristics, eventstore, messaging) |
| `config/`                    | Agent TOMLs, room layout, simulation parameters             |
| `docs/`                      | Architecture, governance, gap, deviations, glossary         |
| `deploy/`                    | systemd units, release manifest schema                      |
| `.github/workflows/`         | 16 CI workflows (build, test, security, supply chain)       |

## Documentation

| Doc                                                          | Purpose                                       |
|--------------------------------------------------------------|-----------------------------------------------|
| [llms.txt](llms.txt)                                         | LLM-friendly project index (read first)       |
| [docs/architecture/togaf-architecture-guide.html](docs/architecture/togaf-architecture-guide.html) | Authoritative architecture reference (v22.1) |
| [docs/governance.md](docs/governance.md)                     | Governance mechanisms ↔ code path mapping     |
| [docs/togaf-gap-v22.md](docs/togaf-gap-v22.md)               | Per-cluster implementation status             |
| [docs/togaf-deviations-v22.md](docs/togaf-deviations-v22.md) | Intentional deviations from the spec          |
| [docs/glossary.md](docs/glossary.md)                         | PixelPerfekt narrative + agent-layer glossary |
| [docs/security-test-report.md](docs/security-test-report.md) | Sandbox breakout test results                 |
| [CONTRIBUTING.md](CONTRIBUTING.md)                           | How to contribute                             |
| [SECURITY.md](SECURITY.md)                                   | Reporting vulnerabilities                     |
| [CHANGELOG.md](CHANGELOG.md)                                 | Release history                               |

## Release status

This is the first **public** release boundary. The project was developed
privately prior to `v0.1.0-alpha`; the tag marks the boundary between
private development and public visibility, not the start of the project.

CI on `main`: ci, lint, coverage, supply-chain (cargo-deny, npm-audit,
go-vuln, rust-audit), conventional-commits, dependency-freshness — green.
CodeQL goes green on the first scheduled run after the public flip
(GHAS gating). Security: dependency audit + `gitleaks` + `trufflehog` clean,
9/9 sandbox breakout tests passing on a privileged host.

See [docs/known-limitations.md](docs/known-limitations.md) for full caveats
and the [Status table above](#status--what-works-in-this-alpha-what-doesnt-yet)
for the per-feature picture.

## License

See [LICENSE](LICENSE).
