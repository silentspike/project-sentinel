# Governed Leadership Recovery

An exhausted unknown-model session is not a new task. Its original model
effects, consumed allowances, reviews and private work remain authoritative.
Schema-2 recovery reconciliation remains bounded to three reviews per session.
Normal budget-window reviews have a separate finite three-review base bound.
An explicit operator extension can authorize at most three additional normal
reviews for one exact exhausted head; all continuations still share the existing
three-window bound.

## Normal Leadership Windows

A ReadyForModel employee can exhaust its current call allowance or reach its
deadline after observing a tool result. This is not an unknown outcome or a
fabricated Blocked decision. Schema 3 binds that normal review to the exact
root allowance, active allowance digest, continuation-history digest, project,
assignment, journal head and observed exhaustion flags. The store authenticates
the original allowance from durable history inside the same transaction as
the governing project, continuation and receipt updates. A historical lookup
alone does not authorize more work.

The assigned Project Manager or Technical Lead makes a real model decision:
Continue selects bounded additional calls and a window from the remaining
original policy; DeferBudget records a refusal without renewing the employee.
Admission itself is not a model decision or permission for a developer call.
Root counters never reset, no pending effect is replaced, and retired reviews
still count toward their finite quota. An exhausted root or finite review/window
limit gets a durable system-policy disposition, not an invented model refusal.
The normal path does not issue an operator recovery epoch.

The leader can inspect the exact authorized retained tool observation through
the private Workbench preparation path. Invocation, request, result digest,
profile and current capabilities must match. Private output stays out of the
governing journal and public DTOs; the context is bounded and treats prior tool
output as untrusted evidence, not current filesystem authority. Missing or
revoked observation authority fails closed.

Normal windows may restore the verified root per-call duration. Recovery
windows preserve preceding narrowing. Effective provider grants and budget
validation use the same source-sensitive history calculation, and all governed
allowances require durable provenance even when their duration equals the
ordinary duration. Fresh-inspection fences remain intact: a one-call window may
fund inspection alone, so leadership must explicitly budget useful subsequent
work. Exact committed replay returns the original receipt after later head
advancement without granting new authority or rewinding the session.

## Normal Review Extension

`/operator/workflow/adaptive-budget-review-extensions` is a separate operator
issuance endpoint for normal schema-3 budget-window reviews. Authentication
resolves the principal on the server. Only Operator principals with Project
Manager or Technical Lead authority may use it; employee leadership roles and
customer credentials do not confer operator authority. Issuance requires active
model work, the workflow mutation fence and the current authoritative project
and session. The source must be an exact exhausted ReadyForModel head with a
recorded normal review-limit disposition, not an unknown-model recovery source.

GET requires `project_id` and `session_id`. It returns the existing immutable
issuance receipt for that head, or a server-bound draft containing `request`,
`requires_explicit_submission: true` and `model_decision_recorded: false`.
Reading a draft does not write evidence or authorize another review. POST
accepts the typed request from the draft after explicit operator submission.
An optional GET `expected_session_version` selects an existing receipt for an
exact head; it cannot obtain a new draft for a stale head or bypass the current
authoritative project/session lookup.
Its bindings are:

- `schema_version`, `operation_id`, `tenant_id`, `project_id`, `session_id` and
  `expected_session_version`;
- `budget_limit_receipt_digest`, `source_digest`, `base_global_review_count`
  and `base_head_review_count`;
- `additional_reviews`, `reason_ref` and `expires_at_unix_ms`.

The server validates these bindings against durable current state. A caller
cannot substitute another tenant, source head, receipt or review count.
`additional_reviews` is finite, from one through three, and fresh issuance may
expire no more than 24 hours after its issuance clock. There is one immutable
extension per tenant, session and exact head. Exact replay returns the original
receipt without refilling slots or renewing expiry, including after the
extension expires. A different operation ID cannot create a second extension
for the same head.

The extension raises only the normal review quota. It neither creates a
leadership decision nor grants employee model/tool work. A subsequently
authorized schema-3 review must still obtain a real model-selected Continue or
DeferBudget decision. Root model/tool ceilings and the three-continuation-window
ceiling remain unchanged; an extension cannot bypass either ceiling. Root
counters, continuation history, retired reviews and the previous limit receipt
remain intact. Retired extension reviews consume their slots permanently.

## One Explicit Intervention

The authenticated operator endpoint
`/operator/workflow/adaptive-review-epochs` accepts only Operator principals
with Project Manager or Technical Lead authority. Employee and customer
credentials cannot authorize this intervention.

A GET proposes a request from verified current state without writing evidence
or creating authority. POST atomically records one immutable recovery epoch,
one designated leadership review and issuance audits. The epoch is unique for
the tenant and original session, including after expiry, restart or another
release. A new operation ID does not grant another epoch.

The request binds the exact project, session, journal head, assignment,
unknown-model proof and three retired, dispatched historical reviews. It also
binds protected installed-release evidence. Caller-provided release hashes
alone do not establish eligibility. The daemon also resolves the bounded
systemd MainPID and pins its process directory. Its running executable must
match the installed file identity before and after hashing, and the serving
PID must remain unchanged.

The five-minute issuance deadline does not renew on replay. The designated
review authorizes a real leadership model to choose Continue or KeepUnknown;
issuance itself is neither a decision nor proof that a model was called.
The immutable issuance receipt does not claim current decision state.
Continue permits a model-selected number of employee calls within the
original unspent root budget and a bounded window no larger than the original
root window. The prompt exposes the exact immutable limits. Root model/tool limits and
continuation limits remain enforced. Expiry or an unknown review consumes the
slot permanently. Tool-unknown effects are outside this recovery authority.

## Installed Repair Evidence

The daemon verifies the root-protected file
`/etc/sentinel/adaptive-recovery-release.json`, the installed release manifest
and actual Gateway binary bytes. The repair file has schema version 1,
purpose `adaptive-leadership-schema-repair`, a release tuple containing the
source Git SHA, manifest digest and Gateway binary digest, and a canonical
`gate_evidence_digest`. This is local root-attested evidence, not remote
attestation or a substitute for review, gates and live health readback.
Changed or unprotected evidence blocks fresh issuance and dispatch/adoption.
Historical exact issuance replay does not require new serving authority.

## Diagnostic Is Not Outcome

The Gateway forwards the fixed category `codex_output_schema_rejected` only
on the authenticated internal native model-work error path. The HTTP body
remains generic. Arbitrary provider text, prompts and stderr are not forwarded.

The bridge accepts exactly one diagnostic header on HTTP 502 without a
contradictory pre-provider admission header. It atomically appends the fixed
diagnostic and seals the exact before-send registered model reservation as
UnknownOutcome. Existing unknown reasons and known responses remain intact.
Diagnostic persistence does not append usage, execute an action, refund an
allowance, prove nonbilling or authorize another provider call.

## Recovery And Replay

The store validates epoch membership at dispatch, decision completion and
session continuation. Continuation caps are checked before authoritative
audit append. Crash replay reuses the original recorded clock and exact
operation/request/issuer binding. It never rewrites the original unknown
effect, resets a session or invents a customer acceptance.
