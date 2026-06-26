# Security Threat Model

TOGAF cluster: 03 Infrastructure

Status: baseline threat model for issue #390. This document classifies the
security risks that are not solved by using Rust alone. It is intentionally
focused on assets, attacker classes, current mitigations, and prioritized gaps;
implementation work stays in follow-up issues.

## Scope

This threat model covers the Project Sentinel runtime and control plane:
agents, the Cortex Gateway, the daemon/operator APIs, the console/projection
read side, the event/snapshot stores, sentinel-fs, sandboxed tool execution, and
the dependency/build pipeline.

Out of scope for this document: formal certification, a complete STRIDE table,
and implementation of new mitigations.

## Asset Inventory

| Asset | Why it matters | Primary components |
| --- | --- | --- |
| Agent identity, persona, bio-state, mood, perception, episodic/semantic memory | Core simulation integrity; poisoned state can steer future behavior | `config/agents/`, `crates/sentinel-bio/`, `crates/sentinel-hippocampus/`, `services/sentinel-nightrun/` |
| Event Store, Outbox, snapshots, deterministic replay hashes | Audit trail and time-machine trust boundary | `crates/sentinel-limbo/`, daemon runtime, nightrun replay |
| Operator API, console controls, restore/control endpoints | Human control plane; misuse can alter or reset the simulation | `services/sentinel-daemon/`, `services/sentinel-dashboard-backend/`, `console/`, projection DB |
| Cortex Gateway auth, prompt compiler, provider credentials, LLM responses | Boundary between diegetic simulation and external model providers | `cmd/cortex-gateway/`, provider registry, prompt compiler |
| sentinel-fs CAS, chunks, trash, agent home mounts | Shared filesystem integrity and cross-agent isolation | `crates/sentinel-fs/`, `crates/sentinel-sandbox/` |
| Projection DB and console WebTransport/read models | Operator visibility; stale or forged projections hide incidents | `/opt/sentinel/data/projection.db`, `services/sentinel-dashboard-backend/`, `console/src/` |
| Sandbox/runtime/kernel primitives | Last line between untrusted tool code and host resources | bwrap, Landlock, cgroups, netns, Wasmtime, eBPF, FUSE |
| CI, release artifacts, dependency graph | Supply-chain trust root for binaries and deployed services | GitHub Actions, Cargo, Go, Bun/npm, Wasmtime, aya, system packages |

## Attacker Class 1 - Compromised Agent From Inside

A compromised agent is an in-simulation principal whose prompt, memory, tool
input, or perception stream has been influenced to make the agent act against
the operator's intent.

### Attack Vectors

- Prompt injection through room chat, operator messages, generated tickets, or
  external text that enters perception or memory.
- Confused-deputy tool use: a model convinces the gateway or tool runtime to
  execute an action with permissions the agent should not have.
- Memory poisoning: injected facts become durable semantic or episodic memory
  and bias later decisions.
- Restore/replay manipulation: malicious content survives a snapshot restore
  and appears trustworthy because it is replayed from the event log.

### Affected Components

Cortex Gateway normalizer/compiler/detection/extraction/capability stages,
operator chat/API paths, WASM tool runtime, sandbox policy, event store,
snapshot/replay, hippocampus memory consolidation, dashboard control endpoints.

### Existing Mitigations

- Gateway pipeline separates normalization, prompt compilation, detection,
  extraction, capability handling, and proxying.
- Issue #391 adds server-side action validation: the gateway loads per-agent
  `[capabilities].tools` from agent TOML, filters extracted actions before
  response/persistence, and writes `agent_action_rejected` audit events for
  blocked actions.
- Self-recognition/fourth-wall detection protects one known class of diegetic
  boundary failure, but it is not a complete prompt-injection defense.
- Tool execution is constrained by bwrap, Landlock, cgroups, netns, Wasmtime,
  and sentinel-fs mounted agent homes.
- Event sourcing and deterministic replay make suspicious state transitions
  auditable after the fact.

### Open Gaps

- Tool permission checks now cover agent identity, action type, tool, and
  declared target, but remaining memory write paths still need explicit trust
  labels or provenance.
- Memory write paths need explicit trust labels or provenance so injected
  content can be filtered, quarantined, or aged out.

