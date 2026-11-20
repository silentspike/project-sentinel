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
  Its authenticated `/operator/runtime-readiness` response uses the same
  runtime snapshot without invoking workflow-store health. Full company
  readiness remains a separate `/operator/runtime-health` acceptance check;
  passing the boot probe does not prove that employee work is ready.
- `nightrun` posts the fixed operator action using that credential file; the
  credential value never enters argv or the result record.

The merged full-stack M0 preflight remains authoritative for projection-store,
episode-frontier, release-manifest and complete service-topology agreement.
Systemd timers are ordered after their direct dependencies without ordering any
unit after `sentinel.target`, which avoids a target dependency cycle.

This source phase performs no deployment or runtime verification. The later
authorized `.240` run supplies systemd credentials and exercises the installed
helper through the activation controller.

### Activation lock configuration

Before activation, install `/etc/sentinel/activation-control.env` as a regular
root-owned file with mode `0600`. Use one literal assignment with an absolute
path, for example:

```dotenv
SENTINEL_ACTIVATION_CONTROL_LOCK=/opt/sentinel/.m0-activation-control.lock
```

The activation controller creates and locks this file. The health monitor
resolves the same path through the controller and opens the existing lock
read-only for its shared lock. A missing or unsafe configuration prevents
activation and automatic restarts. Health observations continue.

For an upgrade, configure the path to the lock from the installed release
before changing either component. Keep that lock inode throughout the
transition. Update the controller first and verify that `lock-path` resolves
to the existing lock. Then install the updated health monitor and its unit
and reload systemd. To change the lock location later, stop the monitor
timer and wait for both the controller and health monitor to finish before
switching the configuration.
