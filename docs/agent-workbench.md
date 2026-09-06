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

### Single-call subscription allowance

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

The completion receipt retains only outcome, resource accounting, artifact references, and safe error classification. Transient tool output and file contents are removed before persistence. Receipt writes are bounded, synced, and installed without overwrite; an existing receipt must match byte-for-byte after decoding.

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

## Verification boundary

Focused protocol, authorization, workspace, tool, idempotency, cancellation, and restart tests are necessary but do not prove production isolation. Issue #694 can close only after #75 and #472 are merged and the exact merged release is verified on the authorized single-node target behind an issue-specific VM snapshot. Live evidence must include positive static-site work, denied unassigned-agent and network probes, runtime selection, cgroup/Landlock/capability readback, events/projection, artifact digests, restart counters, and secret scans.
