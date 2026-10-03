# Agent Workbench

This document describes the M0 `web-project-v1` workbench implementation and its fail-closed production boundaries. The normative product contract remains [Virtual Company Work Execution](virtual-company-work-execution.md).

## Production path

Tool-bearing agent work has one supported path:

1. The daemon resolves a current assignment and constructs an authority snapshot.
2. The request is authorized against the intersection of agent, role, assignment, project, and tool-profile capabilities.
3. The daemon reserves the digest-bound invocation in its durable workbench store.
4. `NanoRuntimeRegistry` selects the configured secure runtime. M0 requires `bwrap-landlock`; an unavailable or mismatched runtime fails closed.
5. The daemon launches `agent-runtime` inside the #75 full-cage sandbox and exchanges versioned JSONL messages.
6. The daemon retains one World owner capability across resource attestation and runtime I/O, revalidates that same capability before decoding, artifact acceptance, and durable terminal adoption, and separately re-resolves the assignment authority. Revoked or stale authority leaves the invocation recoverable without publishing a terminal event.

There is no host-shell, ECS-only, or less-isolated fallback for an M0 tool request.

### Private observations for model continuation

`observation.retain_private` is an opt-in capability, checked through the same
agent/role/assignment/project/profile intersection as execution. Existing profiles
do not grant it implicitly. It authorizes bounded, request-bound tool feedback,
not additional commands, network access, provider calls, or business completion.

For a retained invocation, agent-runtime writes a version-3 private completion
receipt before emitting the terminal result. Its safe result remains separate
from the private observation and both are validated on recovery. The observation
contains the terminal outcome, validated immutable artifact references, sanitized
error classification, and bounded transient output. File inspection, failed-command
diagnostics, and a packaged artifact digest can therefore drive the next model
round without reading changed state or executing the tool again. Version-1/2
receipts and ordinary invocations retain their previous output-free replay behavior.

The daemon commits the private observation and its terminal reference in one
transaction in the existing `workbench.redb`. Public invocation records contain
only the reference digest; events and normal status replay contain no private
output. The internal `PrivateObservation` dispatch resolves current authority
before and after reading and requires the exact profile and retention capability.
It performs no runtime invocation and publishes no business event. Private output
is untrusted task data, never evidence of permission or independent QA.

A missing or conflicting observation is an integrity failure, not permission to
repeat a tool. Backups must retain the entire workbench database and runtime
receipt roots together with their existing invocation frontier. Legacy rows remain
version 3; retained rows use version 4. Once retained invocations exist, rollback
requires a compatible reader or restoration of the complete pre-activation state;
do not remove private rows or downgrade their version to force compatibility.

This transport is necessary but is not by itself proof of a live autonomous loop.
Activation still requires the bounded adaptive journal, productive Gateway and
Workbench adapters, release-profile pinning, and real-model continuation evidence.

The workflow core now owns a bounded adaptive-session journal. A session seals
the exact employee/runtime authority, provider-authority digest, model/tool call
ceilings and deadline. Every model or tool effect receives one stable UUID and
request digest before I/O. Reconciliation may recover that exact effect after a
restart; an unknown outcome cannot mint another effect. Confirmed tool output,
including a nonzero command status, safe failure details, and immutable artifact
references, becomes a digest-bound observation for the next model round and does
not by itself fail or complete the company work item.

The journal uses the existing workflow SQLite transaction boundary and its
append-only operation chain. It creates no second business workflow. Current
organization authority is checked before and after model or Workbench I/O.
Completion remains a proposal until the normal artifact, independent QA, release
and customer authorities accept it. The productive adapters use the same Gateway
subscription claim and Workbench authority paths; source tests do not substitute
for activation and live acceptance on the reviewed single-node release.

Queue scheduling selects which employee assignment may begin a model round.
Reauthorization of that selected round is a different operation: it resolves
the exact persisted tenant, project, work item, assignment, session, allowance
and effect. Another eligible project for the same employee cannot replace that
binding during provider I/O or the Gateway dispatch claim. Reauthorization
never creates a session or reconciles a tool; missing or changed bindings fail
closed. Current role/profile authority, reservation ownership, request/context
digests, deadline, remaining budget and the one-shot dispatch claim remain
mandatory. This does not authorize retrying an unknown provider outcome.
The final company model claim rechecks its persisted active assignment and
allowance inside the claim transaction, so a concurrent reassignment cannot
authorize a new effect using a snapshot read before that transaction.

Fresh inspection is required for each resumed window. A known scoped file may
be inspected directly; directory discovery is needed when its layout or path
is unknown. After the authenticated inspection clears that window's fence,
another model call does not by itself require repeating discovery. New windows
still require fresh inspection; historical observations cannot clear the fence.

Runtime health inspects validated durable session heads and journals separately
from execution admission. A changed profile, a new assignee or a completed
work item cannot hide an earlier unknown model or tool effect. Historical
grants identify evidence; they do not authorize fresh work. Every current
dispatch and leadership continuation still requires its current role, profile,
assignment, deadline and budget. Corrupt or missing durable lineage makes
health unavailable rather than silently omitting the affected session.

Customer progress is separate from company work state. The authenticated,
tenant-scoped overview reads only the exact current assignment's validated
journal head. It shows fixed labels for a missing private observation, unknown
model or tool outcome, and a ready model round whose active work window is
expired, spent or too short to admit a call. A window pause does not promise
another leadership review or a continuation; an unknown effect is not a budget
pause and cannot be retried from this view. Normal, in-flight and completed
states receive no invented progress label. Changed assignment/profile bindings
or failed integrity reads cannot substitute stale progress. The frontend accepts
only canonical status/label pairs and suppresses previous progress after a
failed refresh. These reads do not grant calls, reset counters, issue commands,
alter journals or complete work.

### Same-session funding request boundary

