#!/usr/bin/env python3
"""Conservative CI routing and failure-closed hosted job aggregation."""

import argparse
import json
import os
from pathlib import PurePosixPath
import subprocess
import sys

FLAGS = ("rust", "go", "console", "schemas", "config", "bans")
ALWAYS = {"changes", "lint"}
JOBS = {"bans", "rust-static", "rust-doc", "rust-test", "rust-ebpf", "go", "console", "schemas"}


def classify(paths):
    result = dict.fromkeys(FLAGS, False)
    if not paths:
        raise ValueError("empty change inventory")
    for path in paths:
        parts = PurePosixPath(path).parts
        if not parts or path.startswith("/") or ".." in parts:
            raise ValueError("invalid changed path")
        # Only known prose-only inputs may bypass runtime gates. Unknown inputs,
        # fixtures, deployment scripts and workflow changes invalidate all gates.
        if path.endswith(".md") and (path.startswith("docs/") or len(parts) == 1):
            continue
        if path.startswith(("crates/", "services/")):
            result["rust"] = True
        elif path.startswith(("cmd/", "pkg/")) or path in {"go.work", "go.work.sum"}:
            result["go"] = True
        elif path.startswith("console/"):
            result["console"] = True
        elif path.startswith("schemas/"):
            result.update(rust=True, go=True, schemas=True)
        elif path.startswith("config/"):
            result["config"] = True
        elif PurePosixPath(path).name in {"Cargo.toml", "Cargo.lock"}:
            result.update(rust=True, bans=True)
        else:
            result = dict.fromkeys(FLAGS, True)
        if path.startswith(("services/sentinel-judge/", "services/sentinel-nats-bridge/")):
            result["go"] = True
        if PurePosixPath(path).name in {"Cargo.toml", "Cargo.lock"}:
            result["bans"] = True
    return {key: str(value).lower() for key, value in result.items()}


def evaluate(needs):
    if set(needs) != ALWAYS | JOBS:
        raise ValueError("unexpected or missing CI jobs")
    for name in ALWAYS:
        if needs[name]["result"] != "success":
            raise ValueError(f"mandatory job did not succeed: {name}")
    outputs = needs["changes"]["outputs"]
    if set(outputs) != set(FLAGS) or any(v not in {"true", "false"} for v in outputs.values()):
        raise ValueError("classifier outputs missing or invalid")
    required = set(ALWAYS)
    if outputs["rust"] == "true" or outputs["config"] == "true":
        required.update({"rust-static", "rust-doc", "rust-test", "rust-ebpf"})
    required.update(key for key in {"bans", "go", "console", "schemas"} if outputs[key] == "true")
    for name in JOBS:
        allowed = {"success"} if name in required else {"success", "skipped"}
        if needs[name]["result"] not in allowed:
            raise ValueError(f"unexpected job result: {name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("classify", "evaluate"))
    parser.add_argument("--base")
    parser.add_argument("--head", default="HEAD")
    args = parser.parse_args()
    try:
        if args.command == "evaluate":
            evaluate(json.loads(os.environ["CI_NEEDS"]))
            print("All required CI jobs succeeded.")
        else:
            if not args.base or set(args.base) == {"0"}:
                raise ValueError("missing comparison base")
            raw = subprocess.check_output(["git", "diff", "--name-only", "--no-renames", "-z", args.base, args.head, "--"])
            paths = [path.decode("utf-8", errors="strict") for path in raw.split(b"\0") if path]
            outputs = classify(paths)
            with open(os.environ["GITHUB_OUTPUT"], "a", encoding="ascii") as stream:
                for key, value in outputs.items():
                    stream.write(f"{key}={value}\n")
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError) as exc:
        print(f"CI routing failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
