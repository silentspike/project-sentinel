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
and digest-bound. The dependency-independent tests supply `PrincipalV1`, but
productive callers may not: the future authenticated API adapter must derive the
principal and roles server-side, then obtain an opaque current-authority receipt
immediately before every sensitive command. The core rejects receipt, adapter
contract, tenant, role, actor, generation, digest, and validity mismatches.

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
There is no productive `DeliveryCore` constructor or runtime wiring in this
phase. `UnavailableDeliveryIntegration` and `UnavailableDeliveryEffects` report
typed unavailability and reject authority- or effect-dependent commands before
local state adoption. The test-only store can still start and report health.

## 3. Canonical data contract

`DELIVERY_SCHEMA_V1` identifies the first wire and persistence schema. A
`ContentDigest` is a lowercase SHA-256 value over recursively key-sorted JSON,
framed with the explicit record type, schema version, and canonical-byte length.
Persisted and wire structs reject unknown fields; lifecycle outcomes,
case/model/flake classifications, findings, and authority roles are closed
enums. Golden vectors prove that record or schema substitution changes the
digest. Digest fields are cleared before a record computes its own digest.

The core defines the following record families:

| Family | Primary binding |
| --- | --- |
| `VersionedRefV1` | stable ID, generation, digest |
| `PrincipalV1` | tenant, principal, authority generation, server-derived roles |
| `ReleaseCandidateV1` | agreement, project, work-item set, source, artifacts, toolchain, runtime profile, acceptance criteria, implementers, cost |
| `QaEvaluationPlanV1` | exact candidate and workflow refs, required/optional cases, fixtures, evaluator/aggregation/release policies, runner/toolchain/sandbox/capabilities/environment/credentials, seeds and retry policy |
| `QaDatasetManifestV1` | dataset generation and provenance, fixtures, snapshots, licenses, source, classification, encryption/access/redaction/retention/audit controls |
| `QaEvaluationRunReceiptV1` | exact plan, stable request, actors, durable event generation, workbench-attempt summary, digest-bound case-attempt history, outcomes and cleanup |
| `QaCaseAttemptEvidenceV1` | one gapless case attempt number, exact run/case generation, closed outcome/reason, deterministic evidence refs and sealed attempt digest |
| `QaCaseResultV1` | exact immutable attempt history, derived terminal status, union of assertion refs, slices, provenance and flake disposition |
| `QaDeterministicEvidenceV1` | byte-stable assertion subset and evidence digest |
| `QaModelEvaluationV1` | schema scaffold only; every populated model record is typed unavailable until #749 supplies calibration and independent authority |
| `QaFlakeRecordV1` | append-only attempt refs, deterministic/model split, disposition authority and expiry |
| `QaReleaseGateReceiptV1` | exact plan/candidate/evidence set, evaluator authority, policy, validity and future manifest input digest |
| `ReleaseManifestV1` | agreement, project, work items, exact candidate, artifacts, QA gate, source/toolchain/runtime, release actor, cost and rollback reference |
| `ReleaseV1` | immutable manifest reference and release lifecycle |
| `DeliveryReceiptV1` | exact release/customer plus server-issued `DELIVERY_PREVIEW_TTL_POLICY_V1`, strictly positive TTL capped at 15 minutes, expiry and receipt digest |
| `CustomerFeedbackV1` | authenticated customer action, exact delivery and linked rework items |
| `AcceptanceV1` | explicit customer, delivery and release binding |
| `RollbackV1` | exact source/target releases, reason, actor and effect receipt |
| `ProjectCloseoutV1` | exact acceptance/release, source lineage and authoritative memory publication receipt |

Only the evaluation-plan digest and explicitly deterministic evidence subsets
are byte-stability gates. Model/calibration fields remain schema scaffolds but
must be absent from every valid #696 graph and gate until #749 supplies their
productive calibration and independent-authority contract.

