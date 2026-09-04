# Virtual Company Work Execution Contract

Status: M0 implementation contract
Profile: `web-project-v1`
Final acceptance: issue #650
Contract gate: issue #693

## Purpose and Claim Boundary

Project Sentinel's TOGAF guide is the target-architecture vision. This document is the repository contract that turns the single-node virtual-company lane of that vision into executable engineering work.

The M0 claim is deliberately bounded:

> A customer can give the canonical single-node Project Sentinel deployment a bounded static-website assignment. The virtual company qualifies and agrees the work, plans and executes it through its hierarchy, uses isolated tools to produce real artifacts, independently validates and releases the result, obtains explicit customer acceptance, and retains durable organizational knowledge.

M0 does not claim Cluster-12 GA, N-node availability, single-survivor continuity, native bare-metal operation, arbitrary project types, or external production hosting. Those target capabilities remain in the TOGAF vision and their dedicated delivery programs.

A configured roster, healthy service list, room conversation, generated brief, mock inference response, or completed task row is not sufficient proof. Product acceptance requires one durable customer-to-acceptance lineage with matching API, event, artifact, runtime, cost, memory, and Console evidence.

## Product Profile

`config/work-profiles/web-project-v1.toml` is the canonical M0 profile. It defines the roles, lifecycle, tool profiles, required artifacts, quality gates, cost rules, and security limits used by implementation and acceptance tests.

The profile is data, not an alternate source of authority. Code must validate it against versioned schemas and fail closed on unknown versions, unknown capabilities, invalid transitions, or an incomplete gate set.

The M0 deliverable is a dependency-free static website assembled from customer-provided or repository-owned inputs. Runtime package installation and unrestricted Internet access are excluded. This keeps the first product proof real while preserving deterministic, isolated execution.

The profile is complete only when all of these outcomes exist:

1. Explicit customer agreement.
2. A governed project and dependency graph.
3. Multi-role implementation through the agent workbench.
4. Independent QA bound to the exact candidate digest.
5. An immutable release and delivery receipt.
6. Explicit customer acceptance or governed rework.
7. Durable closeout memory with source provenance.

## Single-Node Deployment Boundary

The canonical M0 deployment runs on `.240` with cluster configuration absent. The cluster-disabled path is first-class and must not wait for peer discovery, quorum, remote ownership, migration readiness, or a cluster boot latch.

The deployment must provide every service used by the profile:

- `sentinel-daemon` for ECS, orchestration, operator commands, ownership, workbench dispatch, and durable events.
- Cortex Gateway for authenticated provider routing, caller roles, model policy, usage, and cost.
- NATS/JetStream and the Sentinel bridge where the selected event path requires them.
- `sentinel-projection` for customer, project, runtime, cost, and Console read models.
- Agent runtime plus the selected bwrap/Landlock/cgroup sandbox path.
- Existing NMDA, memory, Night-Run, background-agent, and Gaia paths used by closeout and oversight.
- Console/backend surfaces required to inspect the full lineage.

Single-node startup owns local scopes through the established single-node fast path. Cluster-specific code may be present in the binary but must remain inert when cluster configuration is absent.

Runtime truth is derived from all relevant identity sets, not from one count. Configured, currently scheduled, resident ECS, external runtime, event-store, and projection identities must reconcile. A tool-bearing task must never silently fall back from its secure runtime to ECS-only or host execution.

## Actors and Authority

Actors use stable identities and authenticated caller roles. Display names are never authority.

| Actor or role | May do | Must not do |
|---|---|---|
| Customer | Submit, clarify, accept/reject proposal, inspect delivery, accept or request changes | Assign internal agents, approve QA, promote releases, mutate history |
| Sales | Qualify requests and author proposals within policy | Infer customer acceptance or approve implementation |
| Project Manager | Create the project plan, manage scope and blockers, request assignments | Execute unrestricted tools or approve its own implementation |
| Technical Lead | Decompose technical work, validate dependencies, assign eligible specialists | Change the customer agreement or self-approve QA |
| Designer | Produce design artifacts through the assigned workbench | Promote releases or access another project workspace |
| Developer | Produce implementation artifacts and tests through the assigned workbench | Self-approve QA or bypass runtime policy |
| QA | Execute the approved quality profile and record findings | Modify candidate artifacts or approve a different digest |
| Release Manager | Promote a fully approved current candidate and perform governed rollback | Waive missing gates or impersonate customer acceptance |
| Gaia | Observe, diagnose, escalate, and recommend | Bypass agreement, budget, assignment, QA, release, or customer authority |
| NMDA/Night-Run/background processing | Derive source-linked memory and insights | Rewrite authoritative workflow history directly |

