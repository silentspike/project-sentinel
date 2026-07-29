# OSS single-node resource-control study

- Status: ORC decision candidate
- Issue: [#720](https://github.com/silentspike/project-sentinel/issues/720)
- Parent: [#659](https://github.com/silentspike/project-sentinel/issues/659)
- Source-review baseline: `1ea2c6b9d9290150737d4bee0b31b4af30cf3c25`
- Delivery base and current integrated Sentinel baseline:
  `cbd7c25d2bb57df99462d4a180aae5ab00eaf651`
- Research cut: 2026-07-29
- Runtime evidence: none; this is a source and test audit, not a deployment or
  performance benchmark

## 1. Executive decision

Sentinel should keep Linux cgroup v2 as its per-agent enforcement plane and use
systemd's existing resource-control surface for service-level budgets. It should not
adopt a cluster scheduler or replace the Linux scheduler for M0. The current code
already has useful cgroup, PSI, adaptive-tick, activity-profile, and restart
building blocks, but they are not one recoverable resource-control contract.

The proposed decisions are:

1. **Configure existing dependency:** define one coherent systemd slice policy for
   Sentinel services using weights, soft memory protection/pressure limits, hard
   last-resort ceilings, PID limits, OOM grouping, and restart budgets.
2. **Reimplement minimal:** introduce a versioned `ResourceContractV1` with desired
   and verified-applied generations, mandatory-controller capability checks,
   ordered writes, readback, compensating rollback, reconciliation, and durable
   outcomes. Production agent limits must include `pids.max`.
3. **Reimplement minimal:** replace independent PSI reactions with a bounded node
   `PressureGovernorV1`. It uses sustained pressure, hysteresis, hold-down and
   recovery windows, admission classes, and a fixed degradation order that
   preserves simulation correctness and control-plane liveness.
4. **Port algorithm/contract:** take Kubernetes' checkpointed allocation,
   hint-before-allocation, threshold grace, reclaim-before-evict, remeasurement, and
   victim-ranking contracts. Do not add Kubernetes.
5. **Configure existing dependency:** use systemd-oomd only after the protected
   service set, eligible victim set, sustained pressure window, dry-run evidence,
   and restart budget have been approved. Do not add Meta oomd.
6. **Keep Sentinel:** preserve deterministic ECS schedule order. Resource pressure
   may change admission and wall-clock pacing, but not reorder the simulation or
   silently skip required shift work.
7. **Reject for M0:** do not adopt sched_ext/scx, Slurm, Flux, or Nomad. sched_ext is
   an optional, capability-gated `POST_M0` experiment under #655; cluster schedulers
   remain owned by #690 and #691.
8. **Keep Sentinel with correction:** retain per-agent PSI publication and
   platform-controlplane health actions, but make stale/missing telemetry explicit
   and route all resource mutations through the new resource contract.

One current source defect is `BLOCKS_M0`: when memory PSI is above threshold during
a shift transition, Sentinel removes the old shift, records the new shift as
current, and continues without spawning the new shift. The transition predicate
then cannot retry until a later shift change. The remaining accepted gaps are
`M0_HARDENING`; topology and scheduler replacement are `POST_M0`.

This document is a decision package. It does not authorize dependency additions,
issue materialization, runtime mutation, or the proposed numeric policy values.

## 2. Method and reproducibility

### 2.1 Evidence rules

- Sentinel claims are tied to the source-review baseline and current tests, not to
  closed issue labels.
- Upstream claims are tied to immutable commits and load-bearing source, tests,
  failure paths, security material, licenses, and operator surfaces.
- Upstream documentation is used for public kernel or operator contracts, never as
  the only evidence for an implementation claim.
- Upstream benchmark harnesses identify measurement methods only. No upstream
  result is Sentinel performance evidence.
- A mechanism is preferred over a brand. Integration cost includes privilege,
  language/runtime boundary, persistence, restart recovery, and operational
  ownership.
- No upstream source was copied, vendored, built, or executed.

### 2.2 Reproduction method

The read-only upstream review used:

```text
git clone --filter=blob:none --no-checkout <repository>
git -C <checkout> fetch --depth=1 origin <commit>
git -C <checkout> checkout --detach <commit>
git -C <checkout> rev-parse HEAD
rg -n '<mechanism, failure, or test term>' <source-and-test-roots>
nl -ba <load-bearing-file> | sed -n '<range>p'
```

The Sentinel baseline used:

```text
git rev-parse HEAD
git status --short --branch
rg -n '<cgroup, PSI, tick, profile, restart, or topology term>' \
  crates services deploy config docs
gh issue view <owner> --json number,title,state,labels,url,body
```

The Linux repository was reviewed through immutable GitHub source objects when a
full filtered checkout proved unnecessarily expensive. No claim depends on a
mutable branch view.

## 3. Current Sentinel baseline

### 3.1 Resource-control data path

| Plane | Current implementation and source | What it proves | Claim boundary or defect |
|---|---|---|---|
| Adaptive node pressure | [`AdaptiveTickRate`](../../../services/sentinel-daemon/src/adaptive_tick.rs#L97-L219) polls global `/proc/pressure/{cpu,memory,io}` on a tick interval. CPU pressure doubles the requested tick period up to a configured ceiling; memory pressure returns a spawn-block flag; IO pressure returns 500 ms. | PSI parsing and three local policy functions exist. | Read/parse failures retain old values without freshness or error state. Only `avg10` is used; there is no sustained-window state machine. |
| Tick pacing | The orchestrator computes effective pacing and sleeps after the ECS and background cycle ([source](../../../services/sentinel-daemon/src/orchestrator.rs#L7442-L7457)). | Wall-clock pacing can reduce node CPU demand without changing ECS schedule order. | Sleep is not an admission controller. The code does not prove a bounded latency budget for control, durability, or required work under pressure. |
| Memory-pressure shift admission | Shift transitions are detected every 60 ticks and old agents are removed before the pressure check ([source](../../../services/sentinel-daemon/src/orchestrator.rs#L6135-L6174)). On high memory PSI, the code records `current_shift = new_shift` and continues before spawning replacements ([source](../../../services/sentinel-daemon/src/orchestrator.rs#L6521-L6533)). | The intent to delay spawning under pressure is explicit. | The only retry predicate is `new_shift != current_shift`; recording the shift consumes it. Required agents can remain absent until another shift. This is a source-proved missed-work defect. |
| IO batching | [`batching_window_ms`](../../../services/sentinel-daemon/src/adaptive_tick.rs#L208-L219) computes a 500 ms window above the IO threshold. | The policy value is unit-tested. | Repository-wide call-site review finds only tests; production never consumes the value. |
| Agent profiles | [`ResourceManager::cycle`](../../../services/sentinel-daemon/src/resource_manager.rs#L37-L125) runs periodically, detects activity, applies hysteresis, resizes the cgroup, and emits an event. | Idle/Normal profile transitions and a best-effort audit path exist. | [`detect_profile`](../../../services/sentinel-daemon/src/resource_manager.rs#L128-L141) implements only Idle/Normal. Heavy/Suspended are future comments. Profile state, pending transitions, and heavy count are process-local. |
| Forced profile | [`force_profile_and_apply`](../../../services/sentinel-daemon/src/resource_manager.rs#L175-L205) resizes, mutates in-memory state, then appends the event. | Operators can request an immediate resource profile. | A failed event leaves applied kernel state ahead of durable state. Restart loses the applied generation. There is no CAS, readback, or compensation. |
| Per-agent limits | [`CgroupLimits`](../../../crates/sentinel-sandbox/src/cgroups.rs#L69-L89) contains CPU quota, memory hard maximum, and IO IOPS/bytes. Profiles select static values ([source](../../../crates/sentinel-sandbox/src/cgroups.rs#L14-L55)). | Agent cgroups have real kernel enforcement primitives. | There are no weights, `memory.high/low/min`, `memory.oom.group`, `pids.max`, uclamp, or cpuset fields. A profile is not a complete resource contract. |
| Controller delegation | [`delegate_controllers`](../../../crates/sentinel-sandbox/src/cgroups.rs#L161-L181) attempts CPU, memory, PID, and IO controllers independently. | Partial host capability is tolerated and logged. | It returns success when any one controller succeeds. A caller cannot distinguish a complete mandatory set from a single controller. |
| Cgroup creation | [`create_cgroup`](../../../crates/sentinel-sandbox/src/cgroups.rs#L220-L272) creates a directory, writes CPU and hard memory limits, then applies IO best-effort. | CPU and memory failures stop creation; IO availability is returned. | Earlier successful writes/directories are not rolled back. PID enforcement is absent. IO failure still yields a usable agent with a weaker contract. |
| Hot resize | [`resize_cgroup`](../../../crates/sentinel-sandbox/src/cgroups.rs#L274-L311) writes CPU, derives a memory floor from current usage plus 16 MiB, then writes IO best-effort. | A direct hot-resize path exists. | Multi-file kernel updates are non-atomic. IO errors are discarded. The actual memory limit can differ from the requested profile and is not returned or read back. |
| Process-tree cleanup | [`kill_cgroup_processes`](../../../crates/sentinel-sandbox/src/cgroups.rs#L325-L371) prefers `cgroup.kill`, falls back to PID SIGKILL, and polls for an empty cgroup. | Bounded cleanup and a typed failure exist. | Cleanup is not tied to a resource-generation transition or restart budget. |
| Per-agent PSI | [`PsiPublisher`](../../../crates/sentinel-sandbox/src/psi_publisher.rs#L25-L74) publishes CPU and memory PSI every five seconds; the eBPF plane can read CPU, memory, and IO PSI ([source](../../../crates/sentinel-ebpf/src/psi.rs#L29-L71)). | Per-agent pressure telemetry exists independently of global adaptive tick. | Missing cgroups/reads are skipped, not represented as stale or unknown. The publisher omits IO. No resource decision consumes a freshness-bound per-agent snapshot. |
| Control-plane metrics | [`collect_metrics`](../../../services/sentinel-daemon/src/platform_controlplane/metrics.rs#L40-L146) gathers best-effort agent/service/projection/storage metrics. Memory pressure is current/max ratio. | The rule engine receives a bounded operational snapshot. | Collection errors collapse some values to zero. Memory ratio is not PSI or `memory.events`; unknown can look healthy. |
| Control-plane actions | [`evaluate_rules`](../../../services/sentinel-daemon/src/platform_controlplane/rules.rs#L42-L225) proposes agent/service restart, projection restart, profile Idle, and SIGSTOP actions with grace/cooldown checks. | Operational reactions and cooldowns exist. | Resource actions bypass one durable desired/applied contract. There is no global restart token budget or proof that unrelated rows continue after one action fails. |
| Concurrency/thread pools | LLM calls use a configurable semaphore ([source](../../../services/sentinel-daemon/src/llm_bridge.rs#L632-L653)); evolution jobs use a separate semaphore ([source](../../../services/sentinel-daemon/src/evolution_task.rs#L52-L85)); Zenoh bounds global and per-agent in-flight work ([source](../../../crates/sentinel-zenoh/src/inflight.rs#L60-L113)). | Several queues already have local backpressure primitives. | There is no node-wide capacity model, reserved control capacity, or governor generation joining these independent limits. Tokio worker-pool sizing is not a resource-policy substitute. |
| Runtime health | Runtime reconciliation validates runtime PIDs and cgroup membership ([source](../../../services/sentinel-daemon/src/runtime_health.rs)). | Stale process and cgroup membership can be detected. | It does not validate controller delegation, intended limit values, resource generation, pressure-governor state, or systemd slice policy. |
| Service budgets | Unit files define individual `MemoryMax`, selected CPU quotas/affinity, `TasksMax`, OOM score, and restart settings ([daemon](../../../deploy/systemd/sentinel-daemon.service), [gateway](../../../deploy/systemd/sentinel-gateway.service), [dashboard](../../../deploy/systemd/sentinel-dashboard-backend.service)). | Important services have some local limits and restart throttling. | No shared slice establishes aggregate headroom or relative weights. Coverage differs by service; hard maxima can sum above host capacity. |
| Boot setup | [`init-cgroups.sh`](../../../deploy/scripts/init-cgroups.sh) enables controllers and creates roots; [`provision-runtime-base.sh`](../../../deploy/provision-runtime-base.sh) probes host features. | Deployment has an explicit cgroup bootstrap surface. | Controller writes are best-effort. Boot readiness is not bound to the exact policy/catalog digest consumed by the daemon. |

The configured defaults enable both adaptive tick and resource management
([`config/daemon.toml`](../../../config/daemon.toml#L18-L25),
[`config/daemon.toml`](../../../config/daemon.toml#L69-L75)). Default-on behavior
therefore makes fail-open capability drift a product concern rather than an
experimental-only concern.

### 3.2 Test and failure-path baseline

Targeted unit tests prove threshold arithmetic, profile hysteresis, formatting, and
selected rule predicates:

- Adaptive pressure tests cover above/below/exact thresholds, spawn blocking, IO
  policy values, and the tick-period ceiling
  ([source](../../../services/sentinel-daemon/src/adaptive_tick.rs#L250-L401)).
- Resource-manager tests cover only in-memory profile detection, hysteresis, direct
  force, and counters
  ([source](../../../services/sentinel-daemon/src/resource_manager.rs#L275-L385)).
- Platform-controlplane tests cover stall grace, cooldown, memory pressure, write
  anomaly, circuit behavior, and service health
  ([source](../../../services/sentinel-daemon/src/platform_controlplane/rules.rs#L281-L746)).
- Ignored VM tests exercise memory, fork, and CPU attacks
  ([source](../../../crates/sentinel-sandbox/tests/breakout.rs#L428-L519)).

Those tests do not prove the production contract:

1. The fork test writes `pids.max` itself because `CgroupLimits` cannot express it
   ([source](../../../crates/sentinel-sandbox/tests/breakout.rs#L452-L468)).
2. No integration test drives provider-independent node pressure through shift
   removal, delayed admission, pressure recovery, and exactly-once replacement
   spawn.
3. No crash/restart test reconstructs desired and actual kernel limits.
4. No failpoint test covers partial controller writes, compensation, or durable
   outcomes.
5. No test proves global service limits preserve reserved capacity for the daemon,
   event/projection path, and operator control under hostile agent load.
6. No test proves a bounded restart/kill budget or recovery without oscillation.

### 3.3 Incident and existing-owner map

Live issue state was read on 2026-07-29.

| Owner | Live state | Existing responsibility | #720 treatment |
|---|---|---|---|
| [#74](https://github.com/silentspike/project-sentinel/issues/74) | Closed, completed | PSI metrics to biological stress pipeline | Retain the telemetry/biology mapping; it is not the node admission authority. |
| [#147](https://github.com/silentspike/project-sentinel/issues/147) | Closed, verified | Connect PSI to adaptive tick/spawn/IO policy | Record that IO batching has no production consumer and shift pressure can consume a transition without retry. Closed status is historical evidence, not current correctness proof. |
| [#196](https://github.com/silentspike/project-sentinel/issues/196) | Closed, verified with contradictory labels | PSI-to-biology integration | Keep as domain-effect owner; do not make biological state authoritative for kernel safety. |
| [#227](https://github.com/silentspike/project-sentinel/issues/227) | Closed, verified | Control-plane cycle budget | Preserve its latency objective; pressure evaluation and reconciliation need an explicit per-cycle budget. |
| [#265](https://github.com/silentspike/project-sentinel/issues/265) | Closed, verified with contradictory size/quality/priority labels | Smart cgroups, profiles, IO, and resource UI history | Treat as delivered-history owner. Heavy/Suspended and transactional hot resize are not present in current source. |
| [#624](https://github.com/silentspike/project-sentinel/issues/624) | Open, backlog | Panic strategy and supervision semantics | Owns panic containment; the resource epic owns restart tokens and pressure-victim policy. |
| [#650](https://github.com/silentspike/project-sentinel/issues/650) | Open, blocked | Single-node product acceptance | Downstream acceptance owner for approved resource thresholds and final live evidence. It is not an implementation prerequisite for token-free contracts. |
| [#690](https://github.com/silentspike/project-sentinel/issues/690) | Open, ready | Slurm mechanism study | Keeps deep batch/reservation/accounting research outside this M0 single-node decision. |
| [#691](https://github.com/silentspike/project-sentinel/issues/691) | Open, ready | Flux mechanism study | Keeps hierarchical/distributed scheduling research outside this M0 single-node decision. |
| [#705](https://github.com/silentspike/project-sentinel/issues/705) | Open, blocked | Dependency necessity/ownership | Mandatory gate before any scx/oomd/scheduler dependency, replacement, or wrapper. No M0 dependency is proposed here. |
| [#656](https://github.com/silentspike/project-sentinel/issues/656) | Open, backlog | Dependency upgrades | Owns an upgrade contract only after #705 accepts a dependency. |
| [#655](https://github.com/silentspike/project-sentinel/issues/655) | Open, backlog | CPU/NUMA/cache/memory topology ADR | Owns the `POST_M0` topology and sched_ext experiment decision. |
| [#502](https://github.com/silentspike/project-sentinel/issues/502) | Open, ready | Read-only scheduler telemetry | Reuse for topology/load observation, not mutation or admission. |

### 3.4 TOGAF target versus current source

The TOGAF guide describes:

- adaptive scheduling and shift behavior
  ([target](../../architecture/togaf-architecture-guide.html#principles));
- per-agent `cpu.max`, `memory.max`, `io.max`, OOM score, and PSI
  ([target](../../architecture/togaf-architecture-guide.html#infra));
- tick, IO, and supported-agent performance targets
  ([target](../../architecture/togaf-architecture-guide.html#sdd));
- a portable cgroup/NUMA baseline with resctrl and sched_ext as optional,
  capability-gated, reversible extensions
  ([target](../../architecture/togaf-architecture-guide.html#target));
- soft affinity, hysteresis, cooldown, and budgeted graph/load scheduling
  ([target](../../architecture/togaf-architecture-guide.html#target));
- no experimental-kernel single point of failure
  ([target](../../architecture/togaf-architecture-guide.html#target)).

These are target statements. Current source implements only part of them. The
accepted delta must add an explicit node-pressure state machine, resource
generation/reconciliation contract, service budget hierarchy, admission semantics,
and capability/readiness model. It must also clarify that topology optimization
cannot gate M0.

## 4. Landscape and shortlist

### 4.1 Rubric

Each candidate was scored from 0 (poor) to 3 (strong) against ten factors:

1. direct mechanism fit;
2. active maintenance and operational maturity;
3. license compatibility and provenance clarity;
4. a private security-reporting path;
5. compatible privilege and trust boundary;
6. deterministic single-node behavior;
7. cgroup/pressure/topology model depth;
8. source tests for adverse and restart paths;
9. integration and operational cost;
10. usefulness without adopting the product.

`Deep` requires a direct load-bearing fit and at least 20/30. A lower score can
still be landscape evidence. Scores compare suitability for Sentinel's M0 problem,
not overall project quality.

### 4.2 Candidate inventory

| Candidate and pin | Score | License/security posture | Shortlist result and reason |
|---|---:|---|---|
| [Linux kernel `fc02acf6`](https://github.com/torvalds/linux/commit/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8) | 28 | Kernel source carries per-file SPDX rules and GPL-2.0-only default in [`COPYING`](https://github.com/torvalds/linux/blob/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/COPYING); private reporting is documented in [`security-bugs.rst`](https://github.com/torvalds/linux/blob/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/Documentation/process/security-bugs.rst). | **Deep.** It is the actual enforcement/failure-semantic substrate. |
| [systemd `08ca33fd`](https://github.com/systemd/systemd/commit/08ca33fddebdb029ef84b97bb645d9325b783838) | 27 | Mixed GPL/LGPL and permissive files are enumerated in [`LICENSES`](https://github.com/systemd/systemd/tree/08ca33fddebdb029ef84b97bb645d9325b783838/LICENSES); private reporting is in [`SECURITY.md`](https://github.com/systemd/systemd/blob/08ca33fddebdb029ef84b97bb645d9325b783838/docs/SECURITY.md). | **Deep.** Already in the deployment boundary; rich service/cgroup/OOM controls. |
| [sched_ext/scx `926d12c2`](https://github.com/sched-ext/scx/commit/926d12c2adf6ea593704ef4359a908a2fd8b3f4c) | 21 | GPL-2.0 [`LICENSE`](https://github.com/sched-ext/scx/blob/926d12c2adf6ea593704ef4359a908a2fd8b3f4c/LICENSE); no repository security policy was found at the pin. | **Deep, rejection-focused.** Strong scheduler mechanisms and kernel failback, but high privilege/kernel/BPF operational cost. |
| [Meta oomd `c286e646`](https://github.com/facebookincubator/oomd/commit/c286e646d69bcb826ce895e373482b66cb1d7ced) | 21 | GPL-2.0 [`LICENSE`](https://github.com/facebookincubator/oomd/blob/c286e646d69bcb826ce895e373482b66cb1d7ced/LICENSE); no repository security policy was found at the pin. | **Deep, rejection-focused.** Useful sustained-pressure and victim-selection contracts; systemd-oomd is already the lower-cost deployment-native choice. |
| [Kubernetes `7c2b6c32`](https://github.com/kubernetes/kubernetes/commit/7c2b6c32644a2cc24029ec77576940b63ecca7e7) | 24 | Apache-2.0 [`LICENSE`](https://github.com/kubernetes/kubernetes/blob/7c2b6c32644a2cc24029ec77576940b63ecca7e7/LICENSE); private reporting is in [`.github/SECURITY.md`](https://github.com/kubernetes/kubernetes/blob/7c2b6c32644a2cc24029ec77576940b63ecca7e7/.github/SECURITY.md). | **Deep.** Strong checkpoint/admission/reclaim contracts; product adoption is out of scope. |
| [Slurm `a21d8cde`](https://github.com/SchedMD/slurm/commit/a21d8cdef240a59c7d304f02d92c086601c18a03) | 17 | GPL with OpenSSL exception in [`COPYING`](https://github.com/SchedMD/slurm/blob/a21d8cdef240a59c7d304f02d92c086601c18a03/COPYING); private reporting in [`SECURITY.md`](https://github.com/SchedMD/slurm/blob/a21d8cdef240a59c7d304f02d92c086601c18a03/SECURITY.md). | Landscape only. Batch queues, reservations, cgroup plugins, affinity, accounting, and multi-node control are valuable but disproportionately broad; #690 owns the deep study. |
| [Flux core `b680eba2`](https://github.com/flux-framework/flux-core/commit/b680eba2645aa7a936013c329c33df731628aa9f) plus [Flux sched `c05ff4c1`](https://github.com/flux-framework/flux-sched/commit/c05ff4c13a45fc07f145fb1c727c9058782ff00a) | 16 | LGPL-3.0 [`LICENSE`](https://github.com/flux-framework/flux-core/blob/b680eba2645aa7a936013c329c33df731628aa9f/LICENSE); no project security file was found at either pin. | Landscape only. Hierarchical resources/planners are relevant, but the broker and scheduler stack is a distributed batch architecture; #691 owns the deep study. |
| [Nomad `b8e77321`](https://github.com/hashicorp/nomad/commit/b8e77321b4cc26718306cca0f407c982c62ea9aa) | 14 | Business Source License terms in [`LICENSE`](https://github.com/hashicorp/nomad/blob/b8e77321b4cc26718306cca0f407c982c62ea9aa/LICENSE); no repository security file was found at the pin. | Reject. It adds another orchestrator/control plane and a less suitable license without replacing Sentinel's simulation-specific authority. |

The inventory exceeds the required five candidates and covers kernel primitives,
service management, pluggable scheduling, OOM response, node allocation, and
cluster/batch schedulers. No product was shortlisted because of popularity or an
upstream performance claim.

## 5. Deep source reviews

### 5.1 Linux cgroup v2, PSI, scheduler, and sched_ext

**Mechanisms.** The cgroup v2 interface provides relative CPU weight, hard CPU
bandwidth, CPU pressure, memory protection, throttle/reclaim, hard maximum, grouped
OOM, IO limits, and PID limits
([CPU](https://github.com/torvalds/linux/blob/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/Documentation/admin-guide/cgroup-v2.rst#L1163-L1217),
[memory](https://github.com/torvalds/linux/blob/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/Documentation/admin-guide/cgroup-v2.rst#L1327-L1458),
[IO](https://github.com/torvalds/linux/blob/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/Documentation/admin-guide/cgroup-v2.rst#L2132-L2149),
[PID](https://github.com/torvalds/linux/blob/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/Documentation/admin-guide/cgroup-v2.rst#L2401-L2418)).
Kernel guidance treats `memory.high` as the main control and `memory.max` as the
last-resort boundary
([source](https://github.com/torvalds/linux/blob/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/Documentation/admin-guide/cgroup-v2.rst#L3447-L3485)).
PSI exposes `some`, `full`, accumulated stall time, averages, and pollable threshold
triggers
([source](https://github.com/torvalds/linux/blob/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/Documentation/accounting/psi.rst#L38-L99)).

**Scheduler boundary.** Utilization clamping is deliberately inactive until a
consumer enables it, limiting baseline overhead
([source](https://github.com/torvalds/linux/blob/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/kernel/sched/core.c#L1587-L1604)).
It is a performance/power hint, not resource accounting or priority inheritance.
CPU weights, affinity, quota, nice/RT policy, and uclamp solve different problems
and must not be represented as one "priority" number.

**Failure and recovery.** sched_ext restores the default scheduler on a detected
error, runnable-task stall, or explicit SysRq-S and emits debug state
([contract](https://github.com/torvalds/linux/blob/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/Documentation/scheduler/sched-ext.rst#L19-L26)).
Its watchdog exits a scheduler after a runnable stall
([source](https://github.com/torvalds/linux/blob/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/kernel/sched/ext/ext.c#L3463-L3533)),
and bypass mode intentionally keeps tasks moving during error handling
([source](https://github.com/torvalds/linux/blob/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/kernel/sched/ext/ext.c#L5429-L5454)).

**Tests and operations.** Kernel selftests cover clean scheduler exit, RT-stall
behavior, repeated reload, and NUMA behavior
([tests](https://github.com/torvalds/linux/tree/fc02acf6ac0ccde0c805c2daa9148683cdd01ba8/tools/testing/selftests/sched_ext)).
These establish an upstream failback contract, not Sentinel compatibility. The M0
boundary remains cgroup v2 and the default kernel scheduler.

### 5.2 systemd resource control and systemd-oomd

**Mechanisms.** systemd maps service/slice configuration to CPU weight/quota and
allowed CPUs, memory protection/high/max, task limits, IO weight/caps, delegation,
managed OOM, and pressure-watch controls
([resource-control manual](https://github.com/systemd/systemd/blob/08ca33fddebdb029ef84b97bb645d9325b783838/man/systemd.resource-control.xml#L187-L710),
[delegation/OOM/pressure](https://github.com/systemd/systemd/blob/08ca33fddebdb029ef84b97bb645d9325b783838/man/systemd.resource-control.xml#L1421-L1715)).
The manager implementation applies these through one cgroup realization path
([source](https://github.com/systemd/systemd/blob/08ca33fddebdb029ef84b97bb645d9325b783838/src/core/cgroup.c)).

**OOM selection.** systemd-oomd requires sustained pressure duration, ranks
candidates with preference/avoid metadata, and protects its own control boundary
([source](https://github.com/systemd/systemd/blob/08ca33fddebdb029ef84b97bb645d9325b783838/src/oom/oomd-util.c#L91-L276)).
The manager kills at most one selected high-pressure cgroup before delaying the
next action
([source](https://github.com/systemd/systemd/blob/08ca33fddebdb029ef84b97bb645d9325b783838/src/oom/oomd-manager.c#L572-L637)).

**Tests and failures.** Integration suites cover cgroup properties and delegation,
sustained OOM pressure and preferences, and pressure-watch behavior
([cgroup tests](https://github.com/systemd/systemd/blob/08ca33fddebdb029ef84b97bb645d9325b783838/test/units/TEST-19-CGROUP.sh),
[OOM tests](https://github.com/systemd/systemd/blob/08ca33fddebdb029ef84b97bb645d9325b783838/test/units/TEST-55-OOMD.sh),
[pressure tests](https://raw.githubusercontent.com/systemd/systemd/08ca33fddebdb029ef84b97bb645d9325b783838/test/units/TEST-79-PRESSURE.sh)).
Tests explicitly skip unsupported capabilities; Sentinel readiness must not turn
such a skip into a false enforcement claim.

**Boundary.** systemd is already the service lifecycle boundary, so configuration
adds no embedded scheduler or daemon dependency. Sentinel still needs its own
desired/applied generation, per-agent semantics, domain admission, and audit
events.

### 5.3 sched_ext/scx

**Mechanisms.** scx contains minimal examples and schedulers described as more
production-oriented; the repository explicitly distinguishes the two
([inventory](https://github.com/sched-ext/scx/blob/926d12c2adf6ea593704ef4359a908a2fd8b3f4c/scheds/README.md#L13-L39)).
`scx_layered` can group workloads by cgroup and use CPU, NUMA, and last-level-cache
topology with configurable growth policies
([scheduler](https://github.com/sched-ext/scx/tree/926d12c2adf6ea593704ef4359a908a2fd8b3f4c/scheds/rust/scx_layered)).
Its topology-growth implementation exposes sticky, linear, random, topology,
balanced, and spread behavior
([source](https://github.com/sched-ext/scx/blob/926d12c2adf6ea593704ef4359a908a2fd8b3f4c/scheds/rust/scx_layered/src/layer_core_growth.rs#L17-L93)).

**Failure and operations.** Kernel sched_ext provides failback, but safe use still
requires a compatible kernel, BPF permissions, scheduler daemon supervision,
capability probes, an exact fallback policy, and load-specific validation. The scx
integration harness launches privileged schedulers with `stress-ng` and forcefully
cleans them up
([test harness](https://github.com/sched-ext/scx/blob/926d12c2adf6ea593704ef4359a908a2fd8b3f4c/scheds/rust/scx_layered/integration/run_tests.sh#L1-L36)).

**Decision boundary.** The mechanism is attractive for future topology/load
experiments, but it is not required for cgroup enforcement, PSI backpressure, OOM
prevention, or deterministic ECS scheduling. It is rejected for M0 and may be
evaluated only under #655, #705, and #656 with immediate default-scheduler
fallback. A scheduler benchmark without Sentinel services and sidecars is not
evidence.

### 5.4 Meta oomd

**Mechanisms.** Meta oomd compiles detector groups and action chains. The
`pressure_above` detector requires a 10-second PSI average to remain above a
threshold for a configured duration before it continues the action chain
([source](https://github.com/facebookincubator/oomd/blob/c286e646d69bcb826ce895e373482b66cb1d7ced/src/oomd/plugins/PressureAbove.cpp#L91-L116)).
Victim plugins sort by kill preference before a mechanism-specific score and try
the next candidate after a failed kill
([selection contract](https://github.com/facebookincubator/oomd/blob/c286e646d69bcb826ce895e373482b66cb1d7ced/src/oomd/OomdContext.h#L125-L146),
[fallback contract](https://github.com/facebookincubator/oomd/blob/c286e646d69bcb826ce895e373482b66cb1d7ced/src/oomd/plugins/BaseKillPlugin.h#L85-L101)).
Pre-kill hooks are bounded by time and action chains have a post-action delay
([ruleset](https://github.com/facebookincubator/oomd/blob/c286e646d69bcb826ce895e373482b66cb1d7ced/src/oomd/engine/Ruleset.h#L38-L111)).

**Tests and failures.** Parser/compiler tests reject malformed configuration, while
plugin tests cover PSI thresholds, memory/swap state, ranking, dry-run, failed
kills, and action termination
([compiler tests](https://github.com/facebookincubator/oomd/blob/c286e646d69bcb826ce895e373482b66cb1d7ced/src/oomd/config/ConfigCompilerTest.cpp),
[plugin tests](https://github.com/facebookincubator/oomd/blob/c286e646d69bcb826ce895e373482b66cb1d7ced/src/oomd/plugins/CorePluginsTest.cpp)).

**Decision boundary.** The contracts validate sustained pressure, eligible-victim
ranking, retrying another victim, dry-run, and cooldown. Adding a second privileged
C++ daemon and GPL integration surface would duplicate the already deployed
systemd boundary. Sentinel should configure systemd-oomd and port only the
product-specific admission/audit contracts.

### 5.5 Kubernetes CPU, topology, and eviction managers

**Checkpointed allocation.** CPU Manager stores policy name and CPU allocations in
a checksum-protected checkpoint. Startup fails if restore fails or the configured
policy differs; a corrupt current-format checkpoint does not silently fall back to
an older format
([source](https://github.com/kubernetes/kubernetes/blob/7c2b6c32644a2cc24029ec77576940b63ecca7e7/pkg/kubelet/cm/cpumanager/state/state_checkpoint.go#L47-L74),
[restore](https://github.com/kubernetes/kubernetes/blob/7c2b6c32644a2cc24029ec77576940b63ecca7e7/pkg/kubelet/cm/cpumanager/state/state_checkpoint.go#L114-L224)).
Restart tests create allocations and verify the restored state
([tests](https://github.com/kubernetes/kubernetes/blob/7c2b6c32644a2cc24029ec77576940b63ecca7e7/pkg/kubelet/cm/cpumanager/policy_static_restore_test.go#L41-L244)).

**Topology admission.** Providers produce hints, policy merges them, admission
rejects an unsatisfied guaranteed alignment, and allocation occurs only after the
decision
([source](https://github.com/kubernetes/kubernetes/blob/7c2b6c32644a2cc24029ec77576940b63ecca7e7/pkg/kubelet/cm/topologymanager/scope_container.go#L52-L103)).
This ordering is transferable even though Sentinel does not need Pods or kubelet.

**Pressure and eviction.** The eviction manager records first-observed thresholds,
applies grace/transition/min-reclaim windows, requires fresh stats, ranks
thresholds, attempts node-level reclaim, ranks victims, and evicts at most one
candidate in a synchronization cycle
([source](https://github.com/kubernetes/kubernetes/blob/7c2b6c32644a2cc24029ec77576940b63ecca7e7/pkg/kubelet/eviction/eviction_manager.go#L254-L430)).
It remeasures after reclaim before choosing eviction
([source](https://github.com/kubernetes/kubernetes/blob/7c2b6c32644a2cc24029ec77576940b63ecca7e7/pkg/kubelet/eviction/eviction_manager.go#L480-L512)).

**Failure caution.** A critical pod is protected, but the shown eviction helper
returns success after attempting a kill even if the kill function reports an error
([source](https://github.com/kubernetes/kubernetes/blob/7c2b6c32644a2cc24029ec77576940b63ecca7e7/pkg/kubelet/eviction/eviction_manager.go#L633-L660)).
Sentinel must require verified effect readback rather than copying this completion
semantic.

**Decision boundary.** Port the checkpoint/admission/reclaim contracts. Reject
Kubernetes, CRI, Pod APIs, and its control plane as dependencies.

## 6. Mechanism comparison

### 6.1 Functional matrix

| Mechanism | Sentinel today | Linux/systemd | scx | Meta oomd | Kubernetes | Sentinel integration boundary |
|---|---|---|---|---|---|---|
| CPU fairness/ceilings | Per-agent `cpu.max`; selected service quotas/affinity | `cpu.weight`, `cpu.max`, AllowedCPUs, uclamp | Arbitrary BPF scheduling and cgroup grouping | Not a CPU scheduler | Exclusive/shared CPU pools and topology allocation | Add weights and reserved service capacity; keep hard quotas as ceilings, not fairness. |
| Latency sensitivity | Daemon affinity and OOM score; no versioned latency class | CPUWeight, startup weight, allowed CPUs, uclamp | Custom dispatch/preemption policies | N/A | Reserved CPUs and static allocation | Define `control`, `interactive`, `background`, `agent` admission classes; do not promise kernel priority inheritance. |
| Priority inheritance | No cross-subsystem priority-inheritance contract | Kernel PI applies to supported lock primitives; cgroup weights/quotas do not inherit a blocked caller's priority | A scheduler may change dispatch, not application lock ownership | N/A | Pod priority/admission is not mutex PI | Reject a synthetic global PI layer. Use bounded critical sections, protected control capacity, lock-specific PI only when a measured lock path requires it, and never let caller metadata raise authority. |
| Affinity/NUMA/cache | Static daemon affinity; no agent topology policy | cpuset/AllowedCPUs/NUMAMask; NUMA and LLC are discoverable | Layered NUMA/LLC placement | N/A | Hint merge before allocation | Keep soft/capability-gated topology under #655; no M0 scheduler replacement. |
| Work conservation/thread pools | Independent LLM, evolution, and bus semaphores; no node-wide pool budget | CPU weights remain work-conserving while quota can idle capacity; systemd scopes services | Custom dispatch can be work-conserving but policy-specific | N/A | Shared/exclusive pools and admission | Reserve control capacity, coordinate queue concurrency through admission, retain idle-capacity use, and avoid static per-agent threads. |
| CPU pressure | Global avg10 changes tick sleep; per-agent CPU PSI published | PSI some/full totals and triggers; pressure watch | Scheduler-specific stats | PSI detectors with duration | Threshold/grace/recheck loops | Use freshness-bound system and cgroup pressure with sustained transitions. |
| Memory prevention | Hard `memory.max`; spawn flag; ratio-based profile Idle | `memory.low/high/max`, `oom.group`, managed OOM | Not primary mechanism | Sustained PSI and victim ranking | Requests/limits, thresholds, reclaim, eviction | Configure high as throttle/reclaim, max as last resort, protected services, explicit eligible victims. |
| IO control | `io.max` best-effort; computed batching unused | IOWeight and hard caps | Scheduler does not enforce IO | IO pressure can drive rules | Filesystem pressure and reclaim | Require controller/readback when policy says enforced; implement real queue batching/admission rather than an unused value. |
| PID containment | Test manually writes `pids.max` | TasksMax/`pids.max` | Not primary mechanism | Kill target traversal | Pod/container PID policy elsewhere | Add pids to production `ResourceContractV1`; fail readiness if mandatory but unavailable. |
| Delegation | Succeeds if any requested controller succeeds | systemd delegation and kernel subtree rules | Needs BPF/kernel privileges | Needs cgroup read/kill privileges | kubelet owns node cgroups | A signed/config-bound capability snapshot enumerates each mandatory/optional controller and permission. |
| Dynamic resize | Direct sequential writes, no readback/rollback | Unit property realization and kernel files | Dynamic scheduler config | Ruleset reload/actions | Allocation state/checkpoint and reconcile | Desired generation -> validate -> apply -> readback -> durable outcome; compensate or remain degraded. |
| Backpressure/admission | Memory blocks one shift path; tick slowdown | Pressure watches, service start/rate limits | Scheduler can defer dispatch | Detector/action chains | Pod admission and eviction admission | One durable governor controls shift, LLM, workbench, background, and operator-class admission independently. |
| Reclaim/eviction | Profile Idle, SIGSTOP, restart actions | memory.high reclaim; systemd-oomd victim policy | Not primary mechanism | Ranked kill with next-victim fallback | Reclaim first, remeasure, then rank one victim | Apply reversible degradation before destructive action; verify every action and bound it with tokens/cooldown. |
| Restart budgets | Per-unit StartLimit for most services; rule cooldown | StartLimit and restart policy | Scheduler daemon failback | Post-action delay | Per-cycle bounded eviction | Global and per-class token buckets survive restart; control/durability services cannot be pressure victims. |
| Persistence/recovery | Profile/pending pressure state is in memory | systemd desired config persists; kernel state does not survive boot | Scheduler state/process restarts | Config persists, detector timers restart | CPU assignment checkpoint; manager reconstructs | Persist desired generation and non-terminal operations; reconstruct applied state from kernel/systemd readback. |
| Observability | PSI metrics, profile events, tick duration | PSI/events/current and systemd properties | Scheduler-specific stats/debug dump | Structured kill/context logs | Metrics/events/conditions | Export freshness, desired/applied generation, action outcome, admission state, budget tokens, and unknown capability. |
| Rollback | No resource-set transaction | Unit property rollback is operator-managed | Exit returns default scheduler | Disable daemon/rules | Restore checkpoint/restart manager | Disable governor -> stop new mutations -> reconcile last-known-good generation -> verify; scheduler stays default for M0. |

### 6.2 Non-functional and dependency-impact matrix

| Candidate | Correctness/failure semantics | Deterministic 1:n fit | Security boundary | Maintenance and operations | Dependency impact |
|---|---|---|---|---|---|
| Linux cgroup v2/PSI | Kernel-enforced per-cgroup limits; individual pseudo-file writes are immediate, not a multi-file transaction. | Strong for one node and many agent cgroups; policy remains Sentinel-owned. | Root/delegated cgroup writes; path and controller ownership must be fixed, never caller supplied. | Kernel ABI is stable but host capabilities vary; requires boot probes and readback. | Existing operating-system dependency; no new library. |
| systemd | Service/slice hierarchy, restart throttles, OOM and pressure policy; unsupported features must be explicit. | Strong service-level fit; per-agent domain semantics still belong to Sentinel. | PID 1/unit privileges and controlled drop-ins; no untrusted unit/property input. | Already operated; config drift and daemon-reload/boot validation need ownership. | Existing deployment dependency/configuration only. |
| scx | Kernel detects some stalls/errors and falls back; policy bugs can still affect all runnable tasks until detection. | Scheduler decisions can be nondeterministic relative to workload timing; ECS order remains separate. | BPF, compatible kernel, privileged loader, scheduler daemon. | Rapid kernel/userspace evolution and new runbook/CI matrix. | New dependency and high operational surface through #705/#656; rejected for M0. |
| Meta oomd | Sustained detectors, ranked candidate retry, bounded hooks/delay; destructive by design. | Node-level 1:n fit, but policy overlaps systemd-oomd. | Privileged cgroup inspection/kill; config and victim labels are authority. | Separate C++ daemon/config/upgrade/security owner. | New GPL daemon dependency; rejected. |
| Kubernetes managers | Checkpoint/policy mismatch fails closed; staged admission and bounded reclaim loops. | Contracts fit; Kubernetes object/control-plane model does not. | Kubelet/node root, APIs, checkpoints, container runtime. | Very high product and operational scope if adopted. | No dependency; port small contracts only. |

### 6.3 Performance hypotheses, not evidence

The study permits only hypotheses for future implementation issues:

- weights plus reserved headroom should preserve control-plane latency better than
  hard quotas alone during contention;
- `memory.high` should produce earlier reclaim/backpressure than relying on
  `memory.max` OOM;
- PSI threshold notifications should reduce blind polling while retaining a
  periodic reconciliation fallback;
- topology-aware placement may improve cache locality only for measured stable
  workloads and can regress work conservation;
- sched_ext may improve tail latency for selected workload mixes, but any result is
  invalid unless measured with Sentinel services, agents, sidecars, correctness
  gates, and default-scheduler control runs.

No numeric benefit or overhead is accepted by this study.

## 7. Decisions

Each row has exactly one decision from the issue vocabulary.
No row selects `Adopt dependency` or `Patch upstream`: the accepted M0 path uses
existing operating-system contracts, Sentinel-owned minimal state machines, or
mechanism-only ports. The remaining available vocabulary is
`Configure existing dependency`, `Wrap`, `Integrate`, `Port algorithm/contract`,
`Reimplement minimal`, `Keep Sentinel`, and `Reject`.

| ID | Mechanism | Decision | Rationale and rejected alternatives |
|---|---|---|---|
| D1 | Per-agent CPU/memory/IO/PID enforcement | **Reimplement minimal** | Extend the existing cgroup module into `ResourceContractV1` with generation, capability, readback, rollback, and PID enforcement. Reject a container/Kubernetes runtime and reject direct unverified writes. |
| D2 | Service-level CPU/memory/IO/PID hierarchy | **Configure existing dependency** | Use systemd slices/weights/high/max/tasks/OOM/restart controls already in deployment. Reject a second supervisor and reject independent per-unit maxima without aggregate policy. |
| D3 | Node pressure and degradation | **Reimplement minimal** | Sentinel must preserve its tick/work semantics and authority classes. Reject independent threshold callbacks and reject an external product as the domain admission authority. |
| D4 | Allocation/admission/checkpoint ordering | **Port algorithm/contract** | Port Kubernetes' hint/validate-before-allocate and desired-state restore patterns without Kubernetes types or dependency. |
| D5 | Reclaim and victim sequence | **Port algorithm/contract** | Port sustained window -> reversible reclaim -> remeasure -> rank one eligible victim -> verify -> cooldown. Reject immediate kill and unverified "attempt means success". |
| D6 | OOM daemon | **Configure existing dependency** | systemd-oomd is already deployment-native. Reject Meta oomd due duplicate privileged daemon/maintenance surface. |
| D7 | ECS scheduling order | **Keep Sentinel** | Preserve deterministic ECS order; change wall-clock pacing and admission only. Reject host scheduler decisions as simulation-order authority. |
| D8 | IO load response | **Reimplement minimal** | Implement queue-specific admission/batching and verified IO controller outcomes. Reject the current unused return value and reject global sleep as IO backpressure. |
| D9 | Per-agent PSI/metrics | **Keep Sentinel** | Retain publishers and eBPF readers, add freshness/unknown/IO and bind decisions to snapshots. Reject treating missing samples as zero. |
| D10 | Platform-controlplane resource action path | **Wrap** | Route existing rule proposals through the durable resource/restart ports. Reject duplicate direct mutations and keep rule evaluation separate from effect authority. |
| D11 | CPU/NUMA/cache topology | **Integrate** | Integrate capability-gated Linux/systemd topology controls under #655 after observation and benchmarks. Reject M0 hard affinity and cross-node placement scope. |
| D12 | sched_ext/scx | **Reject** | Reject as an M0 dependency. A #655 experiment may revisit it only through #705/#656 with default-scheduler failback and no product dependency. |
| D13 | Slurm/Flux/Nomad | **Reject** | Reject adoption for single-node M0 because they add cluster/batch orchestration, authority, persistence, and operations. #690/#691 may still port independent mechanisms. |
| D14 | Panic/restart interaction | **Integrate** | Integrate #624 supervision outcomes with durable per-class restart tokens; neither owner may duplicate the other's state machine. |
| D15 | Cross-subsystem priority inheritance | **Reject** | Reject a synthetic resource "priority inheritance" layer: cgroup weights, OS scheduling, simulation authority, and mutex PI are distinct. Use protected capacity and only lock-specific kernel PI after a measured blocking path. |
| D16 | Thread-pool sizing/work conservation | **Reimplement minimal** | Add aggregate, admission-controlled concurrency budgets around existing semaphores while preserving idle-capacity work conservation. Reject static per-agent threads and reject Tokio defaults as a node resource policy. |

## 8. Proposed Sentinel resource-control contract

This section specifies the decision package for ORC approval. Names are proposed
wire/domain names, not claims that types exist.

### 8.1 `ResourceContractV1`

Every mutable target has:

- `target_id`: canonical service or agent ID resolved server-side;
- `owner_scope` and `owner_generation`;
- `contract_generation`, `previous_generation`, and canonical contract digest;
- admission class: `control`, `durability`, `interactive`, `agent`, or
  `background`;
- desired CPU weight/quota, memory min/low/high/max/oom-group, IO weight/max, PID
  max, optional allowed CPU/NUMA set, and OOM preference;
- mandatory and optional controller/capability sets;
- issuer principal, issue reason, correlation ID, and created time;
- last-known-good applied generation and exact readback digest.

Callers select a named policy/profile, not arbitrary cgroup paths, device numbers,
systemd units, PIDs, controller files, or OOM scores. The authoritative service
resolves all operating-system targets from an immutable catalog.

### 8.2 Apply state machine

```text
Desired
  -> Validating
  -> Applying
  -> Verifying
  -> Applied

Validating -> Rejected
Applying/Verifying -> Compensating -> RolledBack
Applying/Verifying/Compensating -> DegradedNeedsReconcile
```

The transition contract is:

1. Persist `Desired` with stable operation ID and expected owner/current
   generations.
2. Outside the database writer transaction, read host/controller capability and
   current values.
3. Validate the complete new contract, including aggregate service budget and safe
   memory-ordering rules, before the first mutation.
4. Persist the immutable apply plan and exact prior readback.
5. Apply in a documented safe order. Raising a ceiling precedes raising demand;
   lowering demand precedes lowering a ceiling. PID/memory changes never assume
   multi-file atomicity.
6. Read back every mandatory value. An optional value may be reported
   `Unsupported`, never silently `Applied`.
7. Commit `Applied` only if owner/current generations still match and readback
   equals the effective contract. Otherwise compensate to the prior readback.
8. On crash, restart scans non-terminal operations, re-reads kernel/systemd state,
   and either completes the same operation, compensates, or emits typed manual
   recovery. It never invents success from the database state alone.
9. Durable events/outbox publication is idempotent by operation ID and occurs only
   after the local state transition commits.

The effective memory maximum can differ from a requested profile only if the
contract explicitly models the clamped effective value and the caller accepts the
new digest. Hidden `current + margin` substitution is not an applied match.

### 8.3 Capability and readiness

`ResourceCapabilitySnapshotV1` binds boot ID, cgroup mode/mount, systemd version,
kernel release, controller availability/delegation, relevant files, PSI trigger
support, sched_ext state, topology, block-device mapping, catalog digest, and
observation time.

Readiness is command-specific:

- simulation reads and non-resource local commands remain available if optional
  controls are missing;
- agent launch/resize requiring a missing mandatory controller fails with typed
  `ResourceCapabilityUnavailable`;
- pressure admission fails safe for new expendable work when required telemetry is
  stale/unknown, while control, recovery, and durability work remain admitted;
- optional topology/uclamp/IO features are `Unsupported` unless the active policy
  explicitly makes them mandatory;
- boot readiness is false if current kernel/systemd readback contradicts a
  previously applied mandatory contract.

### 8.4 `PressureGovernorV1`

The durable state machine is:

```text
Normal
  -> Observing
  -> Constrained
  -> Critical
  -> Recovery
  -> Normal

Any state -> TelemetryUnknown
TelemetryUnknown -> Observing or ManualIntervention
Critical -> ManualIntervention
```

Transitions require:

- a versioned policy and signal snapshot;
- monotonic PSI totals plus `some`/`full` averages or triggers;
- freshness and source (`system`, service slice, agent cgroup);
- sustained observation duration, not one sample;
- separate enter/exit thresholds, minimum hold time, and cooldown;
- a stable action operation ID and current governor generation;
- restart reconstruction without resetting a still-active pressure episode.

The fixed action order is:

1. observe and notify;
2. deny new `background` work;
3. reduce background concurrency and apply real IO/LLM/workbench backpressure;
4. deny new non-required agent work while retaining required shift-transition
   intent;
5. slow wall-clock simulation pacing within the approved bound;
6. apply reversible profile reductions to eligible agents;
7. request kernel/systemd reclaim and remeasure;
8. select at most one eligible destructive action under a restart/kill token;
9. verify the effect and enter cooldown;
10. recover capacity gradually and replay durable pending admissions exactly once.

Control, operator recovery, event/outbox persistence, projection correctness, and
the minimum active agent topology defined by product policy are never discarded to
make pressure metrics look healthy.

### 8.5 Admission and exactly-once shift intent

Every required shift transition persists:

- transition ID and from/to shift;
- expected roster digest and per-agent desired state;
- removed, retained, pending-admission, spawning, active, failed, and resolved
  outcomes;
- owner/governor/resource generations;
- retry attempt, bounded next retry, and operator resolution state.

Memory pressure can move a new agent to `PendingAdmission`, but cannot mark the
shift transition complete. Recovery rechecks pressure and retries the same agent
intent. A duplicate loop or restart sees the same transition/agent operation ID and
does not despawn, spawn, or emit a second effect. A permanently unadmitted required
agent makes topology/readiness degraded and visible.

The same port accepts queue-specific admission requests for LLM, workbench,
nightrun, projection maintenance, and operator work. It does not absorb those
subsystems' execution or idempotency state.

### 8.6 Service hierarchy and OOM contract

The target hierarchy reserves capacity before allocating agent/background demand:

```text
sentinel.slice
|- control.slice      daemon, operator/control APIs
|- durability.slice   event bridge, projection, durable workers
|- interactive.slice  gateway and user-facing APIs
|- agents.slice       per-agent delegated cgroups
`- background.slice   nightrun, maintenance, optional analytics
```

Exact numeric weights and limits remain implementation-policy inputs. The contract
requires:

- aggregate host reserve and per-slice weights;
- `memory.high` as the normal pressure boundary and `MemoryMax` as last resort;
- `MemoryOOMGroup` where partial service death is unsafe;
- explicit `ManagedOOMPreference`/eligible victim set;
- `TasksMax`/`pids.max` for every process-creating class;
- IO weights/caps tied to the correct backing device and verified after boot;
- no immortal OOM setting without a reserved-memory/restart/failure analysis;
- per-service and global restart tokens durable across daemon restart;
- systemd-oomd dry-run/observation evidence before kill activation;
- a kill/restart receipt containing selected candidate set, reason, generation,
  action result, readback, and token balance.

### 8.7 Topology and priority boundary

M0 keeps the default Linux scheduler. CPU affinity and NUMA placement are soft or
reserved-capacity policies selected from measured topology, never caller-provided
CPU lists. `uclamp` is an optional performance/power hint and cannot establish
fairness, resource ownership, or simulation priority inheritance.

Under #655, a future experiment must:

1. observe workload CPU/NUMA/LLC behavior without mutation;
2. define reserved control CPUs and work-conservation fallback;
3. compare systemd/cpuset soft placement with default scheduling;
4. test CPU hotplug, missing NUMA, SMT, cpuset exhaustion, and restart;
5. run Sentinel correctness and sidecar gates on the declared target;
6. restore the default scheduler and unrestricted optional placement on any error.

sched_ext additionally requires #705 dependency approval, #656 update ownership,
exact kernel/BPF compatibility, a watchdog/failback receipt, and proof that its
absence never blocks product startup.

### 8.8 Security boundary

- Only authenticated operator or internal service principals may mutate resource
  policy; authorization is derived server-side.
- Agent/customer/provider metadata cannot select profile authority, cgroup path,
  systemd unit, CPU set, block device, OOM preference, or victim.
- Catalog and policy digests bind every operation and readback.
- `/sys/fs/cgroup`, systemd DBus, eBPF, `/proc`, and signal permissions are held by
  the smallest service boundary; API workers do not inherit arbitrary host control.
- Telemetry labels use canonical IDs and bounded cardinality. No command line,
  environment, secret, token, provider prompt, or private path appears in events,
  metrics, or error payloads.
- A stale owner generation, stale capability snapshot, unknown controller,
  conflicting contract, cross-agent target, or policy-digest mismatch fails closed.
- Dry-run is a read-only plan/readback operation and cannot invoke reclaim, signal,
  restart, systemd mutation, or cgroup write paths.

## 9. Negative and failure contracts

| Failure or attack | Required result |
|---|---|
| One of CPU/memory/PID mandatory controllers is unavailable but another succeeds | Launch/resize returns typed unavailable; readiness identifies the exact missing controller. No partial-success claim. |
| IO controller or device mapping is absent | If IO is mandatory, fail before launch. If optional, apply the reduced effective contract and report `Unsupported` with a different digest. |
| Crash after any individual cgroup/systemd write | Restart reads actual values and completes or compensates the same operation. It never emits duplicate success. |
| Event/outbox append fails after verified kernel apply | Applied generation remains durable and publication retries independently; the kernel mutation is not repeated blindly. |
| Owner, contract, or authority generation changes during apply | CAS fails, compensation restores prior readback, and a typed conflict is recorded. |
| Compensation fails | Target enters `DegradedNeedsReconcile`; further conflicting mutation is fenced, unrelated targets continue. |
| PSI file read/parse fails or sample ages out | State becomes `TelemetryUnknown`; no stale value silently authorizes new expendable work. |
| One transient high PSI sample | No destructive action; sustained window and state transition are required. |
| Pressure oscillates at a threshold | Separate enter/exit thresholds, hold-down and cooldown prevent busy loops. |
| Pressure recovers after a delayed shift | The durable pending roster is admitted exactly once; shift completion requires roster/readiness verification. |
| Daemon restarts while shift work is pending | Same transition IDs resume; no old-shift double removal or new-shift double spawn. |
| Agent spoofs a critical role or low OOM preference | Server-side catalog wins; request is rejected and audited. |
| Background workload starves control/durability | Slice reserve and weights preserve the protected class; readiness alerts before destructive response. |
| Destructive candidate disappears or kill fails | Readback reports failure; token/cooldown semantics are explicit, and the next candidate is not attempted without the approved bounded policy. |
| systemd-oomd or resource governor is disabled | Existing daemon starts with static last-known-good limits; mutation endpoints report typed unavailable, reads remain available. |
| sched_ext loader/scheduler crashes | Kernel default scheduler is verified active; Sentinel remains functional because M0 does not depend on scx. |
| CPU hotplug/NUMA topology changes | Optional placement becomes stale and is reconciled or removed; resource ownership and ECS correctness remain intact. |
| Restore/reboot loses kernel state | Desired durable generations are reconciled against new capability/readback before agent admission opens. |
| Restart token store is corrupt or unavailable | Automatic destructive recovery fails closed; operator recovery remains available. |
| Metrics/event sink is unavailable | Local resource state transition remains durable; bounded outbox retries without reapplying effects. |

## 10. Butterfly-effect integration map

```text
Host capability/catalog
        |
        v
ResourceContractV1 -----> cgroup/systemd apply + readback
        |                          |
        |                          v
        |                    runtime readiness
        v
PressureGovernorV1 -----> AdmissionPort
        |                  |   |   |   |
        |                  |   |   |   +--> background/maintenance
        |                  |   |   +------> workbench (#694)
        |                  |   +----------> LLM/gateway
        |                  +--------------> shift/runtime roster
        v
Restart/KillBudgetPort <---- platform rules + #624 supervision
        |
        v
events/outbox -> projections/API/console -> #650 acceptance

Topology observation (#502) -> #655 optional policy -> #705/#656 if scx
Slurm/Flux deep mechanisms -> #690/#691, never an M0 runtime dependency
```

| Consumer or owner | Required delta | Explicit non-overlap |
|---|---|---|
| Daemon orchestrator | Durable shift intent, admission result, wall-clock pacing, governor cycle budget | Does not move ECS schedule order or runtime execution authority. |
| `sentinel-sandbox` | Versioned contract apply/readback/compensation and production PID limit | Does not own product admission or service restart policy. |
| Runtime health/reconciliation | Compare desired/applied/capability generations and repair non-terminal operations | Does not independently select limits. |
| Platform-controlplane | Submit typed resource/restart requests and consume receipts | Does not write cgroups, signals, or systemd directly after cutover. |
| systemd deployment | Slice hierarchy, weights, high/max/OOM/tasks/IO/restart policy and boot validation | Does not own agent roster or simulation policy. |
| Event/CQRS chain | Versioned desired/applied/governor/admission/action outcomes via durable outbox | Projection failure cannot repeat kernel effects. |
| API/console | Read-only resource state, freshness, capability, generation, pending work, and action outcomes; authenticated operator commands | No raw paths, PIDs, arbitrary controller values, secrets, or self-attested role. |
| LLM/gateway | Queue/concurrency admission and bounded backpressure | Provider routing/cost contracts remain separately owned. |
| Workbench #694 | Uses the narrow admission port around invocation starts | #720 does not implement dispatch, sandbox channel, or completion evidence. |
| #624 | Supplies supervision/panic outcome into restart budget | #720 does not choose unwind/abort strategy. |
| #650 | Approves numeric product policy and consumes final live evidence | Does not block token-free implementation or choose dependencies. |
| #502/#655 | Observe then decide topology placement and optional sched_ext experiment | Cannot change M0 portable fallback or make scx mandatory. |
| #690/#691 | Deeply mine Slurm/Flux mechanisms | Cannot silently add cluster/batch control planes to the single node. |
| #705/#656 | Approve and maintain any future dependency | No new M0 dependency is proposed by D1-D10. |

## 11. M0 classification

| Finding | Classification | Evidence and owner acknowledgement |
|---|---|---|
| Memory-pressure shift path consumes the shift transition before replacements spawn | `BLOCKS_M0` | Source lines 6135-6143 and 6521-6529 prove required work can be permanently missed. Proposed resource epic RC1 owns correction; #650 must acknowledge before product acceptance. |
| Mandatory controller delegation can report success after only one controller | `M0_HARDENING` | `delegate_controllers` returns `any_ok`. RC0 owns exact capability/readiness. |
| Production has no `pids.max` although the ignored fork test assumes it | `M0_HARDENING` | Test writes the value manually. RC0 owns production enforcement. |
| Partial/unverified cgroup resize and process-local profile state | `M0_HARDENING` | Direct multi-file writes and best-effort event/IO paths lack crash recovery. RC0 owns desired/applied generations. |
| Global PSI uses stale values after read failure and independent reactions | `M0_HARDENING` | `sample_psi` changes fields only on successful reads. RC1 owns freshness/governor state. |
| IO batching policy has no production consumer | `M0_HARDENING` | Repository call-site inventory contains only tests. RC1 owns real queue admission/batching. |
| No coherent aggregate service slice, soft memory boundary, or global restart tokens | `M0_HARDENING` | Current unit inventory has heterogeneous hard maxima and per-unit restart limits. RC2 owns hierarchy/OOM/restart integration. |
| Heavy/Suspended profile comments exceed implementation | `M0_HARDENING` | `detect_profile` returns only Idle/Normal. RC0/RC1 may delete or implement profiles only when backed by contract/evidence. |
| CPU/NUMA/cache soft placement | `POST_M0` | #655 owns capability-gated topology after observation and benchmarks. |
| sched_ext/scx scheduler replacement | `POST_M0` | Rejected for M0 by D12; only #655/#705/#656 may reopen it. |
| Slurm/Flux/Nomad adoption | `POST_M0` | Rejected for this scope; #690/#691 retain independent research ownership. |

No other source finding is labeled `BLOCKS_M0`. In particular, the absence of
sched_ext, exclusive CPU allocation, or advanced NUMA policy is not an M0
correctness defect.

## 12. Proposed implementation-owner package

No issue in this section has been created or mutated. Materialization requires ORC
approval of D1-D16 and the owner graph.

### 12.1 Proposed epic: M0 single-node resource-control hardening

The epic is `M0_HARDENING` with one `BLOCKS_M0` child finding. It is downstream of
no new dependency. Its ordered children are RC0 -> RC1 -> RC2 -> RC3.

### RC0. Durable resource contract, capability, and reconciliation

**Class/target:** `M0_HARDENING`; implementation tests token-free; live target
`SINGLE_NODE` only after an exact target assignment and issue-specific snapshot.

**Scope:** `ResourceContractV1`, canonical target catalog, command-specific
readiness, exact mandatory/optional controller capability, production `pids.max`,
ordered apply/readback/compensation, durable desired/applied generations,
idempotent event outbox, boot/restart reconciliation, API and health read models.

**ACs:** apply and read back every profile field; reject partial mandatory
capability; recover after every write/outbox failpoint; preserve unrelated target
progress; reject spoofed paths/targets/generations; expose no secrets; prove
restart convergence.

**Negative/failure:** all rows in sections 8.1-8.3 and 9 that concern apply,
capability, crash, CAS, compensation, reboot, or publication.

**Benchmarks:** on the declared live target measure p50/p95/max apply/readback and
reconcile cycle cost under 1, 26, and 60 configured agents, with CPU/memory/IO,
event/projection, service restarts, and correctness sidecars. Values are recorded,
not pass thresholds until #650 approves them.

**Rollout/rollback:** default-off observer -> shadow plans/readback -> one canary
agent -> bounded cohort -> all agents. Rollback closes mutation admission,
reconciles the last-known-good generation, verifies readback, and retains immutable
outcomes. A failed compensation stops fail-closed with the snapshot retained.

**TOGAF delta:** replace direct static profile language with desired/applied
generation, controller capability, PID, readback, and recovery contracts.

### RC1. Pressure governor, admission, and shift correctness

**Class/target:** shift correction `BLOCKS_M0`, remaining scope
`M0_HARDENING`; future live target `SINGLE_NODE` assigned by the runtime owner.

**Depends on:** RC0 ports; does not require RC2 systemd-oomd activation.

**Scope:** `PressureGovernorV1`, freshness-bound PSI snapshot, durable state,
hysteresis/hold/cooldown, admission classes, exactly-once pending shift roster,
queue-specific LLM/workbench/background admission, actual IO batching/backpressure,
bounded pacing, reversible degradation, recovery.

**ACs:** reproduce current missed-spawn sequence; under pressure old agents are
removed only according to the durable transition and required replacements remain
pending; recovery spawns each exactly once; restart at every phase converges; stale
telemetry fails safe without blocking control/durability; independent admission
classes recover without duplicates; simulation schedule order remains unchanged.

**Negative/failure:** transient/oscillating/missing PSI, clock/boot change, duplicate
tick, crash, event failure, persistent pressure, operator cancel, and non-owner
agent requests cannot skip or duplicate work.

**Benchmarks:** governor cycle and tick work/sleep p50/p95/max, admission latency,
queue depth, pressure detection/recovery, roster convergence, and sidecars at 1/26/60
agents. Synthetic load is issue-owned; build time and upstream results are invalid.

**Rollout/rollback:** default-off shadow state -> admission logging -> background
admission -> canary shift -> all queues -> bounded pacing. Rollback disables new
governor actions and drains durable pending admissions through the static
last-known-good policy; it never marks required work complete to clear pressure.

**TOGAF delta:** define pressure state, freshness, exactly-once shift intent,
degradation order, protected work, and recovery rather than three independent
threshold callbacks.

### RC2. Sentinel service slices, OOM policy, and restart budgets

**Class/target:** `M0_HARDENING`; future live target `SINGLE_NODE` assigned by the
runtime owner.

**Depends on:** RC0 capability/readback and RC1 pressure/restart ports; integrates
with #624 without owning panic strategy.

**Scope:** service slice hierarchy, aggregate headroom, CPU/IO weights, memory
low/high/max, OOM grouping/preference, TasksMax, restart/kill tokens, systemd-oomd
observe/dry-run/activation, boot validation, service/API health.

**ACs:** protected control/durability services stay responsive under hostile
agent/background CPU, memory, IO, and fork pressure; wrong/missing unit properties
fail readiness; one eligible victim at most per token/cooldown; failed action is not
success; tokens and non-terminal actions survive restart; no unapproved service is
kill-eligible.

**Negative/failure:** unavailable systemd/oomd, unit drift, aggregate
overcommit, wrong device, fork bomb, OOM storm, restart loop, corrupted token state,
and dry-run mutation all fail as specified in sections 8.6 and 9.

**Benchmarks:** control/API/tick/event/projection latency and throughput, pressure
recovery, reclaim and restart timing, token behavior, NRestarts, and resource
sidecars under reproducible CPU/memory/IO/PID loads. Numeric gates require #650
approval.

**Rollout/rollback:** read-only unit audit -> static slice/weights -> memory high and
PID limits -> OOM observe/dry-run -> separately approved victim activation.
Rollback disables destructive OOM policy first, restores signed unit/catalog
configuration, daemon-reloads, restarts only within budget, and verifies every
property.

**TOGAF delta:** add the service hierarchy, aggregate host reserve, OOM victim and
restart-token boundaries, and command-specific readiness.

### RC3. Final single-node resource acceptance and fault matrix

**Class/target:** `M0_HARDENING`; `SINGLE_NODE` on the exact target assigned by the
runtime owner.

**Depends on:** RC0-RC2 and #650-approved numeric policy. It does not absorb #655
topology work.

**Scope:** issue-specific VM snapshot, deploy verified merge SHA, positive/negative
CPU/memory/IO/PID pressure matrix, restart recovery, event/projection/API readback,
26/26 and 60/60 policy cases where the product configuration requires them,
benchmarks with sidecars, stability soak, rollback drill, evidence.

**ACs:** all RC0-RC2 contracts pass on the declared host; no missed required work,
duplicate effects, secret leakage, panic, drift, restart loop, stale capability, or
unbounded retry; rollback restores the exact baseline.

**Rollback:** retain the issue-specific snapshot and backups on any failure; revert
only after readback proves the declared rollback point. Snapshot deletion is a
post-success, separately verified operation.

**TOGAF delta:** record only measured accepted thresholds and the final portable
single-node contract. sched_ext/NUMA optimizations remain optional.

### 12.2 Exact existing-owner deltas

| Existing owner | Proposed reciprocal delta after ORC approval |
|---|---|
| #74/#196 | Resource governor publishes a freshness-bound pressure view; biological projection is a consumer, never resource authority. |
| #147 | Add a historical correction linking RC1 for unused IO batching and the consumed shift transition. Do not rewrite past delivery evidence as if RC1 were complete. |
| #227 | Require RC0/RC1/RC2 cycle budgets and measured control-plane latency; a slow resource controller cannot run unbounded in the tick. |
| #265 | Mark Heavy/Suspended and transactional resize as superseded target claims owned by RC0/RC1; preserve the closed issue as delivery history. |
| #624 | Define a versioned supervision-outcome input to RC2 restart tokens and keep panic-strategy authority in #624. |
| #650 | Add RC0-RC3 as downstream acceptance dependencies; #650 approves numeric limits/latency/pressure policy, not implementation internals. |
| #690/#691 | Record Slurm/Flux adoption rejected for M0 here; their research may propose later mechanism-only work without changing RC0-RC3. |
| #705 | Record no new M0 dependency. Any future scx/oomd/cluster-scheduler integration requires an exact dependency, privilege, license, security, update, and rollback decision. |
| #656 | Own upgrades only for dependencies approved by #705; kernel/systemd compatibility claims stay in the deployment/RC owners unless represented as repository dependencies. |
| #502 | Add read-only topology/load/resource-generation telemetry needed by #655 and RC read models; no mutating scheduler authority. |
| #655 | Own topology and optional sched_ext experiment, explicitly `POST_M0`, capability-gated, reversible, and unable to block M0 startup. |
| #659 | Register this study and, after approval, the single RC0-RC3 epic as its only new implementation graph. |

### 12.3 Materialization gate

Before any GitHub body or child mutation:

1. ORC approves or changes D1-D16.
2. ORC approves the RC0-RC3 ordering and exact existing-owner deltas.
3. Each new/changed body receives runtime target, complete ACs, negative criteria,
   benchmarks, rollout, rollback, TOGAF delta, dependencies, and reciprocal links.
4. Labels are normalized without preserving contradictory history labels on new
   work.
5. Every new/changed owner receives a fresh Issue Quality Gate PASS and exact body
   SHA-256 readback.
6. #720 AC-6/AC-N5 become PASS only after that live graph exists. Until then they
   remain pending by design, not waived.

## 13. Rollout, rollback, and evidence policy

### 13.1 Ordered delivery

1. RC0 token-free state machine, fakes, parser/catalog, storage, failpoints, and
   unit/integration tests.
2. RC0 read-only host observation, then explicitly authorized canary enforcement.
3. RC1 token-free pressure/admission/shift tests and deterministic signal fakes.
4. RC1 shadow observation before any admission mutation.
5. RC2 unit catalog and property validator before systemd mutation.
6. RC2 static non-destructive slice/weights/high/PID policy before OOM actions.
7. systemd-oomd observe/dry-run evidence and separate activation approval.
8. RC3 final exact-main live matrix, benchmark, rollback drill, and stability
   readback.
9. #650 consumes the evidence and alone approves product thresholds/readiness.

### 13.2 Rollback invariants

- Git/config rollback never asserts that kernel state changed; readback is
  mandatory.
- Disabling the governor stops new resource actions, not event publication,
  reconciliation, control, or pending required-work recovery.
- Last-known-good resource generations and immutable outcomes are retained through
  rollback.
- A failed compensation, unknown capability, or property mismatch remains
  degraded/fenced; no loop retries without a bound.
- A failed OOM/restart action does not refund or consume a token silently.
- A scheduler experiment always verifies the default scheduler after exit.
- Runtime snapshots are created only by the future issue with explicit target and
  authority; this research issue creates none.

### 13.3 Required implementation evidence

Every implementation child must record:

- exact source/base/merge/deployed SHA and config/catalog digest;
- capability and desired/applied readback before and after;
- failpoint/restart matrix and negative tests;
- event/outbox/projection/API correlation by stable operation ID;
- process tree, service NRestarts, logs, pressure freshness, and token state;
- benchmark method, workload, sidecars, p50/p95/max, and correctness results;
- snapshot/backup/rollback verification and final cleanup;
- secret/public-safety negative scans;
- explicit distinction between mock, local, single-node live, and upstream evidence.

## 14. TOGAF target deltas

After implementation approval, the architecture guide should:

1. replace threshold-only adaptive tick with the pressure-governor state machine,
   freshness, hysteresis, hold/cooldown, admission classes, degradation order, and
   exactly-once pending work;
2. define the service slice and reserved-capacity hierarchy;
3. define `ResourceContractV1`, capability snapshot, desired/applied generation,
   ordered apply/readback/compensation/reconcile, and PID control;
4. distinguish CPU fairness weight, hard quota, affinity, uclamp, OS priority, and
   simulation schedule order;
5. define memory low/high/max/OOM-group and approved victim/restart-token policy;
6. define IO weight/cap/device/readback and queue-specific backpressure;
7. retain cgroup/systemd/default scheduler as the portable M0 path;
8. keep NUMA/cache/resctrl/sched_ext optional, capability-gated, measured,
   reversible, and `POST_M0`;
9. replace undocumented "completed profile" implications with current implemented
   and target states;
10. link numeric performance/pressure thresholds only after RC3/#650 approval.

This research issue does not edit the TOGAF HTML.

## 15. Acceptance-criteria status

| Criterion | Study evidence | Status before ORC decision |
|---|---|---|
| AC-1 | Sections 3.1-3.4 map current source, tests, runtime contracts, claim drift, incidents, TOGAF targets, and all named owners. | PASS |
| AC-2 | Section 4 evaluates eight candidate families with a reproducible ten-factor rubric and explicit rejection reasons. | PASS |
| AC-3 | Section 5 reviews five pinned systems through source, tests, failures, security, license, and operations. | PASS |
| AC-4 | Section 6 covers every requested mechanism and all five deep-review systems, including correctness, failure semantics, 1:n/determinism, security, maintenance, dependency cost, and integration boundary. | PASS |
| AC-5 | Section 7 assigns exactly one explicit decision to every mechanism and records rejected alternatives. | PENDING ORC APPROVAL |
| AC-6 | Section 12 provides implementation-ready RC0-RC3 and exact existing-owner deltas, but live materialization is explicitly forbidden until ORC approval. | PENDING ORC APPROVAL/MATERIALIZATION |
| AC-7 | Section 11 classifies every finding and limits `BLOCKS_M0` to the source-proved missed-shift defect. | PENDING M0 OWNER ACKNOWLEDGEMENT |
| AC-8 | This document is the sole repository change; final ASCII/public-safety, typo, local/external link, provenance, render, scope, and diff gates pass on the frozen delivery head. | PASS |
| AC-N1 | No dependency is added or recommended merely because another project uses it. | PASS |
| AC-N2 | Every deep review records license, security, provenance, maintenance/operations, and integration boundary; no code is copied. | PASS |
| AC-N3 | Closed labels and unit tests are treated as bounded historical evidence, not current optimality proof. | PASS |
| AC-N4 | Runtime target is NONE; no VM, deployment, Cargo/Rust, provider, or performance benchmark was used. | PASS |
| AC-N5 | Every accepted gap has an exact proposed owner; live ownership remains pending because pre-approval GitHub mutation is forbidden. | PENDING MATERIALIZATION |

## 16. Limitations and pending decisions

- No upstream or Sentinel binary was built or tested. Source tests establish design
  intent and failure contracts, not compatibility or achieved product behavior.
- No runtime was read or mutated. There is no Sentinel pressure, latency, topology,
  OOM, throughput, or power measurement in this study.
- Exact numeric CPU/memory/IO/PID limits, PSI thresholds, durations, hysteresis,
  cooldown, restart tokens, reserved headroom, benchmark gates, and soak duration
  remain unapproved policy inputs for RC0-RC3/#650.
- The systemd version and kernel capability available on a future target are not
  assumed from repository source. RC0/RC2 must discover and verify them live under
  an explicit runtime assignment.
- systemd-oomd is a conditional decision: the eligible/protected set, dry-run
  result, memory/swap model, and rollback must be approved before destructive
  activation.
- sched_ext/scx is rejected for M0, not declared universally unsuitable. #655 may
  propose a bounded experiment later.
- Slurm and Flux source mechanisms are intentionally left to #690/#691 for deep
  study; this document rejects product adoption, not future independent mechanism
  proposals.
- D1-D16, RC0-RC3, issue deltas, and the M0 blocker classification require ORC
  review before any GitHub materialization. Until that decision, AC-5, AC-6,
  AC-7, and AC-N5 are not claimed complete.