Assignment validates every plan reference and every policy/input digest as
nonzero and canonical, requires disjoint required/optional case sets, and
validates the declared seed and retry contract plus classification, key owner,
access, redaction, retention-frontier, and audit bindings in `DataControlV1`.
An empty seed set is an explicit deterministic plan. A zero retry limit requires
an empty retry-class set; a positive retry limit requires at least one canonical
retry class.
The fixture inventory is a domain-separated digest over the sorted
`(case_id, generation, case_digest)` map, so replacing a fixture while retaining
its case ID changes the plan binding.

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

Only one release ID is active in an aggregate. The test fixture adopts promotion
or rollback state in one local CAS commit after a separately durable effect-saga
receipt exists. The external effect and local adoption are never described as
one atomic transaction.

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
cross-tenant replay. The test record repeats all four namespace components;
lookup, duplicate adoption, and `health()` require the table key to equal those
fields exactly, so key-only authority tampering fails closed.

## 6. Test persistence and productive append/publication boundary

`DeliveryStore::open_test_only` is a deterministic redb fixture, not a
productive trajectory, event, or publication authority. It proves the core's
restart, CAS, idempotency, envelope, and receipt contracts without competing
with #732 or #733. It uses five tables:

| Table | Key and purpose |
| --- | --- |
| `delivery_meta` | schema version |
| `delivery_aggregates` | `tenant:project` aggregate snapshot |
| `delivery_journal` | `tenant:project:revision` append-only operation event |
| `delivery_idempotency` | `tenant:principal:command:key` request/receipt binding |
| `delivery_outbox` | event-digest keyed canonical publication request and receipt |

One fixture write transaction checks the expected aggregate revision and writes
the aggregate, journal entry, idempotency record, and outbox row. A duplicate
request with the same command digest receives the original operation receipt
with `duplicate=true`; different content under the same
tenant/principal/command/key namespace is a typed conflict.

Productive construction stays unavailable until #732 supplies the canonical
aggregate/expected-revision append adapter and #733 supplies the canonical
publication-state adapter. The two narrow traits are separate even though the
test fixture implements both. This PR makes no productive CQRS, journal, outbox,
or event-store claim.

The outbox uses a stable namespaced operation ID:

```text
delivery:<tenant>:<project>:<revision>:<event-type>
```

The publication request binds operation ID, event type, aggregate and row
identity, exact canonical envelope bytes, envelope digest, and request digest.
The publisher returns schema version, canonical non-empty event ID, operation
ID, aggregate/row identity, payload digest, and request digest. Only an exact
receipt marks the matching fixture row published. Crash-before-publication,
crash-after-publication-before-local-ACK, duplicate ACK, wrong-schema,
empty-event-ID, wrong-row/wrong-digest receipt, and idempotency collision remain
recoverable or fail closed.

The test fixture's `health()` decodes every table and validates schema, aggregate
key/revision, contiguous journal history, domain-separated envelope digest,
journal/outbox linkage, canonical payload bytes, publication receipt, and
the full idempotency key/tenant/principal/command/caller-key receipt binding.
Connecting this to Sentinel's event/CQRS chain is deferred
to #732/#733.

## 7. Narrow integration port

`DeliveryIntegrationPort` exposes only:

- readiness with exact contract version, authority generation, and immutable
  contract digest;
- opaque principal/role current-authority validation;
- a read-only workflow authority/currentness snapshot; and
- execution of one stable QA workbench evidence request.

The authority snapshot binds agreement, project, work-item digest, current
candidate generation/digest, participant principals, and snapshot digest.
Promotion resolves it again so a stale candidate cannot pass on old evidence.

