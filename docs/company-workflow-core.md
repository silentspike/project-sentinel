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

All newly written public schemas carry `schema_version = 2`. The version 1 to
version 2 migration runs under one SQLite `BEGIN IMMEDIATE` transaction. The
schema-version marker is written last. An injected failure after any destructive
DDL step rolls back the complete transaction, leaves the version 1 image
readable, and permits the same migration to be retried deterministically. IDs
are durable UUIDv7-based opaque strings, except the existing validated
`AgentId` type. Legacy operation IDs become global fail-closed tombstones: their
principal and tenant cannot be proven, so their stored responses are never
replayed into a version 2 namespace.

The workflow is deployment-safe and disabled by default.
`SENTINEL_COMPANY_WORKFLOW_ENABLED=true` is the only value that enables it.
When disabled, the daemon starts without loading workflow deployment files and
all workflow endpoints return the typed `workflow_unavailable` response. When
enabled, the server loads `web-project-v1` from
`SENTINEL_WORK_PROFILE_FILE` or the checked-in default path and loads the
server-owned principal registry. Profile bytes must exactly match the copy
embedded in the release binary. Profile ID, schema version, and SHA-256 digest
are bound into the Proposal digest and copied unchanged into the Agreement and
Project. Unknown IDs, altered bytes, stale digests, missing principals, missing
roles or specialties, missing immutable artifacts, missing quality gates, and
insufficient topology fail closed before the workflow API becomes available.

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
| `in_progress` | assignee requests completion and an opaque independent authority receipt proves the invocation, outputs, ownership, and declared gate | `done` |
| non-terminal | authoritative blocker | `blocked` |
| `blocked` | authorized blocker resolution | `ready` or `proposed` |

No tick, chat message, active-agent flag, caller-supplied profile, or provider
text can advance these states. Reassignment revokes the prior durable
Assignment and increments the assignment version. A Project Manager cannot
self-assign. A Technical Lead can assign only a declared direct report. The
engine resolves the assignee through `OrganizationRuntimePort`; the immutable
Assignment records the exact profile plus the authoritative organization
generation and digest. Claim and dispatch re-read that authority and reject a
generation or digest change before any execution call. If authority drifts
after reservation, dispatch completes that outbox row as `authority_conflict`,
persists a typed conflict outcome, blocks the work item for reassignment, and
continues with independent rows. An authorized resolution deactivates the stale
assignment and returns the item to a recoverable state; it does not re-arm or
busy-loop the stale invocation. Cross-project, cross-tenant, stale-authority,
and unroutable assignments fail closed.

### Collaboration and governance

- Project and team rooms contain only project participants and are references
  for collaboration, not state-mutation channels.
- Decisions are structured records with alternatives, rationale, evidence,
  actor, and optional work-item scope. An optional work-item ID must resolve to
  the same project and authenticated tenant.
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

Agents cannot submit output references or attest their own gates. The completion
command only creates a durable, digest-bound evidence request in transaction 1.
That request fixes its schema, request and operation digest, invocation, project
and project version, work item and work-item version, assignment version,
assignment authority generation and digest, agent, input digest, and replay
domain. The writer transaction commits before `CompletionEvidencePort` is
called.

`CompletionEvidencePort` is the narrow authority boundary for #694. It returns
an opaque result whose production representation and verification mechanism are
owned by the adapter; the core exposes no public constructor, public hash
recipe, or caller-submittable receipt. Transaction 2 re-reads the complete
request, Project, Work Item, Assignment, and authoritative organization
snapshots and compares every version and digest before committing completion.
The receipt must bind the request digest, all aggregate and assignment versions,
authority generations, validity interval, and replay domain. Every artifact
must prove project/work/invocation ownership, and the canonical output-bundle
digest must be covered by an independent QA or Release Manager gate receipt.
The issuer must be an active Project participant in the current authoritative
organization snapshot with the exact receipt-bound generation and digest.
Forged roles, non-participants, revoked or stale authority, caller-computed
hashes, and cross-assignment or cross-version replay fail closed.

The evidence request is restart-recoverable between either transaction and the
external authority call. Timeout and dependency failures retain the pending
request for at most three attempts; exhaustion makes it durably terminal instead
of busy-looping. A crash after receipt acquisition but before transaction 2
causes an idempotent re-query of the authority and the same full CAS check.
Duplicate completion commands cannot create another request or complete twice.

## Event store, projection, backup, and restore

The workflow SQLite file is the authoritative
event/aggregate/operation/execution-outbox/completion-evidence-outbox store.
`workflow_entity_history` is append-only by entity type, ID, and version;
`workflow_entities` is the current-state index. Project projections are
rebuildable read models. A durable projection checkpoint records the source
event high watermark, projected high watermark, project count, and last rebuild
time. Every in-process projection update advances both watermarks in the same
transaction. A backup is refused unless those watermarks agree.

`WorkflowStore::backup_to` creates a transactionally consistent standalone
SQLite image with `VACUUM INTO` and returns a manifest containing schema
version, database SHA-256, event and entity-history watermarks, entity,
operation, execution-outbox, completion-evidence-outbox, and project-projection
counts, and the projection checkpoint.
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
or agent routes. While the workflow is disabled, every workflow route returns
typed `503 workflow_unavailable` and the rest of the daemon remains available.
When enabled and provisioned, mutating routes return typed
`503 dispatcher_not_ready` until the production #694 dispatcher, authoritative
organization adapter, and completion evidence authority all report ready;
read-only authenticated recovery remains available.

## Verification and deferred boundaries

The dependency-independent tests cover schema serialization through durable
round trips, legal and illegal transitions, stale versions and digests,
proposal expiry, duplicate operations, transaction rollback, DAG cycles,
capability/hierarchy policy, organization-generation TOCTOU rejection,
authority-conflict recovery without queue poisoning, two-transaction completion
evidence recovery, opaque authority receipts, forged roles and artifact
ownership, non-participant and revoked-issuer rejection, self-attested gate and
public-hash rejection, assignment/version/replay-domain isolation, collaboration
records, provider and project ceilings, outbox restart, bounded timeout and
retry, crash-atomic schema migration from every destructive-DDL failpoint, event
ordering, projection recovery, verified backup/restore, principal-scoped
idempotency, cross-project and cross-tenant decision denial, authority spoofing,
disabled and fully provisioned daemon startup, and chat-only rejection.

AC-6 is only partially satisfied: claim, durable reservation, deterministic
port dispatch, completion evidence, and tick-independent state transitions are
implemented, but the real #694 Workbench adapter remains open. AC-12 and all
runtime benchmarks remain open until #472 -> #701 -> #694 is merged and an
issue-specific `.240` snapshot and live run are explicitly authorized. No
deployment, VM access, real provider call, or runtime benchmark is evidence for
this core change.
