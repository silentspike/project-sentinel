#!/usr/bin/env bash
# Install the common Sentinel runtime underlay without touching node identity,
# cluster configuration, credentials, or runtime data.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "Usage: $0 <ssh-target>" >&2
  exit 2
fi

SSH_TARGET="$1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

CONTRACT="${REPO_ROOT}/deploy/runtime-base.env"
AGENT_RUNTIME="${REPO_ROOT}/target/release/agent-runtime"
LANDLOCK_WRAPPER="${REPO_ROOT}/target/release/landlock-wrapper"
DAEMON_UNIT="${REPO_ROOT}/deploy/systemd/sentinel-daemon.service"
INIT_CGROUPS="${REPO_ROOT}/deploy/scripts/init-cgroups.sh"
INIT_SYSCTL="${REPO_ROOT}/deploy/scripts/init-sysctl.sh"
INIT_BASE_DIRS="${REPO_ROOT}/deploy/scripts/init-runtime-base-dirs.sh"
APT_PIN="${REPO_ROOT}/deploy/apt/sentinel-runtime.pref"
BWRAP_SYSCTL="${REPO_ROOT}/deploy/vm-config/99-sentinel-bwrap.conf"

for source in \
  "${CONTRACT}" \
  "${AGENT_RUNTIME}" \
  "${LANDLOCK_WRAPPER}" \
  "${DAEMON_UNIT}" \
  "${INIT_CGROUPS}" \
  "${INIT_SYSCTL}" \
  "${INIT_BASE_DIRS}" \
  "${APT_PIN}" \
  "${BWRAP_SYSCTL}"; do
  if [ ! -f "${source}" ]; then
    echo "ERROR: required runtime-base artifact is missing: ${source}" >&2
    exit 1
  fi
done

if [ ! -x "${AGENT_RUNTIME}" ] || [ ! -x "${LANDLOCK_WRAPPER}" ]; then
  echo "ERROR: remote release build artifacts must be executable" >&2
  exit 1
fi

# shellcheck disable=SC1090
source "${CONTRACT}"

declare -a sources=(
  "${CONTRACT}"
  "${AGENT_RUNTIME}"
  "${LANDLOCK_WRAPPER}"
  "${DAEMON_UNIT}"
  "${INIT_CGROUPS}"
  "${INIT_SYSCTL}"
  "${INIT_BASE_DIRS}"
  "${APT_PIN}"
  "${BWRAP_SYSCTL}"
)
declare -a hashes=()
for source in "${sources[@]}"; do
  hashes+=("$(sha256sum "${source}" | awk '{print $1}')")
done

REMOTE_DIR="$(ssh -o BatchMode=yes -o ConnectTimeout=5 "${SSH_TARGET}" \
  'mktemp -d /tmp/sentinel-runtime-base.XXXXXX')"
case "${REMOTE_DIR}" in
  /tmp/sentinel-runtime-base.*) ;;
  *)
    echo "ERROR: remote staging path is outside the expected prefix" >&2
    exit 1
    ;;
esac

