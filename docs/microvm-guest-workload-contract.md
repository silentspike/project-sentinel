# microVM guest workload contract

Status: source contract only. The repository does not yet package the guest
launcher, build a canonical root filesystem containing it, attest guest
readiness to the requested workload, or durably reconcile Firecracker launch
ownership after a host crash. Consequently, `runtime = "microvm"` is rejected
by the production daemon and the adapter is not registered there. This document
defines the contract a future implementation must satisfy; it is not evidence
that the contract is implemented.

The `microvm` NanoRuntime does not treat a successful Firecracker boot as a
successful workload launch. The configured root filesystem must contain a
guest launcher that consumes the complete requested `NanoWorkloadSpec` and
executes its command as guest PID 1.

## Host-to-guest contract

For every fresh boot the adapter adds these kernel command-line fields:

- `init=<absolute guest launcher path>`
- `sentinel.nano_contract=kernel-cmdline-v1`
- `sentinel.workload_spec_hex=<lowercase hex encoded JSON NanoWorkloadSpec>`

The launcher path defaults to `/opt/sentinel/bin/sentinel-nano-init` and can be
set with `SENTINEL_MICROVM_GUEST_INIT`. It must be absolute and contain no
whitespace. The complete kernel command line is bounded to 4096 bytes. An empty
workload command or an oversized contract is rejected before Firecracker is
started.

The guest launcher must:

1. read and decode the versioned fields from `/proc/cmdline`;
2. reject an unknown contract version, invalid hex, invalid JSON, or an empty
   command;
3. retain the workload identity (`workload_id`, `agent_id`, name, role, room,
   shift, capabilities, and metadata) for guest-side audit and policy;
4. execute exactly the declared command and surface launch failure by exiting
   non-zero; and
5. remain PID 1 or correctly forward signals and reap descendants.

The kernel and rootfs paths remain configured by `SENTINEL_MICROVM_KERNEL` and
`SENTINEL_MICROVM_ROOTFS`. A generic rootfs that lacks the launcher is not a
valid production artifact. Existing repository tests cover only adapter-level
host-side behavior and do not prove a packaged launcher, guest execution,
workload-bound readiness, or restart recovery.

## Snapshot and restore

Fresh boot uses the contract above. Firecracker snapshot restore resumes the
captured guest memory and device state and therefore does not invoke the fresh
boot launcher again. The snapshot envelope still binds the full workload
identity and the daemon assigns a new per-incarnation `instance_id` to the
restored host handle. Stale pre-restore handles cannot control that incarnation.
