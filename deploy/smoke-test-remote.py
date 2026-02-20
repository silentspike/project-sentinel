#!/usr/bin/env python3
"""Remote smoke test script — runs on the target VM via SSH.
Checks health endpoints and systemd services within a timeout.
Usage: python3 smoke-test-remote.py [TIMEOUT_SEC]
"""
import json
import subprocess
import sys
import time

HEALTH_ENDPOINTS = [
    (8080, "/health", "sentinel-daemon"),
    (8000, "/api/health", "sentinel-dashboard"),
]

SERVICES = [
    "sentinel-daemon",
    "sentinel-cortex",
    "sentinel-dashboard",
    "sentinel-nightrun.timer",
    "nats-server",
    "sentinel-nats-bridge",
    "sentinel-judge",
]

timeout = int(sys.argv[1]) if len(sys.argv) > 1 else 30
start = time.time()

# Poll health endpoints until all healthy or timeout
all_healthy = False
while time.time() - start < timeout:
    ok = 0
    for port, path, _name in HEALTH_ENDPOINTS:
        try:
            r = subprocess.run(
                ["curl", "-sf", "http://localhost:%d%s" % (port, path)],
                capture_output=True,
                text=True,
                timeout=3,
            )
            if r.returncode == 0:
                d = json.loads(r.stdout)
                if d.get("status") == "ok":
                    ok += 1
        except Exception:
            pass
    if ok == len(HEALTH_ENDPOINTS):
        all_healthy = True
        break
    time.sleep(1)

elapsed = int(time.time() - start)

# Final detailed check
print("%-25s %-10s %s" % ("Service", "Status", "Detail"))
print("-" * 60)

health_pass = 0
for port, path, name in HEALTH_ENDPOINTS:
    try:
        r = subprocess.run(
            ["curl", "-sf", "http://localhost:%d%s" % (port, path)],
            capture_output=True,
            text=True,
            timeout=3,
        )
        if r.returncode == 0:
            d = json.loads(r.stdout)
            if d.get("status") == "ok":
                print("%-25s %-10s %s" % (name, "PASS", r.stdout.strip()))
                health_pass += 1
            else:
                print("%-25s %-10s status=%s" % (name, "FAIL", d.get("status")))
        else:
            print("%-25s %-10s port %d not responding" % (name, "FAIL", port))
    except Exception as e:
        print("%-25s %-10s %s" % (name, "FAIL", e))

print()
print("systemd Services:")
print("-" * 60)

svc_pass = 0
for svc in SERVICES:
    try:
        r = subprocess.run(
            ["systemctl", "is-active", svc],
            capture_output=True,
            text=True,
            timeout=3,
        )
        state = r.stdout.strip()
    except Exception:
        state = "unknown"
    if state == "active":
        svc_pass += 1
    print("%-25s %s" % (svc, state))

print()
total_ep = len(HEALTH_ENDPOINTS)
total_svc = len(SERVICES)
print(
    "Results: %d/%d health endpoints OK, %d/%d services active (%ds elapsed)"
    % (health_pass, total_ep, svc_pass, total_svc, elapsed)
)

if all_healthy and svc_pass >= 5:
    print("SMOKE TEST PASSED (%ds <= %ds timeout)" % (elapsed, timeout))
    sys.exit(0)
else:
    print("SMOKE TEST FAILED", file=sys.stderr)
    sys.exit(1)
