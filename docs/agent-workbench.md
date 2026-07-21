# Agent Workbench

This document describes the M0 `web-project-v1` workbench implementation and its fail-closed production boundaries. The normative product contract remains [Virtual Company Work Execution](virtual-company-work-execution.md).

## Production path

Tool-bearing agent work has one supported path:

1. The daemon resolves a current assignment and constructs an authority snapshot.
2. The request is authorized against the intersection of agent, role, assignment, project, and tool-profile capabilities.
3. The daemon reserves the digest-bound invocation in its durable workbench store.
4. `NanoRuntimeRegistry` selects the configured secure runtime. M0 requires `bwrap-landlock`; an unavailable or mismatched runtime fails closed.
5. The daemon launches `agent-runtime` inside the #75 full-cage sandbox and exchanges versioned JSONL messages.
6. The daemon validates the result against the reserved request and declared artifact kinds before committing terminal state and publishing safe events.

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
                     `-> timed_out
```

`reserved` may also become `failed`, `cancelled`, or `timed_out` before launch. Terminal records are immutable. An identical terminal result is an idempotent replay; changed resources, artifacts, error classification, or outcome conflict instead of overwriting evidence.

Recovery dispatches a `reserved` request, probes an `executing` request, and replays a terminal record. It must never blindly execute an `executing` request after restart. Private command output and file contents are not persisted in the invocation record.

## Workspace and tools

Workspace roots are assigned per project and work item. Tool paths are relative, parent traversal and absolute paths are rejected, and symlinks are denied at effect boundaries. Input mounts are explicit and content-addressed. Outputs remain inside the assigned workspace or artifact root.

The M0 tools are:

- bounded UTF-8 file inspection;
- atomic file creation/update with optional digest precondition;
- digest-bound text replacement with expected occurrence counts;
- allowlisted command and test execution with a cleared environment;
- immutable artifact-manifest packaging for request-declared artifact kinds.

The immutable start configuration is `config/workbench-profiles/web-authoring-v1.toml`. Its file SHA-256 is carried as `tool_profile_digest`. The daemon rejects requests whose runtime, capability set, artifact kinds, command rules, test suite, environment contract, or resource limits exceed that exact profile. Profile replacement therefore requires a new digest-bound request; there is no silent live mutation.

Commands run in a new process group. Cancellation or deadline expiry kills the group. The sandbox/cgroup layer remains authoritative for CPU, memory, process, syscall, capability, filesystem, and network enforcement; the runtime also applies bounded I/O and wall-clock handling.

## Output acceptance and redaction

Before terminal commit, the daemon checks that every artifact kind was declared by the reserved request, every artifact identifier matches its lowercase SHA-256, manifest paths are relative, identifiers are unique, and success/error combinations are coherent. Persisted records contain only authority bindings, state, timing, resource accounting, safe error data, and artifact references.

Command stdout/stderr are bounded and redacted in the transient response. Environment values, credentials, request content, private file content, and raw tool output must not appear in logs, events, projections, or durable workbench records.

## Verification boundary

Focused protocol, authorization, workspace, tool, idempotency, cancellation, and restart tests are necessary but do not prove production isolation. Issue #694 can close only after #75 and #472 are merged and the exact merged release is verified on the authorized single-node target behind an issue-specific VM snapshot. Live evidence must include positive static-site work, denied unassigned-agent and network probes, runtime selection, cgroup/Landlock/capability readback, events/projection, artifact digests, restart counters, and secret scans.