Effective authority is the intersection of:

- authenticated actor identity and caller role;
- current organization hierarchy and delegation;
- project membership and project authority;
- work-item assignment or approval role;
- declared agent capabilities;
- profile policy and budget;
- current aggregate state and version.

Every material command records actor, role, aggregate, expected version, before state, after state, reason, request ID, and canonical request digest.

## Domain Model

All records use explicit schema versions, stable IDs, millisecond timestamps for audit, and monotonic aggregate versions for concurrency. Wall-clock time never determines authority by itself.

### CustomerRequest

Contains customer identity reference, request text reference, constraints, desired outcome, clarification history, qualification result, and idempotency key. Private customer content may be stored in protected runtime storage; public evidence uses only redacted references and digests.

### Proposal and Agreement

A proposal contains scope, deliverables, exclusions, acceptance criteria, assumptions, cost ceiling, expiry, and version. Customer acceptance binds the exact proposal digest and creates an immutable agreement. Later changes create a new proposal/agreement generation; they do not rewrite the accepted record.

### Project

Contains agreement ID and digest, profile ID/version, project owner, participants, budget, current release generation, status, and closeout reference.

### WorkItem and Dependency

A work item declares:

- stable ID and project ID;
- title and bounded objective;
- required role and capabilities;
- dependency IDs;
- immutable input references;
- required output artifact kinds;
- quality gate;
- owner and assignee;
- budget and deadline policy;
- state, version, and completion evidence.

Dependencies form a directed acyclic graph. A work item cannot become ready until every required predecessor has a successful, digest-bound output.

### Assignment, Decision, Handoff, and Blocker

Assignments bind an eligible agent to a work item and expire or revoke explicitly. Decisions and handoffs are structured records linked to project and work-item IDs. A blocker has owner, impact, required resolution authority, and status. Free-form chat may propose these commands but is never itself authoritative state.

### WorkbenchInvocation

Contains invocation ID, project/work-item/agent/workspace IDs, capability set, tool profile, tool, canonical input digest, deadline, attempt, runtime, state, outcome, resource accounting, and output artifact references.

### Artifact and Manifest

Artifacts are immutable outputs identified by digest. A manifest binds artifact kind, media/schema version, producer invocation, project/work item, source/input digests, toolchain/runtime profile, size, provenance, and retention class.

### Review, Release, Delivery, and Closeout

Reviews bind an independent reviewer and evidence to one candidate digest. Releases bind all required approvals and provenance into an immutable manifest. Delivery receipts bind release and customer. Closeout binds accepted release, project decisions, lessons, and source references into organizational memory.

## State Machines

Unknown states or transitions fail closed. Commands use compare-and-set aggregate versions and idempotency keys.

### Customer request and agreement

```text
Submitted -> Clarifying -> Qualified -> Proposed -> Accepted
     |            |           |           |-> Rejected
     |            |           |           |-> Expired
     +------------+-----------+-----------> Cancelled
```

Only explicit authenticated customer acceptance moves `Proposed` to `Accepted`. Internal agents, provider output, silence, or chat sentiment cannot do so.

### Project

```text
Planned -> Active -> DeliveryCandidate -> InReview -> Released -> Delivered -> Accepted -> Closed
   |         |             |                  |          |           |
   |         +-> Blocked ---+                  +-> Rework -+-----------+
   +---------------------------------------------------------------> Cancelled
```

Rework creates a new work/candidate generation while preserving prior release and review history.

### Work item

```text
Proposed -> Ready -> Assigned -> Claimed -> InProgress -> InReview -> Done
    |         |         |          |            |            |
    +---------+---------+----------+------------+-----------> Cancelled
                                      +-> Blocked -> Ready
```

`InProgress` requires an authorized claim or executing workbench step. `Done` requires all declared outputs plus completion evidence. Agent presence or elapsed ticks cannot advance state.

