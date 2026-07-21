#!/usr/bin/env python3
"""Measure Issue #75 full-cage costs on the deployment VM.

This harness intentionally measures runtime behavior rather than build-host
performance. It exercises the same namespace and bind shape as
`BwrapConfig::for_agent` and the two `/proc/.../ns/net` reads used by the
post-spawn isolation verifier.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import statistics
import subprocess
import tempfile
import time
from typing import Callable


def percentile(sorted_values: list[int], percentile_value: int) -> int:
    index = max(0, (len(sorted_values) * percentile_value + 99) // 100 - 1)
    return sorted_values[min(index, len(sorted_values) - 1)]


def summarize_ns(samples: list[int]) -> dict[str, float]:
    ordered = sorted(samples)
    return {
        "samples": len(ordered),
        "p50_us": percentile(ordered, 50) / 1_000,
        "p95_us": percentile(ordered, 95) / 1_000,
        "max_us": ordered[-1] / 1_000,
        "mean_us": statistics.fmean(ordered) / 1_000,
    }


def measure(samples: int, operation: Callable[[], None]) -> list[int]:
    durations = []
    for _ in range(samples):
        started = time.perf_counter_ns()
        operation()
        durations.append(time.perf_counter_ns() - started)
    return durations


def full_cage_command(home: pathlib.Path) -> list[str]:
    return [
        "bwrap",
        "--unshare-all",
        "--die-with-parent",
        "--hostname",
        "sentinel-bench-spawn",
        "--ro-bind",
        "/usr",
        "/usr",
        "--ro-bind",
        "/lib",
        "/lib",
        "--ro-bind",
        "/lib64",
        "/lib64",
        "--ro-bind",
        "/etc/resolv.conf",
        "/etc/resolv.conf",
        "--ro-bind",
        "/work/company",
        "/company",
        "--bind",
        str(home),
        "/home/bench-spawn",
        "--tmpfs",
        "/tmp",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "true",
    ]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--spawn-samples", type=int, default=1_000)
    parser.add_argument("--verify-samples", type=int, default=10_000)
    parser.add_argument("--agent-pid", type=int, required=True)
    args = parser.parse_args()

    if args.spawn_samples < 1 or args.verify_samples < 1:
        raise SystemExit("sample counts must be positive")

    agent_netns_path = pathlib.Path(f"/proc/{args.agent_pid}/ns/net")
    try:
        own_netns = os.readlink("/proc/self/ns/net")
        agent_netns = os.readlink(agent_netns_path)
    except FileNotFoundError as error:
        raise SystemExit(f"agent PID {args.agent_pid} is not running") from error
    except PermissionError as error:
        raise SystemExit("run with enough privilege to inspect the agent namespace") from error
    if own_netns == agent_netns:
        raise SystemExit("agent PID shares the benchmark process network namespace")

    def verify_isolation() -> None:
        if os.readlink("/proc/self/ns/net") == os.readlink(agent_netns_path):
            raise RuntimeError("agent network namespace became unisolated")

    with tempfile.TemporaryDirectory(prefix="sentinel-issue75-") as home:
        command = full_cage_command(pathlib.Path(home))

        def spawn_full_cage() -> None:
            subprocess.run(
                command,
                check=True,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )

        spawn_full_cage()
        spawn_durations = measure(args.spawn_samples, spawn_full_cage)

    verify_isolation()
    verify_durations = measure(args.verify_samples, verify_isolation)

    result = {
        "benchmark": "sentinel-sandbox-netns (#75)",
        "host": os.uname().nodename,
        "bwrap_version": subprocess.run(
            ["bwrap", "--version"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip(),
        "agent_pid": args.agent_pid,
        "daemon_netns": own_netns,
        "agent_netns": agent_netns,
        "full_cage_spawn": summarize_ns(spawn_durations),
        "netns_verify": summarize_ns(verify_durations),
    }
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
