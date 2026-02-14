#!/usr/bin/env bash
set -euo pipefail

if [[ "$(id -u)" -ne 0 ]]; then
  echo "error: run as root" >&2
  exit 1
fi

cat > /usr/local/sbin/sentinel-boot-guard.sh <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

MARKER="/root/.sentinel-pending"
SAFE_CMDLINE="root=ZFS=rpool/ROOT/pve-1 boot=zfs"

if [[ ! -f "${MARKER}" ]]; then
  exit 0
fi

if [[ "$(cat /proc/cmdline)" != *"root=ZFS="* ]]; then
  printf "%s\n" "${SAFE_CMDLINE}" > /etc/kernel/cmdline
  proxmox-boot-tool refresh || true
fi

# Avoid accidental RAM starvation due to hugepage over-reservation.
printf "%s\n" "vm.nr_hugepages=0" > /etc/sysctl.d/90-sentinel-hugepages.conf
sysctl -p /etc/sysctl.d/90-sentinel-hugepages.conf >/dev/null || true
echo 0 > /proc/sys/vm/nr_hugepages || true

rm -f "${MARKER}"
EOF

chmod 0755 /usr/local/sbin/sentinel-boot-guard.sh

cat > /etc/systemd/system/sentinel-boot-guard.service <<'EOF'
[Unit]
Description=Sentinel boot guard with auto-rollback
ConditionPathExists=/root/.sentinel-pending
After=local-fs.target

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/sentinel-boot-guard.sh

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable sentinel-boot-guard.service >/dev/null

echo "boot_guard_installed=yes"
systemctl status sentinel-boot-guard.service --no-pager -l | sed -n '1,20p' || true
