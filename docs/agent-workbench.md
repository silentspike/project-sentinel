# Agent Workbench

This document describes the M0 `web-project-v1` workbench implementation and its fail-closed production boundaries. The normative product contract remains [Virtual Company Work Execution](virtual-company-work-execution.md).

## Production path

Tool-bearing agent work has one supported path:

1. The daemon resolves a current assignment and constructs an authority snapshot.
2. The request is authorized against the intersection of agent, role, assignment, project, and tool-profile capabilities.
3. The daemon reserves the digest-bound invocation in its durable workbench store.
4. `NanoRuntimeRegistry` selects the configured secure runtime. M0 requires `bwrap-landlock`; an unavailable or mismatched runtime fails closed.
5. The daemon launches `agent-runtime` inside the #75 full-cage sandbox and exchanges versioned JSONL messages.
6. The daemon re-resolves current authority, validates the result against the reserved request and declared artifact kinds, then commits terminal state and publishes an idempotent safe event. Revoked or stale authority is rejected before runtime I/O and again before output acceptance.

There is no host-shell, ECS-only, or less-isolated fallback for an M0 tool request.

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

## Workspace and tools

Workspace roots are assigned per project and work item. Tool paths are relative, parent traversal and absolute paths are rejected, and symlinks are denied at effect boundaries. Input mounts are explicit and content-addressed. Outputs remain inside the assigned workspace or artifact root.

The runtime derives the only accepted workspace ID as `<project_id>:<work_item_id>` and maps it to `/workspace/<project_id>/<work_item_id>`. The matching artifact boundary is `/artifacts/<project_id>/<work_item_id>`. Declared input files live under the separately read-only `/workspace/.inputs/<project_id>/<work_item_id>` bind and must match both their SHA-256 and `sha256:<digest>` artifact binding before any effect begins. Input inspection and exact declared command arguments resolve through that boundary; undeclared `.inputs` arguments, parent replacement, and writes remain unavailable. Command arguments otherwise reject absolute, parent-relative, and home-relative paths.

For `agent-runtime`, bubblewrap binds `/workspace` and `/artifacts` from symlink-rejected subdirectories of that agent's private backing filesystem and overlays `/workspace/.inputs` from a mandatory read-only sibling. The broad agent-home, `/company`, and resolver-file binds used by non-workbench agent sandboxes are absent. The parent daemon environment is cleared before bwrap starts and replaced with the four immutable profile variables. Landlock permits only the fixed runtime and profile toolchain executables; its `/proc` read grant sees only bwrap's private PID namespace and supports bounded process-group accounting. The cgroup sets the M0 memory and process ceilings and those ceilings are restored before every invocation. The executor additionally measures the command process group and enforces request CPU-time, memory, process-count, wall-time, output, and file-size limits.

The M0 tools are:

- bounded UTF-8 file inspection;
- atomic file creation/update with optional digest precondition;
- digest-bound text replacement with expected occurrence counts;
- allowlisted command and test execution with a cleared environment;
- immutable artifact-manifest packaging for request-declared artifact kinds.

Packaging installs every file as an immutable SHA-256 blob and writes a digest-named immutable manifest that binds the invocation, authority digests, workspace, file paths, blob IDs, and sizes. The manifest never embeds private file content.

The immutable start configuration is `config/workbench-profiles/web-authoring-v1.toml`. Its file SHA-256 is carried as `tool_profile_digest`. The daemon rejects requests whose runtime, capability set, artifact kinds, command rules, test suite, environment contract, or resource limits exceed that exact profile. Profile replacement therefore requires a new digest-bound request; there is no silent live mutation.

Commands run in a new process group. Cancellation or deadline expiry kills the group. The sandbox/cgroup layer remains authoritative for CPU, memory, process, syscall, capability, filesystem, and network enforcement; the runtime also applies bounded I/O and wall-clock handling.

## Output acceptance and redaction

Before terminal commit, the daemon checks that the current assignment and capability intersection still match the persisted authority generations, every artifact kind was declared by the reserved request, every artifact identifier matches its lowercase SHA-256, manifest paths are relative, identifiers are unique, and success/error combinations are coherent. Persisted records contain only authority bindings, effective capabilities, state, timing, resource accounting, safe error data, and artifact references. Event operation IDs bind the invocation to the exact redacted payload digest, so retrying publication is idempotent while distinct lifecycle states remain observable.

Command stdout/stderr are bounded and redacted in the transient response. Environment values, credentials, request content, private file content, and raw tool output must not appear in logs, events, projections, or durable workbench records.

## Verification boundary

Focused protocol, authorization, workspace, tool, idempotency, cancellation, and restart tests are necessary but do not prove production isolation. Issue #694 can close only after #75 and #472 are merged and the exact merged release is verified on the authorized single-node target behind an issue-specific VM snapshot. Live evidence must include positive static-site work, denied unassigned-agent and network probes, runtime selection, cgroup/Landlock/capability readback, events/projection, artifact digests, restart counters, and secret scans.
