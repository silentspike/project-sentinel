#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:8084}"
OUT_DIR="${OUT_DIR:-/tmp/issue279-bench-$(date -u +%Y%m%dT%H%M%SZ)}"
START_TIME="$(date -u '+%Y-%m-%d %H:%M:%S')"

mkdir -p "$OUT_DIR"

secret="$(python3 - <<'PY'
import pathlib, tomllib
cfg = tomllib.loads(pathlib.Path('/opt/sentinel/config/daemon.toml').read_text())
print(cfg.get('daemon', {}).get('operator_api', {}).get('shared_secret') or '')
PY
)"

CURL_AUTH=()
if [ -n "$secret" ]; then
  CURL_AUTH=(-H "x-sentinel-operator-key: $secret")
fi

op_get() {
  curl -fsS "${CURL_AUTH[@]}" "$BASE_URL$1"
}

op_post() {
  local path="$1"
  local body="$2"
  curl -fsS "${CURL_AUTH[@]}" -H 'Content-Type: application/json' -d "$body" "$BASE_URL$path"
}

json_field() {
  local file="$1"
  local field="$2"
  python3 - "$file" "$field" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)
value = payload.get(sys.argv[2])
if value is None:
    raise SystemExit(f"missing field: {sys.argv[2]}")
print(value)
PY
}

record_result() {
  local name="$1"
  local actual="$2"
  local target="$3"
  local unit="$4"
  local source="$5"
  local pass="PASS"
  if [ "$actual" -ge "$target" ]; then
    pass="FAIL"
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$name" "$actual" "$target" "$unit" "$pass" "$source" >> "$OUT_DIR/bench-results.tsv"
  if [ "$pass" = "FAIL" ]; then
    echo "BENCH_FAIL $name actual=$actual target=$target unit=$unit source=$source" >&2
    exit 1
  fi
}

assert_healthy() {
  local file="$1"
  python3 - "$file" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    h = json.load(handle)
expected = h["expected_active_agents"]
checks = {
    "runtime_agents": h["runtime_agents"] == expected,
    "projection_agents": h["projection_agents"] == expected,
    "live_cgroup_dirs": h["live_cgroup_dirs"] == expected,
    "stale_runtime_entries": h["stale_runtime_entries"] == 0,
    "orphan_cgroups": h["orphan_cgroups"] == 0,
    "zombie_tracked_pids": h["zombie_tracked_pids"] == 0,
    "projection_drift_detected": not h["projection_drift_detected"],
}
failed = [key for key, ok in checks.items() if not ok]
if failed:
    raise SystemExit("unhealthy: " + ",".join(failed))
print(
    f"healthy expected={expected} runtime={h['runtime_agents']} projection={h['projection_agents']} "
    f"cgroups={h['live_cgroup_dirs']} stale={h['stale_runtime_entries']} "
    f"orphans={h['orphan_cgroups']} zombies={h['zombie_tracked_pids']} "
    f"snapshot_us={h.get('snapshot_build_elapsed_us')}"
)
PY
}

wait_healthy() {
  local label="$1"
  local file="$OUT_DIR/health-${label}.json"
  for attempt in $(seq 1 90); do
    op_get /operator/runtime-health > "$file"
    if assert_healthy "$file" > "$OUT_DIR/health-${label}.txt" 2>/dev/null; then
      cat "$OUT_DIR/health-${label}.txt"
      return 0
    fi
    sleep 2
  done
  assert_healthy "$file"
}

process_snapshot() {
  local label="$1"
  local daemon_pid
  local projection_pid
  daemon_pid="$(pgrep -x sentinel-daemon | head -n1 || true)"
  projection_pid="$(pgrep -f '/opt/sentinel/bin/sentinel-projection' | head -n1 || true)"
  {
    echo "label=$label"
    echo "daemon_pid=$daemon_pid"
    echo "projection_pid=$projection_pid"
    if [ -n "$daemon_pid$projection_pid" ]; then
      ps -o pid,ppid,stat,%cpu,%mem,rss,comm -p "$daemon_pid" "$projection_pid" 2>/dev/null || true
    fi
  } > "$OUT_DIR/ps-${label}.txt"
}

