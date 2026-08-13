# M0 activation and restart control

`control.py` is the source harness for the separately authorized single-node
M0 runtime phase. It has two commands:

- `activate` validates the raw provision-receipt digest, manifest digest, Git
  SHA, stopped unit set, and repository-defined topology before it reloads
  systemd and starts only `sentinel.target`. The existing runtime preflight is
  the readiness authority. A failed activation stops only units observed as
  started by this invocation and writes a bounded rollback receipt.
- `restart-journey` validates a SHA-256-pinned restart-control plan against the
  exact Journey plan and its complete checkpoint set. Every checkpoint maps to
  one canonical product service. The controller stops at the checkpoint,
  restarts that service, runs the existing preflight, verifies that ledger and
  evidence did not change during restart, and resumes with the same files and
  operation IDs. Final success requires the Journey runner's authoritative
  replay evidence for every completed step.

The restart-control plan is an explicit operator-approved input because the
current Journey plan defines checkpoint names but does not assign a service to
each checkpoint. The controller neither guesses that mapping nor accepts a
service outside the canonical single-node service set.

Both commands emit public-safe JSON and always keep `m0_acceptance_pass=false`.
Source tests use injected fake executors only. Production execution, snapshot,
deployment, credentials, and `.240` access require separate #650 authorization.
