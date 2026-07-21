# Governance

This document maps the project's governance mechanisms to concrete code
paths. Governance here means: how is policy expressed, how are decisions
made, and how is each decision verifiable after the fact?

For the architectural-decision context see
[docs/architecture/togaf-architecture-guide.html](architecture/togaf-architecture-guide.html)
(TOGAF v22.1, cluster 05b).

## Three controlplanes

Sentinel runs three independent observe / decide / act / verify loops.
Each owns one decision domain and never reaches across.

| Controlplane     | Owns                     | Lives in                                                  |
|------------------|--------------------------|-----------------------------------------------------------|
| Agent CP         | per-agent biology, mood  | `services/sentinel-daemon/src/controlplane/`              |
| Platform CP      | infra health, budgets    | `services/sentinel-daemon/src/platform_controlplane/`     |
| API CP           | LLM cost + provider mix  | `cmd/cortex-gateway/internal/apicp/`                      |

### Loop structure

Every controlplane implements the same four files:

| Phase   | Agent CP file                                  | Purpose                                                |
|---------|------------------------------------------------|--------------------------------------------------------|
| observe | `controlplane/observe.rs`                      | gather state from ECS + projections                    |
| decide  | `controlplane/decide.rs`                       | apply rules, produce candidate `Action` with TTL       |
| act     | `controlplane/act.rs`                          | enact the action against world state, log to event log |
| verify  | `controlplane/verify.rs`                       | check post-condition; rollback if invariant broken     |

Platform CP follows the same pattern under
`services/sentinel-daemon/src/platform_controlplane/`. API CP applies it to
LLM provider routing in `cmd/cortex-gateway/internal/apicp/`.

## Policy expression

Policy is **always code or config**, never an out-of-band runbook step.

| Policy domain           | Where it lives                                     |
|-------------------------|----------------------------------------------------|
| Agent personalities     | `config/agents/AGENT-*.toml` (Big Five, role, shift) |
| Office layout           | `config/rooms.toml` (26 rooms, adjacency graph)    |
| Simulation tick rate    | `config/simulation.toml`                           |
| LLM provider routing    | `config/cortex-gateway.toml`                       |
| Synthesis intercept     | `cmd/cortex-gateway/internal/synthesis/rules.go`   |
| Sandbox profiles        | `crates/sentinel-sandbox/src/profile.rs`           |
| Daemon resource limits  | `config/daemon.toml` + `deploy/systemd/sentinel-daemon.service` |
| NATS streams            | `pkg/sentinel-go/messaging/streams.go` (SSOT)      |

## Verifiability — events as the audit trail

Every state-changing decision goes through the **event store** before it
takes effect, so the audit trail is the simulation itself.

| Mechanism            | Code                                           |
|----------------------|------------------------------------------------|
| Append-only event log | `crates/sentinel-limbo/`                      |
| Idempotency key       | `operation_id` UNIQUE INDEX                   |
| Causation chain       | `correlation_id` + `causation_id`             |
| Saga pattern          | `compensation_type` field on each `DomainEvent` |
| Outbox delivery       | `append_with_outbox()` for at-least-once dispatch |
| Snapshots             | `save_snapshot()` / `get_latest_snapshot()`   |
| Monotonic offsets     | `update_offset()` enforces no-rewind          |

Deterministic replay re-runs the entire event log into the ECS. Hash chain
is computed in `services/sentinel-nightrun/` so replay divergence is detectable.

## Boundaries — sandbox isolation

Each LLM-persona agent runs inside a sandbox profile so a misbehaving agent
cannot escape its assigned room of state.

| Boundary           | Mechanism                                  | Code                                                |
|--------------------|--------------------------------------------|-----------------------------------------------------|
| File system        | bwrap + Landlock LSM                       | `crates/sentinel-sandbox/src/bwrap.rs`              |
| CPU + memory       | cgroups v2 (per-agent)                     | `crates/sentinel-sandbox/src/cgroups.rs`            |
| Network            | bwrap full-cage netns, loopback only       | `crates/sentinel-sandbox/src/{bwrap.rs,enforcer.rs}`|
| Tool execution     | Wasmtime WASM runtime                      | `crates/sentinel-wasm/`                             |
| Verification       | 9/9 breakout tests                         | [docs/security-test-report.md](security-test-report.md) |

## CI / supply-chain governance

Sixteen GitHub Actions workflows enforce policy on every push and PR.
Policy is in `.github/workflows/` and is itself version-controlled.

| Workflow                    | Enforces                                       |
|-----------------------------|------------------------------------------------|
| `ci.yml`                    | Rust + Go + Dashboard build, lint, test        |
| `codeql.yml`                | SAST across Go + Rust + JavaScript             |
| `coverage.yml`              | cargo-tarpaulin coverage to Codecov            |
| `deny.yml`                  | License, advisory, source bans (`deny.toml`)   |
| `security.yml`              | cargo audit + govulncheck + npm audit          |
| `scorecard.yml`             | OSSF Scorecard                                 |
| `pr-quality.yml`            | PR body has the seven required sections        |
| `pr-lint.yml`               | Conventional Commits enforced                  |
| `issue-quality.yml`         | Issue carries AC + verify + evidence           |
| `main-push-guard.yml`       | Direct-push to main raises an incident issue   |
| `release.yml`               | Tag triggers CHANGELOG extract + CycloneDX SBOM |
| `deps-freshness.yml`        | Toolchain + dependency freshness report        |
| `renovate.yml`              | Weekly Renovate-bot dependency PRs             |
| `auto-label.yml`            | Auto-applies labels on issue/PR open           |
| `labels.yml`                | Manual label-schema sync (~42 labels)          |

## Decision log

Architectural-decision history is captured in
[CHANGELOG.md](../CHANGELOG.md) (per release) and in the TOGAF guide's
deviation register
([docs/togaf-deviations-v22.md](togaf-deviations-v22.md)). ADRs are kept
internal — they contain pre-public iteration history that is not useful
to a public reader.