cleanup() {
  ssh -n -o BatchMode=yes -o ConnectTimeout=5 "${SSH_TARGET}" \
    "rm -rf -- '${REMOTE_DIR}'" >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

scp -q "${sources[@]}" "${SSH_TARGET}:${REMOTE_DIR}/"

ssh -o BatchMode=yes -o ConnectTimeout=5 "${SSH_TARGET}" bash -s -- \
  "${REMOTE_DIR}" \
  "${SENTINEL_BASE_OS_ID}" \
  "${SENTINEL_BASE_OS_VERSION_ID}" \
  "${SENTINEL_BASE_ARCH}" \
  "${SENTINEL_DAEMON_USER}" \
  "${SENTINEL_DAEMON_GROUP}" \
  "${SENTINEL_DATA_USER}" \
  "${SENTINEL_DATA_GROUP}" \
  "${SENTINEL_BUBBLEWRAP_VERSION}" \
  "${SENTINEL_BUBBLEWRAP_BINARY_SHA256}" \
  "${hashes[@]}" <<'REMOTE'
set -euo pipefail

staging_dir="$1"
expected_os="$2"
expected_version="$3"
expected_arch="$4"
daemon_user="$5"
daemon_group="$6"
data_user="$7"
data_group="$8"
bwrap_version="$9"
bwrap_binary_sha="${10}"
shift 10
expected_hashes=("$@")

artifacts=(
  runtime-base.env
  agent-runtime
  landlock-wrapper
  sentinel-daemon.service
  init-cgroups.sh
  init-sysctl.sh
  init-runtime-base-dirs.sh
  sentinel-runtime.pref
  99-sentinel-bwrap.conf
)

if [ "${#expected_hashes[@]}" -ne "${#artifacts[@]}" ]; then
  echo "ERROR: incomplete runtime-base hash set" >&2
  exit 1
fi

verify_hash() {
  local expected="$1"
  local path="$2"
  local actual
  actual="$(sha256sum "${path}" | awk '{print $1}')"
  if [ "${actual}" != "${expected}" ]; then
    echo "ERROR: SHA-256 mismatch for ${path}" >&2
    exit 1
  fi
}

require_inactive() {
  local unit="$1"
  local state
  state="$(systemctl is-active "${unit}" 2>/dev/null || true)"
  if [ "${state}" != "inactive" ] && [ "${state}" != "unknown" ]; then
    echo "ERROR: ${unit} must be stopped before runtime-base provisioning (state=${state})" >&2
    exit 1
  fi
}

protected_metadata_digest() {
  local root="$1"
  if ! sudo test -e "${root}"; then
    printf 'ABSENT'
    return
  fi

  # Never open protected file contents. This record covers every entry,
  # including directories and symlinks, plus type, ownership, permissions,
  # size, timestamps, inode/link metadata, and the symlink target.
  sudo find -P "${root}" -xdev \
    -printf '%P\037%y\037%U\037%G\037%m\037%s\037%T@\037%C@\037%D\037%i\037%n\037%b\037%l\0' \
    | LC_ALL=C sort -z \
    | sha256sum \
    | awk '{print $1}'
}

for i in "${!artifacts[@]}"; do
  verify_hash "${expected_hashes[$i]}" "${staging_dir}/${artifacts[$i]}"
done

# All safety gates precede the first host mutation.
require_inactive sentinel-daemon.service
require_inactive sentinel-projection.service
sudo -n true
getent passwd "${daemon_user}" >/dev/null
getent group "${daemon_group}" >/dev/null
getent passwd "${data_user}" >/dev/null
getent group "${data_group}" >/dev/null

# shellcheck disable=SC1091
source /etc/os-release
if [ "${ID}" != "${expected_os}" ] || [ "${VERSION_ID}" != "${expected_version}" ]; then
  echo "ERROR: unsupported runtime-base OS: ${ID} ${VERSION_ID}" >&2
  exit 1
fi
if [ "$(uname -m)" != "${expected_arch}" ]; then
  echo "ERROR: unsupported runtime-base architecture: $(uname -m)" >&2
  exit 1
fi
if [ "$(stat -fc %T /sys/fs/cgroup)" != "cgroup2fs" ]; then
  echo "ERROR: cgroup v2 is required" >&2
  exit 1
fi

if [ ! -d /opt/sentinel/config ] || [ ! -d /opt/sentinel/data ]; then
  echo "ERROR: node config and data overlays must exist before base provisioning" >&2
  exit 1
fi
if [ -L /etc/machine-id ] || [ ! -f /etc/machine-id ]; then
  echo "ERROR: machine identity must be a regular file" >&2
  exit 1
fi

config_metadata_before="$(protected_metadata_digest /opt/sentinel/config)"
data_metadata_before="$(protected_metadata_digest /opt/sentinel/data)"
secret_metadata_before="$(protected_metadata_digest /etc/sentinel)"
machine_id_metadata_before="$(protected_metadata_digest /etc/machine-id)"
company_metadata_before="$(protected_metadata_digest /work/company)"

sudo install -o root -g root -m 0644 \
  "${staging_dir}/sentinel-runtime.pref" /etc/apt/preferences.d/sentinel-runtime
sudo apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
  "bubblewrap=${bwrap_version}" \
  libcap2-bin

installed_version="$(dpkg-query -W -f='${Version}' bubblewrap)"
if [ "${installed_version}" != "${bwrap_version}" ]; then
  echo "ERROR: bubblewrap version mismatch: ${installed_version}" >&2
  exit 1
fi
if [ "$(readlink -f "$(command -v bwrap)")" != "/usr/bin/bwrap" ]; then
  echo "ERROR: canonical bwrap path is not /usr/bin/bwrap" >&2
  exit 1
fi
verify_hash "${bwrap_binary_sha}" /usr/bin/bwrap
if [ "$(stat -c '%U:%G:%a' /usr/bin/bwrap)" != "root:root:755" ]; then
  echo "ERROR: unexpected /usr/bin/bwrap ownership or mode" >&2
  exit 1
fi
if ! command -v getcap >/dev/null; then
  echo "ERROR: getcap is required to validate the bwrap binary" >&2
  exit 1
fi
if [ -n "$(getcap /usr/bin/bwrap)" ]; then
  echo "ERROR: /usr/bin/bwrap must not carry file capabilities" >&2
  exit 1
fi

sudo env \
  SENTINEL_BASE_USER=root \
  SENTINEL_BASE_GROUP=root \
  SENTINEL_DATA_USER="${data_user}" \
  SENTINEL_DATA_GROUP="${data_group}" \
  bash "${staging_dir}/init-runtime-base-dirs.sh"

sudo install -o root -g root -m 0755 \
  "${staging_dir}/agent-runtime" /usr/bin/agent-runtime
sudo install -o root -g root -m 0755 \
  "${staging_dir}/landlock-wrapper" /opt/sentinel/bin/landlock-wrapper
sudo install -o root -g root -m 0644 \
  "${staging_dir}/sentinel-daemon.service" /etc/systemd/system/sentinel-daemon.service
sudo install -o root -g root -m 0755 \
  "${staging_dir}/init-cgroups.sh" /opt/sentinel/scripts/init-cgroups.sh
sudo install -o root -g root -m 0755 \
  "${staging_dir}/init-sysctl.sh" /opt/sentinel/scripts/init-sysctl.sh
sudo install -o root -g root -m 0755 \
  "${staging_dir}/init-runtime-base-dirs.sh" /opt/sentinel/scripts/init-runtime-base-dirs.sh
sudo install -o root -g root -m 0644 \
  "${staging_dir}/runtime-base.env" /opt/sentinel/share/runtime-base.env
sudo install -o root -g root -m 0644 \
  "${staging_dir}/99-sentinel-bwrap.conf" /etc/sysctl.d/99-sentinel-bwrap.conf

sudo bash /opt/sentinel/scripts/init-cgroups.sh
sudo bash /opt/sentinel/scripts/init-sysctl.sh
sudo sysctl --load /etc/sysctl.d/99-sentinel-bwrap.conf >/dev/null
sudo systemctl daemon-reload

require_inactive sentinel-daemon.service
require_inactive sentinel-projection.service

verify_hash "${expected_hashes[1]}" /usr/bin/agent-runtime
verify_hash "${expected_hashes[2]}" /opt/sentinel/bin/landlock-wrapper
verify_hash "${expected_hashes[3]}" /etc/systemd/system/sentinel-daemon.service
verify_hash "${expected_hashes[4]}" /opt/sentinel/scripts/init-cgroups.sh
verify_hash "${expected_hashes[5]}" /opt/sentinel/scripts/init-sysctl.sh
verify_hash "${expected_hashes[6]}" /opt/sentinel/scripts/init-runtime-base-dirs.sh
verify_hash "${expected_hashes[0]}" /opt/sentinel/share/runtime-base.env
verify_hash "${expected_hashes[7]}" /etc/apt/preferences.d/sentinel-runtime
verify_hash "${expected_hashes[8]}" /etc/sysctl.d/99-sentinel-bwrap.conf

if [ "$(systemctl show -p User --value sentinel-daemon.service)" != "${daemon_user}" ] \
  || [ "$(systemctl show -p Group --value sentinel-daemon.service)" != "${daemon_group}" ]; then
  echo "ERROR: daemon service identity does not match the runtime-base contract" >&2
  exit 1
fi
if [ "$(systemctl show -p NoNewPrivileges --value sentinel-daemon.service)" != "yes" ]; then
  echo "ERROR: daemon NoNewPrivileges must be enabled" >&2
  exit 1
fi
if ! systemctl show -p AmbientCapabilities --value sentinel-daemon.service \
  | grep -qw cap_sys_ptrace; then
  echo "ERROR: daemon CAP_SYS_PTRACE ambient capability is missing" >&2
  exit 1
fi
if [ "$(cat /sys/module/apparmor/parameters/enabled 2>/dev/null || true)" != "Y" ]; then
  echo "ERROR: AppArmor must remain enabled" >&2
  exit 1
fi
if [ "$(sysctl -n kernel.apparmor_restrict_unprivileged_userns)" != "0" ] \
  || [ "$(sysctl -n kernel.unprivileged_userns_clone)" != "1" ]; then
  echo "ERROR: Bubblewrap user-namespace sysctls are not effective" >&2
  exit 1
fi
for controller in cpu io memory pids; do
  if ! grep -qw "${controller}" /sys/fs/cgroup/sentinel/cgroup.subtree_control; then
    echo "ERROR: cgroup controller is not delegated: ${controller}" >&2
    exit 1
  fi
done

probe_name="runtime-base-probe-$$"
probe_home="/ram/agents/${probe_name}"
probe_cgroup="/sys/fs/cgroup/sentinel/${probe_name}"
probe_log="${staging_dir}/sandbox-probe.log"
probe_info="${staging_dir}/sandbox-info.json"
probe_pid=""
probe_child_pid=""
probe_cleanup() {
  if [ -n "${probe_pid}" ] && sudo test -e "/proc/${probe_pid}"; then
    sudo kill -KILL "${probe_pid}" >/dev/null 2>&1 || true
    wait "${probe_pid}" >/dev/null 2>&1 || true
  fi
  sudo rmdir "${probe_cgroup}" >/dev/null 2>&1 || true
  sudo rm -rf -- "${probe_home}" >/dev/null 2>&1 || true
}
trap probe_cleanup EXIT HUP INT TERM

sudo install -d -o "${daemon_user}" -g "${daemon_group}" -m 0700 "${probe_home}"
sudo mkdir "${probe_cgroup}"

(
  sleep 2
  printf 'shutdown\n'
) | sudo -n bash -c '
  set -euo pipefail
  probe_cgroup="$1"
  probe_info="$2"
  daemon_user="$3"
  daemon_group="$4"
  shift 4
  printf "%s" "$$" > "${probe_cgroup}/cgroup.procs"
  exec 3>"${probe_info}"
  exec setpriv \
    --reuid="$(id -u "${daemon_user}")" \
    --regid="$(getent group "${daemon_group}" | cut -d: -f3)" \
    --init-groups \
    --no-new-privs \
    env -i PATH=/usr/bin:/bin "$@"
' runtime-base-probe \
  "${probe_cgroup}" \
  "${probe_info}" \
  "${daemon_user}" \
  "${daemon_group}" \
  bwrap \
    --unshare-all \
    --die-with-parent \
    --hostname sentinel-runtime-probe \
    --ro-bind /usr /usr \
    --ro-bind /lib /lib \
    --ro-bind /lib64 /lib64 \
    --ro-bind /etc/resolv.conf /etc/resolv.conf \
    --ro-bind /work/company /company \
    --bind "${probe_home}" "/home/${probe_name}" \
    --tmpfs /tmp \
    --proc /proc \
    --dev /dev \
    --info-fd 3 \
    --ro-bind /opt/sentinel/bin/landlock-wrapper /landlock-wrapper \
    /landlock-wrapper "${probe_name}" -- /usr/bin/agent-runtime \
    2>"${probe_log}" &
probe_pid="$!"

for _ in $(seq 1 100); do
  probe_child_pid="$(grep -Eo '"child-pid"[[:space:]]*:[[:space:]]*[1-9][0-9]*' \
    "${probe_info}" 2>/dev/null | tr -cd '0-9' || true)"
  if [ -n "${probe_child_pid}" ] && [ -e "/proc/${probe_child_pid}/cgroup" ]; then
    break
  fi
  sleep 0.02
done
if [ -z "${probe_child_pid}" ] || [ ! -e "/proc/${probe_child_pid}/cgroup" ]; then
  echo "ERROR: bwrap did not report a live sandboxed child PID" >&2
  exit 1
fi
if ! grep -Fq "/sentinel/${probe_name}" "/proc/${probe_child_pid}/cgroup"; then
  echo "ERROR: sandboxed agent-runtime did not execute in the Sentinel probe cgroup" >&2
  exit 1
fi
if [ "$(readlink /proc/self/ns/net)" = "$(readlink "/proc/${probe_child_pid}/ns/net")" ]; then
  echo "ERROR: sandboxed agent-runtime did not enter an isolated network namespace" >&2
  exit 1
fi
if ! wait "${probe_pid}"; then
  echo "ERROR: real bwrap agent-runtime probe failed" >&2
  exit 1
fi
probe_pid=""

if ! grep -Fq '[landlock-wrapper] Landlock enforced' "${probe_log}"; then
  echo "ERROR: Landlock was not enforced in the real sandbox probe" >&2
  exit 1
fi
if ! grep -Fq 'agent-runtime: started' "${probe_log}" \
  || ! grep -Fq 'agent-runtime: shutting down' "${probe_log}"; then
  echo "ERROR: agent-runtime did not complete inside the real sandbox probe" >&2
  exit 1
fi
probe_cleanup
trap - EXIT HUP INT TERM
if sudo test -e "${probe_home}" || sudo test -e "${probe_cgroup}"; then
  echo "ERROR: runtime-base sandbox probe left undeclared residue" >&2
  exit 1
fi

config_metadata_after="$(protected_metadata_digest /opt/sentinel/config)"
data_metadata_after="$(protected_metadata_digest /opt/sentinel/data)"
secret_metadata_after="$(protected_metadata_digest /etc/sentinel)"
machine_id_metadata_after="$(protected_metadata_digest /etc/machine-id)"
company_metadata_after="$(protected_metadata_digest /work/company)"
if [ "${config_metadata_before}" != "${config_metadata_after}" ] \
  || [ "${data_metadata_before}" != "${data_metadata_after}" ] \
  || [ "${secret_metadata_before}" != "${secret_metadata_after}" ] \
  || [ "${machine_id_metadata_before}" != "${machine_id_metadata_after}" ] \
  || { [ "${company_metadata_before}" != "ABSENT" ] \
    && [ "${company_metadata_before}" != "${company_metadata_after}" ]; }; then
  echo "ERROR: protected config, data, secret, machine identity, or company metadata changed" >&2
  exit 1
fi
if [ "${company_metadata_before}" = "ABSENT" ] \
  && { [ "$(sudo stat -c '%U:%G:%a:%F' /work/company)" != "root:root:755:directory" ] \
    || sudo find /work/company -mindepth 1 -print -quit | grep -q .; }; then
  echo "ERROR: newly created company bind root is not empty and canonical" >&2
  exit 1
fi

echo "Runtime base installed and functionally verified; protected metadata preserved without reading protected contents; services remain stopped"
REMOTE
