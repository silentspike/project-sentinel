# Counted Test Archives

The builder compiles once. Registered, sanitized lab clones execute disjoint
partitions of the same archive without Cargo or Rust installed. This does not
replace the hosted matrix, independent review, or final product acceptance.

Runtime target class: NONE (source tooling and builder preparation only)
Deploy targets: none
Read-only targets: none
Forbidden targets: production and cluster runtime hosts
Benchmark targets: N/A
Rollback owner and method: N/A

Before executing on a lab, its controller must declare `LAB`, exact registered
targets and its owned reset procedure. The script neither provisions machines
nor changes kernel policy, root ownership, credentials or system services.

## Build And Seal

Use the persistent project queue from the frozen workspace root. These are
Cargo Remote payloads, not local Cargo commands:

```sh
cargo remote --resources=large --no-copy-lock -c ci-tests.tar.zst -- nextest archive --workspace --cargo-profile ci --locked --archive-file /target/current/ci-tests.tar.zst
```

For the first pilot, replace `--workspace` with explicit packages, for example
`-p sentinel-bio -p sentinel-sandbox -p agent-runtime`. Do not run a second full
workspace stream. Normal archives include non-test executables and build-script
outputs automatically. The pinned Sentinel image must include the nextest
version and SHA-256 from `test-archive-tool.json`. A profile recipe upgrade does
not change existing immutable enrolled image pins. Promote a new pin only
after its pilot; preserve the old image and warm cache for rollback.

The controller supplies a private identity JSON using the CI envelope:

```json
{
  "repository": "silentspike/project-sentinel",
  "pr": 1,
  "head_sha": "<40 lowercase hex>",
  "tree_sha": "<40 lowercase hex>",
  "base_sha": "<40 lowercase hex>",
  "inputs": {
    "toolchain": "1.99.0",
    "profile": "linux-gnu-sentinel",
    "image": "<actual bare Docker image ID, 64 lowercase hex>",
    "input_digest": "<Cargo Remote source_sha256>"
  }
}
```

`profile` identifies the builder. `cargo_profile: ci` in the sealed manifest
identifies the assertion/overflow-preserving Cargo profile. They are distinct.
Use the original Cargo Remote state JSON (its nested `snapshot` is accepted)
and verified result JSON, never a token-bearing full state file in the bundle.

```sh
python3 scripts/ci/test-archive.py seal --snapshot "$STATE" --result "$RESULT" --identity "$IDENTITY" --archive target/ci-tests.tar.zst --source-root "$FROZEN_ROOT" --output "$BUNDLE"
```

Sealing requires the clean original head/tree, source hashes matching the
builder's filtered manifest, successful builder result, actual image identity,
and matching exported archive checksum. No recursive checkout copy: `.git`,
private configuration and credentials are not transferred. Outputs are
`manifest.json`, `tests.tar.zst`, `sources.tar.gz`, `source-inventory.json`.
Retain the manifest checksum in the controller's immutable job record before
transport. Checksums detect corruption; they are not signatures or independent
authorization to trust a public-fork artifact.

## Extract And Partition

`install-tool` downloads the exact checksummed musl executable into a new owned
directory, with no Rust compiler or Cargo installation:

```sh
python3 scripts/ci/test-archive.py install-tool --directory "$TOOLS"
python3 scripts/ci/test-archive.py extract --nextest "$TOOLS/cargo-nextest" --bundle "$BUNDLE" --destination "$EXTRACTED"
python3 scripts/ci/test-archive.py plan --bundle "$BUNDLE" --extracted "$EXTRACTED" --shards 2 --output "$PLAN"
```

Extraction verifies bundle membership, regular-file-only source entries and
content hashes, then uses nextest's archive extraction and JSON listing. The
listing is the authoritative compiled test count, including ignored tests.
`inventory --output PATH` can produce source hints before compilation; those
counts must never be presented as executed or compiled test totals.

Each owner has zero-based contiguous partitions with no empty shard. Sorted
round-robin assigns every listed test to exactly one shard or explicit external
owner. A plan contains exact `(binary_id, test_name)` identities, inventory
digest, expected counts, and separately enumerated non-sharded tests. A newly
ignored test without an unambiguous source owner blocks planning.