## Attacker Class 2 - External Attacker

An external attacker is outside the simulated company and targets exposed
network, API, dashboard, filesystem, or host-service boundaries.

### Attack Vectors

- Unauthorized console/operator API access, including restore/control routes.
- WebSocket or projection-read abuse to exhaust SQLite/WAL readers or hide
  operational state.
- Request smuggling or malformed JSON against the dashboard, daemon, or gateway.
- Misconfigured NATS/Zenoh exposure or service ports outside the intended host
  boundary.
- Local host breakout attempts against sandbox, FUSE, eBPF, or systemd service
  privileges.

### Affected Components

Dashboard Hono server, daemon operator/control API, projection database,
Cortex Gateway HTTP endpoints, NATS/Zenoh bridges, systemd units, sandbox
runtime, sentinel-fs, Deploy-VM service layout.

### Existing Mitigations

- Dashboard reads from optimized projection/read models rather than mutating the
  simulation state directly.
- WebSocket polling was reduced to one projection-owned watermark lookup per
  poll cycle in #277, lowering resource-exhaustion pressure on SQLite WAL.
- Sandbox breakout tests cover bwrap/Landlock/cgroup/netns boundaries.
- Service units separate daemon, projection, dashboard, gateway, and bridge
  processes; gateway can remain stopped for token-safe verification work.
- Public security policy and dependency scanning workflows exist in the GitHub
  repository.
