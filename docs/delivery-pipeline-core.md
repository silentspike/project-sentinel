# Delivery pipeline core

Issue: #696

Status: dependency-independent core. Productive adapters, authenticated APIs,
single-node deployment, browser acceptance, memory publication, and live
benchmarks remain gated on the explicitly named dependencies and later ORC
authorization.

## 1. Purpose and authority boundary

The delivery core turns an exact workflow candidate into independently evaluated
QA evidence, a release, a bounded customer delivery, explicit customer feedback,
an acceptance or rework outcome, rollback history, and closeout. It does not
infer customer acceptance from internal approval, trust an agent's completion
claim as QA evidence, or let one principal span implementation, QA, release, and
customer authority.

Every durable record is tenant- and project-scoped, versioned, generation-bound,
and digest-bound. A caller supplies an authenticated `PrincipalV1`; the
productive API adapter must derive it from its authenticated server-side
principal. The core never accepts a free-form role string as authority.

The dependency-independent module lives under
`services/sentinel-daemon/src/delivery`. It deliberately does not modify the
existing workflow, runtime, common event, event-store, projection, or operator
API ownership surfaces.

## 2. Runtime and rollout contract

- Runtime target class: `SINGLE_NODE`.
- Deploy targets in this phase: none.
- Read-only runtime targets in this phase: none.
- Forbidden targets in this phase: every VM, Proxmox host, and cluster node.
- Live benchmark target in this phase: none.
- Rollback in this phase: revert the delivery-core commit.
- Productive runtime activation: blocked until the #694 and #695 adapters are
  integrated, ORC grants the integrated phase, and an issue-specific snapshot
  and rollback plan exist.

The daemon library remains startable when productive integration is absent.
`UnavailableDeliveryIntegration` reports typed unavailable readiness and rejects
candidate registration or external QA execution before any delivery aggregate is
created. Local store health and readback remain usable.

## 3. Canonical data contract

`DELIVERY_SCHEMA_V1` identifies the first wire and persistence schema. A
`ContentDigest` is a lowercase SHA-256 value over recursively key-sorted JSON.
Digest fields are cleared before a record computes its own digest.

The core defines the following record families:

| Family | Primary binding |
| --- | --- |
| `VersionedRefV1` | stable ID, generation, digest |
| `PrincipalV1` | tenant, principal, authority generation, server-derived roles |
| `ReleaseCandidateV1` | agreement, project, work-item set, source, artifacts, toolchain, runtime profile, acceptance criteria, implementers, cost |
| `QaEvaluationPlanV1` | exact candidate and workflow refs, required/optional cases, fixtures, evaluator/aggregation/release policies, runner/toolchain/sandbox/capabilities/environment/credentials, seeds and retry policy |
| `QaDatasetManifestV1` | dataset generation and provenance, fixtures, snapshots, licenses, source, classification, encryption/access/redaction/retention/audit controls |
| `QaEvaluationRunReceiptV1` | exact plan, stable request, actors, durable event generation, attempts, outcomes and cleanup |
| `QaCaseResultV1` | case and evidence generations, assertion and observed-output digests, artifact refs, failure class, timing, provenance and cleanup |
| `QaDeterministicEvidenceV1` | byte-stable assertion subset and evidence digest |
| `QaModelEvaluationV1` | provider/model/request/prompt/rubric/score/cost variance without a reproducibility claim |
| `QaFlakeRecordV1` | append-only attempt refs, deterministic/model split, disposition authority and expiry |
| `QaReleaseGateReceiptV1` | exact plan/candidate/evidence set, evaluator authority, policy, validity and future manifest input digest |
| `ReleaseManifestV1` | agreement, project, work items, exact candidate, artifacts, QA gate, source/toolchain/runtime, release actor, cost and rollback reference |
| `ReleaseV1` | immutable manifest reference and release lifecycle |
| `DeliveryReceiptV1` | exact release, customer, bounded preview, expiry and receipt digest |
| `CustomerFeedbackV1` | authenticated customer action, exact delivery and linked rework items |
| `AcceptanceV1` | explicit customer, delivery and release binding |
| `RollbackV1` | exact source/target releases, reason, actor and effect receipt |
| `ProjectCloseoutV1` | exact acceptance/release, source lineage and authoritative memory publication receipt |

Only the evaluation-plan digest and explicitly deterministic evidence subsets are
byte-stability gates. Model identity, request, cost, timing, attempts, and
probabilistic results remain visible operational evidence and are never relabeled
as deterministic.

## 4. Legal lifecycle

### 4.1 Candidate

```text
Draft -> QaAssigned -> QaRunning -> GatePassed -> Promoted
                              \-> GateFailed
```

There is no `Draft -> Promoted` shortcut. A candidate cannot be promoted without
an exact currentness snapshot, completed QA plan, valid independent gate, and
canonical manifest.

### 4.2 QA run

