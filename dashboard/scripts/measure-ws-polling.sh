#!/usr/bin/env bash
set -euo pipefail

duration_seconds="${1:-10}"
pid="${DASHBOARD_PID:-}"

if [[ -z "$pid" ]]; then
  pid="$(pgrep -f 'bun.*dashboard|bun.*src/index.ts|sentinel-dashboard' | head -n 1 || true)"
fi

if [[ -z "$pid" ]]; then
  echo "dashboard pid not found" >&2
  exit 1
fi

if ! command -v strace >/dev/null 2>&1; then
  echo "strace not found" >&2
  exit 1
fi

tmp_file="$(mktemp)"
cleanup() {
  rm -f "$tmp_file"
}
trap cleanup EXIT

echo "dashboard_pid=$pid"
echo "duration_seconds=$duration_seconds"
echo "trace=pread64"

if strace -e trace=pread64 -p "$pid" -c -o "$tmp_file" &
then
  strace_pid=$!
else
  echo "failed to start strace for pid $pid" >&2
  exit 1
fi

sleep "$duration_seconds"
kill -INT "$strace_pid" >/dev/null 2>&1 || true
wait "$strace_pid" >/dev/null 2>&1 || true
cat "$tmp_file"
