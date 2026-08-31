# cortex-gateway

## Purpose

`cmd/cortex-gateway` is the Go LLM gateway. It proxies provider calls, assembles agent prompts, applies synthesis/interception rules, validates extracted actions, enforces capability policy, exposes control-plane endpoints, and records optional event-store telemetry.

## Interfaces

- Public proxy server on `CORTEX_PORT` (default `8080`).
- Control plane on `CORTEX_CONTROL_PORT` (default `8081`).
- `/metrics` exposes Prometheus metrics.
- Internal packages under `internal/` implement provider routing, guardrails, capability policy, extraction, synthesis, sequencing, traffic control, APICP, and observability.
- Optional event persistence is enabled by `SENTINEL_CORTEX_EVENT_STORE_PATH`.

## Dependencies

- `pkg/sentinel-go/eventstore` and `pkg/sentinel-go/judge`.
- `modernc.org/sqlite`, `prometheus/client_golang`, and `BurntSushi/toml`.
- External provider credentials are supplied through protected files or the provider's native authentication store; no provider call is required for unit tests.

## Codex CLI provider

`codex-cli` is the default single-node subscription-backed provider. The
gateway executes the pinned native Codex CLI in ephemeral, read-only mode with
user configuration, rules, tools, web search, plugins, memories, delegation,
and inherited shell environment disabled. It passes the prompt on standard
input, accepts only a completed natural-language message plus terminal usage,
and rejects every tool event fail-closed.

Install the already downloaded official release with
`deploy/scripts/install-native-codex.sh`. Authenticate directly on the target
host as the `ubuntu` service user:

```bash
sudo -u ubuntu env HOME=/home/ubuntu CODEX_HOME=/home/ubuntu/.codex /opt/sentinel/bin/codex login --device-auth
```

Authentication material must never be copied from a workstation. The gateway
requires the exact Gate B catalog attestation and a token-free local binary and
login-status check before readiness succeeds. Codex JSONL usage is priced with
the public OpenAI rate-card equivalent and marked `usage_price_table`; it is not
misrepresented as the marginal charge of a ChatGPT-plan invocation.

## Verify

```bash
cd cmd/cortex-gateway
go test ./...
go build ./...
```

Gateway runtime or provider-path changes require deploy-VM verification with the gateway intentionally started only when the issue scope allows LLM traffic.