`AdaptiveWorkFundingRequestV1` describes an explicitly finite proposal for
additional developer model/tool capacity and bounded leadership reviews/work
windows. It names the original session and exact source/head, retains original
call ceilings and spending, and permits only a ready model cursor. Unknown
model/tool effects cannot become a funding shortcut. Operator role and tenant,
checked call/review/window limits, immutable request identity and usable time
slack are validated before any future issuance.

Request validation does not authenticate the supplied source or grant capacity.
The store's issuer compares that source against the authoritative
journal and current project in a pinned transaction, then atomically records
only an immutable receipt and its event. Identical replay returns that same
receipt after expiry or later session changes; a changed request or issuer is
not a replay. Receipt proof checks use issuance time, not current expiry.

Issuance implies neither dispatch permission nor a leadership `Continue`.
The registered operator command is distinct from a real leadership review:
the review names one immutable funding epoch and its next global ordinal in
the sealed evidence. Its exact membership is event-backed. Only a genuine
completed `Continue` can adopt the epoch in the same transaction as the
journal continuation and provider allowance. The adoption proof depends on
the immutable review membership, not a mutable latest-policy pointer; journal
replay verifies that proof without recursively loading the journal itself.

Existing grants, v1 resume policies, review decisions and their historical
proof bytes remain unchanged. Original call ceilings remain separate from
the authenticated funded ceilings; spending is cumulative and never refunded.
A successor proposal requires the exact adopted predecessor and current
journal, and cannot replace an unadopted proposal. A funded refusal, expired
review or retirement on the same head cannot trigger an automatic reroll.
Only actual progress in an adopted epoch permits another finite review.
Unknown effects, stale authority and mixed recovery/local-adoption contracts
remain fail-closed. Public endpoints, events and projections do not expose
private source snapshots, credentials or model observations. Unit evidence
does not prove that a paused live employee has resumed or delivered work.

### Opt-in model work proposals

The first M1 bridge is selected by `SENTINEL_MODEL_WORKBENCH_ENABLED=true`.
It is off by default, requires the enabled company workflow, the `llm` feature,
and `SENTINEL_LLM_USAGE_V2_ENABLED=true`, and is visible as
`company_workflow.model_work_enabled` in runtime health. Enabling it is not a
claim that the autonomous-company acceptance has passed.

For one assigned Designer or Developer with exactly one active work-item
provider reservation, the daemon derives a task and authority snapshot from
the existing workflow. Model work accepts one output contract and bounded
upstream text artifacts from declared dependency inputs. The same authority
resolver used by Workbench mounts requires a completed producer, its current
assignment, matching contract generation/digest, and committed output evidence.
The daemon reads the actual immutable files through the protected artifact
reader, not historical write commands or a caller-supplied source snapshot.
Inputs are limited to eight artifacts, 64 files and 64 KiB of UTF-8 content in
total; binary, oversized, missing or invalid inputs fail closed without
truncation. The complete serialized prompt retains its separate 128 KiB bound.
Paths, file digests and content are included in the private provider context;
they are untrusted task data, not instructions, permissions or passing tests.
The context is re-resolved at dispatch and before new plan admission, so changed
upstream results cannot silently authorize execution. Input reads do not stage
files or modify either employee's workspace. Existing no-input context bytes
are unchanged. Input access alone does not manufacture a model review or replace
the separate deterministic QA gate.
Once input-bearing requests have been persisted, recovery requires a binary
that understands those contexts. Do not downgrade across a pending request or
delete its journal to make an older binary accept it.

The provider window is five minutes from the durable
reservation timestamp; a new perception does not renew that window. Unsupported
work fails closed instead of becoming an unbound tool or a host-shell action.

Within that admission window, each model-work Gateway attempt has a separate
maximum duration of 120 seconds from Gateway request start. That deadline covers
queuing and pre-provider waits, is passed to the CLI, and cannot extend an
earlier caller deadline or shorter configured provider timeout. An expired
attempt is rejected before provider dispatch. The queue wrapper rechecks
cancellation after acquiring capacity, including an immediately available grant,
and releases expired capacity without invoking the provider. This is a local
execution bound, not proof that a remote server stops token generation instantly.

The request uses the reservation's stable ID and binds the assignment,
principal, organization, profile, runtime, policy, and task content. Volatile
tick, room chat, and body metadata cannot change its retry identity. The
Gateway still selects the agent's model through its normal authenticated
agent-runtime route, catalog, activation gate, queue, and guardrails. The work
response mode disables synthesis and learned-response substitution. Existing
deterministic personality, quality, and fourth-wall checks reject concerns
without making an additional hidden provider call. An operator-rewritten
response is not accepted as the model's proposal. The requested output-token
ceiling cannot be raised by a larger global default; this does not imply that
every provider transport supports a hard generation-token ceiling.

The model returns only a strict JSON object with `schema_version: 1` and a
bounded `tools` list. It supplies the actual proposed file contents and relative
paths, not project identities, capabilities, credentials, success attestations,
or authority generations. The existing workflow intent compiler derives the
execution plan, checks every tool against the immutable work profile and output
contract, and enqueues work through the existing Workbench adapter. A proposal
does not mark work Done or bypass independent QA, release, or customer acceptance.

The existing private LLM outbox stores the bounded proposal and dispatch context
before admission. Usage is persisted first, including paid responses rejected
after dispatch. Local admission can then retry without another provider call.
Its stable operation ID and the workflow's existing transactional plan/outbox
contract prevent a second execution after restart. Exact plan replay is checked
before new-admission freshness, but changed authority or tool content is still
rejected. Model work never enters the legacy Chat/ToolUse action channel. Failed
or claimed completion rows are not implicitly reactivated. There is no new store
or schema migration; version-1 legacy completion payloads remain readable.

