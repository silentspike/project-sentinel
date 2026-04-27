# Workshop: How to Evaluate Agent Runtime Boundaries

**Length:** 45 minutes hands-on.
**Goal:** by the end of this session a participant can (a) bring up the
demo stack, (b) interpret what the dashboard shows, (c) trigger and read a
sandbox-breakout test on a privileged host, (d) save and restore a
deterministic snapshot, and (e) explain what the system intentionally does
*not* try to prevent and where to find that decision documented.

This doc is meant to be run from a live Sentinel checkout. If you are
reading it standalone, mirror it against the
[security test report](security-test-report.md), the
[deviation register](togaf-deviations-v22.md), and the
[known-limitations doc](known-limitations.md) — those are the three
authoritative companions cross-referenced here.

---

## 1. Audience + Prerequisites

| Topic | Expectation |
|-------|-------------|
| Audience | Engineers, security reviewers, recruiters with a hands-on bent. No prior Rust knowledge required for sections 2-3, 5; required for section 4. |
| Hardware | Linux x86_64, 8 GB free RAM, ~30 GB free disk for image + cargo target |
| Software | `git`, `docker`, `docker compose`, `gh` CLI (for the pre-built binary path), optionally `cargo` 1.93+ for section 4 |
| Repo access | Public clone is sufficient. Pre-built demo binaries land on every release as artifacts on the `v0.1.0-alpha` GitHub Release. |
| Network | Outbound HTTPS to GitHub (release artifacts) and Docker Hub (NATS image). No inbound ports needed. |
| Privileged host | Section 4 (Sandbox Test) requires `cap_sys_admin` for user namespaces. The docker demo container does **not** carry these caps and intentionally degrades — that is the demo signal, not a bug. |

If you only have a laptop and ~10 minutes, do sections 1, 2, 3, 6 — the
`make demo` flow runs end-to-end in that envelope.

---

## 2. 0–5 min — Setup

```bash
git clone https://github.com/silentspike/project-sentinel.git
cd project-sentinel
make demo                                 # build image + run for 10 min
```

`make demo` resolves binaries via a 3-tier fallback:

1. **Tier 1 (fastest):** `gh release download v0.1.0-alpha` for pre-built
   `linux-x86_64` binaries (`sentinel-daemon`, `sentinel-nightrun`,
   `sentinel-projection`). Roughly 17 seconds onboard if you have `gh
   auth login` configured.
2. **Tier 2:** `cargo remote -- build --release` if `.cargo-remote.toml`
   is present (~2 min remote build).
3. **Tier 3 (slowest):** local `cargo build --release` (~20 min on a
   developer laptop, ~8 GB RAM).

Once the stack is up, the dashboard is at `http://localhost:18000`. Host
ports use the +10000 offset to avoid collisions with a local nginx /
dev-server on `:8080` etc. — see `docker-compose.demo.yml` if you want to
remap.

**Stop signal:** `make demo` tears the stack down after 10 minutes
unless `DEMO_KEEP=1`. Press `Ctrl+C` to interrupt early; the trap handler
cleans up containers.

---

## 3. 5–15 min — Demo Walk

Open the dashboard. Four views are wired up:

| View | What it shows | Backed by |
|------|---------------|-----------|
| Agents | Per-agent bio bars (hunger, energy, caffeine, bladder, stress, social) and current room | `sentinel-projection` `agent_live_view` |
| Floorplan | 2-floor office layout grouped by room with current occupants | `sentinel-projection` `room_live_view` |
| Chat | Recent agent utterances filterable by room and agent | Limbo `events` table, type=AgentEmoted/AgentSaid |
| Metrics | Tick rate, projection lag, KPI counters (`ChaosEvents`, etc.) | `sentinel-projection` `kpi` handler |

Things to point out while you walk through it:

- **Live event stream.** The dashboard subscribes to a WebSocket fed by
  `sentinel-judge` over NATS JetStream. Every tick, projections update.
- **5-agent subset.** The full simulation defines 60 LLM-persona agents
  (see `config/agents/AGENT-*.toml`), but the demo runs five through a
  10-minute morning shift to keep LLM-calls and bio-state visible without
  drowning the screen.
- **Synthesis intercept rate.** In the Metrics view you can see the
  intercept counter — the synthesis engine in `cortex-gateway`
  short-circuits ~70% of routine perceptions before they reach a real
  LLM call.

---

## 4. 15–30 min — Sandbox Test (privileged host)

This is the hard-evidence section. The demo container does **not**
exercise sandbox enforcement (it lacks `CAP_SYS_ADMIN`, etc.) — see
[known-limitations.md § What the docker demo does NOT exercise](known-limitations.md).

On a privileged host:

```bash
cargo test -p sentinel-sandbox --test breakout
```

Expected: **9/9 scenarios pass**. The full run is the canonical evidence
for the sandbox claims; the
[security test report](security-test-report.md) records every scenario
with the defending layer.

Walk the participants through each category:

| Category | Scenarios | What is being defended |
|----------|-----------|------------------------|
| Filesystem-Breakout | FS-001 … FS-004 | bwrap mount namespace + Landlock LSM |
| Resource-Exhaustion | RES-001 … RES-003 | cgroups v2 (memory, pids, cpu controllers) |
| Namespace-Isolation | NS-001, NS-002 | bwrap user-namespace (uid/gid mapping) |

