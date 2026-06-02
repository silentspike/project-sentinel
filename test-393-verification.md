# Issue #393 Verification - Kani

Status: verified for PR evidence; issue remains open until merge-to-main verification.

## Scope

- Install and prove Kani/CBMC availability on build server `10.0.0.155`.
- Add at least four proven Kani harnesses for critical deterministic invariants.
- Provide reproducible hard-failing verify script.
- Document proof limits honestly.

## AC Matrix

| AC | Requirement | Evidence |
| --- | --- | --- |
| AC-1 | At least four named Kani harnesses proven on 10.0.0.155 | `scripts/verify-kani.sh` proved six harnesses |
| AC-2 | Properties and non-claims documented | `docs/security/kani-verification.md` |
| AC-3 | Reproducible verify script hard-fails if Kani/CBMC missing | `scripts/verify-kani.sh` checks `cargo kani` and `cbmc` before running |
| AC-4 | CI/manual verify integration present and justified | Manual build-server verify script documented; Kani intentionally not part of normal Rust toolchain |
| AC-5 | Evidence includes command and relevant output per harness | Output excerpts below |

## Toolchain Preflight

```bash
ssh root@10.0.0.155 'cargo geiger --version; cargo kani --version; cbmc --version'
```

Relevant output:

```text
cargo-geiger 0.13.0
cargo-kani 0.67.0
6.8.0 (cbmc-6.8.0)
```

Trivial smoke proof on `10.0.0.155`:

```text
VERIFICATION:- SUCCESSFUL
Manual Harness Summary:
Complete - 1 successfully verified harnesses, 0 failures, 1 total.
```

## Kani Proof Run

```bash
ssh root@10.0.0.155 'cd /tmp/builds/ps-383-392-393-kani && scripts/verify-kani.sh'
```

Relevant output:

```text
cargo-kani: cargo-kani 0.67.0
cbmc: 6.8.0 (cbmc-6.8.0)

== Kani: crates/sentinel-bio ==
Checking harness kani::psi_stress_keeps_stress_and_comfort_bounded...
VERIFICATION:- SUCCESSFUL
Checking harness kani::bio_actions_keep_core_fields_bounded...
VERIFICATION:- SUCCESSFUL
Manual Harness Summary:
Complete - 2 successfully verified harnesses, 0 failures, 2 total.

== Kani: crates/sentinel-common ==
Checking harness kani::snapshot_cursor_roundtrip_preserves_cursor_fields...
VERIFICATION:- SUCCESSFUL
Manual Harness Summary:
Complete - 1 successfully verified harnesses, 0 failures, 1 total.

== Kani: crates/sentinel-limbo ==
Checking harness kani::projection_offset_decision_is_monotonic...
VERIFICATION:- SUCCESSFUL
Checking harness kani::operation_dedup_model_accepts_distinct_operations...
VERIFICATION:- SUCCESSFUL
Checking harness kani::operation_dedup_model_is_idempotent_for_same_operation...
VERIFICATION:- SUCCESSFUL
Manual Harness Summary:
Complete - 3 successfully verified harnesses, 0 failures, 3 total.
```

## Regression Tests

```bash
cargo remote -H root@10.0.0.155 -t /tmp/cargo-remote -d stable -- test -p sentinel-common snapshot_codec
```

Relevant output:

```text
test snapshot_codec::tests::cursor_roundtrip_preserves_replay_fields ... ok
test snapshot_codec::tests::roundtrip_preserves_fs_metadata ... ok
test snapshot_codec::tests::decode_falls_back_to_v1_snapshots ... ok
test result: ok. 3 passed; 0 failed
test world_snapshot_codec_roundtrip ... ok
test world_snapshot_codec_rejects_trailing_bytes ... ok
```

```bash
cargo remote -H root@10.0.0.155 -t /tmp/cargo-remote -d stable -- test -p sentinel-limbo offset
```

Relevant output:

```text
test event_store::tests::test_projection_offsets ... ok
test event_store::tests::test_monotonic_offset_enforcement ... ok
test event_store::tests::test_reset_offset ... ok
test event_store::tests::test_reset_offset_nonexistent ... ok
test ac_08_04_offsets_monotonic ... ok
```

## Final Gates

```bash
cargo remote -H root@10.0.0.155 -t /tmp/cargo-remote -d stable -- clippy --workspace --all-targets -- -D warnings
# PASS
```

```bash
cargo remote -H root@10.0.0.155 -t /tmp/cargo-remote -d stable -- test --workspace
# PASS
```

## Limits

- Full `WorldSnapshot` bincode proof was attempted and did not terminate usefully in CBMC because the complete snapshot graph includes `String` and many `Vec` decode paths. The committed Kani proof uses the same bincode legacy config for a heap-free `SnapshotCursor` contract; full payload roundtrip remains covered by Rust unit tests.
- SQLite I/O is not modeled by Kani. Operation-id dedup and offset monotonicity are proven as deterministic pure models, with real DB behavior covered by integration tests.
- LLM/probabilistic behavior is outside #393.
