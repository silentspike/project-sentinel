# Dependency Patch and Temporary-Fork Registry

Status: normative active registry

This document is the active source-intervention registry required by
[the dependency policy](dependency-policy.md). It is consumed by
`scripts/check-patch-registry.py`.

Governance context:

- [#617](https://github.com/silentspike/project-sentinel/issues/617) covers unsafe
  and security boundaries.
- [#621](https://github.com/silentspike/project-sentinel/issues/621) owns the
  dependency-governance program.
- [#631](https://github.com/silentspike/project-sentinel/issues/631) supplies the
  canonical reachability baseline.
- [#705](https://github.com/silentspike/project-sentinel/issues/705) assigns
  ownership decisions.
- [#656](https://github.com/silentspike/project-sentinel/issues/656) consumes patch
  basis, expiry, owner, gates, and review triggers during upgrades.

## Active State

Repository revision `16c0e353861e29a9b4d181bebd9a9f4a432a49b3` has:

| Cargo intervention mechanism | Active declarations |
| --- | ---: |
| `[patch.<source>]` | 0 |
| `[replace]` | 0 |
| Repository Cargo config `source.<name>.replace-with` | 0 |
| Active registry rows | 0 |

No active registry entry is invented.

The repository also has four direct Git dependency declarations for `aya`,
`aya-log`, `aya-ebpf`, and `aya-log-ebpf`, all pointing to the official
`https://github.com/aya-rs/aya` upstream. `Cargo.lock` resolves that upstream at
`b93ee8c26e96af85d77b040fa4fae8447c8fd7f8`. These declarations are not Cargo
patches, source replacements, or fork URLs, so they are not patch-registry rows.
They are recorded in the direct-Git allowlist below. The checker compares the
allowlist and every monitored Cargo manifest in both directions: additions,
removals, source selectors, package aliases, and URL changes fail closed.

## Machine-Readable Active Registry

The marker pair and TOML payload are part of the checker contract. Do not add a row
without adding the matching Cargo override in the same commit.

<!-- patch-registry:toml:start -->
```toml
schema_version = 2
entries = []

[[direct_git_dependencies]]
id = "cargo-git-aya-ebpf"
dependency_key = "git:crates/sentinel-ebpf/Cargo.toml:dependencies:aya"
manifest = "crates/sentinel-ebpf/Cargo.toml"
table = "dependencies"
dependency = "aya"
package = "aya"
source = "git=https://github.com/aya-rs/aya"
owner = "component:ebpf"
reason = "Official upstream required for the compatible BTF map format"

[[direct_git_dependencies]]
id = "cargo-git-aya-log"
dependency_key = "git:crates/sentinel-ebpf/Cargo.toml:dependencies:aya-log"
manifest = "crates/sentinel-ebpf/Cargo.toml"
table = "dependencies"
dependency = "aya-log"
package = "aya-log"
source = "git=https://github.com/aya-rs/aya"
owner = "component:ebpf"
reason = "Official upstream required with the Aya runtime dependency"

[[direct_git_dependencies]]
id = "cargo-git-aya-ebpf-probes"
dependency_key = "git:crates/sentinel-ebpf-probes/Cargo.toml:dependencies:aya-ebpf"
manifest = "crates/sentinel-ebpf-probes/Cargo.toml"
table = "dependencies"
dependency = "aya-ebpf"
package = "aya-ebpf"
source = "git=https://github.com/aya-rs/aya"
owner = "component:ebpf"
reason = "Official upstream required for flat BTF-compatible map definitions"

[[direct_git_dependencies]]
id = "cargo-git-aya-log-ebpf"
dependency_key = "git:crates/sentinel-ebpf-probes/Cargo.toml:dependencies:aya-log-ebpf"
manifest = "crates/sentinel-ebpf-probes/Cargo.toml"
table = "dependencies"
dependency = "aya-log-ebpf"
package = "aya-log-ebpf"
source = "git=https://github.com/aya-rs/aya"
owner = "component:ebpf"
reason = "Official upstream required with the Aya eBPF probe dependency"
```
<!-- patch-registry:toml:end -->

## Entry Schema

Every `[[entries]]` row has these fields:

| Field | Type | Contract |
| --- | --- | --- |
| `id` | string | Stable, unique registry identifier. |
| `ecosystem` | string | Must be `cargo` in this registry version. |
| `package` | string | Actual patched package name; `*` only for a source-wide replacement. |
| `version` | string | Exact affected upstream package version or bounded version set. |
| `kind` | enum | `PATCH_UPSTREAM` or `FORK_TEMPORARY`. |
| `manifest` | string | Repository-relative manifest or Cargo config path. |
| `override_key` | string | Canonical key emitted by the checker. |
| `source` | string | Exact normalized Cargo override source emitted by the checker. |
| `reason` | string | Reproducible defect, security need, or product requirement. |
| `evidence` | string | Approved implementation or incident issue containing the baseline and gates. |
| `upstream_basis` | string | Exact upstream release or commit from which the diff is carried. |
| `diff_lines` | integer | Current added plus removed patch lines; zero or greater. |
| `upstream_pr` | string | Upstream issue or PR URL. An active intervention cannot omit it. |
| `owner` | string | Component owner accountable for updates and incidents. |
| `status` | enum | Must be `ACTIVE`; historical rows are removed from this active registry. |
| `expires_on` | ISO date string | Hard deadline. The gate fails on or after this date. |
| `revisit_condition` | string | Upstream release, advisory, API, format, or operational trigger. |
| `advisory_ids` | string array | Relevant RUSTSEC/CVE identifiers; use an empty array only after review. |
| `rollback` | string | Exact source, binary, data, or configuration rollback path. |

Example shape, not an active entry:

```toml
[[entries]]
id = "cargo-example-patch"
ecosystem = "cargo"
package = "example"
version = "1.2.3"
kind = "PATCH_UPSTREAM"
manifest = "Cargo.toml"
override_key = "patch:Cargo.toml:crates-io:example"
source = "git=https://github.com/example/example;rev=0123456789abcdef"
reason = "Reproducible issue-specific reason"
evidence = "https://github.com/silentspike/project-sentinel/issues/NNN"
upstream_basis = "example 1.2.3"
diff_lines = 12
upstream_pr = "https://github.com/example/example/pull/NNN"
owner = "component:example"
status = "ACTIVE"
expires_on = "2099-01-01"
revisit_condition = "Remove after the first upstream release containing the change"
advisory_ids = []
rollback = "Remove the patch table and restore the last accepted upstream release"
```

## Direct-Git Allowlist Schema

Every `[[direct_git_dependencies]]` row describes one real direct Cargo
declaration. It is inventory, not a patch/fork intervention row.

| Field | Contract |
| --- | --- |
| `id` | Stable unique allowlist identifier. |
| `dependency_key` | Canonical checker key for manifest, table, and dependency alias. |
| `manifest` | Exact repository-relative Cargo manifest. |
| `table` | Exact Cargo dependency table, including workspace or target context. |
| `dependency` | Dependency alias used by the manifest. |
| `package` | Upstream package name after any Cargo `package` rename. |
| `source` | Exact normalized Git URL plus optional branch, revision, or tag. |
| `owner` | Component owner accountable for source review and updates. |
| `reason` | Bounded reason the official Git source is required. |

The actual and allowlisted key sets must be identical. Matching keys must also
match every manifest, table, alias, package, and source field. A switch from an
official upstream URL to a fork URL therefore fails before it can enter the tree.

## Canonical Override Keys

The checker produces stable keys:

```text
patch:<manifest>:<source-table>:<dependency-alias>
replace:<manifest>:<package-and-version>
source:<cargo-config>:<source-name>
git:<manifest>:<dependency-table>:<dependency-alias>
```

It also normalizes source fields in deterministic key order. Registry authors must
run the checker and copy the reported key and source; they must not guess them.

## Lifecycle

1. Open and approve a bounded implementation or incident issue.
2. Record baseline, exact used API, upstream basis, security/format impact, gates,
   owner, expiry, migration, and rollback.
3. Open the upstream issue or PR before activating a local patch or fork.
4. Add the Cargo override and complete registry row in the same commit.
5. Run checker, conformance, security, migration, target-runtime, and rollback gates
   selected by the issue.
6. During every upgrade, rebase the minimal diff, update source and basis, map new
   advisories, and test whether upstream made the intervention removable.
7. Remove the override and active row when upstream absorbs it or rollback is chosen.

A temporary fork cannot be renewed by editing only the date. Renewal requires owner,
security, and implementation-issue approval with a new upstream and risk readback.

## CI Contract

```bash
python3 scripts/check-patch-registry.py
python3 -m unittest discover -s scripts/tests -p 'test_check_patch_registry.py'
```

The checker fails on:

- an override without a registry row;
- a registry row without an active override;
- any missing required field;
- a manifest, package, or source mismatch;
- duplicate IDs or override keys;
- an expired patch or temporary fork.
- a new or changed direct Git dependency without an exact allowlist match;
- an allowlist row whose direct Git dependency was removed.

Ordinary official-upstream Git dependencies remain allowlisted inventory and do
not silently become patch/fork entries. A future fork must use a recognized Cargo
override mechanism, carry a patch-registry row, and pass this registry gate.