This bridge is not the full M1 conversation/tool-result loop. Model-selected team decisions,
and the real-provider customer-to-artifact journey remain #856 acceptance work.
In particular, the existing monetary reservation API must not be presented as a
valid substitute for ChatGPT subscription call/token/time limits or be populated
with an invented marginal USD price. The activation stays off until that provider
contract and the target-runtime readiness are verified. Test fixtures use the
existing `local-loop` exemption, not a production OAuth exemption.

### Independent source review

An assigned QA employee uses the separate `web-review-v1` profile. It has no
command rules, shell, network, patch tools or developer artifact authority.
The model returns a bounded `SourceReview` JSON object tied to the complete
input path/SHA-256 inventory. It may report `pass` with no findings or
`changes_requested` with concrete source paths, one-based lines and reasons.
Unknown fields, invented test attestations, invalid source lines, self-review,
ambiguous inputs and contradictory verdicts are rejected.

The server carries that model-authored report through exactly two existing
Workbench operations: write `review.json` in the QA work item's own workspace,
then seal it as `qa_report`. No other write path or command is admitted, including
through a caller-supplied raw execution plan. Source inputs remain read-only.
Profile selection follows the current role/assignment; missing review-profile
configuration denies review admission without disabling older developer work.

When model work is enabled, delivery requires a completed QA report from the
current independent QA employee covering the entire current candidate. The
sealed report must equal the model execution, and its plan must match the
durable provider dispatch and canonical nonempty usage event. Unresolved or
operator-resolved provider outcomes cannot approve delivery. A retained provider
payload is also checked against the report; normal payload cleanup does not
remove the canonical usage and completed execution evidence. A rejecting report
blocks promotion. Passing source review is separate from the technical QA
runner, which must still pass. Token-free M0 mode retains its explicitly
deterministic gate and is not evidence of independent model reasoning.

Leadership can append review work through `POST /agent/workflow/source-reviews`
using the normal operation envelope with `append_source_review`, project ID,
expected project version and the new work specification. The project must be a
completed delivery candidate, not already present in delivery. The new item
must cover every developer/designer output, use an independent QA owner and
fit the existing budget. Existing work and provider records are preserved.
`assign_source_review` on the same route assigns that appended item to its QA
owner with an explicit `web-review-v1` profile binding and
`reason_ref=source-review-profile`. Profile ID, digest and generation must match
the installed immutable profile. The assignment retains this binding without
rewriting the accepted participant/governance profile. Other work cannot use
this specialization. Both commands remain versioned and replay-safe.

The ordinary command routes reject these internal commands. Their dedicated route
shares the delivery mutation fence; replay uses the original operation result.

`grant_source_review_call` on that route performs one developer-to-QA handoff,
bound to the previous allowance ID and the assigned QA work. Canonical model
usage and adopted completed execution are required; unknown or operator-resolved
outcomes cannot authorize it. The consumed developer allowance is retained in
`source_review_previous_call`, remains counted in the provider campaign and
cannot be claimed again or switched to monetary accounting. Replays return the
original result; a second handoff is denied. Empty history is omitted from old
project serialization. A binary without this field cannot read a handed-off
project; rollback therefore requires the matching pre-handoff store snapshot,
not an in-place downgrade or removal of provider history.

This source path still needs integrated provenance tests and the real single-node
acceptance journey; a new fixture project is not that proof.

### Pre-agreement Sales inquiries

Sales consultation uses the existing customer request, not a fabricated project
or work assignment. The daemon resolves the configured Sales principal and its
healthy runtime, freezes the exact request version and conversation, and sends
it through the normal authenticated Gateway/Cortex agent-runtime route.
The provider returns a strict `ask_question` decision; neither tests nor the
operator supply its answer. Only the customer may answer that question or accept
a subsequent proposal. Sales may author a policy-bound offer through the same
durable decision path; no employee or operator may accept it for the customer.

Request execution uses Gateway metadata schema 2 with a `customer_request`
subject. Schema 1 continues to describe project work. Mixed subjects are denied.
The existing LLM completion outbox persists the response and usage before the
workflow adopts the question. Local adoption retries reuse that result without
another provider call; the question and completion receipt commit atomically.
Usage schema 4 binds tenant and permanent allowance without project/assignment
fields. The allowance links it to the exact customer request and Sales principal.
Schema 3 retains its stricter project authority requirements. Deploy the updated
projection reader before enabling schema-4 production; an old reader fails closed.

Preparation requires `SENTINEL_REQUEST_SALES_TENANT`, the exact
`SENTINEL_MODEL_WORK_ALLOWANCE_ID`, and the protected
`SENTINEL_REQUEST_SALES_TOTAL_LIMIT` (1 by default; approved rungs are 10 and 40).
The authenticated operator endpoint `POST /operator/workflow/request-provider`
accepts a stable operation ID, request ID/version, registered Sales principal ID,
model/catalog digest, concurrency bound, and expiry. It derives principal
authority, provider, total ceiling, token policy, and 120-second duration on the
server. Without autonomous intake enabled, the configured allowance must equal
the canonical operation-derived ID.
The request body cannot raise the configured total ceiling. Both services must
bind the same allowance and catalog before any provider effect is enabled.
The existing credential initializer provisions the independent `workflow-operator`
credential and the daemon loads it through systemd. It is not a customer or
employee credential and is not exposed to the customer dashboard.

`SENTINEL_REQUEST_SALES_AUTONOMOUS_ENABLED=true` is an explicit, protected
Operator configuration opt-in in both daemon and Gateway. It defaults to false;
it cannot be enabled by a customer request or model output. The daemon uses the
immutable configured grant only as the policy anchor: its granting Operator
must still match the authenticated principal registry exactly, and its provider,
model, catalog, concurrency and duration remain binding. New intake uses the
smaller of the configured total ceiling and the anchor's total ceiling, and
never reserves more concurrency than that effective total. Enabling
intake does not revive or dispatch the anchor's consumed call.

