# Company workflow core

Issue #695 introduces the dependency-independent `sentinel-workflow` core for
the bounded `web-project-v1` company profile. The core is authoritative for
customer intent, commercial agreement, project governance, work planning,
assignment policy, collaboration records, cost admission, and progress read
models. It does not execute tools. Tool execution remains behind the narrow
`WorkExecutionPort` boundary until #694 is available.

## Authority and durability

Command bodies contain only an operation ID and a typed command. Customer,
operator, and agent identity never comes from that body. The daemon resolves a
Bearer credential through the server-owned principal registry and supplies the
principal ID, tenant, principal kind, role, customer ID, and agent ID to the
engine. The registry file contains environment-variable names, not credential
values; those values are loaded and hashed in memory at startup. Missing,
duplicate, short, or structurally inconsistent bindings fail closed.

The engine derives a canonical SHA-256 digest from the authenticated principal
and command. Operation IDs are namespaced by tenant, principal kind, and
principal ID. Reusing an operation ID with the same digest in that namespace
returns the original response; reusing it with another digest fails closed,
while another principal or tenant owns a disjoint namespace. Aggregate writes,
immutable entity revisions, events, operation records, projections, and
execution-outbox reservations use one SQLite `BEGIN IMMEDIATE` transaction
with WAL and `synchronous=FULL`.

The append-only event stream records the schema version, tenant, principal,
legacy audit actor ID, server-derived role, operation ID and digest, aggregate,
before/after state, reason, payload, and timestamp. Event reads are tenant
filtered. Project projections contain the accepted agreement, participants,
work graph, assignment policy snapshots, completion evidence, rooms,
decisions, handoffs, blockers, approvals, action items, unresolved questions,
spend, progress, and the last projected event sequence.

All newly written public schemas carry `schema_version = 2`. The store migrates
version 1 records without reinterpreting their authority fields. IDs are durable
UUIDv7-based opaque strings, except the existing validated `AgentId` type.
Legacy operation IDs become global fail-closed tombstones: their principal and
tenant cannot be proven, so their stored responses are never replayed into a
version 2 namespace.

## Legal transitions

### Customer request

| Current | Command | Actor | Next |
| --- | --- | --- | --- |
| absent | submit | matching authenticated customer | `submitted` |
| `submitted` or `clarifying` | clarify | matching customer or Sales | `clarifying` |
| `submitted` or `clarifying` | qualify | Sales | `qualified` |
| `qualified` | create proposal | Sales | `proposed` |
| `proposed` | accept exact unexpired proposal digest | matching customer | `accepted` |
| `proposed` | reject exact proposal digest | matching customer | `rejected` |
| `submitted`, `clarifying`, `qualified`, or `proposed` | cancel | matching customer | `cancelled` |
| `accepted`, `rejected`, or `cancelled` | record bounded feedback reference | matching customer | unchanged |

Acceptance writes the immutable proposal binding into the Agreement and
creates its Project in the same transaction. The binding includes scope,
deliverables, exclusions, acceptance criteria, assumptions, project cost
ceiling, immutable provider ceilings, expiry, governance profile and policy,
owner, and the complete participant set. `AcceptProposal` contains only the
request/proposal identity, expected version, and proposal digest; it cannot
override internal authority. Acceptance after expiry, acceptance with another
digest, and cancellation after acceptance are illegal.

### Project and work graph

| Current | Command/evidence | Next |
| --- | --- | --- |
| `planned` | valid Work DAG committed once | `active` |
| `active` | authoritative blocker or exhausted budget | `blocked` |
| `blocked` | final authorized blocker resolution | `active` |
| `active` | every work item has passed its declared gate | `delivery_candidate` |

A Work DAG is admitted atomically only when IDs are unique, every dependency
exists, the graph is acyclic, each item has an owner in the project, inputs,
required role and capabilities, required output kinds, a quality gate, and a
positive budget, and the sum of item budgets is within the Agreement ceiling.

| Work item current state | Command/evidence | Next |
| --- | --- | --- |
| `proposed` | every dependency becomes `done` | `ready` |
| `ready` or `assigned` | authorized assignment policy passes | `assigned` |
| `assigned` | current assignee claims with digest and future deadline | `claimed` |
| `claimed` | idempotent execution port accepts durable invocation | `in_progress` |
| `in_progress` | current assignment version, required output digests, and declared gate pass | `done` |
| non-terminal | authoritative blocker | `blocked` |
| `blocked` | authorized blocker resolution | `ready` or `proposed` |

No tick, chat message, active-agent flag, caller-supplied profile, or provider
text can advance these states. Reassignment revokes the prior durable
Assignment and increments the assignment version. A Project Manager cannot
self-assign. A Technical Lead can assign only a declared direct report. The
engine resolves the assignee through `OrganizationRuntimePort`; the immutable
Assignment records the exact profile plus the authoritative organization
generation and digest. Claim and dispatch re-read that authority and reject a
generation or digest change before any execution call. Cross-project,
cross-tenant, stale-authority, and unroutable assignments fail closed.

### Collaboration and governance

- Project and team rooms contain only project participants and are references
  for collaboration, not state-mutation channels.
- Decisions are structured records with alternatives, rationale, evidence,
  actor, and optional work-item scope.
- Action items and unresolved questions have stable owners and explicit
  open/resolved states.