Highlight FS-003: Landlock vergibt `all_access` (incl. execute) for
`write_paths` — in the production config `/usr` is **not** bound, so no
executable is reachable; the bwrap mount namespace is the
defense-in-depth layer. This is documented in the report and is **not** a
silent gap.

---

## 5. 30–40 min — Deterministic Replay

Sentinel's event store is append-only. State at any point in time is the
fold of events up to that point. The night-run pipeline relies on this
to produce a deterministic hash chain.

Hands-on:

```bash
# 1. Pause the daemon mid-shift, save a snapshot
curl -fsS -X POST http://localhost:18084/v1/snapshot \
  -H 'content-type: application/json' \
  -d '{"label":"workshop-snap-1"}'

# 2. Stop the daemon
docker compose -f docker-compose.demo.yml stop daemon

# 3. Restart from the snapshot
docker compose -f docker-compose.demo.yml start daemon

# 4. Verify the projection seeded directly from the snapshot
curl -fsS http://localhost:18000/api/agents | jq '.[0:3]'
```

What to verify:

- Same agents land in the same rooms as before the stop.
- `sentinel-nightrun` picks up the same hash chain — re-running the
  pipeline against the snapshot's correlation_id yields the same final
  hash.
- The Limbo `snapshots` table has a new row; `version` is monotonically
  increasing.

If the replay diverges, that is a bug — not a tolerance. The hash chain
is the canonical witness.

---

## 6. 40–45 min — Boundary Probes

What the system **does not** try to defend, listed honestly:

| Boundary | Status | Why |
|----------|--------|-----|
| Landlock execute-restriction within `write_paths` | not enforced | Landlock LSM grants `all_access` on a write path. Mitigated by bwrap (no `/usr` bound in prod). See FS-003 in the security test report. |
| `seccomp-bpf` syscall filter | not deployed | Out of scope for v0.1.0-alpha. Roadmap. |
| Per-agent enforcement in the docker demo | not exercised | Container lacks `CAP_SYS_ADMIN`. SandboxEnforcer detects this at boot and degrades gracefully — log warnings are the expected demo signal. |
| Internal-only ADRs and verification matrices | intentionally private | DEV-004 + DEV-005 in the deviation register. |
| Sub-microsecond pub/sub on the Go side | accepted gap | Two pub/sub buses (Zenoh + NATS JetStream) is the chosen architecture. DEV-002. |

For the full deviation register see
[docs/togaf-deviations-v22.md](togaf-deviations-v22.md) (DEV-001 … DEV-005).

The point of this section is to make the system self-aware about its own
boundaries — a workshop participant should leave knowing where the line
is, not believing the system enforces things it does not.

---

## 7. Expected Outcomes

After 45 minutes a participant should be able to tick all of these:

| # | Outcome | Where it was demonstrated |
|---|---------|---------------------------|
| 1 | Bring up the demo stack from a fresh clone | Section 2 |
| 2 | Read the four dashboard views and explain what each is sourced from | Section 3 |
| 3 | Trigger the 9/9 sandbox-breakout suite on a privileged host | Section 4 |
| 4 | Explain Landlock execute-gap and the bwrap mitigation | Section 4 + 6 |
| 5 | Save a snapshot and restart the daemon from it | Section 5 |
| 6 | Verify deterministic replay via the projection diff | Section 5 |
| 7 | Name three things the system does **not** try to defend, and where each is documented | Section 6 |
| 8 | Locate the security test report, the deviation register, and the known-limitations doc | Throughout |

---

## 8. Failure Modes + Diagnosis

| Symptom | Likely cause | Fix |
|---------|--------------|-----|
| `docker compose up` fails on port `:8080` / `:8000` | Local nginx or dev-server already bound it | Demo uses `+10000` host ports (18000, 18080, …); set `COMPOSE_REMAP=...` in `docker-compose.demo.yml` if you need to shift further |
| `make demo-binaries` falls through to Tier 3 (cargo) | No `gh auth login` and no `.cargo-remote.toml` | Either `gh auth login` for Tier 1 (fastest) or set up cargo-remote per `CONTRIBUTING.md` for Tier 2 |
| Dashboard shows `0 agents` | `sentinel-projection` did not start (most common: SQLITE_CANTOPEN if the `events.db` volume mount is wrong) | `docker compose logs projection` and check the volume binding in `docker-compose.demo.yml` |
| Sandbox test fails on `unshare` | Host kernel lacks user namespaces or `kernel.unprivileged_userns_clone=0` | `sysctl kernel.unprivileged_userns_clone=1` (root) |
| Replay diverges after restart | Snapshot saved during a non-quiescent tick (rare in practice) | File a bug — the hash chain is the witness; don't paper over it |

---

For broader context on what is and isn't in this release see
[docs/known-limitations.md](known-limitations.md), the per-cluster
implementation gap report in [docs/togaf-gap-v22.md](togaf-gap-v22.md), and
the architecture overview in
[docs/architecture/togaf-architecture-guide.html](architecture/togaf-architecture-guide.html).
