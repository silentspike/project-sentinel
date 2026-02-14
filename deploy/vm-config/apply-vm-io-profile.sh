#!/usr/bin/env bash
set -euo pipefail

if [[ "$(id -u)" -ne 0 ]]; then
  echo "error: run as root" >&2
  exit 1
fi

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <vmid>" >&2
  exit 1
fi

VMID="$1"

disk_spec="$(qm config "${VMID}" | sed -n 's/^scsi0: //p' | cut -d',' -f1)"
if [[ -z "${disk_spec}" ]]; then
  echo "error: VM ${VMID} has no scsi0 disk" >&2
  exit 1
fi

qm set "${VMID}" --scsihw virtio-scsi-single >/dev/null
qm set "${VMID}" --cpu host --machine q35 --numa 1 --balloon 0 >/dev/null
qm set "${VMID}" --onboot 1 --agent enabled=1 --ostype l26 >/dev/null
qm set "${VMID}" --scsi0 "${disk_spec},discard=on,iothread=1,aio=io_uring" >/dev/null

echo "vm_io_profile_applied=yes vmid=${VMID}"
qm config "${VMID}" | egrep '^(name|memory|balloon|cores|cpu|numa|machine|scsi0|scsihw|net0|ostype|onboot|agent):'
