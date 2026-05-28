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

tmp_file=""
needs_sudo=0
if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  needs_sudo=1
  tmp_file="/tmp/sentinel-ws-strace-${USER:-ubuntu}-$$.txt"
  sudo -n rm -f "$tmp_file"
  sudo -n touch "$tmp_file"
  sudo -n chmod 644 "$tmp_file"
else
  tmp_file="$(mktemp)"
fi
cleanup() {
  if [[ "$needs_sudo" -eq 1 ]]; then
    sudo -n rm -f "$tmp_file" >/dev/null 2>&1 || true
  else
    rm -f "$tmp_file"
  fi
}
trap cleanup EXIT

echo "dashboard_pid=$pid"
echo "duration_seconds=$duration_seconds"
echo "trace=pread64"

strace_cmd=(strace -e trace=pread64 -p "$pid" -c -o "$tmp_file")
if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  strace_cmd=(sudo -n "${strace_cmd[@]}")
fi

if "${strace_cmd[@]}" &
then
  strace_pid=$!
else
  echo "failed to start strace for pid $pid" >&2
  exit 1
fi

sleep 1
if ! kill -0 "$strace_pid" >/dev/null 2>&1; then
  wait "$strace_pid"
  exit $?
fi

sleep "$duration_seconds"
kill -INT "$strace_pid" >/dev/null 2>&1 || sudo -n kill -INT "$strace_pid" >/dev/null 2>&1 || true
wait "$strace_pid" >/dev/null 2>&1 || true
if [[ ! -s "$tmp_file" ]]; then
  echo "strace produced no summary" >&2
  exit 1
fi
cat "$tmp_file"