```text
Planned -> Admitted -> Running -> CompletedPass
                              |-> CompletedFail
                              |-> NeedsHumanReview
                              |-> HarnessError
                              |-> Cancelled
                              |-> Quarantined
non-terminal run -> Superseded
```

Terminal runs do not reopen. A retry is a new record linked through `retry_of`
and `supersedes`; it does not erase earlier attempts or reduce required cases.

### 4.3 Release

```text
Approved -> Active -> Superseded
                   \-> RolledBack
Superseded -> Active
```

Only one release ID is active in an aggregate. Promotion supersedes the previous
active release in the same atomic redb commit. Rollback marks the failed active
release rolled back and reactivates an exact prior release in one commit.

### 4.4 Delivery and customer action

```text
PreviewReady -> Delivered -> Accepted
                         |-> Rejected
                         \-> ChangesRequested
PreviewReady/Delivered -> Expired
```

Acceptance is never inferred. A request for changes must contain authoritative
linked work-item references, which the later #695 adapter must create before the
customer action commits. Existing candidates, releases, evidence, and customer
history remain immutable.

## 5. Separation of duties

The policy is fail-closed:

1. The candidate records all implementer principal IDs.
2. A release manager assigns exactly one QA principal for a run.
3. The release manager cannot assign itself as QA.
4. The QA principal cannot be any candidate implementer.
5. The release manager cannot be any candidate implementer.
6. Only the assigned QA principal can transition and execute the run.
7. A QA gate cannot be issued by an implementer.
8. The release manager cannot issue the QA gate used for its own promotion.
9. Only the delivery's authenticated customer can accept, reject, or request
   changes before expiry.
10. Customer acceptance does not confer release or workflow authority.

Tenant checks precede mutation. Idempotency keys are namespaced by tenant,
principal, command kind, and caller key, preventing cross-principal and
cross-tenant replay.

## 6. Durable store and event publication

`DeliveryStore` is a dedicated redb file owned by the delivery module. It uses
five tables:

| Table | Key and purpose |
| --- | --- |
| `delivery_meta` | schema version |
| `delivery_aggregates` | `tenant:project` aggregate snapshot |
| `delivery_journal` | `tenant:project:revision` append-only operation event |
| `delivery_idempotency` | `tenant:principal:command:key` request/receipt binding |
| `delivery_outbox` | event-digest keyed canonical publication request and receipt |

One redb write transaction checks the expected aggregate revision and writes the
new aggregate, journal entry, idempotency record, and outbox row. A duplicate
request with the same command digest receives the original operation receipt
with `duplicate=true`. Reusing a key with different content is a typed conflict.

The outbox uses a stable namespaced operation ID:

```text
delivery:<tenant>:<project>:<revision>:<event-type>
```

The publication request binds operation ID, topic, event type, canonical payload,
and payload digest. The publisher returns operation ID, event ID, row identity,
and payload digest. The delivery store marks only an exactly matching receipt
published. A crash before marking leaves the row pending; the downstream
publisher must atomically insert or read back the same digest-bound event. A
wrong receipt never advances the local outbox.

The current module proves its local side of this contract with a deterministic
fake. Connecting it to the repository's existing event/CQRS chain requires the
later owned adapter and cannot be claimed by this phase.

## 7. Narrow integration port

`DeliveryIntegrationPort` exposes only:

- readiness with contract version, authority generation, and contract digest;
- a read-only workflow authority/currentness snapshot; and
- execution of one stable QA workbench evidence request.

The authority snapshot binds agreement, project, work-item digest, current
candidate generation/digest, participant principals, and snapshot digest.
Promotion resolves it again so a stale candidate cannot pass on old evidence.

The QA request is called outside any redb writer transaction. It binds the
tenant, project, candidate, QA plan, and stable request digest. The receipt is
opaque evidence from the future #694 authority and binds invocation, assignment,
input/output, artifact ownership and cleanup.

The productive adapter must deduplicate a request digest across process restart.
If a crash occurs after an external result but before the local commit, replaying
the same stable request must return the same effective receipt without rerunning
the effect. The deterministic fake validates this boundary; productive
exactly-once evidence remains gated on #694.

## 8. QA addendum and negative contract

The #717 addendum is represented in the canonical schemas. Productive evaluators
must additionally enforce:

- required case inventory cannot shrink;
- deterministic failure cannot be overridden by model scoring or a correlated
  majority;
- active-scenario lanes require explicit capabilities and cleanup/quarantine;
- candidate data is untrusted input, not policy or instructions;
- unknown provider/model identity is never called byte-reproducible;
- unresolved flakes and human-review results cannot issue a passing release
  gate;
- attempts and dispositions are append-only;
- retention/pruning cannot pass the required effect frontier;
- restored generations must match before readiness.

This core stores those bindings but intentionally does not implement a test
runner, provider, sandbox, retention job, or #709/#710 effect engine.

## 9. Console lineage surface