The QA request is called outside any writer transaction. It binds tenant,
project, exact candidate, plan, run ID and generation, assigned QA principal and
authority generation, stable invocation, and request digest. A separate stable
authority-identity digest binds principal, contract version, contract authority
generation, contract digest, and issuer. The short-lived receipt digest is
carried for authorization/audit but is zeroed while computing the stable request
digest; changing only receipt issuance, expiry, or receipt digest cannot create
another operation identity. Changing any stable authority-identity field changes
the request and rejects an older outcome. The opaque #694 receipt repeats the
exact stable authority identity and binds the stable operation plus input/output,
artifact ownership, structured result inventory, logs, screenshots, failure
summary, harness outcome, and a canonical generation- and digest-bound cleanup
reference. Wrong receipt schema, missing artifact ownership, or malformed
cleanup evidence fails closed. A second authority check after the effect
prevents TOCTOU adoption after revocation or actor replacement while allowing a
renewed receipt for the same stable authority identity.

The receipt is imported as a persisted `QaEvidenceGraphV1`: exact dataset cases,
case results, deterministic assertions, and flake dispositions are reference-
and digest-validated against the terminal run. IDs are unique, record
schemas/generations are current, assertion evidence is bound to the exact plan
and case, outcome/reason matrices are closed, and every PASS result, including
an optional case, has at least one valid passing deterministic assertion.
Populated model records or
grader references fail with typed #749 unavailability before any model verdict
is evaluated; a model can neither authorize nor veto deterministic evidence.
Gate receipts therefore require both `model_evidence_digest` and
`calibration_digest` to be absent. The complete
required/optional case inventory must match the plan's fixture digest. Every
active case requires license, access/contamination policy, DataControl, and at
least one canonical `(owner, type, id, generation, digest)` source tuple; result
sources are a separate nonempty immutable inventory for workbench, event,
artifact, policy, and completion evidence. Both inventories reject only an
exactly repeated full source tuple and reject one immutable
`(owner, type, id, generation)` locator carrying conflicting digests. Different
owners, types, or generations may legitimately share a source ID. Result slices
must exactly equal the digest-bound dataset-case slices, so role, project,
model, language, or surface labels cannot be changed or dropped. There is
also one graph-wide locator-to-digest index across every dataset and result
inventory: repeated identical locator/digest citations across inventories are
valid, while any digest change for the same locator generation is a conflict.
There is exactly one result per required case. Missing, malformed, retired,
superseded, substituted, exactly duplicated, locator-conflicting,
slice-relabeled, or empty-source evidence fails closed, as do stale or
policy-mismatched flake dispositions. Each case result retains a sealed,
generation-bound, gapless `1..N` attempt inventory. Its parent assertion refs
must equal the exact union of per-attempt refs, and its final status is derived
from the terminal attempt. A later pass after a deterministic failure therefore
cannot erase the failure: it requires a current disposition and retains both
attempts and both evidence records. Every unresolved-flake result requires a
current, unexpired disposition owned by the exact QA authority. A resolved
`RetryPassed` disposition additionally binds a deterministic regression ref to
an actually present, matching, passing result used by the terminal attempt;
invented, stale, differently digested, or failed regression evidence is rejected.
The gate derives its inventories from
that graph; caller-supplied nonzero digest flags or
aggregation-policy-as-calibration substitutions are not sufficient.

`execution_saga_readiness` is a separate exact #694 contract. Until a
productive #694 adapter can durably claim the stable request before execution,
persist the opaque outcome before returning, and reconcile that outcome after
crash/disconnect, execution is typed unavailable. The deterministic fake proves
effect-once reconciliation after local adoption failure and cross-run replay
rejection; it is not productive authority.

Workbench execution and external delivery effects use distinct canonical
readiness digests (`workbench-execution-saga-v1` and
`delivery-effect-saga-v1`). The #694 workbench port cannot satisfy #710 effect
readiness and the #710 effect port cannot satisfy #694 workbench readiness;
swapped, stale, zero-generation, or wrong-version contracts fail before either
port is invoked.

The public readiness probe is command-specific. Authority-only commands require
the integration contract; `ExecuteQa` additionally requires the exact #694
workbench saga; and promotion/rollout, rollback, governed rework, and closeout
memory publication additionally require the exact #710 effect saga. There is no
general `Ready` response that omits a dependency required by the requested
command.

