#!/usr/bin/env bash
set -euo pipefail

# Proxmox host profile for stable Sentinel runs.
# Applies runtime state immediately and persists values for next boot.

SAFE_CMDLINE="${SAFE_CMDLINE:-root=ZFS=rpool/ROOT/pve-1 boot=zfs}"
TARGET_CMDLINE="${TARGET_CMDLINE:-${SAFE_CMDLINE} isolcpus=0-3 irqaffinity=4-11}"
TARGET_ARC_MAX="${TARGET_ARC_MAX:-4319084544}"   # 4 GiB
TARGET_HUGEPAGES="${TARGET_HUGEPAGES:-0}"
TARGET_KSM_RUN="${TARGET_KSM_RUN:-0}"

if [[ "${1:-}" == "--safe-cmdline" ]]; then
  TARGET_CMDLINE="${SAFE_CMDLINE}"
fi

if [[ "$(id -u)" -ne 0 ]]; then
  echo "error: run as root" >&2
  exit 1
fi

changed=0

write_file_if_changed() {
  local path="$1"
  local content="$2"
  local current=""
  if [[ -f "$path" ]]; then
    current="$(cat "$path")"
  fi
  if [[ "$current" != "$content" ]]; then
    printf "%s\n" "$content" > "$path"
    changed=1
  fi
}

write_file_if_changed "/etc/kernel/cmdline" "${TARGET_CMDLINE}"
write_file_if_changed "/etc/modprobe.d/zfs.conf" "options zfs zfs_arc_max=${TARGET_ARC_MAX}"
write_file_if_changed "/etc/sysctl.d/90-sentinel-hugepages.conf" "vm.nr_hugepages=${TARGET_HUGEPAGES}"

# Runtime changes (safe and immediate).
echo "${TARGET_ARC_MAX}" > /sys/module/zfs/parameters/zfs_arc_max
echo "${TARGET_HUGEPAGES}" > /proc/sys/vm/nr_hugepages
sysctl -p /etc/sysctl.d/90-sentinel-hugepages.conf >/dev/null

if [[ "${TARGET_KSM_RUN}" == "0" ]]; then
  systemctl disable --now ksmtuned >/dev/null 2>&1 || true
else
  systemctl enable --now ksmtuned >/dev/null 2>&1 || true
fi
echo "${TARGET_KSM_RUN}" > /sys/kernel/mm/ksm/run

# Persist kernel boot entries if needed.
if [[ "${changed}" -eq 1 ]]; then
  proxmox-boot-tool refresh
fi

echo "profile_applied=yes"
echo "running_cmdline=$(cat /proc/cmdline)"
echo "next_cmdline=$(cat /etc/kernel/cmdline)"
echo "arc_max=$(cat /sys/module/zfs/parameters/zfs_arc_max)"
echo "ksm_run=$(cat /sys/kernel/mm/ksm/run)"
grep -E 'MemAvailable|HugePages_Total|Hugetlb' /proc/meminfo

if [[ "$(cat /proc/cmdline)" != *"isolcpus=0-3"* ]]; then
  echo "reboot_required=yes"
else
  echo "reboot_required=no"
fi
