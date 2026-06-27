#!/usr/bin/env python3
# #428 benchmarks (run on the deploy VM, daemon-direct loopback :8084):
# - FS browse latency (dir listing + file read) p50/p95  (1:n note: inode/pointer read in CAS,
#   no data transfer/copy; dedup ratio reported from storage_stats)
# - Pause/Resume command round-trip latency
# Light latency probe of the deployed feature (~ a few hundred quick requests), not a load test.
import json, subprocess, time, statistics, urllib.request

B = "http://127.0.0.1:8084"
AGENT = 8
NAME = "Lena Hoffmann"


def post(path, body):
    data = json.dumps(body).encode()
    req = urllib.request.Request(B + path, data=data, headers={"content-type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=10))


def get(path):
    return json.load(urllib.request.urlopen(B + path, timeout=10))


def timed(fn, n):
    xs = []
    for _ in range(n):
        t0 = time.perf_counter_ns()
        fn()
        xs.append((time.perf_counter_ns() - t0) / 1000.0)  # us
    xs.sort()
    p = lambda q: xs[min(len(xs) - 1, int(q * len(xs)))]
    return {"n": n, "p50_us": round(p(0.5), 1), "p95_us": round(p(0.95), 1),
            "min_us": round(xs[0], 1), "max_us": round(xs[-1], 1)}


def sysline():
    free = subprocess.run(["free", "-m"], capture_output=True, text=True).stdout.splitlines()
    mem = [l for l in free if l.startswith("Mem:")][0].split()
    load = open("/proc/loadavg").read().split()[:3]
    return {"mem_used_mb": int(mem[2]), "mem_total_mb": int(mem[1]), "loadavg": load}


# Populate AGENT-08 with a few real files so the FS browse has dirents to read.
post("/operator/security/fs-dedup-benchmark",
     {"agent_name": NAME, "writes": 8, "bytes_per_write": 512, "file_prefix": "bench428"})
root = get(f"/operator/security/agent-fs?agent_id={AGENT}&inode=1")
subdir = next((e for e in root["entries"] if e["kind"] == "dir"), None)
sub_inode = subdir["inode"] if subdir else 1
listing = get(f"/operator/security/agent-fs?agent_id={AGENT}&inode={sub_inode}")
file_entry = next((e for e in listing["entries"] if e["kind"] == "file"), None)
file_inode = file_entry["inode"] if file_entry else None

print("SYS_BEFORE", json.dumps(sysline()))

res = {}
res["fs_browse_root"] = timed(lambda: get(f"/operator/security/agent-fs?agent_id={AGENT}&inode=1"), 100)
res["fs_browse_dir"] = timed(lambda: get(f"/operator/security/agent-fs?agent_id={AGENT}&inode={sub_inode}"), 100)
if file_inode:
    res["fs_read_file"] = timed(lambda: get(f"/operator/security/agent-fs-read?agent_id={AGENT}&inode={file_inode}"), 100)

# Pause/Resume round-trip: pause then resume, time each command.
pause_us, resume_us = [], []
for _ in range(20):
    t0 = time.perf_counter_ns(); post("/operator/runtime/pause", {"agent_id": AGENT}); pause_us.append((time.perf_counter_ns() - t0) / 1000.0)
    t0 = time.perf_counter_ns(); post("/operator/runtime/resume", {"agent_id": AGENT}); resume_us.append((time.perf_counter_ns() - t0) / 1000.0)
pause_us.sort(); resume_us.sort()
res["pause_cmd"] = {"n": 20, "p50_us": round(pause_us[10], 1), "p95_us": round(pause_us[18], 1)}
res["resume_cmd"] = {"n": 20, "p50_us": round(resume_us[10], 1), "p95_us": round(resume_us[18], 1)}

stats = get(f"/operator/security/agent-fs?agent_id={AGENT}&inode=1")
res["dedup"] = {"dedup_ratio_percent": round(stats["dedup_ratio_percent"], 2),
                "cas_blob_count": stats["cas_blob_count"],
                "dedup_savings_bytes": stats["dedup_savings_bytes"]}

print("SYS_AFTER", json.dumps(sysline()))
print("RESULTS", json.dumps(res, indent=2))
