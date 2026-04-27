#!/usr/bin/env bash
#
# scripts/demo.sh — one-command 10-minute Sentinel demo.
#
# 1. Build the demo image (cached on subsequent runs).
# 2. Bring up NATS + 5 Sentinel services via docker-compose.demo.yml.
# 3. Wait for daemon, gateway, and dashboard to report healthy.
# 4. Open the dashboard in the default browser (skipped under DEMO_NO_OPEN=1).
# 5. Run for DEMO_DURATION_SECONDS (default 600 = 10 min).
# 6. Tear the stack down.
#
# Environment knobs:
#   DEMO_DURATION_SECONDS   how long to keep the stack up before teardown
#   DEMO_NO_OPEN            do not attempt to open a browser (CI-friendly)
#   DEMO_KEEP               leave the stack running after the timer expires
#   COMPOSE                 docker-compose binary override (default: docker compose)

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

duration="${DEMO_DURATION_SECONDS:-600}"
compose=( ${COMPOSE:-docker compose} -f docker-compose.demo.yml )

note() { printf '\n[demo] %s\n' "$*"; }
fail() { printf '\n[demo] FAIL: %s\n' "$*" >&2; exit 1; }

note "building image (cached after first run)..."
"${compose[@]}" build

note "starting stack..."
"${compose[@]}" up -d

note "waiting for daemon TCP (operator API has no /healthz; max 120s)..."
deadline=$(( $(date +%s) + 120 ))
until curl -s -o /dev/null --max-time 3 http://127.0.0.1:18084/ >/dev/null 2>&1; do
    [ "$(date +%s)" -lt "$deadline" ] || fail "daemon did not become reachable in 120s"
    sleep 2
done

note "waiting for gateway /health..."
deadline=$(( $(date +%s) + 60 ))
until curl -fsS --max-time 3 http://127.0.0.1:18080/health >/dev/null 2>&1; do
    [ "$(date +%s)" -lt "$deadline" ] || fail "gateway did not become healthy in 60s"
    sleep 2
done

note "waiting for dashboard /health..."
deadline=$(( $(date +%s) + 60 ))
until curl -fsS --max-time 3 http://127.0.0.1:18000/health >/dev/null 2>&1; do
    [ "$(date +%s)" -lt "$deadline" ] || fail "dashboard did not respond in 60s"
    sleep 2
done

note "stack is up — endpoints:"
cat <<EOF

  Dashboard       http://localhost:18000
  Gateway proxy   http://localhost:18080        (also exposes /metrics)
  Gateway ctrl    http://localhost:18081
  Judge           http://localhost:18082/health (also /metrics)
  NATS bridge     http://localhost:18083/health
  Daemon API      http://localhost:18084
  NATS monitor    http://localhost:8222

EOF

if [ -z "${DEMO_NO_OPEN:-}" ]; then
    if command -v xdg-open >/dev/null 2>&1; then
        xdg-open "http://localhost:18000" >/dev/null 2>&1 || true
    elif command -v open >/dev/null 2>&1; then
        open "http://localhost:18000" >/dev/null 2>&1 || true
    fi
fi

note "running for ${duration}s — Ctrl+C to stop early"
trap 'note "interrupted — tearing down"; "${compose[@]}" down --remove-orphans; exit 130' INT
sleep "$duration"

if [ -n "${DEMO_KEEP:-}" ]; then
    note "DEMO_KEEP set — leaving stack running"
    exit 0
fi

note "tearing stack down"
"${compose[@]}" down --remove-orphans
note "demo finished cleanly"
