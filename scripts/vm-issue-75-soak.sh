#!/usr/bin/env bash
set -euo pipefail

duration="${ISSUE75_SOAK_SECONDS:-1800}"
interval="${ISSUE75_POLL_INTERVAL:-60}"
out_dir="${ISSUE75_OUT_DIR:-/tmp/issue75/soak-$(date -u +%Y%m%dT%H%M%SZ)}"
start_epoch="$(date +%s)"
start_epoch_ms="$((start_epoch * 1000))"
start_since="$(date -u '+%Y-%m-%d %H:%M:%S UTC')"
end_epoch="$((start_epoch + duration))"

mkdir -p "$out_dir"

sidecar_pids=()

start_sidecar() {
  local name="$1"
  shift
  if command -v "$1" >/dev/null 2>&1; then
    "$@" >"$out_dir/${name}.log" 2>&1 &
    sidecar_pids+=("$!")
  else
    echo "$1 missing" >"$out_dir/${name}.log"
  fi
}

stop_sidecars() {
  local pid
  for pid in "${sidecar_pids[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  for pid in "${sidecar_pids[@]}"; do
    wait "$pid" 2>/dev/null || true
  done
}

trap stop_sidecars EXIT

read_health() {
  python3 - <<'PY'
import pathlib
import tomllib
import urllib.request

config = tomllib.loads(pathlib.Path("/opt/sentinel/config/daemon.toml").read_text())
secret = config.get("daemon", {}).get("operator_api", {}).get("shared_secret") or ""
headers = {"x-sentinel-operator-key": secret} if secret else {}
request = urllib.request.Request(
    "http://127.0.0.1:8084/operator/runtime-health", headers=headers
)
with urllib.request.urlopen(request, timeout=5) as response:
    print(response.read().decode(), end="")
PY
}

assert_health() {
  python3 -c 'import json, sys
h = json.load(sys.stdin)
expected = h["expected_active_agents"]
checks = {
    "runtime": h["runtime_agents"] == expected,
    "projection": h["projection_agents"] == expected,
    "cgroups": h["live_cgroup_dirs"] == expected,
    "stale": h["stale_runtime_entries"] == 0,
    "orphans": h["orphan_cgroups"] == 0,
    "zombies": h["zombie_tracked_pids"] == 0,
    "drift": h["projection_drift_detected"] is False,
}
failed = [name for name, passed in checks.items() if not passed]
print(
    "expected={} runtime={} projection={} cgroups={} stale={} orphans={} "
    "zombies={} drift={} failed={}".format(
        expected,
        h["runtime_agents"],
        h["projection_agents"],
        h["live_cgroup_dirs"],
        h["stale_runtime_entries"],
        h["orphan_cgroups"],
        h["zombie_tracked_pids"],
        str(h["projection_drift_detected"]).lower(),
        ",".join(failed) or "none",
    )
)
raise SystemExit(1 if failed else 0)'
}

network_counts() {
  python3 - <<'PY'
import glob
import os

def pids_for(comm):
    result = []
    for path in glob.glob("/proc/[0-9]*/comm"):
        try:
            if open(path, encoding="utf-8").read().strip() == comm:
                result.append(int(path.split("/")[2]))
        except OSError:
            pass
    return sorted(result)

daemon_pids = pids_for("sentinel-daemon")
agent_pids = pids_for("agent-runtime")
if len(daemon_pids) != 1:
    raise SystemExit(f"daemon_pid_count={len(daemon_pids)}")
daemon_ns = os.readlink(f"/proc/{daemon_pids[0]}/ns/net")
agent_ns = [os.readlink(f"/proc/{pid}/ns/net") for pid in agent_pids]
print(
    f"agents={len(agent_pids)} unique_netns={len(set(agent_ns))} "
    f"shared_with_daemon={sum(ns == daemon_ns for ns in agent_ns)}"
)
PY
}

service_counts() {
  local service
  for service in sentinel-daemon sentinel-gateway sentinel-projection sentinel-nats-bridge nats-server; do
    test "$(systemctl is-active "$service")" = "active"
    test "$(systemctl show "$service" -p NRestarts --value)" = "0"
  done
  echo "services=healthy restarts=0"
}

