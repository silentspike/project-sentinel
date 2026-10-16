# Command Sandbox LAB Fixture

Runtime target class: NONE (source and builder checks only).
Deploy targets: none. Read-only runtime targets: none. Benchmarks: N/A.
Kernel execution requires a separate admitted, sanitized LAB lease and its
owner's exact target/rollback declaration. Production and cluster access are
outside this fixture. Do not alter a frozen aggregate checkout or old archives.

## Artifacts And Mount

The source-bound `sentinel-fs-lab-mount` binary belongs to `sentinel-fs` and
requires its existing `fuse-tests` feature. Package it with the matching test
archive, canonical `agent-runtime` and `landlock-wrapper`; record their source
and file hashes. A helper from a different candidate is not equivalent evidence.

Its interface is:

```text
sentinel-fs-lab-mount DATA_DIR MOUNTPOINT WORKSPACE_BUDGET_BYTES
```

Both directories must already exist, be absolute/canonical, caller-owned,
mode 0700, empty and nonoverlapping. Existing stores, symlink aliases and a
second use of a data directory are rejected. Use fresh lease-owned paths under
`/work/tmp/project-sentinel/`; never copy production stores or identities.

The helper creates real CAS/metadata/artifact stores, initializes the base and
`AGENT-07` namespace, sets its actual LayerManager budget, and blocks in the
canonical `start_fuse_layer` implementation. The budget must be between 1 MiB
and the existing LayerManager maximum; 4194304 bytes is a small useful fixture.
The `lab_mount_configured` stderr record is configuration, NOT mount readiness.
The lease owner must verify a live FUSE mount with source `sentinel-fs` at the
exact mountpoint and use its actual `AGENT-07` directory as the workspace root.
No company daemon, provider credential or model call is needed.

## Broker And Cgroups

Use the matching immutable root-owned runtime at its canonical executable path.
Trusted fixture startup uses its existing broker mode:

```text
/usr/bin/agent-runtime --workbench-command-broker-v1 SOCKET WORKSPACE_ROOT INPUT_ROOT COMMAND_CGROUP_ROOT CONTROLLER_PID
```

Before spawning the broker or test descendants, attach the trusted controller
to a lease-owned runtime cgroup. Keep command children in a separate delegated
cgroup-v2 subtree with CPU/I/O/memory/PID controllers, limit/accounting files
and `cgroup.kill`. Root ownership and the actual filesystem must match the
broker's existing checks. Never substitute ordinary directories or put the
controller in the command subtree it must later kill.

The broker pins the live controller PID/start identity. Create a fresh private
UUID directory under `/run/sentinel-command-broker` with mode 0700; the real
broker creates its mode-0600 `socket`. Keep it alive for the serialized tests.
The runtime validates both socket and cgroups; startup must wait for real
readiness and retain broker/controller identities, not create a fake socket.

The test binary is a trusted fixture controller, not a model-controlled agent.
The attested outer `landlock-wrapper` accepts canonical `agent-runtime`, not an
arbitrary test binary. Do not impersonate that entrypoint or disable wrappers.
These tests exercise the real broker's command bwrap/Landlock/cgroup path.
Complete outer-agent startup remains a separate SandboxEnforcer acceptance
obligation; fixture execution alone is not evidence for that broader claim.

## Fixture Environment And Paths

Only the 12 ignored privileged tests call `Fixture::kernel`. Its workspace root
must be an actual FUSE filesystem; mount identity and source binding are also
verified by the lease owner. Supply:

| Variable | Actual value |
| --- | --- |
| `SENTINEL_COMMAND_TEST_RUNTIME` | Matching canonical runtime binary |
| `SENTINEL_COMMAND_TEST_WORKSPACE_PARENT` | Mounted `AGENT-07`, equal to broker `WORKSPACE_ROOT` |
| `SENTINEL_COMMAND_TEST_INPUT_ROOT` | Private root equal to broker `INPUT_ROOT` |
| `SENTINEL_COMMAND_TEST_CGROUP_ROOT` | Actual broker `COMMAND_CGROUP_ROOT` |
| `SENTINEL_COMMAND_TEST_WORKSPACE_BUDGET_BYTES` | The exact budget passed to the real mount helper |
| `SENTINEL_COMMAND_BROKER_SOCKET` | Live broker's private socket |
| `TMPDIR` | Separate lease-owned directory under `/work/tmp/project-sentinel/` |

Each fixture owns a random project directory. Its direct workspace and
WorkbenchExecutor request both resolve to `WORKSPACE_ROOT/<project>/workspace`.
Inputs resolve to `INPUT_ROOT/<project>/workspace`. Requests bind those same
project/work/workspace IDs before computing their canonical digest. Artifacts
and foreign data remain separate private state. The broker's exact two-component
scope, declared-input and child-cgroup validators are unchanged.
The Python fixture rule explicitly grants its existing `-I` prefix; ungranted
flags remain rejected for both Python and Node by unchanged production policy.

The two normal negative tests and local layout/cleanup regressions use
`Fixture::new`, which never reads the privileged environment. They remain
ordinary builder/normal-shard tests, not implicit kernel tests.

## Execution And Cleanup Evidence

Execute all original 12 ignored IDs without changing their assertions or ignore
classification. Serialize them on a fresh namespace: the real budget is per
agent, not per temporary project directory. An environment value or generic
mount cannot establish budget enforcement. The aggregate-growth test must still
observe actual EDQUOT after many small writes and on truncate growth; per-file
RLIMIT or host disk exhaustion is not equivalent.

Retain actual stdout/status, parent/foreign isolation checks, Python/Node
behavior, kernel resource accounting, genuine exit 126, descendant quiescence,
cancellation/deadline outcomes and receipt recovery. A failure remains a failure;
do not replace it with an unsupported-kernel label without the actual reason.

Per-command membership must report quiescence before success. Fixture cleanup
removes only its owned workspace/input project; input cleanup failure fails a
non-unwinding test. After the last test, the lease owner must prove no populated
command children, stop the owned controller/broker, remove only its private
socket directory, unmount the exact owned FUSE mount, wait for the helper to
exit, and read back cleanup before resetting the sanitized lease. Preserve
failed-run evidence and original archive provenance. No kernel PASS is claimed
by source tests, helper configuration output or successful compilation.
