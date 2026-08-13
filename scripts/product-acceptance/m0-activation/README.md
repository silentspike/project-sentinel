# M0 activation and restart control

`control.py` is the source harness for the separately authorized single-node
M0 runtime phase. It has two commands:

- `activate` validates the raw provision-receipt digest, manifest digest, Git
  SHA, stopped unit set, and repository-defined topology before it reloads
  systemd and queues only `sentinel.target` with `--no-block`. A separate bounded activation
  deadline allows asynchronous service startup and recovery while each command
  retains its short timeout. The controller polls without busy-waiting until
  long-lived units are ready, fails immediately on terminal unit or oneshot
  failure, and retries the existing runtime preflight only for its explicit
  time-dependent readiness outcomes. After the target-start mutation, the
  complete prevalidated topology is invocation-owned for rollback: the target
  is stopped first, then both timer-triggered oneshots and every canonical unit
  in reverse order, with bounded readback.
- `restart-journey` validates a SHA-256-pinned restart-control plan against the
  exact Journey plan and its complete checkpoint set. Every checkpoint maps to
  one canonical product service. The controller stops at the checkpoint,
  restarts that service, runs the existing preflight, verifies that ledger and
  evidence did not change during restart, and resumes with the same files and
  operation IDs. Final success requires the Journey runner's authoritative
  ledger/evidence validators and a second complete replay of every plan step.

The restart-control plan is an explicit operator-approved input because the
current Journey plan defines checkpoint names but does not assign a service to
each checkpoint. The controller neither guesses that mapping nor accepts a
service outside the canonical single-node service set.

Both commands emit public-safe JSON and always keep `m0_acceptance_pass=false`.
Children receive a constant minimal environment; only the Journey child also
receives the exact credential variables named by its validated references.
Output and total execution time are streaming-bounded, and failures kill and
wait for the child's dedicated process group before activation rollback or
termination. Successful children are reaped without sending a later process
group signal.
Source tests use injected fake executors only. Production execution, snapshot,
deployment, credentials, and `.240` access require separate #650 authorization.