### Workbench invocation

```text
Reserved -> Executing -> Succeeded
    |           |------> Failed
    |           |------> Cancelled
    |           |------> TimedOut
    +------------------> DigestConflict
```

Replaying a matching completed request returns the durable outcome. Reusing the invocation ID with a different digest is a typed conflict.

### Candidate, release, and delivery

```text
Candidate -> QaRunning -> QaPassed -> Approved -> Released -> Delivered -> Accepted
    |           |           |           |           |           |
    +-----------+-> QaFailed -> Rework <-+-----------+-----------+
```

Promotion requires every profile gate to pass against the same current candidate digest.

## Company Workflow

The canonical M0 flow is:

1. **Intake:** the customer submits a bounded request with an idempotency key.
2. **Qualification:** Sales asks bounded clarifying questions and records constraints and assumptions.
3. **Proposal:** Sales creates a versioned proposal with deliverables, exclusions, acceptance criteria, and cost ceiling.
4. **Agreement:** the customer explicitly accepts the exact proposal digest.
5. **Project creation:** Project Management creates one project atomically from the agreement.
6. **Planning:** Project Management and the Technical Lead produce an acyclic work graph.
7. **Assignment:** eligible specialist agents receive work according to capability, hierarchy, authority, budget, and availability.
8. **Execution:** assigned agents claim steps and use the secure workbench to produce immutable artifacts.
9. **Collaboration:** decisions, questions, blockers, handoffs, and acknowledgements are durable and linked to work.
10. **QA:** an independent QA actor validates the exact candidate digest.
11. **Release:** Release Management promotes only a complete approved candidate.
12. **Delivery:** the customer receives a preview and immutable delivery receipt.
13. **Acceptance or rework:** explicit customer action closes the release or creates linked rework.
14. **Closeout:** accepted work and lessons become source-linked organizational memory.

The orchestrator is event-driven. It reacts to durable state changes and bounded timers; it must not invoke a provider unconditionally on every simulation tick. Blocking work yields a durable blocker and waits for a relevant event or bounded retry schedule.

## Collaboration Contract

Rooms provide human-like communication context, but conversation is not company
authority. Authoritative collaboration is a bounded, versioned session tied to
one tenant, project, exact work item, organization generation, sealed project
policy,
subject, input digest, participant set, and transition sequence. Each material
step is submitted through an authenticated company-workflow command and is
published through the canonical Event Store append and outbox boundary.
Session creation and every later mutation recheck the active work assignment,
including its exact ID, version, and canonical digest, together with the
organization generation and project-profile policy digest before state can
advance. Reassignment freezes the prior session for audit; work continues in a
new session bound to the new assignment rather than inheriting stale authority.
Before a new session can start, a separate admission decision binds its exact
work item, selected capability snapshots, sparse routes, resource limits, and
admission contract digest. Only a newer admission for that same work item
revokes the session; an unrelated work item cannot interrupt it. Session limits
may narrow but never widen the admitted participant, message, transition, or
deadline bounds. Historical version-1 sessions remain readable for audit but
cannot authorize new collaboration effects.

An employee keeps one permanent company role, such as Developer, QA, or
Technical Lead. A collaboration session gives that employee a separate
task-local behavior mandate:

- `Discover` finds bounded facts and explicit unknowns.
- `Implement` produces an artifact against an accepted decision and checks.
- `Verify` returns an independent evidence-backed pass, fail, or blocker.
- `Challenge` searches for counterexamples and unresolved risks.
- `Synthesize` compares claims without inventing consensus.
- `Decide` prepares an authorized choice with residual risk.
- `Escalate` sends the smallest sufficient packet to the required authority.

The Gateway input combines the permanent role with this mandate, its required
inputs, evidence shape, visibility rules, forbidden actions, and stop
condition. A model response may propose a typed command, but cannot persist a
claim, expose peers, acknowledge a handoff, make a decision, or complete work.

When independent judgment matters, participants first commit immutable
`IndependentClaim` records without seeing peer conclusions. Each claim binds
its contributor, mandate, evidence, assumptions, uncertainty, confidence
basis, capability snapshot, and input digest. Only an authorized exposure
barrier reveals committed claims, and only when the source privacy classes are
a subset of the reader's classes. This prevents a senior or early answer from
silently anchoring the rest of the team while still preserving need-to-know
boundaries when evidence is compared afterward.

