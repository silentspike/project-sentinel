# AC-2 identity parity

Result: PASS on the exact deployed release.

- Configured roster: 60 agents.
- Current scheduled roster: 26 agents.
- Runtime agents: 26.
- Projection agents: 26.
- Tracked processes and sandbox handles: 26 each.
- Every active runtime used `bwrap-landlock`, had a live tracked PID, an
  adapter handle, a security-runtime entry, and three live cgroup PIDs.
- Projection drift: false.
- Stale runtime entries: 0.
- Orphan cgroups: 0.
- Zombie tracked PIDs: 0.
- Reconcile repairs and respawn failures: 0.
- Event, hierarchy, and projection offsets met at stable cut `23775255` with
  backlog 0.

Configured-roster digest:
`6b0c1bb6a52c3c18fa736ce9e763541ba6d15e8619d0e258d99140e6b603784c`.
