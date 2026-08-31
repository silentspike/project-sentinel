# Pressure-safe shift admission

Result: PASS on 2026-08-31.

The final release was tested under an issue-specific snapshot with a
deterministic pressure threshold and accelerated simulated shift clock. The
configuration was root-only, temporary, and restored byte-for-byte before the
final preflight.

Blocked phase:

- Log: `old=2`, `new=3`, pressure blocked before every effect.
- Runtime/projection agents: 26/26.
- Projection drift, stale entries, repairs, and respawn failures: 0.
- No shift spawn, despawn, cgroup teardown, consolidation, or shift-completion
  record occurred in the blocked interval.

Recovery phase:

- Pressure admission was released while the shift predicate remained pending.
- Exactly one completion was recorded: `old=2`, `new=1`, `removed=17`,
  `spawned=17`, `active=26`.
- The original daemon configuration SHA-256 was restored on both the live file
  and its root-only backup:
  `4515dcd1be98d3396508d6aac80db6ede80f667db5d9014b933f1c51528ba9eb`.
- Final services were successful with restart counters 0; final M0 preflight
  passed with projection backlog 0.

Raw public-safe evidence digests:

- Blocked runtime: `4447bb020af4639506fd98ca122b95698f04a0be8b705183f6ccf6532627eab3`.
- Pressure journal: `8ae4bbc41a5a2222eb198fce7670f0f206b81ae69e9df5cbd18e5e4313590945`.
- Recovery journal: `b4eae162b0db80164ccebb2cb9bac539d781c3a14a12924a0c062cb0c6c3663d`.
