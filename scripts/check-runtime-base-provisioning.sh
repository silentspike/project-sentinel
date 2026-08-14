#!/usr/bin/env bash
# Static and behavior contract gate for the common runtime-base provisioner.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
PROVISIONER="${REPO_ROOT}/deploy/provision-runtime-base.sh"
CONTRACT="${REPO_ROOT}/deploy/runtime-base.env"
MANIFEST="${REPO_ROOT}/deploy/generate-manifest.sh"
RELEASE_MANIFEST="${REPO_ROOT}/deploy/release-manifest.json"
DEPLOY_PREFLIGHT="${REPO_ROOT}/deploy/deploy-preflight.sh"
WORKBENCH_PROFILE="${REPO_ROOT}/config/workbench-profiles/web-authoring-v1.toml"
UNIT="${REPO_ROOT}/deploy/systemd/sentinel-daemon.service"
SYSCTL="${REPO_ROOT}/deploy/vm-config/99-sentinel-bwrap.conf"
BASE_DIRS="${REPO_ROOT}/deploy/scripts/init-runtime-base-dirs.sh"
CGROUPS="${REPO_ROOT}/deploy/scripts/init-cgroups.sh"
INIT_SYSCTL="${REPO_ROOT}/deploy/scripts/init-sysctl.sh"

require_text() {
  local path="$1"
  local expected="$2"
  if ! grep -Fq -- "${expected}" "${path}"; then
    echo "FAIL: ${path} is missing required contract text: ${expected}" >&2
    exit 1
  fi
}

require_once() {
  local path="$1"
  local expected="$2"
  local count
  count="$(grep -Fc -- "${expected}" "${path}")"
  if [ "${count}" -ne 1 ]; then
    echo "FAIL: ${path} must contain exactly one contract entry: ${expected}" >&2
    exit 1
  fi
}

metadata_digest() {
  local root="$1"
  if [ ! -e "${root}" ] && [ ! -L "${root}" ]; then
    printf 'ABSENT'
    return
  fi
  find -P "${root}" -xdev \
    -printf '%P\037%y\037%U\037%G\037%m\037%s\037%T@\037%C@\037%D\037%i\037%n\037%b\037%l\0' \
    | LC_ALL=C sort -z \
    | sha256sum \
    | awk '{print $1}'
}

runtime_helpers=("${BASE_DIRS}" "${CGROUPS}" "${INIT_SYSCTL}")
bash -n "${PROVISIONER}" "${runtime_helpers[@]}"

require_text "${CONTRACT}" 'SENTINEL_BUBBLEWRAP_VERSION=0.9.0-1ubuntu0.1'
require_text "${CONTRACT}" 'SENTINEL_BUBBLEWRAP_BINARY_SHA256=52231e1caf55bcbc667b269f49c63599a6f7db4767ae6a039580d0ff853db712'
require_text "${CONTRACT}" 'SENTINEL_DATA_GROUP=ubuntu'
require_text "${SYSCTL}" 'kernel.apparmor_restrict_unprivileged_userns = 0'
require_text "${SYSCTL}" 'kernel.unprivileged_userns_clone = 1'
require_text "${UNIT}" 'User=root'
require_text "${UNIT}" 'Group=root'
require_text "${UNIT}" 'EnvironmentFile=-/etc/sentinel/env'
require_text "${UNIT}" 'NoNewPrivileges=true'
require_text "${UNIT}" 'AmbientCapabilities=CAP_SYS_PTRACE'
require_text "${UNIT}" 'ReadWritePaths=/opt/sentinel/data /opt/sentinel/config /opt/sentinel/fs /ram/sentinel /ram/agents'

require_once "${MANIFEST}" 'target/release/agent-runtime|/usr/bin/agent-runtime|binary'
require_once "${MANIFEST}" 'target/release/landlock-wrapper|/opt/sentinel/bin/landlock-wrapper|binary'
require_once "${MANIFEST}" 'deploy/scripts/init-runtime-base-dirs.sh|/opt/sentinel/scripts/init-runtime-base-dirs.sh|script'
require_once "${MANIFEST}" 'deploy/runtime-base.env|/opt/sentinel/share/runtime-base.env|config'
require_once "${MANIFEST}" 'deploy/apt/sentinel-runtime.pref|/etc/apt/preferences.d/sentinel-runtime|config'
require_once "${MANIFEST}" 'deploy/vm-config/99-sentinel-bwrap.conf|/etc/sysctl.d/99-sentinel-bwrap.conf|config'
require_once "${MANIFEST}" 'config/workbench-profiles/web-authoring-v1.toml|/opt/sentinel/config/workbench-profiles/web-authoring-v1.toml|config'
require_once "${RELEASE_MANIFEST}" '"path": "/opt/sentinel/config/workbench-profiles/web-authoring-v1.toml"'
require_once "${RELEASE_MANIFEST}" '"sha256": "6e352d4f34b33cb1f8cd2fa0f94ae6a6b9b2b49165b60f65b2e40ba68f078286"'
require_once "${WORKBENCH_PROFILE}" 'environment = { HOME = "/workspace", LANG = "C.UTF-8", LC_ALL = "C.UTF-8", PATH = "/usr/bin:/bin" }'