Periodic reconciliation inspects a bounded selection of validated, ungranted
requests for registered customers in the configured tenant. Settled requests,
already granted versions and questions awaiting a customer reply do not fill
that selection; an inbox larger than the customer-list API's page bound does
not stop reconciliation of existing projects. It admits the oldest eligible request version to the
unique healthy on-duty Sales employee, at most one grant per turn and only when
pending/unknown Sales calls leave capacity. An unanswered customer question
waits for the customer's reply. The operation derives from the anchor, request
and version; restart recognizes the original immutable reservation instead of
renewing its expiry. An expired or unknown reservation for that version is not
automatically replaced. Unresolved dispatched calls also prevent new authority
for a newer version of the same request.

Fresh inference selects the oldest current eligible grant for that employee.
Preparation, dispatch, completion adoption and recovery instead resolve the
exact tenant and allowance carried by the binding: advancing the inbox never
changes the authority of a previous result. The Gateway admits non-bootstrap
schema-2 Sales IDs only in explicit autonomous mode, then still requires the
daemon's secret-authenticated durable claim and exact receipt before sending.
Catalog/model, subject, request/context digests, expiry and caller checks remain
mandatory. Project planning retains its separate bootstrap policy and accepted
agreement/proposal lineage. These source contracts need live employee-journey
evidence; configuration and tests alone are not company acceptance.

Historical rejected completions whose Sales identity has been revoked remain
failed and immutable. They are not requeued under a replacement identity and
do not prevent daemon startup; corrupt completion envelopes and altered context
bindings still fail closed. This does not resolve or refund an unknown effect.

Every grant counts toward the store-wide cumulative ceiling, including legacy
project grants and expired unsent grants. Unknown dispatched outcomes retain
their concurrency slot; timeout, restart and a new request do not reset them.
An operator may explicitly abandon a terminal inference using the existing
LLM-completion resolution API, after checking that its local provider process
has ended. The operator then submits the allowance ID to
`POST /operator/workflow/request-provider/abandon`. The daemon verifies the exact
immutable Limbo resolution event, its request digest and employee identity, plus
the absence of an unresolved completion. It records that event on the original
grant without deleting its dispatch or decrementing cumulative usage. Only a
separately authorized new operation may try again; the old operation can never
dispatch or adopt a late response. This is an explicit abandonment of an unknown
result, not evidence of zero provider usage. Ordinary timeout or restart cannot
release a slot, and a served Sales question cannot be abandoned this way.
No live acceptance is implied by enabling this configuration or passing fixtures.

### Project subscription allowance

`GrantSubscriptionCall` authorizes one assigned Designer or Developer work item
inside the existing project transaction, journal, and projection. It is separate
from `ReserveCost`: a Project Manager or Technical Lead binds the exact assignment,
employee, provider `codex-cli`, model, and semantic catalog digest. Limits are one
call, one concurrent dispatch, at most 120 seconds locally, and an expiry no later
than five minutes after creation. The explicit token policy is
`measured_without_generation_cap`; it is not a hard generation-token guarantee.

After an accepted model-authored plan is activated, the daemon deterministically
selects the first ready assigned Designer or Developer and grants that exact work
item. Planning adoption and restart reconciliation use the same stable operation
identity. A project already carrying a grant is left unchanged, so recovery
cannot mint a second provider call. This starts the first executable company
task; advancing authority to later dependency work remains a separate governed
transition and is not implied by this initial grant.

The project retains its allowance permanently, including a consumed or unknown
outcome. It cannot create a replacement grant or mix a money reservation into the
same work item. Projects without an allowance retain their previous serialized
bytes and digests. No separate admission database or monetary exemption is added.
An older release that does not know this additive project field must not open an
allowance-bearing store. Roll back through the declared compatible backup/restore
procedure, with provider activity disabled and external outcomes accounted for.

`SENTINEL_MODEL_WORK_ALLOWANCE_ID` remains the shared bootstrap authority for
pre-agreement Sales and model-authored project planning. It is not rewritten for
each employee task. Project work carries its durable allowance ID in the exact
request metadata; the Gateway validates that ID and forwards it to the daemon,
which resolves it against the current project, assignment and grant before
claiming dispatch. A dynamic work ID cannot be used for planning; Sales accepts
its own exact request grant only under the explicit autonomous policy above.
Multiple independently authorized project grants form one employee work queue:
persisted local completions take priority, then still-live consumed calls, then
undispatched work. Within each class, persisted grant creation time and stable
tenant/project/allowance identity determine the order independently of store
iteration. Duplicate allowance authority, changed assignments and conflicting
reservations still fail closed; scheduling never combines project permissions.
Adaptive coding observes its persisted session cursor without starting tools
or creating sessions during selection. Pending model/tool effects stay ahead
of fresh work; saved model completions get recovery priority. Blocked, cancelled
and proposed-completion/collaboration cursors do not monopolize the next job.
Ready states with no remaining call budget are skipped, while a final already
consumed effect remains recoverable. Adaptive completion priority requires the
exact effect digest and employee owner scope, not just a matching request ID.
An exactly validated but inactive adaptive grant returns no provider work,
not an authorization error or a legacy execution fallback. An employee without
the matching project subscription authority remains rejected.
The Bridge separately requires explicit provider authority in subscription and
Sales-scoped modes. An idle or tool-waiting session cannot become an unbound
legacy provider request, including during pre-dispatch reauthorization.
Recovery recognizes the exact result already adopted in the immutable adaptive
journal. It can finish local completion cleanup without another provider call
or model/tool transition; altered effects, results and decisions remain rejected.
Collaboration is adopted only after its exact collaboration commit is durable.
Each saved result is revalidated against its own exact allowance, even when
another job is next. This does not authorize a fresh unselected provider call:
the dispatch callback still requires the current selection and exact pending
request/context digest. Expiry and unresolved provider-outcome rules are unchanged.
The daemon
also requires model work and usage-v2. The Gateway requires its existing protected
operator credential and a loopback-only `SENTINEL_OPERATOR_API_URL`. A
subscription-marked request without the Gateway mode fails closed.

