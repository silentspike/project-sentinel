# Workshop: How to Evaluate Runtime Governance for LLM Coding Agents

> **45 minutes hands-on.** A guided session for engineering leadership,
> security engineers, and customer-engineering teams that need to put
> LLM coding agents (Codex, Claude Code, Gemini CLI, in-house tools)
> into a real workflow and answer the three operational questions
> that always come back: how are they sandboxed, how are their actions
> audited, what happens when something goes wrong.
>
> The workshop is **tool-agnostic**: it does not assume a specific LLM
> provider. The runtime principles transfer.

---

## Audience + Pre-conditions

| Topic | Expectation |
|-------|-------------|
| Audience | Engineering leadership, security engineers, customer-engineering teams |
| Hardware | Linux x86_64, 8 GB free RAM, ~30 GB free disk |
| Software | `git`, `docker`, `docker compose`, `gh` CLI; optionally `cargo` 1.93+ for Section 3 |
| Privileged host | Section 3's sandbox-breakout exercise needs `cap_sys_admin` for user namespaces. The docker demo container does not have it and intentionally degrades — that is the demo signal, not a bug. |

---

## 0–5 min — Why runtime governance for AI agents matters

Three operational questions come back from every team putting agents
into a real workflow:

1. **How are they sandboxed?** What kernel primitives stop a misbehaving
   agent from reaching outside its execution box?
2. **How are their actions audited?** What is the immutable record that
   lets you reproduce what happened, not approximate it?
3. **What happens when something goes wrong?** Who decides to pause,
   throttle, or roll back, and how is that decision recorded?

Project Sentinel makes those questions concrete by running a
real-LLM-call workload of sixty personas through the runtime layer an
organization would actually operate. The workshop walks the runtime,
not the simulation.

---

## 5–15 min — Architecture walkthrough

The runtime is built around a per-agent sandbox stack and three
independent control planes.

```text
   workload (60 LLM-persona agents)
         │
         ▼
  ┌──────────────────────────────────────────────────────────┐
  │  Sandbox stack (per agent, kernel-enforced)              │
  │  bwrap (user-ns) · Landlock LSM · cgroups v2 ·           │
  │  full-cage netns · Wasmtime tool runtime                │
  └──────────────────────────────────────────────────────────┘
         │ stdin/stdout JSON · audited stream
         ▼
  ┌──────────────────────────────────────────────────────────┐
  │  Cortex Gateway (Go) — proxy + 10-rule synthesis engine  │
  │  ~70% of routine perceptions intercepted before LLM call │
  └──────────────────────────────────────────────────────────┘
         │ event stream
         ▼
  ┌──────────────────────────────────────────────────────────┐
  │  Event store (Limbo SQLite, append-only, hash-chained)   │
  │  Three control planes read/govern this:                  │
  │   - Agent CP   (bio · perception)                        │
  │   - Platform CP (infra · health)                         │
  │   - API CP     (cost · routing)                          │
  └──────────────────────────────────────────────────────────┘
         │ projections
         ▼
  Console (SolidJS + Rust WebTransport)
```