Work moves between employees as a digest-bound `HandoffPacket`, not as an
unbounded transcript. The packet names the objective, authority scope, inputs,
artifacts, evidence, assumptions, unresolved questions, uncertainty,
acceptance checks, required capabilities, privacy classes, and relevant
generations. Its authority scope is the exact assignment ID and canonical
assignment digest sealed by the session, not sender-supplied descriptive text.
The receiver may accept, reject, escalate, or request one of four typed
clarifications:

- `DataGap`: required information is absent.
- `SignalCorruption`: supplied evidence cannot be trusted as received.
- `ReferentialDrift`: an identifier, generation, or digest no longer names the
  expected subject.
- `CapabilityGap`: the receiver lacks a required capability or permitted data
  class.

Clarification rounds are bounded. Repeating a basis or supplying no new
information escalates instead of creating an endless conversation. Acceptance
does not mean consumption: `Consumed` is valid only when the exact packet is
bound to a real downstream independent claim, Workbench invocation, review, or
project decision digest.

`DissentRecord` preserves evidence and residual risk even after an authorized
`ProjectDecision`. A dissent record is bound to that exact decision; it cannot
be rebound to make another decision appear contested or supported. Claims and
dissent can support a decision, but neither creates decision authority.
Filtered reads expose only the participant's tenant, project, directed
handoffs, privacy classes, and the claims permitted by the exposure barrier;
operator reads remain tenant and project scoped. The Gateway prompt compiler
uses the same exposure and privacy-class subset rule, so a digest hidden by the
read API cannot leak through model context.

The local workflow database is command and recovery materialization. Its
immutable publication proposals are replayable intents, not a second event
truth. The canonical V2 Event Store validates stream revision, causation,
authority, payload digest, and operation replay, then atomically adopts the
corresponding delivery intent. A crash between workflow commit and Event Store
adoption therefore converges by replay without producing a second event.

Collaboration is admitted separately from the session protocol. `Solo` is the
default for routine reversible work. The daemon derives eligible employees,
task risk, reversibility, ambiguity, uncertainty, separation, privacy, packet
and budget policy, dependency-owner handoffs, load, runtime/tool availability,
and authority fences from server-owned state; the caller can submit neither a
roster nor a weaker policy classification or accounting counter. The workflow
store atomically chooses and reserves the smallest capability-complete team
within the accepted tolerance.
Only verified task-specific evidence may influence routing, and learned
weighting remains disabled until calibrated policy evidence activates it. The
full selection, correlation, reservation, retry, routing, and termination
contract is defined in [Collaboration Admission](collaboration-admission.md).

Team leads are accountable for graph health, assignment, blockers, and
completion evidence. Project Management is accountable for agreement
alignment and cross-team dependencies. Gaia may surface deadlocks and
recommend escalation but cannot resolve them outside the authority model.
An admitted participant may report bounded progress, a blocker, or an
escalation need. Only the exact work owner or governed Project Management or
Technical Lead authority may complete or cancel the collaboration admission;
being invited into a temporary team never grants authority over the work item.

## Agent Workbench Contract

The workbench is the only production path for tool-bearing agent work.
The implementation and recovery boundaries are detailed in [Agent Workbench](agent-workbench.md).

A versioned request binds:

- invocation, project, work-item, agent, and workspace IDs;
- authenticated caller and assignment version;
- requested tool and capability set;
- canonical input references and digest;
- output artifact kinds;
- deadline, resource profile, and attempt;
- selected runtime and tool-profile versions.

M0 tool classes are bounded:

- inspect approved project inputs;
- create or update files under the assigned workspace;
- apply a validated patch;
- execute an allowlisted command from the pinned tool profile;
- run profile-defined tests and browser checks;
- commit immutable artifacts and manifests.

The daemon validates authorization before dispatch. The runtime validates request binding and workspace policy before effects. The daemon validates outputs and assignment freshness before accepting artifacts.

Stdout/stderr are bounded and redacted. Full private logs may live in protected runtime storage; public events carry structured summaries and safe references.

