# Patch Registry Negative Tests

The fixtures are isolated temporary repositories created by
`scripts/tests/test_check_patch_registry.py`. They do not alter the real Cargo tree.
The checker date is pinned to `2026-07-24` for deterministic expiry behavior.

## Unregistered Override

```text
[unregistered] exit=1
ERROR[UNREGISTERED_OVERRIDE] `patch:Cargo.toml:crates-io:demo` (git=https://example.invalid/upstream/demo;rev=abc123)
```

## Stale Registry Row

```text
[stale] exit=1
ERROR[STALE_REGISTRY_ROW] `patch:Cargo.toml:crates-io:demo` has no active Cargo override
```

## Missing Required Field

```text
[missing-field] exit=1
ERROR[MISSING_FIELD] entry[0] missing `owner`
```

## Expired Temporary Fork

```text
[expired-fork] exit=1
ERROR[EXPIRED_TEMPORARY_FORK] entry[0] expired on 2026-07-24
```

All four failures are asserted by the unit suite. A registered patch with an exact
source match passes.

## Direct-Git Allowlist

The official source fixture passes:

```text
[official-git] exit=0
```

A source URL change to a fork fails:

```text
[fork-url] exit=1
ERROR[GIT_DEPENDENCY_MISMATCH] `git:Cargo.toml:dependencies:demo` field `source` allowlisted `git=https://example.invalid/upstream/demo` actual `git=https://example.invalid/fork/demo`
```

A new direct Git dependency without an allowlist row fails:

```text
[new-git] exit=1
ERROR[UNALLOWLISTED_GIT_DEPENDENCY] `git:Cargo.toml:dependencies:demo` (git=https://example.invalid/upstream/demo)
```

A stale allowlist row fails:

```text
[stale-git] exit=1
ERROR[STALE_GIT_ALLOWLIST_ROW] `git:Cargo.toml:dependencies:demo` has no direct Cargo Git dependency
```

## Additional Override Mechanisms

Registered `[replace]` and Cargo source-replacement fixtures pass. Their
unregistered counterparts fail:

```text
[unregistered-replace] exit=1
ERROR[UNREGISTERED_OVERRIDE] `replace:Cargo.toml:demo:1.2.3` (git=https://example.invalid/upstream/demo;rev=abc123)
[unregistered-source] exit=1
ERROR[UNREGISTERED_OVERRIDE] `source:<CARGO_CONFIG>/config.toml:crates-io` (replace-with=vendored;directory=vendor)
```

The final source-replacement output is deterministically normalized to the
repository Cargo-config placeholder required by the public-evidence policy.