# These are deliberately literal fragments from the remote heredoc.
# shellcheck disable=SC2016
for text in \
  'require_inactive sentinel-daemon.service' \
  'require_inactive sentinel-projection.service' \
  'protected_metadata_digest()' \
  'bubblewrap=${bwrap_version}' \
  '--no-new-privs' \
  '/sentinel/${probe_name}' \
  '[landlock-wrapper] Landlock enforced' \
  'runtime-base sandbox probe left undeclared residue' \
  'protected config, data, secret, machine identity, or company metadata changed' \
  'without reading protected contents' \
  'Runtime base installed and functionally verified'; do
  require_text "${PROVISIONER}" "${text}"
done

# Every helper that the runtime-base path executes is enumerated and checked.
# The general init-dirs helper is intentionally excluded because it recursively
# changes protected persistent-data ownership and modes.
if grep -Fq 'init-dirs.sh' "${PROVISIONER}"; then
  echo "FAIL: runtime-base provisioner must not install or invoke init-dirs.sh" >&2
  exit 1
fi
for helper in "${runtime_helpers[@]}"; do
  if grep -Eq '(^|[[:space:]])(chown|chmod)[[:space:]].*(-R|--recursive)' "${helper}"; then
    echo "FAIL: runtime-base helper must not recursively change metadata: ${helper}" >&2
    exit 1
  fi
done

# Join shell continuation lines before scanning so indirect multi-line
# mutations cannot evade the protected-path boundary.
for path in "${PROVISIONER}" "${runtime_helpers[@]}"; do
  normalized="$(sed -e ':again' -e '/\\$/N; s/\\\n/ /; tagain' "${path}")"
  if printf '%s\n' "${normalized}" \
    | grep -Eq '(^|[;&|[:space:]])(chown|chmod|install|cp|mv|rm|rmdir|mkdir|touch|truncate|tee|ln|rsync|sed[[:space:]]+-i|perl[[:space:]]+-i)([[:space:]]|$).*(/opt/sentinel/(config|data)|/etc/sentinel|/etc/machine-id|/work/company)'; then
    echo "FAIL: runtime-base code mutates a protected path: ${path}" >&2
    exit 1
  fi
done

if grep -Eq 'tree_digest|-type[[:space:]]+f.*sha256sum|sha256sum[[:space:]]+/etc/machine-id' "${PROVISIONER}"; then
  echo "FAIL: protected contents must never be read or hashed" >&2
  exit 1
fi
if grep -Eq 'systemctl[[:space:]]+(start|restart|enable)' "${PROVISIONER}"; then
  echo "FAIL: runtime-base provisioner must leave services stopped" >&2
  exit 1
fi

# Exercise the directory helper against a fresh supported root, then rerun it
# over protected fixtures with unusual metadata to prove restart idempotence
# and the absence of direct or indirect protected-tree changes.
check_tmp_root="${RUNTIME_BASE_CHECK_TMPDIR:-${REPO_ROOT}}"
fresh_fixture="$(mktemp -d "${check_tmp_root}/.runtime-base-fresh.XXXXXX")"
protected_fixture="$(mktemp -d "${check_tmp_root}/.runtime-base-protected.XXXXXX")"
preflight_fixture="$(mktemp -d "${check_tmp_root}/.workbench-preflight.XXXXXX")"
cleanup() {
  rm -rf -- "${fresh_fixture}" "${protected_fixture}" "${preflight_fixture}"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "${preflight_fixture}/bin"
cat >"${preflight_fixture}/bin/ssh" <<'EOF'
#!/usr/bin/env bash
case "${WORKBENCH_PREFLIGHT_FIXTURE:-match}" in
  match)
    printf '%s  %s\n' \
      '6e352d4f34b33cb1f8cd2fa0f94ae6a6b9b2b49165b60f65b2e40ba68f078286' \
      '/opt/sentinel/config/workbench-profiles/web-authoring-v1.toml'
    ;;
  missing) printf 'MISSING\n' ;;
  mismatch)
    printf '%064d  %s\n' 0 '/opt/sentinel/config/workbench-profiles/web-authoring-v1.toml'
    ;;
  *) exit 2 ;;
esac
EOF
chmod 0700 "${preflight_fixture}/bin/ssh"
cat >"${preflight_fixture}/manifest.json" <<'EOF'
{
  "version": "1.0",
  "artifacts": [
    {
      "path": "/opt/sentinel/config/workbench-profiles/web-authoring-v1.toml",
      "source": "config/workbench-profiles/web-authoring-v1.toml",
      "sha256": "6e352d4f34b33cb1f8cd2fa0f94ae6a6b9b2b49165b60f65b2e40ba68f078286",
      "type": "config"
    }
  ]
}
EOF
PATH="${preflight_fixture}/bin:${PATH}" \
  WORKBENCH_PREFLIGHT_FIXTURE=match \
  bash "${DEPLOY_PREFLIGHT}" fixture "${preflight_fixture}/manifest.json" >/dev/null