Check the entire timeout chain before granting a call. The Gateway defaults to
a 60-second provider timeout; `SENTINEL_CORTEX_PROVIDER_TIMEOUT_SECONDS=120`
explicitly enables the full two-minute provider window when approved. Shorter
configured timeouts are never extended by the allowance. The proxy HTTP write
deadline covers request reading, the configured in-flight budget, and five
seconds for terminal response delivery. The control-plane deadline remains
60 seconds. The subscription context still caps actual model work at 120 seconds
including queue waiting; the longer socket lifetime does not authorize more
provider time, retries, or another call. A cancelled CLI with an incomplete stream
reports its context deadline, not successful completion or zero consumption.

Immediately before provider I/O, after queue admission and model resolution, the
Gateway calls `POST /operator/workflow/subscription-dispatch`. The daemon checks
the existing EventStore request reservation, current assignment/context, exact
model/catalog and expiry, then atomically records `ClaimSubscriptionCall` in the
workflow store. This command is internal, not a caller-selectable company command.
Each callback uses a fresh operation ID so an identical HTTP retry is denied after
consumption rather than replaying a reusable dispatch permission. The Gateway
neither retries this callback nor follows redirects. A lost response or cancellation
can consume permission without dispatch; it never makes a second call permissible.

All registered providers pass this check while the mode is configured. Unrelated
agent, Judge, Gaia, operator, background and local-loop requests cannot bypass it.
The checked raw HTTP request digest, server-derived context and immutable grant
remain stable across retries. Terminal result adoption additionally requires the
exact consumed dispatch. Local result recovery does not reauthorize provider I/O.
Reported tokens remain linked to that request. Codex `usage_price_table` values
are API-equivalent estimates, not ChatGPT billed spend, and are not committed as
actual subscription charges. Missing terminal usage remains unknown, never zero.

Changing or removing this runtime mode is a separate operator action, not an
automatic reset. Deployment must verify both services use the same bootstrap
configuration and that the daemon resolves the dynamic project grant before
provider activity is enabled. Restoring a pre-dispatch database snapshot is not
permission to repeat an external effect; review its outcome before reactivation.

### Codex OAuth transport limits

The pinned Codex CLI `0.151.0` remains an inference-only subprocess. It uses its
native ChatGPT login and endpoint selection; the Gateway does not extract or
copy its credentials. An explicit `sentinel_chatgpt` provider entry disables
HTTP and stream retries and WebSocket fallback. The separate
`unbounded_connection_retries` feature is also disabled. Overriding these fields
on the built-in `openai` entry would not work: the pinned CLI retains its built-in
provider entry instead of merging that configured replacement. Shell snapshots
are disabled along with tools and delegation because this process only returns
a proposal; actual tools belong to the Workbench sandbox.

These settings address hidden transport redispatch, not durable admission. The
existing request/outbox authority still owns whether an attempt may start, and
an ambiguous result must not authorize another call. A local conformance test
runs the actual pinned binary against a loopback-only fake Responses endpoint,
with an empty private home and no credentials. It verifies one inference request
for HTTP 500, stream disconnection, and successful completion, no advertised
tools, and preservation of successful response text and usage. It is not
ChatGPT authentication or live-product acceptance evidence.

`MaxTokens` is only a prompt request plus a conservative local response-byte
guard in this adapter. The pinned CLI's Responses request has no
`max_output_tokens` field. Terminal token counts measure reported usage; they
do not enforce a pre-consumption ceiling. A local deadline cancels the CLI but
does not prove that the remote service stopped generation immediately. Do not
report a timeout or missing terminal usage as zero consumption. Hard token-cap
acceptance therefore remains unverified; enabling this bridge requires an
explicitly approved provider-appropriate contract, not a fake local-provider
exemption or invented USD price.

