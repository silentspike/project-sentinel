# Testing Strategy

## Gate Matrix

| Gate | Required Tests | Blocking | Target Duration |
|------|----------------|----------|-----------------|
| **PR** | Static (lint, fmt, clippy, vet), Unit, Integration, Security-Basics, Build | Yes | p95 <= 12 min |
| **Main** | Full Integration, System/Artifact Smoke | Yes | <= 20 min |
| **Nightly** | E2E Journeys (playwright-cli), Load Baseline | No (informational) | <= 60 min |
| **Release** | System full, E2E critical, NFR thresholds | Yes | <= 45 min |

## Test Levels

| Level | Command | Environment | Evidence Path |
|-------|---------|-------------|---------------|
| **Static (Rust)** | `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings` | CI (self-hosted) | CI log |
| **Static (Go)** | `cd cmd/cortex-gateway && go vet ./... && golangci-lint run` | CI (self-hosted) | CI log |
| **Static (Dashboard)** | `cd dashboard && bun run typecheck` | CI (self-hosted) | CI log |
| **Unit (Rust)** | `cargo test --workspace` | CI (self-hosted) | CI log / cargo-nextest report |
| **Unit (Go)** | `cd cmd/cortex-gateway && go test -race -count=1 ./...` | CI (self-hosted) | CI log |
| **Unit (Dashboard)** | `cd dashboard && bun test` | CI (self-hosted) | CI log |
| **Integration** | `cargo test --workspace --features integration` | CI with NATS/Zenoh | CI log |
| **Security (Rust)** | `cargo audit` | CI | `deny.toml` advisories |
| **Security (Go)** | `govulncheck ./...` | CI | CI log |
| **Security (Dashboard)** | `cd dashboard && bun audit` | CI | CI log |
| **System/Artifact** | Deploy binary to VM, `curl -sf http://localhost:8080/health` | VM <deploy-vm> | Health response + journalctl |
| **E2E/Smoke** | `cd tests/e2e && bun test` (playwright-cli based) | VM <deploy-vm> + browser | Screenshots + test report |

## Rules

- No merge without green `ci-pass` status check.
- No release without green Release gate.
- `N/A` is only valid with a reason and linked follow-up issue.
- Flaky tests require a dedicated tracking issue (`type:bug`) with expiration date.
- Disabled tests (`#[ignore]`, `skip`) require linked issue number in comment.
- Code review alone is NOT evidence. Only running commands with output counts.
