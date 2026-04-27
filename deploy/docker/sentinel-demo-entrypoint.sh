#!/usr/bin/env bash
#
# Sentinel demo entrypoint — selects a binary based on $SENTINEL_ROLE.
#
# Each role mounts the same image but invokes a different binary so the
# compose file can drive a multi-process stack from a single image.
#
# Roles:
#   daemon         sentinel-daemon       (ECS world, operator API on :8084)
#   gateway        cortex-gateway        (LLM proxy on :8080, control on :8081)
#   judge          sentinel-judge        (quality monitor on :8082)
#   nats-bridge    sentinel-nats-bridge  (event bridge on :8083)
#   projection     sentinel-projection   (CQRS read-model worker, builds projection.db)
#   dashboard      sentinel-dashboard    (Bun UI on :8000)
#   nightrun       sentinel-nightrun     (one-shot nightly batch)
#   help           print this message and exit
#
set -euo pipefail

role="${SENTINEL_ROLE:-${1:-help}}"

# Merge demo overrides on top of default config the first time we see them.
config_dir="${SENTINEL_CONFIG_DIR:-/opt/sentinel/config-default}"
demo_dir="${SENTINEL_DEMO_CONFIG_DIR:-/opt/sentinel/config-demo}"
runtime_dir="/opt/sentinel/config-runtime"

if [ ! -d "$runtime_dir" ]; then
    mkdir -p "$runtime_dir"
    cp -r "$config_dir"/. "$runtime_dir"/
    if [ -d "$demo_dir" ]; then
        cp -r "$demo_dir"/. "$runtime_dir"/
    fi
fi
export SENTINEL_CONFIG_DIR="$runtime_dir"

# Each Go service expects its config at "config/<name>.toml" relative to
# the working directory. Make a `config/` symlink that points at the merged
# runtime dir, so the binaries find their files without needing a flag.
go_workdir="/opt/sentinel"
ln -sfn "$runtime_dir" "$go_workdir/config"

case "$role" in
    daemon)
        exec /usr/local/bin/sentinel-daemon \
            --config "$runtime_dir/daemon.toml"
        ;;
    gateway)
        cd "$go_workdir"
        exec /usr/local/bin/cortex-gateway
        ;;
    judge)
        cd "$go_workdir"
        exec /usr/local/bin/sentinel-judge
        ;;
    nats-bridge)
        cd "$go_workdir"
        exec /usr/local/bin/sentinel-nats-bridge
        ;;
    projection)
        # Long-running CQRS read-model worker. Polls EventStore, builds
        # projection.db that the dashboard reads from.
        exec /usr/local/bin/sentinel-projection \
            --event-store /opt/sentinel/data/events.db \
            --projection-db /opt/sentinel/data/projection.db
        ;;
    dashboard)
        cd /opt/sentinel/dashboard
        export PROJECTION_DB_PATH="${PROJECTION_DB_PATH:-/opt/sentinel/data/projection.db}"
        export EVENT_STORE_DB_PATH="${EVENT_STORE_DB_PATH:-/opt/sentinel/data/events.db}"
        # Wait for projection worker to create projection.db before bun starts.
        # Otherwise the dashboard crashes with SQLITE_CANTOPEN.
        deadline=$(( $(date +%s) + 60 ))
        while [ ! -f "$PROJECTION_DB_PATH" ] && [ "$(date +%s)" -lt "$deadline" ]; do
            echo "[entrypoint] waiting for projection.db ($PROJECTION_DB_PATH) to appear..."
            sleep 2
        done
        exec bun run src/index.ts
        ;;
    nightrun)
        exec /usr/local/bin/sentinel-nightrun \
            --config "$runtime_dir/nightrun.toml"
        ;;
    help|*)
        cat <<EOF
Sentinel demo container.

Usage: docker run --rm -e SENTINEL_ROLE=<role> sentinel-demo:local

Available roles: daemon | gateway | judge | nats-bridge | projection | dashboard | nightrun

The demo stack normally invokes this image once per service via
docker-compose.demo.yml. To run an individual service ad-hoc, set
SENTINEL_ROLE explicitly. Configuration is read from /opt/sentinel/config-runtime
(default config + demo overrides merged at first run).
EOF
        exit 0
        ;;
esac
