# Adaptive Work Recovery

## Product Boundary

An adaptive `Blocked` cursor is a retained model decision. Project leadership
may explicitly resolve that blocker after the stated dependency or constraint
has been addressed. This is a new, typed, authorized decision, not a rewrite of
the prior model result, an operator override, or automatic provider replay.
`reason_ref` identifies the leadership rationale or evidence; accepting it does
not claim that the referenced evidence was independently verified by this API.

The handler changes only the adaptive journal through its workflow store API
and appends an authoritative audit event through the EventStore V2 gateway.
It does not invoke a provider or tool, mint or renew an allowance, edit a
productive execution, change assignments, alter company governance, or mark
customer work accepted. Grant, call counters, observations, model-result digest,
and previously claimed effect IDs remain intact.

Runtime target for this implementation lane: `NONE`. Deploy, provider, VM,
GitHub, and Rust validation actions belong to ORC, not this lane.

## Protected API

`POST /agent/workflow/adaptive-recovery` uses the authenticated workflow handler:

```rust
pub(super) fn resolve_blocked_adaptive_work(
    &self,
    principal: &BoundPrincipal,
    body: &[u8],
) -> WorkflowHttpResponse
```

The route must use the existing authenticated-principal path, enabled-workflow
guard, request-body limit, and exact method/path matching. It must not admit a
caller-supplied principal, tenant, role, agent, assignment, or runtime snapshot.
Unknown DTO fields are rejected.

```json
{
  "schema_version": 1,
  "operation_id": "eb989bf5-9dce-41c7-8380-a5fbc58ace85",
  "project_id": "project-example",
  "work_item_id": "build-site",
  "session_id": "f25b6675-abef-46f1-a3ab-b64924c6a0dd",
  "expected_session_version": 3,
  "expected_reason_code": "dependency_unavailable",
  "reason_ref": "decision:dependency-restored"
}
```

Both UUIDs must be nonnil; the session version is positive and incrementable.
Project/work IDs use existing workflow validation. The reason code matches
the model's bounded identifier grammar: 1-64 ASCII lowercase letters, digits,
or underscores. `reason_ref` is nonblank, at most 4096 UTF-8 bytes, and contains
no control characters. The existing 256 KiB body limit also applies.

Only a currently bound authenticated `Agent` with role `ProjectManager` or
`TechnicalLead` may act. The handler rechecks the complete registered principal
and execution authority, loads the project under that principal's tenant, and
requires the exact governed participant (agent, principal ID, and role).
Customer, Operator, nonleader, ungoverned leader, forged identity, and another
tenant's project are rejected before any audit append or journal mutation.

The exclusive `mutation_fence.write()` spans project/assignment/session reads,
audit append, and transition. The assignee comes only from the single current
active company assignment. `CompanyAuthority::snapshot` revalidates serving
work state, current assignee credentials, profile, capability admission,
governance policy, and runtime health. The exact snapshot must own the journal's
current head, which must have the requested session UUID. A separate nonterminal
productive execution requires its own reconciliation and is rejected.

A new decision requires the exact version and `Blocked` reason. Every other
cursor, including model/tool pending and unknown outcomes, is rejected. Pending
or unknown effects cannot be converted into a leadership-resolved blocker.

Success is HTTP 200 with `schema_version`, `operation_id`, `session_id`,
`session_version`, `resolution_event_id`, and `replay`. Invalid DTOs use 400,
oversized bodies 413, authority denials 403, stale/binding/state conflicts 409,
and unavailable audit dependencies 503. Workflow store errors retain the
existing `workflow_error` mapping. Cross-tenant missing projects use a generic
409 response without looking up the other tenant.

## Durable Decision And Retry

The event type is `adaptive_work_blocked_resolution_authorized`, schema 1,
producer `sentinel-daemon-adaptive-recovery`. The typed payload binds the entire
request, authenticated leadership principal and credential-derived authority,
assignment ID, and exact assignee `RuntimeAuthoritySnapshotV1`. That snapshot
binds tenant, project, work, agent, assignment version/digest, organization,
profile, policy, credentials, capabilities, and runtime generation/digest.

The V2 registry requires authoritative durability, canonical JSON, an allowed
producer, and typed payload validation. The event UUID is domain-separated and
deterministically derived from the operation UUID. Requested IDs must be UUIDv7;
the deterministic namespace uses a fixed epoch-zero timestamp and 74 hash bits,
not a claimed decision time. Ordering and actual append time come from the
store-sealed global position and receipt. It contains no secret credential or
model thought. The store seals the envelope and receipt.

A dedicated `adaptive-recovery:<session UUID>` workflow reference and exact
assignee authority isolate the event stream. `ExpectedStreamRevision::NoStream`
allows one decision per session authority, including the event/transition crash
gap. The scope is independent of the leadership actor, so competing leadership
operations cannot both claim the same session. Changed content for an existing
operation UUID conflicts, including changed rationale, reason, version,
leadership identity, tenant, work, or assignment authority. Credential rotation
may resume the original decision only for the same currently authorized leader
with a non-regressing authority generation and identical decision/assignee
bindings. The original sealed event retains its historical credential binding.

Sequence:

1. Authenticate current leadership and exact assignee/head authority.
2. Read any prior sealed event by its deterministic UUID. Validate the sealed
   event against its recorded typed proposal and compare the complete request,
   assignment and assignee authority with current state. Require the same
   leadership identity and a non-regressing generation. Reuse the historical
   proposal on replay; new decisions bind the current credentials.