The source contract is pinned to OpenAI Codex commit
`78c290807ce710180111df227df3b7a4fe845452`:
[provider selection and retry settings](https://github.com/openai/codex/blob/78c290807ce710180111df227df3b7a4fe845452/codex-rs/model-provider-info/src/lib.rs),
[Responses request fields](https://github.com/openai/codex/blob/78c290807ce710180111df227df3b7a4fe845452/codex-rs/codex-api/src/common.rs),
[stream retry handling](https://github.com/openai/codex/blob/78c290807ce710180111df227df3b7a4fe845452/codex-rs/core/src/responses_retry.rs),
and [feature defaults](https://github.com/openai/codex/blob/78c290807ce710180111df227df3b7a4fe845452/codex-rs/features/src/lib.rs).

The optional `TestCodexCLIPinnedBinarySingleAttemptTransport` test requires
`SENTINEL_TEST_CODEX_CLI_BINARY` to identify the pinned local executable and
`TMPDIR` to point to an issue-owned directory under `/work/tmp`. Without that
explicit binary it is skipped. The normal Go suite always checks the effective
argv contract. No binary download, login, or real provider request is performed
by the conformance test.

## Request binding

`WorkbenchRequest` binds the schema version, invocation and caller identities, agent, project, work item, workspace, assignment and credential generations, policy and profile digests, runtime key, effective capabilities, permitted artifact kinds, content-addressed inputs, command allowlist, resource limits, deadline, attempt, tool parameters, and canonical request digest.

The canonical SHA-256 omits only the `input_digest` field itself. Reusing an invocation ID with any changed bound value is a typed conflict. Unknown JSON fields and unsupported versions are rejected.

## Durable lifecycle

The daemon store uses these transitions:

```text
reserved -> executing -> succeeded
                     |-> failed
                     |-> cancelled
                     |-> timed_out
                     `-> unknown_outcome
```

`reserved` may also become `failed`, `cancelled`, or `timed_out` before launch. Terminal records are immutable. An identical terminal result is an idempotent replay; changed resources, artifacts, error classification, or outcome conflict instead of overwriting evidence.

After restart, a `reserved` request waits for the authoritative caller to replay the same digest-bound request and pass current authorization again. An `executing` request is never re-executed: the daemon sends a `recover` frame carrying the invocation ID and request digest. Before the runtime emits any terminal result, it atomically creates an immutable completion receipt in the artifact boundary. A restarted runtime returns the redacted receipt. A missing, malformed, mismatched, or conflicting receipt becomes durable `unknown_outcome` and requires manual recovery; it is never converted into an ordinary failure or authorization to repeat the tool effect. Terminal and unknown-outcome records remain subject to current assignment and generation authorization when replayed.

The completion receipt retains only outcome, resource accounting, artifact references, safe error classification, and canonical numeric command status (exit code and stdout/stderr byte counts). A failed command returns its bounded, redacted diagnostics to the immediate authorized caller, just like a successful command. Transient tool output and file contents are removed before persistence. Receipt writes are bounded, synced, and installed without overwrite; an existing receipt must match byte-for-byte after decoding.

Runtime receipt version 2 and daemon record version 3 retain this numeric status
across restart without repeating the command. The new readers also accept legacy
runtime receipts (version 1) and daemon records (version 2); absent diagnostics
remain absent, not inferred or reconstructed by rerunning old work. Old readers
reject new versions, so rollback must restore the matching prior state rather
than open a newer store with an older binary. Unknown outcomes remain blocked.
This numeric feedback is a prerequisite, not a claim that the complete model
correction loop or a restart-safe private diagnostic channel is implemented.

### Same-work-item execution revisions

The workflow core can admit a bounded new execution plan for the same work item
after `done` or a confirmed `execution_failed` result. The caller must bind the
previous plan ID, plan digest, work version, complete state digest, and feedback
digest, and present the unchanged active execution authority. A feedback digest
identifies evidence; it grants no authority and does not attest its quality.
Unknown, timed-out, cancelled, or still-running outcomes cannot authorize a
revision.

One SQLite transaction archives the complete predecessor in the existing
`workflow_operations` journal, replaces the current execution, advances the work
version, and appends the new first-step outbox entry and admission event. A failed
outbox insert rolls back the entire transition. No second database or schema
migration is required. Prior plans, execution rows, completion and gate receipts,
and operation IDs remain immutable. Old completed receipt replays resolve their
archived plan and cannot overwrite the current execution. Historical admission
replays remain effect-free after their original deadline.

Admission checks the complete predecessor receipt chain, rejects reused plan or
invocation identities, and permits at most four revisions per work item. Every
new revision needs fresh deadlines and new step/invocation IDs. Reusing the same
admission ID with changed feedback is an idempotency conflict.

This core API does not itself reopen a company assignment, authorize another
provider call, supply private diagnostics to a model, or complete the productive
model-feedback loop. Those boundaries must explicitly use the revision contract;
ordinary initial-plan admission still rejects a different plan for existing work.

The company layer has a separate internal `RequestWorkCorrection` command. Only
the project's manager or technical lead can request it, against exact company
and execution versions. It validates the completed execution and output bindings,
archives the previous company work item, and reopens that same work item as
`assigned` without changing its employee or resetting execution/provider records.
The correction record retains the feedback reference and digest, prior outputs,
gate receipt, and assignment history. Empty correction history remains absent
from legacy project encodings. Older readers cannot open aggregates containing
new correction fields; rollback requires matching prior state.

Corrections are rejected if an affected dependent has already advanced beyond
dependency-pending, a handoff exists, or a relevant project blocker is unresolved.
They do not silently invalidate consumed artifacts. Periodic company-state
synchronization ignores a predecessor superseded by a correction, so its old
`done` cannot complete the new attempt. Later assignment changes preserve the
archived assignment rather than rewriting history.

Complete correction history is visible only to the operator and the exact
governed project manager or technical lead. Ordinary command responses omit
these archived assignment, output, and feedback bindings for other participants.

This command is not accepted through ordinary customer, agent, or operator HTTP
commands. The dedicated `POST /agent/workflow/corrections` route requires an
authenticated, governed project manager or technical lead. It uses the ordinary
operation-ID/command envelope, but checks delivery exclusion under the exclusive
workflow mutation fence before reopening work. Any materialized delivery rejects
internal correction; customer delivery rework remains a different workflow.

The route also requires the exact consumed subscription dispatch, committed
usage event with nonzero model output, and the actual execution plan bound to
that provider request. Normal completion removes the outbox payload; its absence
is accepted only with those durable bindings and no operator-resolution marker.
If the payload still exists, it must be `action_claimed`, match the committed
usage event, and contain the proposal actually adopted as that execution.
A provider status or missing row alone is not proof. Unresolved, foreign or
changed provider evidence fails closed. The
company transaction then verifies the full terminal execution receipt chain.
An exact successful operation replay cannot authorize another call or revision.

The optional `next_subscription_grant` creates a fresh single-call allowance in
the same transaction as correction. Its predecessor must have been consumed and
must name the same work, employee and assignment. The old allowance and dispatch
remain in the correction history, and campaign accounting counts both old and
new identities without double-counting unchanged snapshots. An invalid new grant
rolls back the entire correction. Archived allowances cannot be claimed again.
The daemon selects a unique current project allowance for the assigned employee;
multiple candidates are rejected. Correction admission alone does not invoke the
provider.

Model correction context binds the stored correction, predecessor plan/state,
feedback reference, bounded feedback observations and prior model-authored tools.
The correction route requires `feedback` with `summary` and `artifact_digest`.
Its canonical digest must equal the revision's feedback digest. An existing
output requires its exact sealed artifact digest; a failed execution without an
output uses null. The normal bounded-workflow-text limit applies to the summary.
These are authenticated leadership observations, not proof of independent QA or
permission to execute instructions embedded in feedback. No private tool log is
copied automatically. Legacy stored corrections without feedback remain readable.
Those tools are untrusted
context, not replay instructions or a claim about actual artifact bytes. The
model must propose a complete corrected sequence within the original profile.
Context is bounded by the existing model-work byte ceiling. Adoption rechecks
that context and uses revision admission instead of overwriting the old plan.
Historical execution receipts remain readable after restart.

Source integration tests use controlled model proposals and injected execution
outcomes. They are not evidence of a live model correction, artifact inspection,
independent QA or customer acceptance.

### Runtime quiescence and unresolved outcomes

A durable `executing` row is not proof that a tool process still exists. A lost
response can leave the workflow blocked with an unknown outcome after its
isolated process has been cleaned up. The row must retain its digest and
no-reexecution protection, but it must not indefinitely prevent fresh logical
runtime snapshots.

Before a periodic logical runtime snapshot, the daemon reads the current owning
adapter's `workbench_quiescent` observation under the World tick barrier. This
operation accepts no payload and performs no process, tool, or storage mutation.
The adapter checks the exact runtime incarnation. Running exchanges, pending
cleanup, partial ownership, missing handles, malformed responses, and stale World
authority keep the fence closed. Reserved requests still block transitions.
Successfully cleaned exchanges or a committed fresh idle runtime can release the
logical snapshot fence without changing the unresolved invocation or its workflow
state. Shift changes, whole-World snapshots, and restore admission retain the
existing durable-invocation fence; a quiescence observation is not sufficient to
close their broader recovery contract.

Graceful shutdown saves the logical roster only after every owning adapter has
successfully stopped its workloads. Unfinished shifts and restore fences still
prevent that snapshot. This is not Workbench result recovery or a whole-product
backup: retained ambiguous results still require their existing authorized
recovery path and cannot be accepted, discarded, or executed again by this check.

## Workspace and tools

Workspace roots are assigned per project and work item. Tool paths are relative, parent traversal and absolute paths are rejected, and symlinks are denied at effect boundaries. Input mounts are explicit and content-addressed. Outputs remain inside the assigned workspace or artifact root.

The runtime derives the only accepted workspace ID as `<project_id>:<work_item_id>` and maps it to `/workspace/<project_id>/<work_item_id>`. The matching artifact boundary is `/artifacts/<project_id>/<work_item_id>`. Declared input files live under the separately read-only `/workspace/.inputs/<project_id>/<work_item_id>` bind and must match both their SHA-256 and `sha256:<digest>` artifact binding before any effect begins. Input inspection and exact declared command arguments resolve through that boundary; undeclared `.inputs` arguments, parent replacement, and writes remain unavailable. Command arguments otherwise reject absolute, parent-relative, and home-relative paths.

For `agent-runtime`, bubblewrap binds `/workspace` and `/artifacts` from symlink-rejected subdirectories of that agent's private backing filesystem and overlays `/workspace/.inputs` from a mandatory read-only sibling. The broad agent-home, `/company`, and resolver-file binds used by non-workbench agent sandboxes are absent. The parent daemon environment is cleared before bwrap starts and replaced with the four immutable profile variables. Workbench startup accepts only the wrapper's actual irreversible `FullyEnforced` Landlock result for the requested ABI; partial, absent, or mismatched enforcement rolls the spawn back. Landlock permits only the fixed runtime and profile toolchain executables; its `/proc` read grant sees only bwrap's private PID namespace and supports bounded process-group accounting. The cgroup sets the M0 memory and process ceilings and those ceilings are restored before every invocation. The executor additionally measures the command process group and enforces request CPU-time, memory, process-count, wall-time, output, and file-size limits.

The M0 tools are:

- bounded UTF-8 file inspection;
- atomic file creation/update with optional digest precondition;
- digest-bound text replacement with expected occurrence counts;
- allowlisted command and test execution with a cleared environment;
- immutable artifact-manifest packaging for request-declared artifact kinds.

Packaging installs every file as an immutable SHA-256 blob and writes a digest-named immutable manifest that binds the invocation, authority digests, workspace, file paths, blob IDs, and sizes. Sandbox and daemon acceptance pin the scoped directories and read each manifest and blob only through a no-follow descriptor after checking its device, inode, owner, mode, link count, and size. The manifest never embeds private file content.

The immutable start configuration is selected from the accepted project family
and the assigned role. Web developers use `web-authoring-v1`; Python and Node
developers use `python-coding-v1` and `node-coding-v1`. Design and independent
model review retain their separate profiles. Native technical QA uses
`coding-qa-v1`, not the developer's writable tool policy. Project and Workbench
profiles must match the exact installed release bytes, generation and digest;
missing native profiles never fall back to the web profile. Existing accepted
agreements retain their original binding.

Autonomous recovery distinguishes a recorded policy from new execution
authority. The release recognizes the exact committed pre-observation Web
policy only to inspect an existing outcome; it never aliases that digest to
the current tool grants. Before admitting another model review, recovery checks
the persisted project/version, agreement, work inventory, independent passing
QA, immutable manifest, active release and customer-bound delivery. Only a
matching already-issued outcome is a no-op. Missing or changed lineage remains
pending work, and corrupt references remain errors. This path neither renews
an expired preview nor records customer acceptance, and delivery publication
continues to drain independently. New tool execution still requires exact
installed profile bytes and the current capability intersection.
Recorded QA recognition retains complete required-case coverage, assertion and
attempt bindings, independent authority and passing outcomes. It does not
re-authorize a previously imported flake disposition at a later clock boundary;
new evidence import still rejects expired dispositions. Impossible original
gate validity windows never establish a settled outcome.

The selected file SHA-256 is carried as `tool_profile_digest`. The daemon rejects
requests whose runtime, capability set, artifact kinds, command rules, test suite,
environment contract, or resource limits exceed that exact profile. Profile
replacement therefore requires a new digest-bound request; there is no silent
live mutation.

### Native independent QA

`sentinel-coding-qa` stages the exact immutable declared input inventory while
preserving its relative package tree, including empty Python `__init__.py`
files. Its inventory-only mode establishes source integrity, not code quality.
Final QA runs syntax checks and any discovered unittest or Node test suites;
their failures reject the candidate, but their self-reported counters are not
authority for a passing receipt.

Each native candidate also includes `sentinel-qa.json`, a bounded behavioral
test plan. For example, a Python CLI that adds two integers can declare:

```json
{"schema_version":1,"cases":[{"id":"add-positive","script":"app.py","args":["2","3"],"stdin":"","expected_stdout":"5\n","expected_stderr":"","expected_exit":0}]}
```

The trusted evaluator validates the plan before execution and compares each
child's observed stdout, stderr and exit status with these expectations. Only
the evaluator counts successful cases. Candidate code cannot mint a passing
receipt by printing a forged unittest or JSON summary. Empty plans, duplicate
case IDs, cross-family scripts, traversal and vacuous assertions are rejected.
The separate independent model review determines whether the declared cases
cover the accepted requirements; a behavioral pass alone is not that judgment
or a security assessment of arbitrary code.

Each workbench has a private cgroup namespace rooted at its cumulative agent
parent. The host captures that namespace while the cumulative
agent parent is empty, before delegating its controllers. A trusted helper is
reaped before delegation; the bwrap child joins the runtime leaf first and then
the pinned namespace. Runtime and command leaves are therefore reachable within
the same restricted hierarchy. The host-root cgroup namespace is never used as
the product workaround, and existing controllers are never disabled.

Before releasing the bwrap startup barrier, the host pins the owned init's PID
and mount namespaces and installs a second, private procfs at
`/run/sentinel-command-proc`. It contains only this agent's PID namespace, not
host processes. The trusted broker needs this full procfs to create nested PID
namespaces: bubblewrap's masked `/proc` alone cannot authorize a nested procfs
mount. Controller Landlock grants no access to the private mount, and command
mount construction never imports it. No extra capability is granted to employee
or candidate code. The privileged setup uses immutable `util-linux` helpers,
fixed argv, pinned namespace/root descriptors and a bounded startup deadline.
The private mount stays writable for kernel nested-proc mount eligibility;
this is not a controller or candidate write grant. After a terminal invocation,
the owned runtime is reaped and its complete cgroup tree is removed before a
replacement captures a fresh namespace; the workload identity and terminal
receipt remain retained.

The runtime starts one constrained namespace launcher inside its outer
bubblewrap sandbox before applying irreversible controller Landlock. Landlock
does not permit a restricted process to construct new mounts, so the evaluator
does not invoke bubblewrap itself. It sends bounded, token-bound requests to
that trusted sibling. The launcher constructs each command namespace, applies
mandatory child Landlock, and disables further user namespaces before exec.
There is no unrestricted command fallback.

For QA, the token binds the current evaluator, candidate workspace, command
cgroup, deadline and resource ceilings. Candidate children see a read-only
tree and a separate writable scratch directory, but no launcher socket, token,
receipt directory or foreign workspace. They cannot rewrite the measured
source or tests. Network access remains denied. Disconnect, timeout or failed
isolation aborts the operation and requires full cgroup cleanup before success.

The trusted summary records separate inventory, syntax and behavioral stages
as `pass`, `fail`, `error` or `not_run`, with planned and observed counts. The
runtime accepts only bounded, complete summaries from the exact installed QA
program and matching family/suite. These safe facts persist with the invocation
and replay after restart; raw candidate output stays private. A missing tool,
timeout or truncated summary cannot claim that the later assertions ran.
The native QA profile permits one family selector plus at most 64 input paths;
other profiles retain their own smaller argument and resource ceilings.

Command cancellation and deadlines quiesce the owned cgroup, including detached
descendants; a process-group signal alone is not a cleanup receipt. The sandbox
and cgroup layer enforce CPU, memory, process, filesystem and network limits;
the runtime also bounds I/O and wall-clock handling.

## Output acceptance and redaction

Before terminal commit, the daemon checks that the current assignment and capability intersection still match the persisted authority generations, every artifact kind was declared by the reserved request, every artifact identifier matches its lowercase SHA-256, manifest paths are relative, identifiers are unique, and success/error combinations are coherent. Persisted records contain only authority bindings, effective capabilities, state, timing, resource accounting, safe error data, and artifact references. Event operation IDs bind the invocation to the exact redacted payload digest, so retrying publication is idempotent while distinct lifecycle states remain observable.

Command stdout/stderr are bounded and redacted in the transient response. Environment values, credentials, request content, private file content, and raw tool output must not appear in logs, events, projections, or durable workbench records.

### Read-only artifact access

`read_verified_artifact_file` reuses the manifest resolution used by input
staging, but never creates a destination workspace or copies files. The caller
must authorize the delivery and supply its exact source agent, project and
manifest digest; this filesystem primitive does not grant customer authority.
It accepts only a manifest-declared relative path and a positive byte ceiling
no larger than 4 MiB. Missing, ambiguous, foreign or physically misbound manifests
fail closed. Descriptor-pinned blob reads reject hardlinks, writable files, size
changes and hash mismatches. These checks do not themselves provide HTTP preview
serving or browser isolation; those boundaries require separate integration.

## Verification boundary

Focused protocol, authorization, workspace, tool, idempotency, cancellation, and restart tests are necessary but do not prove production isolation. Issue #694 can close only after #75 and #472 are merged and the exact merged release is verified on the authorized single-node target behind an issue-specific VM snapshot. Live evidence must include positive static-site work, denied unassigned-agent and network probes, runtime selection, cgroup/Landlock/capability readback, events/projection, artifact digests, restart counters, and secret scans.