final_network_check() {
  python3 - <<'PY'
import glob
import os
import subprocess

def pids_for(comm):
    result = []
    for path in glob.glob("/proc/[0-9]*/comm"):
        try:
            if open(path, encoding="utf-8").read().strip() == comm:
                result.append(int(path.split("/")[2]))
        except OSError:
            pass
    return sorted(result)

daemon_pids = pids_for("sentinel-daemon")
agent_pids = pids_for("agent-runtime")
if len(daemon_pids) != 1 or len(agent_pids) != 26:
    raise SystemExit(
        f"unexpected process counts daemon={len(daemon_pids)} agents={len(agent_pids)}"
    )

daemon_ns = os.readlink(f"/proc/{daemon_pids[0]}/ns/net")
agent_ns = []
loopback_only = 0
for pid in agent_pids:
    agent_ns.append(os.readlink(f"/proc/{pid}/ns/net"))
    result = subprocess.run(
        ["nsenter", "-t", str(pid), "-n", "ip", "-o", "link", "show"],
        text=True,
        capture_output=True,
        check=True,
    )
    names = [line.split(": ", 2)[1].split("@", 1)[0] for line in result.stdout.splitlines()]
    loopback_only += names == ["lo"]

probe_pid = agent_pids[0]
probe = subprocess.run(
    [
        "nsenter",
        "-t",
        str(probe_pid),
        "-n",
        "timeout",
        "3",
        "bash",
        "-c",
        "echo >/dev/tcp/1.1.1.1/443",
    ],
    text=True,
    capture_output=True,
)
links = subprocess.run(
    ["ip", "-o", "link", "show"], text=True, capture_output=True, check=True
).stdout
legacy_links = [
    line
    for line in links.splitlines()
    if any(token in line for token in ("br-sentinel", "veth-", "vp-"))
]

checks = {
    "unique_netns": len(set(agent_ns)) == 26,
    "shared_with_daemon": all(ns != daemon_ns for ns in agent_ns),
    "loopback_only": loopback_only == 26,
    "external_blocked": probe.returncode != 0,
    "legacy_links": not legacy_links,
}
failed = [name for name, passed in checks.items() if not passed]
print(
    f"agent_count={len(agent_pids)} unique_agent_netns={len(set(agent_ns))} "
    f"shared_with_daemon={sum(ns == daemon_ns for ns in agent_ns)} "
    f"loopback_only_agents={loopback_only} external_probe_rc={probe.returncode} "
    f"legacy_links={len(legacy_links)} failed={','.join(failed) or 'none'}"
)
raise SystemExit(1 if failed else 0)
PY
}

start_sidecar vmstat vmstat 1
start_sidecar mpstat mpstat 1
start_sidecar iostat iostat -x 1
ss -tanup >"$out_dir/ss-before.txt"

{
  echo "started_at=$start_since"
  echo "duration_seconds=$duration"
  echo "interval_seconds=$interval"
  echo "binary_sha256=$(sha256sum /opt/sentinel/bin/sentinel-daemon | awk '{print $1}')"
} | tee "$out_dir/soak.log"

sample=0
while true; do
  now="$(date +%s)"
  health_json="$(read_health)"
  printf "%s" "$health_json" >"$out_dir/health-$sample.json"
  health_line="$(printf "%s" "$health_json" | assert_health)"
  network_line="$(network_counts)"
  service_line="$(service_counts)"
  printf "t=%4ss %s %s %s\n" "$((now - start_epoch))" "$health_line" "$network_line" "$service_line" | tee -a "$out_dir/soak.log"
  if [ "$now" -ge "$end_epoch" ]; then
    break
  fi
  sleep_for="$interval"
  if [ "$((now + sleep_for))" -gt "$end_epoch" ]; then
    sleep_for="$((end_epoch - now))"
  fi
  sleep "$sleep_for"
  sample=$((sample + 1))
done

final_network_check | tee -a "$out_dir/soak.log"
ss -tanup >"$out_dir/ss-after.txt"
journalctl -u sentinel-daemon --since "$start_since" --no-pager >"$out_dir/daemon-journal.log"

legacy_errors="$(grep -Ec 'Netns setup fehlgeschlagen|Bridge br-sentinel already exists' "$out_dir/daemon-journal.log" || true)"
isolation_log_failures="$(grep -Ec 'NICHT netz-isoliert|AgentIsolationFailed' "$out_dir/daemon-journal.log" || true)"
panic_fatal="$(grep -Eic 'panicked|panic|fatal' "$out_dir/daemon-journal.log" || true)"
isolation_events="$(sqlite3 /opt/sentinel/data/events.db "SELECT COUNT(*) FROM events WHERE event_type='AgentIsolationFailed' AND timestamp_ms >= $start_epoch_ms;")"

printf "FINAL legacy_errors=%s isolation_log_failures=%s isolation_events=%s panic_fatal=%s elapsed_s=%s\n" \
  "$legacy_errors" "$isolation_log_failures" "$isolation_events" "$panic_fatal" "$(( $(date +%s) - start_epoch ))" \
  | tee -a "$out_dir/soak.log"

test "$legacy_errors" = "0"
test "$isolation_log_failures" = "0"
test "$isolation_events" = "0"
test "$panic_fatal" = "0"

echo "SOAK_PASS out_dir=$out_dir" | tee -a "$out_dir/soak.log"
