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
source match passes, and an ordinary official-upstream Git dependency remains
inventory rather than being misclassified as a patch or fork.