### Productive workflow service

The daemon exposes the bounded workflow through separate authenticated customer and agent command surfaces. Caller-supplied display names, roles, tenant IDs, assignment IDs, and authority digests are never credentials. A root-owned principal binding maps independent systemd credentials to one tenant, principal kind, company role, and authority generation; the daemon derives the authority digest from the credential bytes and rejects missing, aliased, mutable, or ambiguous bindings.

The productive service composes four explicit ports:

1. organization authority from the current durable project, assignment, hierarchy, profile, and agent capability set;
2. work execution through the exact #694 Workbench invocation and bwrap NanoRuntime handle;
3. completion evidence from the terminal, digest-bound Workbench record and immutable artifact manifests;
4. independent gate evidence from the #696 QA and delivery composition.

Readiness is fail closed until every port is ready, the workflow store can be read, and at least one complete recovery scan has succeeded. A successful empty scan is not sufficient while a required port is unavailable. Recovery uses stable plan, invocation, operation, and evidence identities: it probes or resumes existing work and never creates a second tool effect merely because the daemon restarted or a response timed out.

The workflow store is the command and recovery authority for customer requests, agreements, projects, work graphs, assignments, decisions, blockers, execution linkage, and company projections. Workbench redb remains the authority for tool execution and artifacts; #696 remains the authority for QA, promotion, release, and delivery. These stores exchange sealed identifiers and digests rather than sharing mutable ownership.

During shutdown the workflow reconciler stops accepting another batch and is joined before the ECS runtime is torn down. If it cannot quiesce within the bounded shutdown window, shutdown is reported degraded rather than claiming a clean workflow stop.

## Runtime and Security Contract

The production daemon selects runtimes through `NanoRuntimeRegistry`. `web-project-v1` selects bwrap for tool-bearing work. Secure-runtime unavailability is a typed failure, never permission to run on the host or fall back to ECS-only.

Each work item receives a contained workspace with:

- read-only declared inputs;
- writable project/work-item output area;
- no visibility into another project or agent workspace;
- no host root, service credentials, SSH material, provider tokens, or protected Sentinel data;
- path traversal and symlink escape prevention;
- cgroup CPU, memory, process, and time limits;
- Landlock and Linux capability restrictions;
- default-deny network isolation from #75;
- a minimal environment allowlist;
- complete process-tree cancellation and cleanup.

M0 has no arbitrary dependency installation. Tool binaries and immutable inputs are provisioned through the versioned release/runtime profile and content-addressed storage. A later profile may add a brokered dependency-fetch capability with digest, policy, cache, and provenance checks.

Security decisions are rechecked at every effect boundary. Cached authorization may accelerate reads but is not authority after assignment, project, profile, or credential generation changes.

## Artifact and Release Contract

Artifact bytes move once into content-addressed storage; references and manifests move through the workflow. Consumers resolve by digest and verify content before use.

The M0 artifact chain is:

```text
customer brief -> agreement -> project plan -> design specification
-> source tree -> QA report -> release manifest -> delivery receipt
-> acceptance record -> closeout memory
```

QA cannot alter candidate artifacts. Developer and QA roles are separated. Every QA result binds candidate digest, check version, runtime/toolchain profile, actor, timing, outcome, and evidence reference.

Release promotion requires:

- the current agreement and project generation;
- all required work outputs;
- all required checks passed against one candidate digest;
- independent approvals;
- complete provenance and cost record;
- an available rollback reference.

Rollback activates a prior approved manifest atomically. It never deletes the failed release, evidence, or audit reason.

Customer acceptance is explicit and immutable. Requested changes create a new linked generation.

## Cortex, Gaia, NMDA, and Night-Run Integration

Cortex Gateway is the provider boundary. Internal callers use explicit roles and versioned wire contracts. The gateway resolves provider/model policy, enforces inventory and spend gates, records effective model/tier/usage/cost, and never trusts public client claims for internal authority.

Gaia observes company state and may diagnose, recommend, or escalate. Gaia is not an alternate workflow administrator and cannot impersonate customer, QA, Release Management, or budget authority.

NMDA and the existing memory paths receive source-linked closeout facts only after the corresponding authoritative records commit. Derived memory includes provenance, valid time, transaction time, and source digest where supported.

