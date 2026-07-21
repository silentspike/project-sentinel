# Company workflow core

Issue #695 introduces the dependency-independent `sentinel-workflow` core for
the bounded `web-project-v1` company profile. The core is authoritative for
customer intent, commercial agreement, project governance, work planning,
assignment policy, collaboration records, cost admission, and progress read
models. It does not execute tools. Tool execution remains behind the narrow
`WorkExecutionPort` boundary until #694 is available.

## Authority and durability

Every command carries an authenticated actor and a caller-supplied operation
ID. The engine derives a canonical SHA-256 digest from the actor and command.
Reusing an operation ID with the same digest returns the original response;
reusing it with another digest fails closed. Aggregate writes, events,
operation records, projections, and execution-outbox reservations use one
SQLite `BEGIN IMMEDIATE` transaction with WAL and `synchronous=FULL`.

The append-only event stream records the schema version, actor, role,
operation ID and digest, aggregate, before/after state, reason, payload, and
timestamp. Project projections contain the accepted agreement, participants,
work graph, assignment policy snapshots, completion evidence, rooms,
decisions, handoffs, blockers, approvals, action items, unresolved questions,
spend, progress, and the last projected event sequence.

All public schemas carry `schema_version = 1`. IDs are durable UUIDv7-based
opaque strings, except the existing validated `AgentId` type.

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
ceiling, immutable provider ceilings, and expiry. Acceptance after expiry,
acceptance with another digest, and cancellation after acceptance are illegal.

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

No tick, chat message, active-agent flag, or provider text can advance these
states. Reassignment revokes the prior durable Assignment and increments the
assignment version. A Project Manager cannot self-assign. A Technical Lead can
assign only a declared direct report. The immutable Assignment record contains
the exact role, capability, hierarchy, active-state, and workload snapshot used
by the policy decision. Cross-project actors and assignees fail closed.

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
stable invocation ID, request digest inputs, assignment version, capabilities,
and deadline. Implementations must be idempotent by invocation ID. The core
uses a deterministic fake in tests and the production daemon deliberately uses
`UnavailableExecutionPort` until #694 supplies the adapter.

Claim and outbox enqueue are atomic. A restart before dispatch recovers the
same request. A receipt is committed with the `claimed -> in_progress`
transition. Dependency failure leaves the request pending, but retries are
explicitly bounded to three attempts. Exhaustion moves the outbox record to
`failed` and creates a durable operator-resolvable blocker; there is no busy
loop or unconditional provider call per tick. Resolving that exact blocker
explicitly re-arms the same durable invocation with its original digest and a
fresh three-attempt budget.

## HTTP surface

The daemon mounts bounded JSON endpoints on its existing Operator API listener:

| Method and path | Authentication | Purpose |
| --- | --- | --- |
| `POST /customer/workflow/commands` | customer key plus customer identity | customer command envelope |
| `GET /customer/workflow/requests?request_id=...` | same customer identity | tenant-isolated request status |
| `POST /operator/workflow/commands` | existing operator authentication | internal command envelope |
| `GET /operator/workflow/projects?project_id=...` | operator | project aggregate |
| `GET /operator/workflow/work-items?work_item_id=...` | operator | work item aggregate |
| `GET /operator/workflow/projections?project_id=...` | operator | complete project read model |
| `GET /operator/workflow/events?after=...&limit=...` | operator | bounded event stream (maximum 1,000) |

Command bodies are limited to 256 KiB. Customer authentication is fail-closed
when `SENTINEL_CUSTOMER_API_KEY` is absent. Customer identity must match both
headers and command actor. Operator calls cannot claim a customer actor.

## Verification and deferred boundaries

The dependency-independent tests cover schema serialization through durable
round trips, legal and illegal transitions, stale versions and digests,
proposal expiry, duplicate operations, transaction rollback, DAG cycles,
capability/hierarchy policy, completion gates, collaboration records, provider
and project ceilings, outbox restart, bounded retry, event ordering, projection
recovery, tenant isolation, and chat-only rejection.

AC-6 is only partially satisfied: claim, durable reservation, deterministic
port dispatch, completion evidence, and tick-independent state transitions are
implemented, but the real #694 Workbench adapter remains open. AC-12 and all
runtime benchmarks remain open until #472 -> #701 -> #694 is merged and an
issue-specific `.240` snapshot and live run are explicitly authorized. No
deployment, VM access, real provider call, or runtime benchmark is evidence for
this core change.
