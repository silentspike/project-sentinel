#!/usr/bin/env bash
# Sanitized OS/runtime underlay only. No product state or credentials are copied.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
test "$(id -u)" = 0
test "$(cat /etc/sentinel-ci-lab)" = sentinel-ci-v1
. /etc/os-release
test "$ID:$VERSION_ID" = ubuntu:24.04
test "$(uname -m)" = x86_64
STAGE=/work/tmp/project-sentinel/ci-lab-prepare
install -d -m 0700 "$STAGE"

apt-get update -qq
apt-get install -y --no-install-recommends \
  linux-image-7.0.0-34-generic=7.0.0-34.34~24.04.1 \
  linux-modules-7.0.0-34-generic=7.0.0-34.34~24.04.1 \
  libcap2-bin util-linux fuse3 \
  libssl3t64 libsqlite3-0 libseccomp2 zstd sqlite3 jq curl ca-certificates \
  git build-essential pkg-config python3 sysstat nftables

# Old pinned package remains available in the official Ubuntu publication
# archive. Validate its installed executable against the repository contract.
for package in \
  bubblewrap_0.9.0-1ubuntu0.1_amd64.deb \
  apparmor_4.0.1really4.0.1-0ubuntu0.24.04.6_amd64.deb \
  libapparmor1_4.0.1really4.0.1-0ubuntu0.24.04.6_amd64.deb \
  qemu-guest-agent_8.2.2+ds-0ubuntu1.16_amd64.deb; do
  curl --fail --location --proto '=https' --tlsv1.2 \
    "https://launchpad.net/ubuntu/+archive/primary/+files/$package" -o "$STAGE/$package"
done
apt-get install -y --allow-downgrades "$STAGE/"*.deb
printf '%s  /usr/bin/bwrap\n' \
  52231e1caf55bcbc667b269f49c63599a6f7db4767ae6a039580d0ff853db712 | sha256sum --check
printf '%s\n' \
  'a89b5f2ed99e6c6427a2645ba556d8aad508c6a04808ed337d32c0e45de7e6ad  /usr/sbin/apparmor_parser' \
  '7edafb7d1aab32591f26c97e631a4881f68e92e1cd0f0dbbfb4338b8ccac5e7c  /usr/lib/x86_64-linux-gnu/libapparmor.so.1' \
  'b818415e23d4f1733219d15fa59747ac917d00d747f819c01918c10867ff2a47  /usr/sbin/qemu-ga' | sha256sum --check
apt-mark hold linux-image-7.0.0-34-generic linux-modules-7.0.0-34-generic \
  bubblewrap apparmor libapparmor1 qemu-guest-agent

# Runtime layout and cgroup initialization use the unchanged repository helpers.
for file in runtime-base.env sentinel-runtime.pref 99-sentinel-bwrap.conf \
  init-runtime-base-dirs.sh init-cgroups.sh init-sysctl.sh sentinel-daemon.service; do
  test -f "$STAGE/$file"
done
install -m 0644 "$STAGE/sentinel-runtime.pref" /etc/apt/preferences.d/sentinel-runtime
install -m 0644 "$STAGE/99-sentinel-bwrap.conf" /etc/sysctl.d/99-sentinel-bwrap.conf
bash "$STAGE/init-runtime-base-dirs.sh"
chown ubuntu:ubuntu /ram/agents /work/company
install -d -o ubuntu -g ubuntu -m 0750 /opt/sentinel/data /opt/sentinel/config
install -d -m 0755 /opt/sentinel/share
install -m 0644 "$STAGE/runtime-base.env" /opt/sentinel/share/runtime-base.env
install -m 0755 "$STAGE/init-cgroups.sh" /opt/sentinel/scripts/init-cgroups.sh
install -m 0755 "$STAGE/init-sysctl.sh" /opt/sentinel/scripts/init-sysctl.sh
install -m 0644 "$STAGE/sentinel-daemon.service" /etc/systemd/system/sentinel-daemon.service
systemctl disable sentinel-daemon.service 2>/dev/null || true

# Follow the measured running production values where startup defaults differ.
# This file contains no identity or company data.
printf '%s\n' 'vm.swappiness=1' 'vm.max_map_count=262144' \
  'vm.dirty_bytes=536870912' 'vm.dirty_background_bytes=134217728' \
  'net.core.rmem_max=4194304' 'net.core.wmem_max=4194304' \
  > /etc/sysctl.d/98-sentinel-ci-parity.conf
sysctl --load /etc/sysctl.d/98-sentinel-ci-parity.conf
sysctl --load /etc/sysctl.d/99-sentinel-bwrap.conf
cat > /etc/systemd/system/sentinel-ci-underlay.service <<'UNIT'
[Unit]
Description=Sentinel CI runtime underlay
After=systemd-sysctl.service ram-sentinel.mount
Requires=ram-sentinel.mount
Before=sentinel-daemon.service
[Service]
Type=oneshot
ExecStart=/opt/sentinel/scripts/init-cgroups.sh
ExecStart=/bin/sh -c 'echo madvise > /sys/kernel/mm/transparent_hugepage/enabled'
RemainAfterExit=yes
[Install]
WantedBy=multi-user.target
UNIT
cat > /etc/systemd/system/ram-sentinel.mount <<'UNIT'
[Unit]
Description=Sentinel hot-tier scratch matching the runtime reference
Before=sentinel-ci-underlay.service
[Mount]
What=tmpfs
Where=/ram/sentinel
Type=tmpfs
Options=rw,nosuid,nodev,noexec,mode=1777,size=4G
[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
systemctl enable --now ram-sentinel.mount
systemctl enable --now sentinel-ci-underlay.service qemu-guest-agent.service
# Do not silently move the qualified kernel on unattended upgrades.
systemctl disable --now unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer || true
systemctl mask apt-daily.service apt-daily-upgrade.service
find /opt/sentinel/data /opt/sentinel/config /work/company -mindepth 1 -print -quit | \
  { if read -r _; then echo 'Unexpected state in fresh underlay' >&2; exit 1; fi; }
test ! -e /etc/sentinel/env
test ! -e /etc/sentinel/credentials
printf '%s\n' 'underlay-prepared; reboot and verify exact kernel before sealing'