## 8. External-effect sagas

Rollout/rollback, governed #695 rework creation, and closeout memory publication
are behind `DeliveryEffectPort`, unavailable by default. Each request binds a
stable operation ID, tenant, project, candidate, subject, optional target,
actor, operation kind, and request digest. Rollback binds both exact source and
target release references and their digests. The short-lived authority receipt
is carried for authorization/audit but excluded from the stable request digest.
The same stable authority-identity digest used by the workbench contract is
included in the request digest and repeated exactly by the receipt. The trusted
receipt returns an opaque effect reference and exact operation, source, target,
actor, authority identity, request, tenant, project, and candidate bindings.

The effect happens outside the local transaction. A Ready #710 adapter must
durably claim `(operation_id, request_digest)` before the real effect, persist
the sealed outcome before returning, and replay/reconcile that same outcome on
retry after caller crash, disconnect, or local revision conflict. The core then
revalidates the same authority identity and performs local CAS adoption.
Missing, stale, cross-tenant, wrong-kind, wrong-project, wrong-candidate,
wrong-source, wrong-target, wrong-actor, or ambiguous receipts cause no state
transition. Without the exact saga readiness contract all effect methods are
typed unavailable. This is explicitly a restartable saga, not an atomic
cross-system effect claim.

Customer feedback performs all local validation before invoking governed
rework: action/acceptance legality, feedback and acceptance ID collisions,
delivery ownership/currentness, transition legality, and exact acceptance
binding. Any locally knowable failure leaves the aggregate unchanged and calls
the external effect zero times.

Delivery preview creation uses server time, requires `issued_at_ms == now_ms`,
and binds policy version 1 into the sealed receipt. TTL is strictly positive and
at most `DELIVERY_PREVIEW_MAX_TTL_MS` (15 minutes); zero, overlong, backdated,
future-issued, and unknown-policy receipts fail before mutation, while the exact
upper boundary is accepted.

## 9. QA addendum and negative contract

The #717 addendum is represented in the canonical schemas. Productive evaluators
must additionally enforce:

- required case inventory cannot shrink;
- populated model evidence remains typed unavailable and is never evaluated as
  a decision until #749 provides calibrated, independently authorized model
  evaluation;
- active-scenario lanes require explicit capabilities and cleanup/quarantine;
- candidate data is untrusted input, not policy or instructions;
- unknown provider/model identity is never called byte-reproducible;
- unresolved flakes and human-review results cannot issue a passing release
  gate;
- every finding has canonical immutable source evidence; every unresolved
  finding blocks review approval, the release gate, and promotion regardless of
  severity until a versioned resolution reference exists;
- attempts and dispositions are append-only;
- retention/pruning cannot pass the required effect frontier;
- restored generations must match before readiness.

This core stores and validates the imported graph and legal pass/fail/harness
gate matrices, but intentionally does not implement a productive runner,
provider, sandbox, retention job, or #709/#710 effect engine.

## 10. Console lineage scaffold

`DeliveryView` is an isolated, unreachable scaffold. It is not a product Console
surface, is not wired into `App`, and has no API/projection adapter. Therefore
this phase makes no AC-9 product, authentication, authorization, or browser
security claim.

It accepts only a narrow `PublicDeliveryLineageDto` whose type deliberately has
no tenant ID, prompts, credentials, private artifacts, or infrastructure fields.
The future authenticated server adapter must enforce tenant authorization and
redact before sending this DTO; browser code is not a redaction boundary.
Defense-in-depth validation rejects:

- schema version;
- a missing server-redacted marker and invalid revision shape;
- unique non-empty node IDs;
- valid SHA-256 digests;
- positive generations;
- non-empty actor roles; and
- finite non-negative costs when present; and
- non-dangling lineage edges;
- credential-shaped text, internal addresses, and local paths.

## 11. Recovery, backup, and retention

