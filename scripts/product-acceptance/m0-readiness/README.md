# M0 Boot Readiness

`readiness.py` is the bounded, fail-closed boot probe installed as
`/opt/sentinel/scripts/m0-readiness.py` by the canonical M0 provisioner.

The three modes have separate authority boundaries:

- `nats` checks the fixed numeric-loopback JetStream readiness endpoint. The
  NATS unit runs it as `ExecStartPost`, so dependent units cannot observe NATS
  as active during JetStream recovery.
- `daemon` reads the operator credential from a systemd credential file and
  validates only daemon-local runtime, security, process, cgroup, repair and
  worker invariants. It deliberately does not wait for the projection service.
- `nightrun` posts the fixed operator action using that credential file; the
  credential value never enters argv or the result record.

The merged full-stack M0 preflight remains authoritative for projection-store,
episode-frontier, release-manifest and complete service-topology agreement.
Systemd timers are ordered after their direct dependencies without ordering any
unit after `sentinel.target`, which avoids a target dependency cycle.

This source phase performs no deployment or runtime verification. The later
authorized `.240` run supplies systemd credentials and exercises the installed
helper through the activation controller.