- Dashboard login is rate-limited per client IP with audit logging, the dashboard
  backend binds loopback-only on the deploy VM, and the operator-key lifecycle is
  documented in `docs/security/dashboard-key-management.md` (#474).

### Open Gaps

- Dashboard login brute force is now rate-limited and audited (#474); the remaining
  pre-exposure gates are a CA-issued/edge-terminated TLS model (#473) and operator
  RBAC.
- Control/restore mutations remain session-gated; per-route rate limits and
  structured mutation-audit events remain future work before remote operation is
  allowed.
- FUSE/eBPF/kernel-adjacent code needs regular privilege and unsafe reviews.
  Follow-up for unsafe review: #392.

## Attacker Class 3 - Supply-Chain Dependency Attacker

A supply-chain attacker compromises a dependency, toolchain, build workflow,
package registry, release artifact, or transitive native/system component.

### Attack Vectors

- Malicious or compromised Cargo, Go, Bun/npm, GitHub Action, or system package
  dependency.
- Vulnerable transitive runtime such as Wasmtime/WASI, aya/eBPF, FUSE, SQLite,
  or sandbox support tooling.
- Build/release artifact substitution before deployment to the runtime VM.
- Unsafe Rust or FFI boundary hiding memory-safety assumptions that are not
  documented or tested.

### Affected Components

Rust workspace, Go gateway/bridge modules, dashboard Bun dependencies,
GitHub Actions, release artifacts, deployment scripts, Wasmtime/WASI, bwrap,
Landlock, FUSE, eBPF, SQLite/Limbo integration.

### Existing Mitigations

- Rust, Go, and dashboard dependency checks are part of the repository workflow
  set.
- Release and deploy paths use explicit manifests/scripts instead of ad-hoc
  local binaries.
- The runtime uses sandbox layers so a compromised tool dependency does not
  automatically receive unrestricted host access.
- Prior dependency security work upgraded vulnerable Wasmtime dependencies.

### Open Gaps

- Unsafe Rust needs a dedicated cargo-geiger audit, SAFETY justifications, and a
  CI threshold. Follow-up: #392.
- Critical kernels need formal verification or model checking where ordinary
  tests are too weak. Follow-up: #393.
- Release artifact provenance should be made explicit before public deployment
  claims rely on binary integrity.

## Prioritized Security Gaps

| Priority | Gap | Risk reduced | Follow-up |
| --- | --- | --- | --- |
| P0 | Capability-based tool permissions plus server-side action validation | Compromised agent / prompt injection | #391 implemented for Gateway action extraction; memory provenance remains future work |
| P1 | Unsafe audit with SAFETY justifications and CI threshold | Supply-chain and kernel-adjacent memory-safety regressions | #392 |
| P1 | Formal verification for critical Event Store, snapshot, and bio invariants | State corruption that normal tests may miss | #393 |
| P2 | Operator/console auth hardening before broad exposure | External attacker control-plane abuse | #402 implemented: server-side session store + httpOnly+SameSite=Strict cookie (operator key no longer JS-readable). #474 implemented: strong operator secret, per-IP login rate limit + audit logging, loopback-only bind + UFW closure. Residual: see Accepted Residual Risks |
| P2 | Release provenance and artifact integrity policy | Supply-chain substitution | Future issue before public release hardening |

## Accepted Residual Risks

| Risk | Attacker class | Why accepted (current) | Direktive / Revisit |
| --- | --- | --- | --- |
| Operator key (login POST body) and session cookie transit over the deploy VM console endpoint (`:8001`) | External Attacker (network) | After #474 the endpoint is no longer LAN-exposed: the backend binds loopback-only and the UFW `:8001` rule is removed, so key/cookie transit happens over loopback or an SSH tunnel rather than the open LAN. Console traffic is HTTPS with a self-signed certificate; same-origin + `SameSite=Strict` block cross-site CSRF; the SolidJS frontend escapes by default (no `innerHTML`). Login is rate-limited + audited (#474). | Deliberate exposure for third-party deployments MUST sit behind an HTTPS-terminating proxy / CA-issued cert (#473); keep `DASHBOARD_COOKIE_SECURE=on`. |
| Remote operator console over an `ssh -L` tunnel is HTTP-only | External Attacker (network) / operability | WebTransport is QUIC/UDP and does not traverse a TCP `ssh -L` tunnel, so remote login/REST works but the live push does not. Accepted for the single-operator setup: full live console requires on-VM access. | Full remote live console via a UDP-capable overlay (WireGuard/ZTNA) is tracked in #522 (lab ops — distinct from the #473 product exposure model). |
| Console WebTransport telemetry | External Attacker (network) | WebTransport uses the same HTTPS origin and requires an authenticated one-time ticket. All control actions (pause/resume/provider/restore/chaos/stimulus/snapshot) go through `/api/control/*` with `require_auth` (session cookie). | Revisit if telemetry is later classified sensitive beyond the operator-console trust boundary. |
| Daemon Prometheus text exporter (`:9090`) | External Attacker (network) | After #525 the exporter binds loopback-only (`[daemon.metrics] bind_addr = 127.0.0.1:9090`, config-driven like `operator_api`); its only consumer is the dashboard-backend `/api/metrics` proxy on the same host, and the deploy UFW carries no `:9090` rule (default-deny). Replaces the previously hardcoded `0.0.0.0:9090` wildcard bind. | Deliberate LAN scrape = override `bind_addr` in `daemon.toml` behind a documented firewall rule; do not re-add a `0.0.0.0` bind. |
| nats-bridge health endpoint (`:8083`) | External Attacker (network) | After #525 the health server binds loopback-only (`[server] health_bind_addr = 127.0.0.1:8083`, respecting the configured `health_port`); the only consumers are on-VM (smoke-test, health-monitor), and the stale `:8083` UFW allow rule is removed. Replaces the previously implicit wildcard `":port"` bind. | Deliberate exposure = override `health_bind_addr`; keep health responses free of sensitive data. |
| Zenoh listen endpoint (daemon-internal bus) | External Attacker (network) | After #525 the Zenoh listen endpoint is pinned to `tcp/127.0.0.1:0` in both `open_session` paths (single-node, daemon-internal — there are no cross-process Zenoh peers on the deploy VM; cross-node traffic is the QUIC ClusterControl stream #569, not Zenoh). UFW default-deny additionally blocks the (anyway ephemeral) listen port from the LAN. The pin is warn-not-fatal, so the absence of a `loopback listen pin failed` line in the daemon log plus `ss` showing the listener on `127.0.0.1` is the definitive proof it took effect. | Revisit only if a future multi-node Zenoh peer set is introduced (would need a real routable listen endpoint); a tested, multi-node-safe loopback-pinning follow-up is tracked separately. |

## Operating Rule

Security claims must name the attacker class they address. A mitigation that
contains external HTTP abuse does not automatically address prompt injection,
and a Rust memory-safety property does not automatically protect against a
compromised model, poisoned memory, or malicious dependency.