The #722 whole-product contract applies to the future productive #732/#733
authority, not to the test-only redb fixture. Its canonical aggregate, journal,
idempotency records, outbox frontier, published receipts, QA evidence
generations, manifests, releases, deliveries, acceptances, rollback history, and
closeouts form one recovery participant.

Before productive activation, recovery integration must:

1. fence all delivery writers;
2. flush and record the canonical store generation and outbox frontier;
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

## 12. Verification matrix

| Criterion | Dependency-independent status |
| --- | --- |
| AC-1 schemas and legal transitions | Implemented and testable |
| AC-2 independent authority | Implemented; live probes deferred |
| AC-3 productive #694 suite | Port and fake only; productive execution deferred |
| AC-4 missing/stale/different gate | Core negative tests cover missing approval, unresolved findings, expiry, plan-digest substitution and incomplete evidence; broader runner matrix deferred |
| AC-5 immutable manifest | Core rejects ID reuse and binds exact gate/candidate/authority; canonical #732 append deferred |
| AC-6 customer flow | Core records/transitions implemented; authenticated API/browser and productive delivery effect deferred |
| AC-7 rework history | Governed-rework request/receipt saga implemented with fake; productive #695 effect deferred |
| AC-8 rollback | Exact source+target receipt, stable operation, revision-conflict reconciliation and restart-safe local adoption tested with a durable fake; productive #710 rollout/rollback effect deferred |
| AC-9 Console lineage | Isolated public-DTO scaffold only; product/API/live criterion OPEN |
| AC-10 memory closeout | Receipt-gated saga and restart readback tested with fake; productive memory path deferred |
| AC-11 Gaia oversight | No bypass surface added; productive observation deferred |
| AC-12 restart/idempotency | Test-only store, QA outcome reconciliation, outbox, effect revision-conflict retry, rollback and closeout boundaries covered; productive adapter crash matrix deferred |
| AC-13 `.240` journey | Not authorized in this phase |
| AC-14 canonical QA schemas | Plan, dataset, run, case, deterministic, flake and gate core are versioned and strictly validated; PARTIAL because model/calibration authority and the productive import lane remain #749/dependency deferred |
| AC-15 deterministic/probabilistic split | Deterministic bindings are implemented; populated model/grader evidence is typed unavailable and model/calibration activation remains #749-deferred |
| AC-16 sandbox/capability enforcement | Contract fields only; #694 implementation deferred |
| AC-17 retry/flake append-only history | Gapless sealed per-case attempt history, retained fail-to-pass evidence, derived terminal status, current QA ownership, and graph-bound passing regression disposition are implemented; productive evaluator/property/restart proof deferred |
| AC-18 #709/#710 effects | Distinct #694-workbench/#710-effect fail-closed readiness plus stable operation/intent/outcome/reconcile contracts and deterministic fake proof; productive integration deferred |
| AC-19 #722 recovery | Participant contract documented; recovery integration deferred |
| AC-20 complete gate negative matrix | Core accepts explicit no-retry/no-seed plans, exact cross-inventory source reuse, and a fully evidenced resolved retry while rejecting zero plan inputs, inconsistent retries, fixture substitution, empty/malformed/duplicate/local-or-graph-wide locator-conflicting sources, relabeled slices, duplicate results, attempt gaps/duplicates/lost failures, PASS without deterministic evidence, invented/stale/failed regression refs, stale/forged flake dispositions, unresolved findings, any populated model/grader evidence, fake calibration, and illegal pass/fail/harness summaries; productive evaluator/retention negatives deferred |

Negative criteria AC-N1 through AC-N4 are enforced by the state machine and
authority boundary. AC-N5 requires later matching API/event/artifact readback.
AC-N6 is enforced for repository evidence; the scaffold rejects sensitive-shaped
DTO text but is not claimed as the productive server security boundary.
AC-N7 remains a release rule: build-server time is never runtime benchmark
evidence. AC-N8 through AC-N12 remain mandatory for the productive evaluator,
effect, retention, and recovery integrations.

## 13. Final integration sequence

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
