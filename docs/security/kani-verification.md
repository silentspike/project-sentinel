# Kani Verification

TOGAF clusters: 03 Infrastructure, 09 Verification

Status: baseline for issue #393.

## Toolchain

Kani is intentionally not assumed to be part of a normal Rust toolchain. The
verified build-server preflight for this baseline is:

- Host: `10.0.0.155`
- `cargo-kani 0.67.0`
- `CBMC 6.8.0`
- A trivial smoke harness completed with `VERIFICATION:- SUCCESSFUL`.

`scripts/verify-kani.sh` fails immediately if `cargo-kani` or `cbmc` are
missing.

## Harnesses

| Crate | Harness | Property |
| --- | --- | --- |
| `sentinel-bio` | `bio_actions_keep_core_fields_bounded` | Eating, drinking water, and bathroom actions keep core 0-100 bio fields bounded for all bounded starting states. |
| `sentinel-bio` | `psi_stress_keeps_stress_and_comfort_bounded` | PSI stress mapping keeps stress and comfort in the 0-100 range for all bounded starting states and pressure inputs. |
| `sentinel-common` | `snapshot_cursor_roundtrip_preserves_cursor_fields` | Bincode snapshot-cursor encode/decode preserves schema version, tick, ECS tick, and last event id. |
| `sentinel-limbo` | `operation_dedup_model_is_idempotent_for_same_operation` | The DB-free model of the `operation_id` unique-index rule inserts a repeated operation once. |
| `sentinel-limbo` | `operation_dedup_model_accepts_distinct_operations` | The same model accepts two distinct operation ids. |
| `sentinel-limbo` | `projection_offset_decision_is_monotonic` | The extracted offset decision rejects decreases, no-ops equal offsets, and accepts initial/advancing offsets. |

## Limits

- The event-store idempotency harness models the deterministic `operation_id`
  uniqueness rule. SQLite `INSERT OR IGNORE` behavior is still covered by
  integration tests because the real path is I/O-bound and not Kani-friendly.
- Full bio tick dynamics with trigonometric/circadian and exponential caffeine
  decay are not part of this baseline; the harnesses cover the irreversible
  bounded action/PSI transitions.
- The full `WorldSnapshot` bincode graph contains `String` and many `Vec`
  payloads. Kani/CBMC does not terminate usefully on that baseline proof, so
  the Kani contract is the heap-free snapshot cursor codec using the same
  bincode legacy config. Full snapshot payload roundtrip remains covered by
  Rust unit tests.
- LLM and probabilistic paths are out of scope for formal verification.

## Verify

Run on the build server, not locally:

```bash
scripts/verify-kani.sh
```

The script runs Kani in:

- `crates/sentinel-bio`
- `crates/sentinel-common`
- `crates/sentinel-limbo`