`DeliveryView` is a disjoint component that renders an injected
`DeliveryLineageSnapshot`. It is not wired to a productive route in this phase
because the authenticated API/projection seam is outside the owned scope.

Before rendering records, the component validates:

- schema version;
- authority scope and monotonic revision shape;
- unique non-empty node IDs;
- valid SHA-256 digests;
- positive generations;
- non-empty actor roles; and
- finite non-negative costs when present; and
- non-dangling lineage edges.

Invalid snapshots fail closed instead of rendering partial authority data. The
public view redacts the tenant identifier, credential-shaped values, internal
addresses, and local filesystem paths. It shows only shortened digests and
bounded per-stage cost values. It does not accept prompt text, credentials, raw
private artifacts, or internal infrastructure identifiers.

## 10. Recovery, backup, and retention

The #722 whole-product contract applies. The delivery redb store, journal,
idempotency records, outbox frontier, published receipts, QA evidence
generations, manifests, releases, deliveries, acceptances, rollback history, and
closeouts are one recovery participant.

Before productive activation, recovery integration must:

1. fence all delivery writers;
2. flush and record the redb generation and outbox frontier;
3. bind the delivery participant into the immutable local recovery envelope;
4. restore the file before projection/readiness;
5. verify schema, journal continuity, aggregate revisions, digest references,
   outbox receipts, and external evidence generations;
6. keep normal admission fenced while any participant is missing or disagrees;
7. admit only validation-only reads before the durable release barrier;
8. enable normal writers only after every participant accepts the same durable
   release and a final readiness CAS commits.

Corrupt or unsupported schema data is fail-closed. Retention cannot delete
evidence referenced by an active/recoverable release, delivery, acceptance,
rollback, closeout, unpublished outbox row, or recovery frontier.

## 11. Verification matrix

| Criterion | Dependency-independent status |
| --- | --- |
| AC-1 schemas and legal transitions | Implemented and testable |
| AC-2 independent authority | Implemented; live probes deferred |
| AC-3 productive #694 suite | Port and fake only; productive execution deferred |
| AC-4 missing/stale/different gate | Core negative tests cover missing approval, expiry, plan-digest substitution and incomplete evidence; broader runner matrix deferred |
| AC-5 immutable manifest | Implemented; existing event-store adapter deferred |
| AC-6 customer flow | Core records/transitions implemented; authenticated API/browser deferred |
| AC-7 rework history | Linked-work-item precondition implemented; productive #695 creation deferred |
| AC-8 rollback | Atomic, idempotent, restart-safe aggregate transition tested; productive effect rehearsal deferred |
| AC-9 Console lineage | Disjoint validated and redacted component with bounded cost readback implemented; API/live screenshot deferred |
| AC-10 memory closeout | Fail-closed receipt requirement and restart readback tested; productive memory path deferred |
| AC-11 Gaia oversight | No bypass surface added; productive observation deferred |
| AC-12 restart/idempotency | Local store/outbox boundaries covered; productive adapter crash matrix deferred |
| AC-13 `.240` journey | Not authorized in this phase |
| AC-14 canonical QA schemas | Implemented |
| AC-15 deterministic/probabilistic split | Represented in schema; evaluator proof deferred |
| AC-16 sandbox/capability enforcement | Contract fields only; #694 implementation deferred |
| AC-17 retry/flake append-only history | Schema/lifecycle represented; productive evaluator deferred |
| AC-18 #709/#710 effects | Stable request boundary only; integration deferred |
| AC-19 #722 recovery | Participant contract documented; recovery integration deferred |
| AC-20 complete gate negative matrix | Core stale/self/missing checks; full evidence evaluator deferred |

Negative criteria AC-N1 through AC-N4 are enforced by the state machine and
authority boundary. AC-N5 requires later matching API/event/artifact readback.
AC-N6 is enforced for the disjoint Console surface and repository evidence.
AC-N7 remains a release rule: build-server time is never runtime benchmark
evidence. AC-N8 through AC-N12 remain mandatory for the productive evaluator,
effect, retention, and recovery integrations.

## 12. Final integration sequence

1. Merge the productive #694 workbench dispatcher and its opaque,
   request-deduplicated evidence receipt.
2. Merge the #695 workflow authority and linked-work-item/currentness adapter.
3. Add an authenticated API and existing CQRS publication adapter in their
   owners' scopes.
4. Add the delivery store to the approved #722 recovery participant set.
5. Wire the Console component to the public-safe projection.
6. Re-run canonical Rust and Console gates on final main.
7. Obtain ORC code approval and an explicit `.240` runtime authorization.
8. Create and verify the issue-specific snapshot before deployment.
9. Execute the full token-free journey, negative probes, restart matrix,
   rollback rehearsal, browser/API/event/artifact/memory readback, stability
   scan, and approved runtime benchmarks.
10. Preserve the snapshot and stop fail-closed on any failed acceptance
    criterion.

No part of this core authorizes deployment, provider use, customer delivery, or
issue closure by itself.
