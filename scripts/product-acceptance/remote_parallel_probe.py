#!/usr/bin/env python3
"""Bounded target-side observation. No database, provider, or runtime writes."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import select
import socket
import signal
import subprocess
import threading
import time
import urllib.error
import urllib.request


UNITS = ("sentinel-daemon", "sentinel-gateway", "sentinel-projection")
ENDPOINTS = {"readiness": "/operator/runtime-readiness", "health": "/operator/runtime-health"}
MAX_BODY = 4 * 1024 * 1024
PHASE_LABELS = {
    "Owned provider phase finished": ("provider", "phase", frozenset((
        "enqueue_wait", "permit_wait", "unreserved_authority", "reservation", "predispatch_authority",
        "http", "response_decode", "persistence", "cleanup", "total"))),
    "Owned completion stage finished": ("adoption", "stage", frozenset((
        "recovery_refresh", "usage", "admission", "claim", "complete", "encode", "enqueue", "load",
        "adaptive_adoption_read", "adaptive_model_commit"))),
    "Adaptive context evidence read": ("context", "phase", frozenset((
        "preparation_snapshot", "latest_observation", "post_callback_snapshot",
        "working_memory_source", "historical_observations"))),
    "Slow initial provider preparation": ("preparation", "phase", frozenset(("selection", "context"))),
    "Initial provider preparation finished": ("preparation", "phase", frozenset(("selection", "context"))),
    "Work funding preparation phase": ("funding", "phase", frozenset((
        "request_total", "recovery_fence_wait", "current_source", "draft_total", "receipt_transaction"))),
    "Company reconciliation phase completed": ("reconciliation", "phase", frozenset((
        "full_scan", "delivery_publication", "collaboration_publication", "sales_intake",
        "project_inventory", "tool_hint_inventory", "unknown_model_recovery", "standing_work_funding",
        "adaptive_leadership"))),
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def distribution(values):
    require(all(type(v) in (int, float) and math.isfinite(v) and v >= 0 for v in values),
            "invalid duration")
    if not values:
        return {"n": 0, "p50_ms": None, "p95_ms": None, "p99_ms": None}
    ordered = sorted(values)
    def quantile(q):
        return ordered[math.ceil(len(ordered) * q) - 1]
    return {"n": len(values), "p50_ms": quantile(.50), "p95_ms": quantile(.95),
            "p99_ms": quantile(.99) if len(values) >= 100 else None,
            "max_ms": ordered[-1], "tail_samples_above_p99_rank": len(values) // 100,
            "sampling_note": "Empirical nearest rank; N is not a confidence or independence guarantee."}


def service_identity():
    result = {}
    for unit in UNITS:
        raw = subprocess.check_output(
            ["systemctl", "show", unit, "-p", "MainPID", "-p", "NRestarts", "-p", "ActiveState"],
            timeout=5, text=True)
        fields = dict(line.split("=", 1) for line in raw.splitlines())
        result[unit] = {"pid": int(fields["MainPID"]), "restarts": int(fields["NRestarts"]),
                        "active": fields["ActiveState"] == "active"}
    return result


def counters(services):
    mem = {}
    for line in Path("/proc/meminfo").read_text().splitlines():
        key, value = line.split(":", 1)
        if key in ("MemTotal", "MemAvailable", "Cached", "Buffers", "SwapTotal", "SwapFree", "Dirty"):
            mem[key + "_kib"] = int(value.split()[0])
    vm = dict(line.split() for line in Path("/proc/vmstat").read_text().splitlines())
    processes = {}
    for unit, service in services.items():
        try:
            root = Path("/proc") / str(service["pid"])
            stat = (root / "stat").read_text().rsplit(")", 1)[1].split()
            io = dict(line.split(":", 1) for line in (root / "io").read_text().splitlines())
            processes[unit] = {"user_ticks": int(stat[11]), "system_ticks": int(stat[12]),
                               "start_ticks": int(stat[19]), "rss_pages": int(stat[21]),
                               **{key: int(io[key]) for key in ("read_bytes", "write_bytes")}}
        except (OSError, ValueError, KeyError, IndexError):
            processes[unit] = None
    pressure = {}
    for kind in ("cpu", "memory", "io"):
        path = Path("/proc/pressure") / kind
        pressure[kind] = path.read_text().strip() if path.exists() else None
    return {"monotonic_ns": time.monotonic_ns(), "memory": mem,
            "cpu_ticks": [int(n) for n in Path("/proc/stat").read_text().splitlines()[0].split()[1:]],
            "load": [float(n) for n in Path("/proc/loadavg").read_text().split()[:3]],
            "vm": {key: int(vm[key]) for key in ("pgfault", "pgmajfault", "pswpin", "pswpout")},
            "diskstats": [{"device": [int(n) for n in line.split()[:2]],
                           "counters": [int(n) for n in line.split()[3:]]}
                          for line in Path("/proc/diskstats").read_text().splitlines()
                          if not line.split()[2].startswith(("loop", "ram"))],
            "pressure": pressure, "processes": processes}


def parse_phase(entry):
    message = entry.get("MESSAGE")
    if type(message) is list and len(message) <= 65536 and all(type(v) is int and 0 <= v <= 255 for v in message):
        message = bytes(message).decode("utf8", errors="replace")
    if type(message) is not str or len(message) > 65536:
        return None
    message = re.sub(r"\x1b\[[0-9;]*m", "", message)
    for label, (family, field, allowed) in PHASE_LABELS.items():
        if label not in message:
            continue
        fields = dict(re.findall(r'\b([a-z_]+)="?([A-Za-z0-9:_-]{1,192})(?:"|\s|$)', message))
        phase = fields.get(field)
        if phase not in allowed:
            return None
        unit = "us" if "elapsed_us" in fields else "ms"
        elapsed = fields.get("elapsed_" + unit, "")
        if not elapsed.isdigit() or len(elapsed) > 20:
            return None
        if "elapsed_us" in fields and "elapsed_ms" in fields:
            require(int(fields["elapsed_us"]) // 1000 == int(fields["elapsed_ms"]), "inconsistent phase units")
        row = {"family": family, "phase": phase, "elapsed": int(elapsed), "unit": unit}
        for key in ("request_id", "agent"):
            if key in fields:
                row[key] = fields[key]
        for key in ("accepted", "succeeded"):
            if fields.get(key) in ("true", "false"):
                row["accepted"] = fields[key] == "true"
        timestamp = str(entry.get("__REALTIME_TIMESTAMP", ""))
        if timestamp.isdigit() and len(timestamp) <= 20:
            row["timestamp_us"] = int(timestamp)
        row["attribution"] = "request_id" if "request_id" in row else "process_only_not_original_task"
        return row
    return None


def journal_metadata(pid):
    child = subprocess.Popen(["journalctl", "_PID=" + str(pid), "--since=-10min", "--lines=2000",
                              "--grep=" + "|".join(PHASE_LABELS), "--output=json", "--no-pager"],
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    chunks, size, started = [], 0, time.monotonic()
    try:
        while True:
            remaining = 25 - (time.monotonic() - started)
            require(remaining > 0 and select.select([child.stdout], [], [], remaining)[0], "journal timed out")
            chunk = os.read(child.stdout.fileno(), 65536)
            if not chunk:
                break
            size += len(chunk)
            require(size <= MAX_BODY, "journal exceeded byte bound")
            chunks.append(chunk)
        code = child.wait(timeout=1)
        require(code == 0 or (code == 1 and size == 0), "journal read failed")
    finally:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=5)
    lines = b"".join(chunks).splitlines()
    require(len(lines) <= 2000, "journal exceeded row bound")
    rows = [parse_phase(json.loads(line)) for line in lines]
    return {"records": [row for row in rows if row is not None], "window_seconds": 600,
            "possibly_truncated": len(lines) == 2000,
            "scope": "Existing threshold-censored log spans only, not a workload distribution. Missing records are NOT_MEASURED. Context/funding without request IDs are process-only."}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, newurl):
        return None


def sample(endpoint, token, timeout):
    request = urllib.request.Request("http://127.0.0.1:8084" + ENDPOINTS[endpoint],
                                     headers={"Authorization": "Bearer " + token}, method="GET")
    started = time.monotonic_ns()
    try:
        with urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect()).open(
                request, timeout=timeout) as response:
            raw = response.read(MAX_BODY + 1)
            require(len(raw) <= MAX_BODY, "response exceeded bound")
            value = json.loads(raw)
            # Never export agent/customer bodies or the operator credential.
            safe = {key: value[key] for key in (
                "expected_active_agents", "runtime_agents", "projection_agents",
                "stale_runtime_entries", "orphan_cgroups", "zombie_tracked_pids",
                "projection_drift_detected", "snapshot_build_elapsed_us")
                    if key in value and type(value[key]) in (int, bool)}
            return {"ok": response.status == 200, "status": response.status,
                    "elapsed_ms": (time.monotonic_ns() - started) / 1e6, "summary": safe}
    except urllib.error.HTTPError as error:
        return {"ok": False, "status": error.code,
                "elapsed_ms": (time.monotonic_ns() - started) / 1e6}
    except (OSError, ValueError, TimeoutError):
        return {"ok": False, "status": None,
                "elapsed_ms": (time.monotonic_ns() - started) / 1e6}


def observe(args):
    require(socket.gethostname() == args.expected_hostname, "wrong runtime host")
    require(1 <= args.samples <= 1000 and .05 <= args.interval <= 10,
            "sample count/interval outside observation bounds")
    require(1 <= args.seconds <= 600 and 1 <= args.timeout <= 45, "invalid time bound")
    require(args.endpoint != "health" or args.samples <= 10, "full-health observation limited to ten reads")
    manifest_path = Path("/opt/sentinel/release-manifest.json")
    manifest = manifest_path.read_bytes()
    require(hashlib.sha256(manifest).hexdigest() == args.manifest_sha256, "release pin mismatch")
    token = Path(args.token_file).read_text().strip()
    before = service_identity()
    require(all(s["active"] and s["pid"] > 0 for s in before.values()), "service inactive")
    stop = threading.Event()
    sidecars, errors = [], []
    def monitor():
        try:
            while not stop.is_set():
                sidecars.append(counters(before))
                stop.wait(1)
        except Exception:
            errors.append("sidecar_failed")
    worker = threading.Thread(target=monitor, daemon=True)
    worker.start()
    started = time.monotonic()
    rows = []
    try:
        for _ in range(args.samples if args.endpoint != "phases" else 0):
            remaining = args.seconds - (time.monotonic() - started)
            if remaining <= 0:
                break
            rows.append(sample(args.endpoint, token, min(args.timeout, remaining)))
            if len(rows) < args.samples:
                time.sleep(min(args.interval, max(0, args.seconds - (time.monotonic() - started))))
    finally:
        stop.set()
        worker.join(timeout=5)
    after = service_identity()
    stable = before == after and manifest_path.read_bytes() == manifest
    phases = journal_metadata(before["sentinel-daemon"]["pid"]) if args.endpoint == "phases" else None
    if phases is not None:
        sidecars.append(counters(before))
        stable = stable and before == service_identity() and manifest_path.read_bytes() == manifest
    complete = args.endpoint == "phases" or (len(rows) == args.samples and all(r["ok"] for r in rows))
    return {"schema": 1, "kind": "read_only_observation", "runtime_target_class": args.target_class,
            "status": "OBSERVED" if stable and complete and sidecars and not errors else "INCOMPLETE",
            "release_stable": stable, "manifest_sha256": args.manifest_sha256,
            "source_git": json.loads(manifest).get("git_sha"), "endpoint": args.endpoint,
            "elapsed_seconds": time.monotonic() - started, "requested_samples": args.samples,
            "http": distribution([r["elapsed_ms"] for r in rows if r["ok"]]),
            "rows": rows, "sidecars": sidecars, "sidecar_errors": errors, "phase_observation": phases,
            "clock_ticks_per_second": os.sysconf("SC_CLK_TCK"), "page_bytes": os.sysconf("SC_PAGE_SIZE"),
            "provider_latency": "NOT_MEASURED", "balloon_assignment": "REQUIRES_HYPERVISOR_SIDECAR",
            "cache_state": "uncontrolled_observed_only", "concurrent_load": "uncontrolled_observed_only",
            "business_mutations": 0, "provider_dispatches": 0,
            "scope": "Readiness/health HTTP only; not persistence, work completion, or release benchmark acceptance."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expected-hostname", required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--target-class", choices=("SINGLE_NODE", "LAB"), required=True)
    parser.add_argument("--token-file", default="/etc/sentinel/credentials/operator-api")
    parser.add_argument("--endpoint", choices=(*ENDPOINTS, "phases"), default="readiness")
    parser.add_argument("--samples", type=int, default=100)
    parser.add_argument("--interval", type=float, default=.2)
    parser.add_argument("--seconds", type=float, default=60)
    parser.add_argument("--timeout", type=float, default=10)
    args = parser.parse_args()
    try:
        def expired(_signal, _frame):
            raise TimeoutError("observation wall deadline")
        signal.signal(signal.SIGALRM, expired)
        signal.setitimer(signal.ITIMER_REAL, max(1, min(args.seconds, 600)) + 30)
        result = observe(args)
        print(json.dumps(result, allow_nan=False))
        return 0 if result["status"] == "OBSERVED" else 2
    except Exception:
        print(json.dumps({"status": "BLOCKED", "reason": "target pin, prerequisite, or observation failure"}))
        return 2
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)


if __name__ == "__main__":
    raise SystemExit(main())
