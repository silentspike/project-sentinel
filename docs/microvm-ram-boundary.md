# microVM snapshot: the RAM-page boundary (#500a, AC-5)

This note states honestly which parts of a microVM (Firecracker) snapshot the
`#500a` CAS-manifest format can carry, and which it cannot — so no later claim
overpromises.

## What a microVM snapshot is

`MicrovmNanoRuntime::snapshot()` pauses the VM and asks Firecracker for a **full
snapshot**, which writes **two host files** into the snapshot directory:

| File | Contents |
|------|----------|
| `snapshot.state` | the VM device/CPU state |
| `snapshot.mem`   | the **entire guest RAM**, byte for byte |

The `NanoSnapshot.payload` carries only **stable metadata + deterministic file
paths** to those two files (semantics `MicrovmMemory`), never the volatile RAM
bytes. That separation is what makes `restore(snapshot(x))` payload-stable for
the conformance contract (#408).

## Manifest-capable (what #500a does)

`snapshot.state` and `snapshot.mem` are ordinary disk files, so they are
**content-addressable** into the CAS. `crates/sentinel-microvm/src/manifest.rs`
provides `content_address_file` / `restore_file`: a snapshot file is stored once
and travels as a `BlockRef` (`cas-blob:v1:sha256:<hex>`) instead of an inline
copy — the 1:n "pointer, not bytes" principle applied to the microVM files.

**Dedup of the RAM file is intentionally not attempted.** A multi-GB guest-RAM
dump changes almost entirely between two snapshots, so content-defined chunking
of it yields ≈ 0 dedup while costing a lot of CPU/IO. The RAM file is therefore
referenced as a **single SHA-256 whole-blob**, not chunked. This is the honest,
measured-by-construction position; it is not a performance promise.

## Not manifest-capable (the RAM-page residual → Track F)

- The **live guest-RAM pages** of a running VM are non-deterministic and are
  never serialized into the snapshot payload — only the file paths are.
- **Deep microVM migration** — post-copy of live pages, the consistency class,
  dirty-page tracking, incremental RAM diffs — is **Track F (#554)**, not #500a.

A RAM page is not an ECS state is not a bwrap home. #500a only proves the
file-content-addressing mechanism for the microVM parts; it makes **no**
live-migration claim for microVMs.

## Verification (AC-5)

`crates/sentinel-microvm/src/manifest.rs` tests
`ac5_snapshot_file_content_addresses_and_roundtrips`: a **small synthetic**
state/mem file (not a real RAM dump) is content-addressed to a `BlockRef` and
restored byte-identically from the CAS.

```
cargo remote -c -- test -p sentinel-microvm manifest
```