capture_sidecars() {
  local label="$1"
  free -m > "$OUT_DIR/free-${label}.txt"
  vmstat 1 5 > "$OUT_DIR/vmstat-${label}.txt"
  if ! command -v iostat >/dev/null 2>&1; then
    echo "iostat missing; install sysstat before benchmark acceptance" >&2
    exit 1
  fi
  iostat -x 1 5 > "$OUT_DIR/iostat-${label}.txt"
  process_snapshot "$label"
}

printf '%s\t%s\t%s\t%s\t%s\t%s\n' "name" "actual" "target_exclusive" "unit" "pass" "source" > "$OUT_DIR/bench-results.tsv"

capture_sidecars before
wait_healthy pre
expected_active_agents="$(json_field "$OUT_DIR/health-pre.json" expected_active_agents)"

max_reconcile_us=0
for idx in $(seq 1 5); do
  file="$OUT_DIR/runtime-reconcile-${idx}.json"
  op_post /operator/runtime/reconcile '{"dry_run":true,"respawn_missing":true,"projection_rebuild":false}' > "$file"
  elapsed="$(json_field "$file" elapsed_us)"
  printf '%s\t%s\n' "$idx" "$elapsed" >> "$OUT_DIR/runtime-reconcile-runs.tsv"
  if [ "$elapsed" -gt "$max_reconcile_us" ]; then
    max_reconcile_us="$elapsed"
  fi
done
record_result "runtime_reconcile_${expected_active_agents}_agents" "$max_reconcile_us" 5000 us runtime-reconcile-runs.tsv

max_projection_us=0
for idx in $(seq 1 5); do
  file="$OUT_DIR/projection-divergence-${idx}.json"
  op_get /operator/runtime-health > "$file"
  elapsed="$(json_field "$file" snapshot_build_elapsed_us)"
  printf '%s\t%s\n' "$idx" "$elapsed" >> "$OUT_DIR/projection-divergence-runs.tsv"
  if [ "$elapsed" -gt "$max_projection_us" ]; then
    max_projection_us="$elapsed"
  fi
done
record_result projection_divergence_detection "$max_projection_us" 5000 us projection-divergence-runs.tsv

flood_file="$OUT_DIR/analysis-flood.json"
op_post /operator/runtime/analysis-flood-test '{"count":10000}' > "$flood_file"
flood_per_request_ns="$(json_field "$flood_file" enqueue_per_request_ns)"
record_result analysis_flood_cap "$flood_per_request_ns" 250000 ns_per_request analysis-flood.json

candidate_id="$(python3 - "$OUT_DIR/health-pre.json" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as handle:
    h = json.load(handle)
for agent in h["agents"]:
    if agent["runtime_present"] and agent["tracked_pid_alive"] and agent["cgroup_live_pid_count"] > 0:
        print(agent["agent_id"])
        break
else:
    raise SystemExit("no healthy benchmark candidate")
PY
)"
stall_file="$OUT_DIR/stall-restart-direct.json"
op_post /operator/runtime/stall-restart-test "{\"agent_id\":${candidate_id},\"mode\":\"direct\",\"stall_secs\":1}" > "$stall_file"
stall_ns="$(json_field "$stall_file" bookkeeping_elapsed_ns)"
record_result stall_recovery_bookkeeping "$stall_ns" 50000 ns stall-restart-direct.json
wait_healthy post-stall

capture_sidecars after

journal_file="$OUT_DIR/daemon-journal-panic-drift.txt"
journalctl -u sentinel-daemon --since "$START_TIME" --no-pager \
  | grep -Ei 'panicked|thread .*panicked|projection drift detected|runtime drift|projection_drift_detected=true' \
  > "$journal_file" || true
if [ -s "$journal_file" ]; then
  echo "BENCH_FAIL daemon journal contains panic/drift markers: $journal_file" >&2
  exit 1
fi

{
  echo "BENCH_PASS out_dir=$OUT_DIR"
  cat "$OUT_DIR/bench-results.tsv"
  echo "sidecars:"
  ls -1 "$OUT_DIR"/free-*.txt "$OUT_DIR"/vmstat-*.txt "$OUT_DIR"/iostat-*.txt "$OUT_DIR"/ps-*.txt
} | tee "$OUT_DIR/summary.txt"
