# sentinel-physics

## Purpose

`sentinel-physics` models deterministic office-environment physics: acoustics, CO2, temperature, smell propagation, transit, hallway encounters, and chaos events.

## Interfaces

- `calculate_noise_level` and `noise_to_text` map room occupancy to perception.
- `calculate_temperature`, `calculate_co2`, and `co2_to_text` model room environment.
- `SmellEvent`, `smell_intensity_at_distance`, and `is_smell_active` support smell perception.
- `start_transit`, `tick_transit`, and `check_hallway_encounter` implement movement and encounter timing.
- Chaos helpers provide deterministic frequency, duration, and room impact values.

## Dependencies

- `sentinel-common` for room and agent identifiers.
- Dev verification uses `bolero` for invariant testing.

## Verify

```bash
cargo remote -c -- test -p sentinel-physics
cargo remote -c -- test -p sentinel-physics --test bolero_invariants
```

Physics changes should also be checked through ECS integration tests when they affect perception or event emission.
