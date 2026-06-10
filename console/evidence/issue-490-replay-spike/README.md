# Issue #490 — Replay-Spike Live Evidence (deploy VM, i7-3930K)

All JSON outputs of `replay-spike` on the deploy VM (same machine), with `vmstat`/`mpstat`
sidecars (`*-bench.txt`, `mpstat.txt`, `vmstat.txt`; VM idle ~86% during the runs).

- `runall-<window>-<variant>-<psi>.txt` — T1/T2/T3/T7 + core for the full matrix
  {100,1000,10000} × {clean,gap-probe} × {scripted,zero}.
- `trace-*.txt` — T5 first-divergence tick (clean 426, gap-probe 401; anchor at 400).
- `order-probe.txt` — T6 (100×, stable hash).
- `event-replay.txt` — T4 (78/78 agent actions reconstructed, match=true).
- `bench-*.txt` — wall-clock live-compute vs replay + snapshot size.
- `live.txt`/`replay.txt`/`xp-compare.txt` — cross-process determinism (equal=true).

Verdict + analysis: `docs/spikes/SPIKE-490-exact-replay.md`.