Night-Run and background agents may consolidate, index, or derive insights from accepted project outcomes. Their outputs remain derived knowledge. They cannot directly rewrite agreements, work state, releases, delivery, or acceptance; any proposed change returns through the authenticated command path.

Restart or reprocessing must not turn a derived memory event into a duplicate provider action or authoritative transition.

## Idempotency and Recovery

Every mutating command carries a stable request ID and canonical request digest. Durable reservations occur before non-repeatable effects. A request is complete only after its authoritative outcome and outbox/event linkage commit.

The recovery pattern is:

1. Reserve request and digest.
2. Persist intent/state transition permitted before the effect.
3. Execute or probe the effect.
4. Persist structured outcome.
5. Publish through the durable outbox.
6. Advance dependent aggregate state by compare-and-set.

Crash after an effect but before outcome commit uses an outcome probe or provider idempotency contract; it must not blindly repeat an external action. Replays with the same digest return or converge on the durable outcome. Replays with another digest fail as conflicts.

Recovery is tested at request acceptance, agreement creation, project creation, assignment, invocation, artifact commit, QA, release promotion, delivery, customer acceptance, rollback, and memory closeout boundaries.

## Cost and Resource Control

M0 defaults to token-free providers and deterministic fixtures. One real-provider leg is allowed only with explicit maintainer approval and a hard spend ceiling recorded in #650 evidence.

Cost is checked:

- before project admission;
- before each billable provider action;
- when retrying or changing models;
- before accepting scope changes.

Usage records bind project, work item, request, caller role, provider, requested and effective model/tier, input/output units, price source, and cost. Concurrent actions share one atomic project budget. Budget exhaustion creates a typed blocker requiring authorized resolution.

Resource accounting records workbench CPU time, peak memory, process count, bytes read/written, artifact bytes, and duration. The 1 Hz simulation tick remains stable under the declared M0 load.

## Observability and Evidence

The system emits structured state and metrics for:

- customer request and agreement status;
- project/work-item graph and blockers;
- assignments, decisions, handoffs, and approvals;
- workbench invocation/runtime/tool/outcome/resource use;
- artifacts and provenance;
- QA, release, delivery, acceptance, rollback, and closeout;
- provider usage and cost;
- service readiness, restart counters, drift, and queue/outbox health.

Console views are read models, not authority. Every displayed critical state must match an authenticated API and durable event/artifact source. Screenshots alone are insufficient.

Public evidence records exact commands or requests, relevant bounded output, timestamp, target scope, IDs/digests, and result. Credentials, prompts, customer-private content, source artifacts, private infrastructure, and secrets are redacted.

Runtime benchmarks are measured only on the canonical deployment VM. The Rust build server is for compilation and tests, never product-performance evidence.

## Acceptance and Rollout

Delivery order is:

1. #693 contract, profile, matrix, validator, and CI gate.
2. #75 network isolation and #472 runtime-selection foundations.
3. #694 agent workbench and isolated execution.
4. #695 customer/project/company workflow.
5. #696 QA, release, delivery, Console, and memory closeout.
6. #650 final `.240` deployment and product acceptance.

Before every runtime-changing deployment, create an issue-specific `.240` VM snapshot and record rollback metadata. Delete only that snapshot after the issue passes all live criteria. A failed live criterion restores or safely leaves the snapshot available and keeps the issue open.

#650 creates the final pre-deploy snapshot, deploys the exact merged release, executes the full positive and negative journey, performs restart injection and rollback rehearsal, completes at least 600 ticks and a 60-minute service window, reconciles all durable views, and deletes the issue-specific snapshot only after success.

The final label is `Single-Node Product Ready`. It is not `Cluster GA`, `N-node HA`, `single-survivor`, or `native host`.

## Future Profiles

Later profiles may add software projects with dependency brokerage, data-analysis work, sales operations, longer-running campaigns, external deployment targets, and cross-node execution. They reuse the same authority, workbench, artifact, idempotency, cost, and evidence contracts while adding profile-specific tools and gates.

Adding a profile requires a versioned configuration, threat model, tool/runtime declaration, artifact chain, quality gates, automated tests, target-runtime acceptance, and matrix entries. A new profile cannot weaken an existing security or authority contract silently.