3. For a new decision, or a prior event whose transition is still missing,
   recheck exact blocked version/reason before appending/replaying the event.
   A completed retry requires the exact next version and `BlockedResolved`
   cursor; a prior event never permits replay from pending/unknown state.
4. Call the existing atomic `advance_adaptive_session` with a domain-separated
   operation UUID derived from session, caller operation and expected version,
   exact assignee snapshot, and
   `ResolveBlocked { expected_reason_code, resolution_event_id }`.
5. Return the committed resolution version and event UUID. The store checks
   current authorization before its atomic operation replay path.

A crash before append leaves no decision. A crash after append but before the
transition leaves the same durable decision available to an exact authenticated
retry. A crash after transition recovers the same journal operation, event UUID,
and resolution version; `replay` is then true. There is no startup scan that
blindly applies an old leadership decision. Recovery requires an exact retry
under current authority. Revoked credentials, changed leadership identity,
regressed generations, health, assignment, governance, or replacement of the
session head fail closed, including on retries. An authenticated credential
rotation of the same leader can finish a prior decision across the crash gap
without a new model call or alteration of the historical event.

## One-Time Admission Repair Review

The operator admission-repair path addresses a different failure: a bounded
leadership review was retired before provider dispatch while an admission defect
was present. It authorizes one exceptional additional leadership review for the
original tenant and session. It is not a refunded ordinary review, an automatic
retry, or a model decision. The separate permanent slot remains consumed after
expiry, release changes, restarts and exact request replay. Existing recovery
epochs and ordinary review receipts remain unchanged.

The authenticated operator route is
`/operator/workflow/adaptive-review-epochs`. Preparation uses
`GET ?mode=admission_repair_source&project_id=<id>&session_id=<uuid>` to read the
server-validated mixed review history and candidate fingerprints. This read
requires no repair attestation and issues no authority. It does not assert that
an absent completion row, zero attempt count or missing dispatch proves that a
provider was never contacted.

A protected server-side attestation must bind the complete source and inventory,
the exact subset of eligible retired reviews, their historical failed release,
and the currently installed repaired daemon and Gateway. Historical deployment
and admission-ordering evidence is a trusted producer assertion backed by
protected, digest-bound records; it is not a fabricated provider receipt.
Unknown or dispatched effects retain their existing disposition requirements.
Evidence for one failed release cannot be applied to reviews from another.

With valid evidence, `GET ?mode=admission_repair` returns a draft. An explicit
authenticated POST of that exact schema-3 request issues a schema-4 review with
a version-2 recovery binding. The store revalidates current source and proof
before its first write. Exact historical replay returns the immutable issuance
receipt without creating another slot or extending its lifetime.

Issuance preserves the original session, tools and model counters, root limits,
active ceilings, continuation windows, assignment and customer task. Only a real
subsequent leadership model decision can authorize a continuation inside the
remaining root allowance. Defer, expiry, denial and unknown outcomes do not
silently refill any budget. Gateway schema selection follows the authoritative
review grant; caller metadata cannot upgrade an ordinary review to this mode.

For example, when ordinary reviews are exhausted after an admission defect,
the operator may establish the exact failed-release evidence and issue the one
exceptional review. Leadership can then decide to continue the same work within
its remaining allowance or to defer it. Neither preparation nor issuance
implements the task or claims customer acceptance.

## ORC Integration Contract

`workflow_api.rs` declares `mod adaptive_recovery;`, an `ADAPTIVE_RECOVERY_PATH`
constant, the workflow path classification and authenticated POST dispatch.
No governance helper visibility change is needed:
the child module can use parent `governed_project_participant` and
`CompanyAuthority::snapshot`, and project readback validates governance.

The core lane must supply this variant:

```rust
AdaptiveTransitionV1::ResolveBlocked {
    expected_reason_code: String,
    resolution_event_id: String,
}
```

It must require an exact matching `Blocked` reason and nonnil event UUID, retain
that reason/event in a `BlockedResolved` cursor (serialized kind
`blocked_resolved`), increment version once, and preserve all other journal
evidence and provider-grant bounds. It must not allow pending/unknown effects to
resolve, or dispatch an effect from the resolved cursor. The separate core lane
owns expired-grant rollover; leadership resolution alone is not new provider
authority. Queue selection and all exhaustive cursor matches must be integrated
there by ORC. Replacing a head intentionally ends this handler's exact replay
availability for the old session; the sealed historical decision remains.

## Verification

Inline tests reuse `model_work::configured_adaptive_test_api` and
`configured_test_api` under `cfg(all(test, feature = "llm"))`. They cover exact
retry and restart, event-before-transition crash recovery, competing decisions
in that gap, preserved grant/counters/observations/effect IDs/business project,
customer/operator/nonleader/ungoverned leader, forged and cross-tenant identity,
stale version/session/work/project/reason, all four pending/unknown effect
cursors, changed operation content, bounded DTO/raw identity rejection, missing
EventStore, unavailable assignee runtime authority, and changed assignment or
rotated leadership credentials after a durable decision.

These Rust tests have not been run in this lane. ORC must wire both lanes and
queue focused daemon tests, formatting, and compilation through its authorized
Rust queue, then perform any required product acceptance separately. Source
inspection and local static checks are not live customer acceptance evidence.
