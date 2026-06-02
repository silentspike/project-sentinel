# sentinel-bio

## Purpose

`sentinel-bio` owns deterministic physiology transitions for agents. It updates hunger, energy, caffeine decay, bladder pressure, stress, social need, and comfort from simulation time, personality, work context, and PSI pressure inputs.

## Interfaces

- `update_bio_state(...)` advances one agent's `BioState` for a tick.
- `eat_meal`, `drink_water`, `drink_coffee`, and `use_bathroom` apply discrete agent actions.
- `apply_psi_stress(...)` maps CPU and memory pressure into stress and comfort changes.
- `#[cfg(kani)] src/kani.rs` proves bounded action and PSI transitions.

## Dependencies

- `sentinel-common` provides `BioState`, `Personality`, and `WorkContext`.
- Dev-only verification uses `approx`, `bolero`, and Kani through `scripts/verify-kani.sh`.

## Verify

```bash
cargo remote -c -- test -p sentinel-bio
scripts/verify-kani.sh
```

Run Kani on the build server with `cargo-kani` and CBMC installed; do not treat harness files as verified unless the proof run reports `VERIFICATION:- SUCCESSFUL`.
