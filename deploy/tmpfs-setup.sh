#!/usr/bin/env bash
set -euo pipefail

# Guest-side helper for Sentinel tmpfs mount.
# Safe to run multiple times.

if [[ "$(id -u)" -ne 0 ]]; then
  echo "error: run as root" >&2
  exit 1
fi

install -d -m 1777 /ram/sentinel

line='tmpfs /ram/sentinel tmpfs rw,nosuid,nodev,noexec,relatime,size=4G,mode=1777,huge=within_size 0 0'
if ! grep -Fq ' /ram/sentinel tmpfs ' /etc/fstab; then
  printf "%s\n" "${line}" >> /etc/fstab
fi

mountpoint -q /ram/sentinel || mount /ram/sentinel

echo "tmpfs_setup_done=yes"
mount | grep ' /ram/sentinel '
