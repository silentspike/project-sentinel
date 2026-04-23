#!/usr/bin/env bash
set -euo pipefail

duration="${ISSUE279_SOAK_SECONDS:-1800}"
interval="${ISSUE279_POLL_INTERVAL:-60}"
out_dir="${ISSUE279_OUT_DIR:-/tmp/issue279-soak-$(date -u +%Y%m%dT%H%M%SZ)}"
start_epoch="$(date +%s)"
end_epoch="$((start_epoch + duration))"
start_since="$(date -u '+%Y-%m-%d %H:%M:%S UTC')"

mkdir -p "$out_dir"

opcurl() {
  if [ -x /tmp/opcurl ]; then
    /tmp/opcurl "$@"
  else
    curl -s "$@"
  fi
}

read_health() {
  opcurl http://127.0.0.1:8084/operator/runtime-health
}

health_line() {
  python3 -c 'import sys,json
h=json.load(sys.stdin)
print("{expected}\t{runtime}\t{projection}\t{cgroups}\t{stale}\t{orphans}\t{zombies}\t{drift}\t{depth}\t{dropped}\t{coalesced}".format(
    expected=h["expected_active_agents"],
    runtime=h["runtime_agents"],
    projection=h["projection_agents"],
    cgroups=h["live_cgroup_dirs"],
    stale=h["stale_runtime_entries"],
    orphans=h["orphan_cgroups"],
    zombies=h["zombie_tracked_pids"],
    drift=h["projection_drift_detected"],
    depth=h["analysis_queue_depth"],
    dropped=h["analysis_queue_dropped_total"],
    coalesced=h["analysis_queue_coalesced_total"],
))'
}

api_agents() {
  curl -s http://127.0.0.1:8000/api/agents \
    | python3 -c 'import sys,json; print(len(json.load(sys.stdin)))'
}

projection_active_agents() {
  sqlite3 /opt/sentinel/data/projection.db \
    "SELECT COUNT(*) FROM agent_live_view WHERE status='active';"
}

cgroup_dirs() {
  find /sys/fs/cgroup/sentinel -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l
}

cgroup_live_dirs() {
  python3 - <<'PY'
import os
root = "/sys/fs/cgroup/sentinel"
count = 0
try:
    names = os.listdir(root)
except FileNotFoundError:
    names = []
for name in names:
    if not os.path.isdir(os.path.join(root, name)):
        continue
    try:
        if open(os.path.join(root, name, "cgroup.procs")).read().strip():
            count += 1
    except FileNotFoundError:
        pass
print(count)
PY
}

system_line() {
  local daemon_pid projection_pid
  daemon_pid="$(pgrep -x sentinel-daemon | head -1 || true)"
  projection_pid="$(pgrep -f '/opt/sentinel/bin/sentinel-projection' | head -1 || true)"
  printf "%s\t" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if [ -n "$daemon_pid" ]; then
    ps -o pid=,rss=,%cpu=,comm= -p "$daemon_pid" | awk '{printf "daemon_pid=%s\tdaemon_rss_kb=%s\tdaemon_cpu=%s\t", $1, $2, $3}'
  else
    printf "daemon_pid=missing\tdaemon_rss_kb=0\tdaemon_cpu=0\t"
  fi
  if [ -n "$projection_pid" ]; then
    ps -o pid=,rss=,%cpu=,comm= -p "$projection_pid" | awk '{printf "projection_pid=%s\tprojection_rss_kb=%s\tprojection_cpu=%s\t", $1, $2, $3}'
  else
    printf "projection_pid=missing\tprojection_rss_kb=0\tprojection_cpu=0\t"
  fi
  df -k /opt/sentinel/data | awk 'NR==2 {printf "data_used_kb=%s\tdata_avail_kb=%s\n", $3, $4}'
}

{
  echo "# started_at=$start_since"
  echo "# duration_seconds=$duration"
  echo "# interval_seconds=$interval"
  echo -e "sample\ttimestamp\texpected\truntime\tprojection\tcgroups\tstale\torphans\tzombies\tdrift\tqueue_depth\tdropped\tcoalesced\tapi_agents\tprojection_db_active\tcgroup_dirs\tcgroup_live_dirs"
} >"$out_dir/health.tsv"
echo -e "timestamp\tdaemon_pid\tdaemon_rss_kb\tdaemon_cpu\tprojection_pid\tprojection_rss_kb\tprojection_cpu\tdata_used_kb\tdata_avail_kb" >"$out_dir/system.tsv"

sample=0
while [ "$(date +%s)" -le "$end_epoch" ]; do
  sample=$((sample + 1))
  timestamp="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  health_json="$(read_health)"
  printf "%s" "$health_json" >"$out_dir/health-$sample.json"
  line="$(printf "%s" "$health_json" | health_line)"
  api_count="$(api_agents)"
  projection_count="$(projection_active_agents)"
  cgroup_count="$(cgroup_dirs)"
  live_cgroup_count="$(cgroup_live_dirs)"
  echo -e "$sample\t$timestamp\t$line\t$api_count\t$projection_count\t$cgroup_count\t$live_cgroup_count" | tee -a "$out_dir/health.tsv"
  system_line | tee -a "$out_dir/system.tsv" >/dev/null
  sleep "$interval"
done

final_json="$(read_health)"
printf "%s" "$final_json" >"$out_dir/final-health.json"
api_final="$(api_agents)"
projection_final="$(projection_active_agents)"
cgroup_final="$(cgroup_dirs)"
cgroup_live_final="$(cgroup_live_dirs)"

journalctl -u sentinel-daemon --since "$start_since" --no-pager >"$out_dir/daemon-journal.log"
journalctl -u sentinel-projection --since "$start_since" --no-pager >"$out_dir/projection-journal.log"

printf "%s" "$final_json" | python3 -c 'import sys,json
h=json.load(sys.stdin)
ok = (
    h["expected_active_agents"] == 26
    and h["runtime_agents"] == 26
    and h["projection_agents"] == 26
    and h["live_cgroup_dirs"] == 26
    and h["stale_runtime_entries"] == 0
    and h["orphan_cgroups"] == 0
    and h["zombie_tracked_pids"] == 0
    and h["projection_drift_detected"] is False
)
print("final_health={}/{}/{}/{} stale={} orphans={} zombies={} drift={}".format(
    h["expected_active_agents"],
    h["runtime_agents"],
    h["projection_agents"],
    h["live_cgroup_dirs"],
    h["stale_runtime_entries"],
    h["orphan_cgroups"],
    h["zombie_tracked_pids"],
    h["projection_drift_detected"],
))
raise SystemExit(0 if ok else 1)'

test "$api_final" = "26"
test "$projection_final" = "26"
test "$cgroup_final" = "26"
test "$cgroup_live_final" = "26"

if grep -Ei 'panic|drift' "$out_dir/daemon-journal.log" >/dev/null; then
  echo "daemon_journal_panic_or_drift=FAIL"
  grep -Ei 'panic|drift' "$out_dir/daemon-journal.log"
  exit 1
fi

echo "api_agents=$api_final"
echo "projection_db_active=$projection_final"
echo "cgroup_dirs=$cgroup_final"
echo "cgroup_live_dirs=$cgroup_live_final"
echo "daemon_journal_panic_or_drift=0"
echo "SOAK_PASS out_dir=$out_dir"