The Mermaid version of the same diagram lives in the
[README's Architecture-at-a-Glance section](../README.md#architecture-at-a-glance);
the [TOGAF v22.1 Architecture Guide](architecture/togaf-architecture-guide.html)
is the per-cluster authoritative spec.

**Key claim:** kernel-bound primitives (bwrap, Landlock, cgroups, netns,
eBPF, FUSE) are *implemented + tested* on a provisioned VM but are *not*
exercised in the docker demo because they need host capabilities the
container does not have. The
[README status table](../README.md#status--what-works-in-this-alpha-what-doesnt-yet)
makes that split explicit row-by-row.

---

## 15–30 min — Hands-on

Three exercises that all use the same demo stack. Each maps to a copy-
pasteable file in [`examples/`](../examples/).

### Exercise A — Sandbox-policy inspection (5 min)

Open [`examples/minimal-sandbox-policy.toml`](../examples/minimal-sandbox-policy.toml).
Walk through the three blocks (`[bwrap]`, `[landlock]`, and `[cgroups]`)
and identify which kernel primitive enforces what. The bwrap
`share_net = false` setting creates the loopback-only network cage. The TOML
mirrors the production defaults at `crates/sentinel-sandbox/src/`.

### Exercise B — Event-stream replay (5 min)

Follow [`examples/audit-replay-pattern.md`](../examples/audit-replay-pattern.md):
take a snapshot, stop the daemon, restart, and `diff` the agent state
before and after. Expected output is `REPLAY IDENTICAL`. The hash chain
in `sentinel-nightrun` is the formal witness behind the diff.

### Exercise C — Control-plane isolation test (5 min)

Follow [`examples/control-plane-pattern.md`](../examples/control-plane-pattern.md):
read the three control-plane decision ledgers, then run the cross-
pollination check that confirms an Agent-CP decision never targets a
platform component name. Boundary breach = bug.

---

## 30–40 min — Walkthrough of the 9/9 sandbox breakout test report

On a privileged host:

```bash
cargo test -p sentinel-sandbox --test breakout
```

Expected: **9/9 scenarios pass**. The
[security test report](security-test-report.md) records every scenario
and the layer that defended it. Each row matters because it tells a
customer which kernel primitive is the load-bearing one for that class
of attack:

| ID | Scenario | Defending layer |
|----|----------|-----------------|
| FS-001 | Write `/etc/passwd` | bwrap mount-ns + Landlock |
| FS-002 | Read another agent's home | bwrap mount-ns |
| FS-003 | Write + execute in `/tmp` | bwrap (no `/usr` bound in prod) — Landlock has a documented `all_access` gap, mitigated by defense-in-depth |
| FS-004 | Symlink to `/etc/shadow` | bwrap + Landlock |
| RES-001 | Memory bomb | cgroups v2 `memory.max` |
| RES-002 | Fork bomb | cgroups v2 `pids.max=50` |
| RES-003 | CPU burn | cgroups v2 `cpu.max` |
| NS-001 | PID-list visibility | bwrap PID-ns |
| NS-002 | Hostname leakage | bwrap UTS-ns |

The point of this section is to make the system honest about what it
*does not* enforce (FS-003 Landlock execute-gap, no `seccomp-bpf`,
docker-demo not exercising kernel primitives) and where each gap is
documented.

---

## 40–45 min — Q&A + production-vs-demo limitations

Cover the limits explicitly:

- **The docker demo is a behavioral subset.** It exercises ECS world,
  bio-engine, gateway pipeline, dashboard — not the kernel-bound
  sandbox layer. The
  [README's "What the docker demo shows — and what it does not"
  table](../README.md#what-the-docker-demo-shows--and-what-it-does-not)
  lists this row-by-row.
- **Production runs on a provisioned VM**, not in a container. The full
  sandbox stack assumes user namespaces, cgroups v2, `CAP_BPF`, and a
  writeable bpf-fs / `/dev/fuse` as applicable. The full-cage network
  model needs no bridge, veth pair, nftables rules, or `CAP_NET_ADMIN`.
- **Synthesis intercept rate ~70%** is measured on the demo workload;
  your mileage will vary by prompt distribution.
- **No multi-tenant company configs yet** ([#266](https://github.com/silentspike/project-sentinel/issues/266));
  the workload is single-tenant for v0.1.0-alpha.
- **`seccomp-bpf` is on the roadmap**, not in v0.1.0-alpha. Documented
  in the [deviation register](togaf-deviations-v22.md) as DEV-005.

---

## See also (cross-repo context)

- **Operator-supervision pair:** [silentspike/noaide](https://github.com/silentspike/noaide)
  observes individual AI-coding-agent sessions; Sentinel runs the
  governance layer underneath when those sessions become workloads.
- **Private-code context:** [silentspike/mainrag](https://github.com/silentspike/mainrag)
  is the retrieval layer for grounding agents in private code without
  pushing it into a hosted LLM.

For the workshop's longer companion docs see
[docs/research-context.md](research-context.md) (the synthetic workload
explained), [docs/known-limitations.md](known-limitations.md) (full
caveat list), and [docs/governance.md](governance.md) (governance
mechanisms ↔ code path mapping).