## Lab Execution

The lab controller must map this clone's `source` to `/workspace` and
`build/target` to `/target/current`. Both are checked by file identity. A
`--workspace-remap` alone cannot repair embedded `CARGO_MANIFEST_DIR` or
`CARGO_BIN_EXE_*` paths. Source `target` points at the extracted build, and the
historical `target/debug/breakout-helper` path aliases the archived CI helper.
Do not overwrite an existing unrelated mount or directory to create mappings.

Run one shard at a time per clone. Independent clones may run concurrently:

```sh
python3 scripts/ci/test-archive.py run --nextest "$TOOLS/cargo-nextest" --bundle "$BUNDLE" --extracted "$EXTRACTED" --plan "$PLAN" --shard lab-normal-0 --output "$RECEIPT_DIR"
python3 scripts/ci/test-archive.py verify --plan "$PLAN" "$RECEIPT_DIR_0" "$RECEIPT_DIR_1"
```

Exact nextest selection is checked before execution. Large filtersets use
bounded disjoint batches to respect operating-system argument limits, without
recompiling. Every executed identity must appear once in successful JUnit,
with no skipped, extra, missing or duplicated cases. Retries are disabled;
snapshots use `INSTA_UPDATE=no`. Failed nextest exits never become PASS.
Receipts bind the plan, original identity, command(s), inventory, counts,
log and JUnit SHA-256. The aggregate verifier must additionally require the
external gates. `verify` always reports `aggregate_pass: false` deliberately.
The summary's `owner_inventory` binds each owner's `test_count` and
`test_inventory_digest` to exact compiled IDs, including zero-count owners.

## Execution Owners

| Test Category | Execution Owner And Required Evidence |
| --- | --- |
| Normal workspace tests | `lab-normal`; exact nextest/JUnit identities |
| Snapshot assertions | Same normal shards, source snapshots present, no updates |
| Ignored kernel/privileged tests | `lab-ignored`; authorized kernel, user namespaces, delegated cgroups, FUSE, xattrs, sparse files and KVM prerequisites |
| WASM/Wasmtime | Normal and explicitly ignored lab tests; archived `.wasm` fixtures and source present; rebuilding fixtures belongs to the audited builder WASM profile |
| Trybuild/compiler test | `builder-compiler`; queued `cargo remote --no-copy-lock -- test -p sentinel-daemon --profile ci --test runtime_lifecycle_visibility --locked`; not a lab shard |
| Doctests, including compile-fail docs | Builder; queued `test --workspace --profile ci --doc --locked`; capture actual rustdoc test counts |
| eBPF feature | Builder compile/lint plus lab kernel load/verification; the daemon enables eBPF by default but feature-specific gate evidence remains explicit |
| Excluded eBPF probes | Specialized audited builder target/nightly, then lab kernel; default workspace archive is not coverage |
| Excluded optional inference | Specialized compiler/model/GPU gate if affected; default workspace archive is not coverage |
| Protected read diagnostics | `protected-diagnostic`; explicit read-only copied/sanitized state protocol required, never run against production by default |
| Ignored live dashboard tests | `live-session`; scoped lab session and authorized live stack required |
| Ignored footprint benchmark | `manual-benchmark`; matched lab workload, not ordinary test PASS |

Root-owned namespace wrappers and runtime helpers must come from the same
qualified source artifacts through the lab's canonical provisioning. The
archive runner does not weaken the tested broker or install setuid helpers.
Non-sharded owners remain pending until their own evidence or an independently
frozen affected-contract N/A decision and rationale is supplied. Their exact
inventory hashes remain visible even when not applicable. No blanket
`--ignored` run can imply complete coverage; optional GPU/KVM diagnostics are
not silently made product prerequisites.

## Mechanism Checks

```sh
python3 scripts/ci/test-archive-tests.py -v
```

Synthetic tests verify corruption, missing/duplicate shards, stale identity,
inventory tampering, empty/count-mismatched tests, exact partition selection
and unsuccessful/skipped JUnit handling. They are not executable archive or
kernel acceptance evidence. Actual archive creation, listing, extraction,
two-clone pilot and hosted-comparison/negative qualification remain separate.
