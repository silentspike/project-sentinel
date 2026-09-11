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

### Opt-in model work proposals

The first M1 bridge is selected by `SENTINEL_MODEL_WORKBENCH_ENABLED=true`.
It is off by default, requires the enabled company workflow, the `llm` feature,
and `SENTINEL_LLM_USAGE_V2_ENABLED=true`, and is visible as
`company_workflow.model_work_enabled` in runtime health. Enabling it is not a
claim that the autonomous-company acceptance has passed.

For one assigned Designer or Developer with exactly one active work-item
provider reservation, the daemon derives a task and authority snapshot from
the existing workflow. The initial slice accepts one output contract and no
upstream artifact inputs. The provider window is five minutes from the durable
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

This bridge is not the full M1 conversation/tool-result loop. Additional inputs,
iterative rework, model-selected team decisions,
and the real-provider customer-to-artifact journey remain #856 acceptance work.
In particular, the existing monetary reservation API must not be presented as a
valid substitute for ChatGPT subscription call/token/time limits or be populated
with an invented marginal USD price. The activation stays off until that provider
contract and the target-runtime readiness are verified. Test fixtures use the
existing `local-loop` exemption, not a production OAuth exemption.

### Pre-agreement Sales inquiries

Sales consultation uses the existing customer request, not a fabricated project
or work assignment. The daemon resolves the configured Sales principal and its
healthy runtime, freezes the exact request version and conversation, and sends
it through the normal authenticated Gateway/Cortex agent-runtime route.
The provider returns a strict `ask_question` decision; neither tests nor the
operator supply its answer. Only the customer may answer that question or accept
a subsequent proposal. This initial consultation increment does not implement
automatic proposal generation or the complete autonomous company.

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
server. The configured allowance must equal the canonical operation-derived ID.
The request body cannot raise the configured total ceiling. Both services must
bind the same allowance and catalog before any provider effect is enabled.
The existing credential initializer provisions the independent `workflow-operator`
credential and the daemon loads it through systemd. It is not a customer or
employee credential and is not exposed to the customer dashboard.

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

The project retains its allowance permanently, including a consumed or unknown
outcome. It cannot create a replacement grant or mix a money reservation into the
same work item. Projects without an allowance retain their previous serialized
bytes and digests. No separate admission database or monetary exemption is added.
An older release that does not know this additive project field must not open an
allowance-bearing store. Roll back through the declared compatible backup/restore
procedure, with provider activity disabled and external outcomes accounted for.

Set the same `SENTINEL_MODEL_WORK_ALLOWANCE_ID` on the daemon and Gateway for this
bounded mode. Configure it only after creating the grant through the authenticated
company command API, with provider activity still disabled during preparation.
The daemon also requires model work and usage-v2. The Gateway requires its existing
protected operator credential and a loopback-only `SENTINEL_OPERATOR_API_URL`.
A subscription-marked request without the Gateway mode fails closed.

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
automatic reset. Deployment must verify both services use the same grant before
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
The configured allowance selector still explicitly controls which new grant may
dispatch; correction admission alone does not invoke the provider.

Model correction context binds the stored correction, predecessor plan/state,
feedback reference and prior model-authored tools. Those tools are untrusted
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

The immutable start configuration is `config/workbench-profiles/web-authoring-v1.toml`. Its file SHA-256 is carried as `tool_profile_digest`. The daemon rejects requests whose runtime, capability set, artifact kinds, command rules, test suite, environment contract, or resource limits exceed that exact profile. Profile replacement therefore requires a new digest-bound request; there is no silent live mutation.

Commands run in a new process group. Cancellation or deadline expiry kills the group. The sandbox/cgroup layer remains authoritative for CPU, memory, process, syscall, capability, filesystem, and network enforcement; the runtime also applies bounded I/O and wall-clock handling.

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