- Handoffs bind producer, consumer, work item, and artifact digests; only the
  designated consumer can acknowledge an offered handoff.
- Blockers are `open -> escalated -> resolved`; resolution requires the role
  fixed on the blocker and a durable resolution reference.
- QA or Release Manager approvals bind a gate and subject digest. The current
  assignee cannot approve its own work.
- Gaia is observational for authoritative cost and blocker decisions.

## Cost admission

Provider ceilings are part of the immutable accepted Proposal; an API caller
cannot supply or raise them during reservation. A reservation is admitted only
when the aggregate provider total, project total, and optional work-item total
remain within their ceilings. A failed admission creates a durable typed
`BudgetExhausted` blocker, moves the project to `blocked`, and requires an
authorized Project Manager resolution. A cost can be committed only against a
prior reservation and never above the reserved amount.

## Execution boundary and recovery

`WorkExecutionPort::reserve` receives a versioned `PendingExecution` with a
stable invocation ID, tenant, request digest inputs, assignment version,
organization generation and digest, capabilities, and deadline.
Implementations must be idempotent by invocation ID. The core uses a
deterministic fake in tests and the production daemon deliberately uses
`UnavailableExecutionPort` and `UnavailableOrganizationRuntimePort` until #694
supplies the production dispatcher and authority adapters.

Claim and outbox enqueue are atomic. A restart before dispatch recovers the
same request. A receipt is committed with the `claimed -> in_progress`
transition. Dependency failure leaves the request pending, but retries are
explicitly bounded to three attempts. Exhaustion moves the outbox record to
`failed` and creates a durable operator-resolvable blocker; there is no busy
loop or unconditional provider call per tick. Resolving that exact blocker
explicitly re-arms the same durable invocation with its original digest and a
fresh three-attempt budget.

## Event store, projection, backup, and restore

The workflow SQLite file is the authoritative event/aggregate/operation/outbox
store. `workflow_entity_history` is append-only by entity type, ID, and version;
`workflow_entities` is the current-state index. Project projections are
rebuildable read models. A durable projection checkpoint records the source
event high watermark, projected high watermark, project count, and last rebuild
time. Every in-process projection update advances both watermarks in the same
transaction. A backup is refused unless those watermarks agree.

`WorkflowStore::backup_to` creates a transactionally consistent standalone
SQLite image with `VACUUM INTO` and returns a manifest containing schema
version, database SHA-256, event and entity-history watermarks, entity,
operation, outbox, and project-projection counts, and the projection checkpoint.
The destination must not exist, and a failed creation or verification removes
the incomplete destination. Restore is an offline operation into another
non-existent path: it verifies the source image, manifest, SQLite integrity,
schema, current-state-to-history linkage, counts, hash, and caught-up projection,
copies and `fsync`s a temporary file, verifies it again, then atomically renames
and `fsync`s the parent directory. It never overwrites a live database.
Projection rebuild clears only derived project read models, regenerates them
from authoritative state, and records the current event watermark.

## HTTP surface

The daemon mounts bounded JSON endpoints on its existing Operator API listener:

| Method and path | Authentication | Purpose |
| --- | --- | --- |
| `POST /customer/workflow/commands` | authenticated customer principal | customer command envelope |
| `GET /customer/workflow/requests?request_id=...` | authenticated customer principal | tenant-isolated request status |
| `POST /operator/workflow/commands` | authenticated operator principal | operator command envelope |
| `POST /agent/workflow/commands` | authenticated agent principal | agent command envelope |
| `GET /operator/workflow/projects?project_id=...` | authenticated operator principal | tenant-isolated project aggregate |
| `GET /operator/workflow/work-items?work_item_id=...` | authenticated operator principal | tenant-isolated work item aggregate |
| `GET /operator/workflow/projections?project_id=...` | authenticated operator principal | tenant-isolated project read model |
| `GET /operator/workflow/events?after=...&limit=...` | authenticated operator principal | tenant-isolated bounded event stream (maximum 1,000) |

Command bodies are limited to 256 KiB and reject unknown fields. The principal
registry path is configured with `SENTINEL_WORKFLOW_PRINCIPALS_FILE`; each
binding points to a credential environment variable and fixes one tenant,
principal kind, role, and subject. No credential or credential digest is
serialized into workflow state. A credential cannot cross customer, operator,
or agent routes. All mutating routes return `503 dispatcher_not_ready` until
both the production #694 dispatcher and authoritative organization adapter
report ready; read-only authenticated recovery remains available.

## Verification and deferred boundaries

The dependency-independent tests cover schema serialization through durable
round trips, legal and illegal transitions, stale versions and digests,
proposal expiry, duplicate operations, transaction rollback, DAG cycles,
capability/hierarchy policy, organization-generation TOCTOU rejection,
completion gates, collaboration records, provider and project ceilings, outbox
restart, bounded retry, event ordering, projection recovery, verified
backup/restore, principal-scoped idempotency, cross-tenant read and mutation
denial, authority spoofing, and chat-only rejection.

AC-6 is only partially satisfied: claim, durable reservation, deterministic
port dispatch, completion evidence, and tick-independent state transitions are
implemented, but the real #694 Workbench adapter remains open. AC-12 and all
runtime benchmarks remain open until #472 -> #701 -> #694 is merged and an
issue-specific `.240` snapshot and live run are explicitly authorized. No
deployment, VM access, real provider call, or runtime benchmark is evidence for
this core change.
