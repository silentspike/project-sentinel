#!/usr/bin/env bash
# generate-manifest.sh — Generates deploy/release-manifest.json with SHA-256 hashes
# for all deployment artifacts. Run from the repo root before deploying.
# Usage: bash deploy/generate-manifest.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
OUTPUT="${REPO_ROOT}/deploy/release-manifest.json"

# Artifact mapping: source (repo-relative) -> destination (VM path) -> type
# Format: "source|dest|type"
ARTIFACT_DEFS=(
  # Binaries (Rust: target/release/, Go: per-module build output)
  "target/release/sentinel-daemon|/opt/sentinel/bin/sentinel-daemon|binary"
  "target/release/sentinel-nightrun|/opt/sentinel/bin/sentinel-nightrun|binary"
  "target/release/sentinel-projection|/opt/sentinel/bin/sentinel-projection|binary"
  "target/release/sentinel-dashboard-backend|/opt/sentinel/bin/sentinel-dashboard-backend|binary"
  "target/release/sentinel-gaia-loop|/opt/sentinel/bin/sentinel-gaia-loop|binary"
  "target/release/sentinel-ctl|/opt/sentinel/bin/sentinel-ctl|binary"
  "target/release/sentinel-gaia|/opt/sentinel/bin/sentinel-gaia|binary"
  "cmd/cortex-gateway/cortex-gateway|/opt/sentinel/bin/cortex-gateway|binary"
  "services/sentinel-judge/sentinel-judge|/opt/sentinel/bin/sentinel-judge|binary"
  "services/sentinel-nats-bridge/sentinel-nats-bridge|/opt/sentinel/bin/sentinel-nats-bridge|binary"
  # Configs
  "config/daemon.toml|/opt/sentinel/config/daemon.toml|config"
  "config/cortex-gateway.toml|/opt/sentinel/config/cortex-gateway.toml|config"
  "config/nightrun.toml|/opt/sentinel/config/nightrun.toml|config"
  "config/judge.toml|/opt/sentinel/config/judge.toml|config"
  "config/nats-bridge.toml|/opt/sentinel/config/nats-bridge.toml|config"
  "config/simulation.toml|/opt/sentinel/config/simulation.toml|config"
  "config/rooms.toml|/opt/sentinel/config/rooms.toml|config"
  "config/company.toml|/opt/sentinel/config/company.toml|config"
  "config/controlplane.toml|/opt/sentinel/config/controlplane.toml|config"
  "config/nats.conf|/etc/nats/nats.conf|config"
  # systemd units
  "deploy/systemd/sentinel-daemon.service|/etc/systemd/system/sentinel-daemon.service|systemd"
  "deploy/systemd/sentinel-gateway.service|/etc/systemd/system/sentinel-gateway.service|systemd"
  "deploy/systemd/sentinel-judge.service|/etc/systemd/system/sentinel-judge.service|systemd"
  "deploy/systemd/sentinel-nats-bridge.service|/etc/systemd/system/sentinel-nats-bridge.service|systemd"
  "deploy/systemd/sentinel-nightrun.service|/etc/systemd/system/sentinel-nightrun.service|systemd"
  "deploy/systemd/sentinel-nightrun.timer|/etc/systemd/system/sentinel-nightrun.timer|systemd"
  "deploy/systemd/sentinel-projection.service|/etc/systemd/system/sentinel-projection.service|systemd"
  "deploy/systemd/sentinel-dashboard-backend.service|/etc/systemd/system/sentinel-dashboard-backend.service|systemd"
  "deploy/systemd/sentinel-gaia-loop.service|/etc/systemd/system/sentinel-gaia-loop.service|systemd"
  "deploy/systemd/nats-server.service|/etc/systemd/system/nats-server.service|systemd"
  "deploy/systemd/sentinel.target|/etc/systemd/system/sentinel.target|systemd"
  # Init scripts
  "deploy/scripts/init-cgroups.sh|/opt/sentinel/scripts/init-cgroups.sh|script"
  "deploy/scripts/init-dirs.sh|/opt/sentinel/scripts/init-dirs.sh|script"
  "deploy/scripts/init-dashboard-auth.sh|/opt/sentinel/scripts/init-dashboard-auth.sh|script"
  "deploy/scripts/install-native-claude.sh|/opt/sentinel/scripts/install-native-claude.sh|script"
  "deploy/scripts/init-hugepages.sh|/opt/sentinel/scripts/init-hugepages.sh|script"
  "deploy/scripts/init-sysctl.sh|/opt/sentinel/scripts/init-sysctl.sh|script"
  "deploy/scripts/init-tmpfs.sh|/opt/sentinel/scripts/init-tmpfs.sh|script"
)

cd "${REPO_ROOT}"

GIT_SHA="$(git rev-parse HEAD)"
CREATED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

echo "Generating release manifest..."
echo "  Git SHA:    ${GIT_SHA}"
echo "  Created at: ${CREATED_AT}"
echo ""

# Start JSON
{
  printf '{\n'
  printf '  "version": "1.0",\n'
  printf '  "created_at": "%s",\n' "${CREATED_AT}"
  printf '  "git_sha": "%s",\n' "${GIT_SHA}"
  printf '  "artifacts": [\n'
} > "${OUTPUT}"

FIRST=1
MISSING=0
MISSING_LIST=()

for def in "${ARTIFACT_DEFS[@]}"; do
  IFS='|' read -r source dest type <<< "${def}"

  if [ ! -f "${source}" ]; then
    MISSING=$((MISSING + 1))
    MISSING_LIST+=("${source}")
    continue
  fi

  HASH="$(sha256sum "${source}" | awk '{print $1}')"

  if [ "${FIRST}" -eq 1 ]; then
    FIRST=0
  else
    printf ',\n' >> "${OUTPUT}"
  fi

  {
    printf '    {\n'
    printf '      "path": "%s",\n' "${dest}"
    printf '      "source": "%s",\n' "${source}"
    printf '      "sha256": "%s",\n' "${HASH}"
    printf '      "type": "%s"\n' "${type}"
    printf '    }'
  } >> "${OUTPUT}"

  echo "  OK: ${source} -> ${dest} [${HASH:0:12}...]"
done

{
  printf '\n  ]\n'
  printf '}\n'
} >> "${OUTPUT}"

echo ""
echo "Manifest written to: ${OUTPUT}"

if [ "${MISSING}" -gt 0 ]; then
  echo "" >&2
  echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!" >&2
  echo "WARNING: MANIFEST IS INCOMPLETE — ${MISSING} artifact(s) not found:" >&2
  for missing_src in "${MISSING_LIST[@]}"; do
    echo "  MISSING: ${missing_src}" >&2
  done
  echo "" >&2
  echo "  Build missing artifacts before generating the manifest." >&2
  echo "  Rust: cargo remote -- build --workspace --release" >&2
  echo "  Go:   cd cmd/cortex-gateway && go build -o cortex-gateway ./..." >&2
  echo "        cd services/sentinel-judge && go build -o sentinel-judge ./..." >&2
  echo "        cd services/sentinel-nats-bridge && go build -o sentinel-nats-bridge ./..." >&2
  echo "" >&2
  echo "  Do NOT use this manifest for deploy-preflight — it will" >&2
  echo "  report false MISSING for every skipped artifact." >&2
  echo "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!" >&2
  exit 1
fi
