# agent-runtime

## Purpose

`agent-runtime` is the capability-scoped tool process launched inside an agent sandbox. It accepts only the versioned Project Sentinel workbench protocol and executes one digest-bound operation at a time inside the workspace and resource boundary supplied by the daemon.

## Interfaces

- stdin and stdout use newline-delimited JSON (`WorkbenchCommand` and `WorkbenchMessage`).
- Commands cover execute, cancel, and health. Messages cover progress, result, cancellation, health, and safe protocol errors.
- The runtime rejects unknown schema versions, unknown fields, stale deadlines, digest conflicts, undeclared capabilities, paths outside the assigned workspace, symlinks at effect boundaries, and commands outside the digest-bound allowlist before the requested effect.
- Supported M0 tools inspect UTF-8 files, atomically create or update files, apply digest-bound text patches, run allowlisted commands/tests, and commit immutable content-addressed artifact manifests.
- EOF requests bounded shutdown. Cancellation and deadline expiry terminate the complete process group for a running command.
- stderr carries lifecycle diagnostics only. Request content, environment values, command output, and credentials are not logged.

## Dependencies

- The shared `sentinel-common` protocol types and bounded serialization/digest helpers.
- Runtime host enforcement comes from the parent sandbox: bwrap, Landlock, namespaces, cgroups, and daemon process supervision.

The process does not establish the security boundary by itself. Production tool-bearing work must be launched through the daemon's authorized `NanoRuntimeRegistry` bwrap selection; direct host execution is not a supported production path.

## Verify

```bash
cargo remote -c -- test -p agent-runtime
cargo remote -c -- build -p agent-runtime --release
```

Live behavior is verified through the daemon workbench and sandbox acceptance path because the daemon owns authority, durable invocation state, runtime selection, launch, and supervision.