for rejected_fixture in missing mismatch; do
  if PATH="${preflight_fixture}/bin:${PATH}" \
    WORKBENCH_PREFLIGHT_FIXTURE="${rejected_fixture}" \
    bash "${DEPLOY_PREFLIGHT}" fixture "${preflight_fixture}/manifest.json" >/dev/null 2>&1; then
    echo "FAIL: deploy preflight accepted ${rejected_fixture} workbench profile" >&2
    exit 1
  fi
done

for _ in 1 2; do
  SENTINEL_ROOT_PREFIX="${fresh_fixture}" \
    SENTINEL_BASE_USER="$(id -un)" \
    SENTINEL_BASE_GROUP="$(id -gn)" \
    SENTINEL_DATA_USER="$(id -un)" \
    SENTINEL_DATA_GROUP="$(id -gn)" \
    bash "${BASE_DIRS}" >/dev/null
done

for path in \
  /opt/sentinel \
  /opt/sentinel/bin \
  /opt/sentinel/scripts \
  /opt/sentinel/share \
  /opt/sentinel/fs \
  /ram \
  /ram/agents \
  /ram/sentinel \
  /ram/sentinel/ecs \
  /ram/sentinel/sessions \
  /ram/sentinel/zenoh \
  /ram/sentinel/bench \
  /work \
  /work/company; do
  if [ "$(stat -c '%u:%g:%a:%F' "${fresh_fixture}${path}")" != "$(id -u):$(id -g):755:directory" ]; then
    echo "FAIL: fresh-host directory postcondition mismatch: ${path}" >&2
    exit 1
  fi
done
if find "${fresh_fixture}/ram/agents" -mindepth 1 -print -quit | grep -q . \
  || find "${fresh_fixture}/work/company" -mindepth 1 -print -quit | grep -q .; then
  echo "FAIL: fresh-host directory contract left undeclared state" >&2
  exit 1
fi

mkdir -p \
  "${protected_fixture}/opt/sentinel/config/nested" \
  "${protected_fixture}/opt/sentinel/data/db" \
  "${protected_fixture}/etc/sentinel" \
  "${protected_fixture}/work/company/source"
printf 'node-local-config\n' >"${protected_fixture}/opt/sentinel/config/nested/daemon.toml"
printf 'node-local-data\n' >"${protected_fixture}/opt/sentinel/data/db/events.db"
printf 'node-local-secret\n' >"${protected_fixture}/etc/sentinel/env"
printf 'node-local-identity\n' >"${protected_fixture}/etc/machine-id"
printf 'node-local-company\n' >"${protected_fixture}/work/company/source/README"
ln -s nested/daemon.toml "${protected_fixture}/opt/sentinel/config/current"
chmod 0711 "${protected_fixture}/opt/sentinel/config/nested"
chmod 0640 "${protected_fixture}/opt/sentinel/config/nested/daemon.toml"
chmod 0750 "${protected_fixture}/opt/sentinel/data"
chmod 0710 "${protected_fixture}/work/company"
chmod 0600 \
  "${protected_fixture}/opt/sentinel/data/db/events.db" \
  "${protected_fixture}/etc/sentinel/env" \
  "${protected_fixture}/work/company/source/README"

protected_before="$(
  metadata_digest "${protected_fixture}/opt/sentinel/config"
  metadata_digest "${protected_fixture}/opt/sentinel/data"
  metadata_digest "${protected_fixture}/etc/sentinel"
  metadata_digest "${protected_fixture}/etc/machine-id"
  metadata_digest "${protected_fixture}/work/company"
)"

for _ in 1 2; do
  SENTINEL_ROOT_PREFIX="${protected_fixture}" \
    SENTINEL_BASE_USER="$(id -un)" \
    SENTINEL_BASE_GROUP="$(id -gn)" \
    SENTINEL_DATA_USER="$(id -un)" \
    SENTINEL_DATA_GROUP="$(id -gn)" \
    bash "${BASE_DIRS}" >/dev/null
done

protected_after="$(
  metadata_digest "${protected_fixture}/opt/sentinel/config"
  metadata_digest "${protected_fixture}/opt/sentinel/data"
  metadata_digest "${protected_fixture}/etc/sentinel"
  metadata_digest "${protected_fixture}/etc/machine-id"
  metadata_digest "${protected_fixture}/work/company"
)"
if [ "${protected_before}" != "${protected_after}" ]; then
  echo "FAIL: base-directory helper changed protected metadata" >&2
  exit 1
fi

echo "Runtime-base provisioning contract: OK"
