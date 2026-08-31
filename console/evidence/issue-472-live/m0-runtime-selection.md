# Production runtime-selection evidence

Result: PASS on the canonical single-node release.

- Every one of the 26 scheduled agents had an explicit `bwrap-landlock`
  runtime key.
- Runtime, adapter handle, security-runtime entry, tracked process, and cgroup
  state agreed for every agent.
- No secure-runtime fallback, stale handle, orphan cgroup, or ECS-only
  tool-bearing runtime was present.
- Manifest identity and runtime preflight passed on deployed revision
  `6c90ae73742ab678daa9868f5d07a4a68643d163`.
- The workbench journey resolved through the production NanoRuntimeRegistry and
  produced durable artifacts and a terminal workbench receipt.
