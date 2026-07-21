# microVM guest workload contract

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
valid #472 live-test artifact. Repository unit tests prove host-side encoding
and validation; the actual image and guest execution require the issue-specific
Deploy-VM gate.

## Snapshot and restore

Fresh boot uses the contract above. Firecracker snapshot restore resumes the
captured guest memory and device state and therefore does not invoke the fresh
boot launcher again. The snapshot envelope still binds the full workload
identity and the daemon assigns a new per-incarnation `instance_id` to the
restored host handle. Stale pre-restore handles cannot control that incarnation.
