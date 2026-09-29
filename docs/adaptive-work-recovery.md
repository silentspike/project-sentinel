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

ORC wires `POST /agent/workflow/adaptive-recovery` to:

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
principal, tenant, work, or assignment authority.

Sequence:

1. Authenticate current leadership and exact assignee/head authority.
2. Read any prior sealed event by its deterministic UUID and compare its
   canonical request digest and typed payload to the freshly bound request.
3. For a new decision, or a prior event whose transition is still missing,
   recheck exact blocked version/reason before appending/replaying the event.
   A completed retry requires the exact next version and `BlockedResolved`
   cursor; a prior event never permits replay from pending/unknown state.
4. Call the existing atomic `advance_adaptive_session` with the request's
   operation UUID and expected version, exact assignee snapshot, and
   `ResolveBlocked { expected_reason_code, resolution_event_id }`.
5. Return the committed resolution version and event UUID. The store checks
   current authorization before its atomic operation replay path.

A crash before append leaves no decision. A crash after append but before the
transition leaves the same durable decision available to an exact authenticated
retry. A crash after transition recovers the same journal operation, event UUID,
and resolution version; `replay` is then true. There is no startup scan that
blindly applies an old leadership decision. Recovery requires an exact retry
under current authority. Changed credentials, health, assignment, governance,
or replacement of the session head fail closed, including on retries.

## ORC Integration Contract

This base does not yet define the transition or resolved cursor. Add
`mod adaptive_recovery;` and an `ADAPTIVE_RECOVERY_PATH` constant in
`workflow_api.rs`, include the path in `is_workflow_path`, and dispatch its
authenticated POST branch in `WorkflowApi::handle` to the method above.
No governance helper visibility change is needed:
the child module can use parent `governed_project_participant` and
`CompanyAuthority::snapshot`, and project readback validates governance.

The core lane must supply this variant:

```rust
AdaptiveTransitionV1::ResolveBlocked {
    expected_reason_code: String,
    resolution_event_id: uuid::Uuid,
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
