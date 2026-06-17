# ADR-<issue>: <Title>

- **Gate:** <G-id, e.g. G0 / G-D0 / none>
- **Status:** Proposed
- **Primary issue:** #<issue>
- **Related issues / gates:** #<...>, <G-...>
- **Supersedes / Superseded by:** <ADR-... or —>

## Context

What forces this decision now? What does the codebase look like today (cite
`path:line` for load-bearing facts)? What does the TOGAF SSOT say (cite the line),
and where is it silent (silence = this ADR decides)?

## Problem

The single question this ADR answers, stated sharply. If there is a fork, list the
options that were genuinely on the table.

## Decision

The decision, unambiguous and with no remaining fork. If options were weighed, state
which one is chosen and why the others were rejected.

## Non-Goals

What this ADR explicitly does **not** decide or build (deferred to which track/ADR).

## Data Types

The concrete types/fields this decision binds (or that the implementing issue must
produce). For Track-A ADRs that integrate with existing code, cite the existing type
(`path:line`).

## State Machine / Protocol

State transitions, message exchanges, or ordering guarantees, if any.

## Failure Modes

Partition, crash mid-operation, stale state, restart, race. For each: what is the
correct behavior, and which invariant protects it.

## Tests

How the decision is verified (unit / integration / 2-node / property). At least one
negative test per safety invariant where applicable.

## Benchmarks

What is measured, the sweep axes (tuning knobs), the optimum criterion, and the
consequence on failure. `n/a` only for pure-correctness gates (state why).

## Backward Compatibility

Impact on existing snapshots/events/configs. Migration via `#[serde(default)]` +
`schema_version`? Any forbidden migration?

## Security

Trust domain, authentication, what is NOT a security boundary, replay/poisoning
considerations.

## Public Claim Boundary

What may be claimed publicly today vs. what is target-only / measured-later. Keeps
PR/issue/TOGAF language honest.

## Open Follow-ups

Known gaps deferred to later tracks, each with a pointer.
