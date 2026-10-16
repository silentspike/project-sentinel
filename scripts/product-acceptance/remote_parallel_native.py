#!/usr/bin/env python3
"""Prepared ABBA comparison for the existing release-native query-only diagnostic.

Run only on the declared runtime target with reviewed artifacts and frozen inputs.
This runner never snapshots production or installs/deploys a binary.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import stat
import subprocess
import tempfile
import threading
import time

from remote_parallel_probe import counters, distribution, require, service_identity


TEST = "store::tests::diagnose_existing_expired_funding_source_read_only"
PASS = re.compile(r"READ_ONLY_HEALTH_PASS pass=([123]) repeat=3 elapsed_us=([0-9]+) read_cache_records=([0-9]+) total_changes=0")
DIAGNOSTIC = re.compile(r"READ_ONLY_FUNDING_DIAGNOSTIC mode=adaptive_health_inventory selected=Some\(true\) elapsed_ms=[0-9]+ error_code=None failures=\[\] writes=0")
MAX_OUTPUT = 2 * 1024 * 1024


def checked_file(root, reference, maximum):
    path = root / reference["file"]
    require(not Path(reference["file"]).is_absolute() and path.resolve().is_relative_to(root.resolve()),
            "input outside owned root")
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and 0 < info.st_size <= maximum,
            "invalid bounded regular input")
    require(re.fullmatch(r"[0-9a-f]{64}", reference["sha256"]), "invalid input digest")
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    require(digest == reference["sha256"], "input changed")
    return path


def parse_native(text, code):
    require(code == 0 and len(text.encode()) <= MAX_OUTPUT, "native exit/output failure")
    require(len(re.findall(r"^test result: ok\. 1 passed; 0 failed; 0 ignored;.*$", text, re.MULTILINE)) == 1,
            "native filter did not execute exactly one passing test")
    diagnostics = [line for line in text.splitlines() if line.startswith("READ_ONLY_FUNDING_DIAGNOSTIC")]
    require(len(diagnostics) == 3 and all(DIAGNOSTIC.fullmatch(line) for line in diagnostics),
            "native zero-write/selection diagnostic failed")
    lines = [line for line in text.splitlines() if line.startswith("READ_ONLY_HEALTH_PASS")]
    passes = [PASS.fullmatch(line) for line in lines]
    require(len(passes) == 3 and all(passes) and [int(row[1]) for row in passes] == [1, 2, 3],
            "native pass order/count/zero-write mismatch")
    return [{"pass": int(row[1]), "elapsed_ms": int(row[2]) / 1000,
             "process_state": "process_cold" if row[1] == "1" else "same_process_warm"} for row in passes]


def child(binary, environment, root, timeout):
    with tempfile.TemporaryFile(dir=root) as output:
        start = time.monotonic()
        process = subprocess.Popen([str(binary), TEST, "--exact", "--ignored", "--nocapture", "--test-threads=1"],
                                   cwd=root, env=environment, stdout=output, stderr=subprocess.STDOUT,
                                   start_new_session=True)
        try:
            while True:
                pid, status, usage = os.wait4(process.pid, os.WNOHANG)
                if pid:
                    process.returncode = os.waitstatus_to_exitcode(status)
                    break
                require(time.monotonic() - start < timeout, "native wall deadline")
                require(os.fstat(output.fileno()).st_size <= MAX_OUTPUT, "native output bound")
                time.sleep(.02)
        finally:
            if process.returncode is None:
                os.killpg(process.pid, signal.SIGKILL)
                _, status, usage = os.wait4(process.pid, 0)
                process.returncode = os.waitstatus_to_exitcode(status)
        elapsed = (time.monotonic() - start) * 1000
        output.seek(0)
        text = output.read(MAX_OUTPUT + 1).decode("utf8", errors="replace")
        passes = parse_native(text, process.returncode)
        return {"wall_ms": elapsed, "cpu_ms": (usage.ru_utime + usage.ru_stime) * 1000,
                "peak_rss_kib": usage.ru_maxrss, "input_blocks": usage.ru_inblock,
                "output_blocks": usage.ru_oublock, "passes": passes}


def compare(config):
    require(config["runtime_target_class"] in ("LAB", "SINGLE_NODE"), "not a runtime target")
    require(socket.gethostname() == config["expected_hostname"], "wrong runtime host")
    root = Path(config["root"])
    require(root.is_absolute() and root.resolve().is_relative_to("/work/tmp/project-sentinel")
            and not root.is_symlink(), "invalid owned input root")
    require(1 <= config.get("abba_blocks", 1) <= 3, "bounded ABBA block count required")
    manifest_path = Path("/opt/sentinel/release-manifest.json")
    manifest = manifest_path.read_bytes()
    require(hashlib.sha256(manifest).hexdigest() == config["manifest_sha256"], "runtime pin mismatch")
    services = service_identity()
    binaries = {label: checked_file(root, config[label], 512 * 1024 * 1024) for label in ("baseline", "candidate")}
    image = checked_file(root, config["image"], 2 * 1024 ** 3)
    metadata = checked_file(root, config["metadata"], 256 * 1024)
    env = {"PATH": "/usr/bin:/bin", "HOME": str(root), "TMPDIR": str(root),
           "SENTINEL_DIAGNOSTIC_READ_ONLY_DB": str(image), "SENTINEL_DIAGNOSTIC_FUNDING_METADATA": str(metadata),
           "SENTINEL_DIAGNOSTIC_MODE": "adaptive_health_inventory", "SENTINEL_DIAGNOSTIC_REPEAT": "3",
           "SENTINEL_WORKFLOW_READ_CACHE_DIAGNOSTICS": "1"}
    sidecars, errors, trials = [], [], []
    stop = threading.Event()
    def monitor():
        try:
            while not stop.is_set():
                sidecars.append(counters(services))
                stop.wait(1)
        except Exception:
            errors.append("sidecar_failed")
    worker = threading.Thread(target=monitor, daemon=True)
    worker.start()
    try:
        for label in ("baseline", "candidate", "candidate", "baseline") * config.get("abba_blocks", 1):
            checked_file(root, config[label], 512 * 1024 * 1024)
            checked_file(root, config["image"], 2 * 1024 ** 3)
            checked_file(root, config["metadata"], 256 * 1024)
            trial = child(binaries[label], env, root, 120)
            checked_file(root, config["image"], 2 * 1024 ** 3)
            require(manifest_path.read_bytes() == manifest and service_identity() == services,
                    "runtime changed during measurement")
            trials.append({"candidate": label, **trial})
    finally:
        stop.set()
        worker.join(timeout=5)
    require(not errors and len(sidecars) >= 2, "resource sidecar incomplete")
    metrics = {}
    for label in ("baseline", "candidate"):
        selected = [t for t in trials if t["candidate"] == label]
        metrics[label] = {"process_cold": distribution([t["passes"][0]["elapsed_ms"] for t in selected]),
                          "same_process_warm": distribution([p["elapsed_ms"] for t in selected for p in t["passes"][1:]])}
    return {"schema": 1, "status": "READ_ONLY_NATIVE_MEASUREMENTS", "kind": "native_diagnostic_comparison",
            "runtime_target_class": config["runtime_target_class"], "manifest_sha256": config["manifest_sha256"],
            "inputs": {k: config[k]["sha256"] for k in ("baseline", "candidate", "image", "metadata")},
            "order": [t["candidate"] for t in trials], "metrics": metrics, "trials": trials, "sidecars": sidecars,
            "native_sql_changes": 0, "provider_dispatches": 0,
            "cache_state": "process_cold_not_os_cold_input_hash_reads_warm_pages",
            "balloon_assignment": "REQUIRES_MATCHED_HYPERVISOR_SIDECAR",
            "scope": "One frozen query-only image and exact native test. No release p99, whole-product gain, live persistence or actual acceptance claim."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("config", type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        print(json.dumps(compare(json.loads(args.config.read_text())), allow_nan=False))
        return 0
    except Exception:
        print(json.dumps({"status": "BLOCKED", "reason": "native target, input, test, or sidecar qualification failed"}))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
