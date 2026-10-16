#!/usr/bin/env python3
"""Validate an audited candidate receipt and promote unchanged artifact bytes.

Receipts are assertions by the trusted controller, not proof from a test VM.
The reporter authenticates these assertions separately. No receipt creates QA
authority, production acceptance, or permission to replace required hosted CI.
"""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys

REPOSITORY = "silentspike/project-sentinel"


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def digest(value, length=64):
    return isinstance(value, str) and re.fullmatch(f"[0-9a-f]{{{length}}}", value) is not None


def relative(value):
    return (isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_.\-/]+", value)
            and not value.startswith("/") and all(p not in {"", ".", ".."} for p in value.split("/")))


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n").encode("ascii")


def load(path):
    def pairs(values):
        result = {}
        for key, value in values:
            require(key not in result, "duplicate JSON key")
            result[key] = value
        return result
    require(path.stat().st_size <= 1024 * 1024, "receipt too large")
    return json.loads(path.read_bytes(), object_pairs_hook=pairs)


def validate(receipt, *, required_gates, expected_head, expected_base):
    require(set(receipt) == {"schema_version", "repository", "pr", "head_sha", "tree_sha", "base_sha",
                             "inputs", "gates", "test_inventory_digest", "shards", "artifacts"}, "receipt shape")
    require(type(receipt["schema_version"]) is int and receipt["schema_version"] == 1, "receipt schema")
    require(receipt["repository"] == REPOSITORY, "foreign repository")
    require(type(receipt["pr"]) is int and receipt["pr"] > 0, "invalid PR")
    require(all(digest(receipt[k], 40) for k in ("head_sha", "tree_sha", "base_sha")), "invalid commit identity")
    require(receipt["head_sha"] == expected_head and receipt["base_sha"] == expected_base, "stale candidate")
    inputs = receipt["inputs"]
    require(set(inputs) == {"toolchain", "profile", "image", "input_digest"}, "build inputs shape")
    require(inputs["toolchain"] == "1.99.0" and inputs["profile"] == "linux-gnu-sentinel", "unsupported build environment")
    # Cargo Remote records the audited local Docker content identity, not
    # necessarily a pullable registry reference. Preserve its exact form.
    require(digest(inputs["image"]), "unpinned build image")
    require(digest(inputs["input_digest"]) and digest(receipt["test_inventory_digest"]), "missing input inventory")
    gates = receipt["gates"]
    require(isinstance(gates, list) and gates and required_gates, "missing qualification gates")
    require(len({g["name"] for g in gates}) == len(gates), "duplicate qualification gate")
    require({g["name"] for g in gates} == set(required_gates), "qualification gate inventory mismatch")
    for gate in gates:
        require(set(gate) == {"name", "source_digest", "command", "exit_code", "counts", "evidence_sha"}, "gate shape")
        require(type(gate["exit_code"]) is int and gate["exit_code"] == 0, "qualification gate failed")
        require(gate["source_digest"] == inputs["input_digest"] and digest(gate["evidence_sha"]), "stale gate evidence")
        require(isinstance(gate["command"], list) and gate["command"] and all(isinstance(p, str) and p for p in gate["command"]), "missing command")
        counts = gate["counts"]
        require(set(counts) == {"passed", "failed"} and all(type(v) is int and v >= 0 for v in counts.values()), "invalid test counts")
        require(counts["failed"] == 0, "failed tests")
        if "test" in gate["name"] or "shard" in gate["name"]:
            require(counts["passed"] > 0, "zero tests are not acceptance")
    shards = receipt["shards"]
    require(isinstance(shards, list) and shards, "missing shards")
    total = len(shards)
    require(all(type(s["partition"]) is int for s in shards), "invalid shard partition")
    require(sorted(s["partition"] for s in shards) == list(range(total)), "missing or duplicate shard")
    for shard in shards:
        require(set(shard) == {"partition", "total", "expected_count", "observed_count", "missing", "duplicates"}, "shard shape")
        require(type(shard["total"]) is int and shard["total"] == total, "shard total mismatch")
        require(type(shard["expected_count"]) is int and shard["expected_count"] > 0, "empty shard")
        require(type(shard["observed_count"]) is int and shard["expected_count"] == shard["observed_count"], "shard count mismatch")
        require(shard["missing"] == [] and shard["duplicates"] == [], "incomplete test coverage")
    artifacts = receipt["artifacts"]
    require(isinstance(artifacts, list) and artifacts, "missing artifacts")
    require(len({a["path"] for a in artifacts}) == len(artifacts), "duplicate artifact")
    for artifact in artifacts:
        require(set(artifact) == {"path", "size", "sha256", "build_origin"}, "artifact shape")
        require(relative(artifact["path"]) and digest(artifact["sha256"]), "invalid artifact")
        require(type(artifact["size"]) is int and artifact["size"] > 0, "empty artifact")
        require(artifact["build_origin"] == receipt["head_sha"], "artifact source relabeling forbidden")
    return receipt


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def promote(receipt, *, root, artifact_root, merge_sha):
    require(digest(merge_sha, 40), "invalid merge commit")
    require(git(root, "rev-parse", f'{receipt["head_sha"]}^{{tree}}') == receipt["tree_sha"], "source tree mismatch")
    require(git(root, "rev-parse", f"{merge_sha}^{{tree}}") == receipt["tree_sha"], "merge tree changed; requalification required")
    # A matching arbitrary historical tree is not evidence of this promotion.
    require(subprocess.run(["git", "-C", str(root), "merge-base", "--is-ancestor", receipt["base_sha"], merge_sha]).returncode == 0, "merge is not based on qualified base")
    require(subprocess.run(["git", "-C", str(root), "merge-base", "--is-ancestor", receipt["head_sha"], merge_sha]).returncode == 0, "qualified head is not an ancestor; requalification required")
    for artifact in receipt["artifacts"]:
        path = artifact_root / artifact["path"]
        current = artifact_root
        for part in PurePosixPath(artifact["path"]).parts:
            current = current / part
            require(not current.is_symlink(), "symlink artifact forbidden")
        require(path.is_file() and path.stat().st_size == artifact["size"], "artifact size changed")
        hasher = hashlib.sha256()
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                hasher.update(chunk)
        require(hasher.hexdigest() == artifact["sha256"], "artifact bytes changed")
    return {"schema_version": 1, "status": "PROMOTION_VERIFIED", "source_sha": receipt["head_sha"],
            "merge_sha": merge_sha, "tree_sha": receipt["tree_sha"], "receipt_sha256": hashlib.sha256(canonical(receipt)).hexdigest(),
            "artifacts": receipt["artifacts"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("verify", "promote"))
    parser.add_argument("--receipt", required=True, type=Path)
    parser.add_argument("--head", required=True)
    parser.add_argument("--base", required=True)
    parser.add_argument("--required-gate", action="append", required=True)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--artifact-root", type=Path)
    parser.add_argument("--merge-sha")
    args = parser.parse_args()
    try:
        receipt = validate(load(args.receipt), required_gates=args.required_gate, expected_head=args.head, expected_base=args.base)
        if args.command == "promote":
            require(args.artifact_root is not None and args.merge_sha is not None, "promotion inputs missing")
            result = promote(receipt, root=args.repo_root, artifact_root=args.artifact_root, merge_sha=args.merge_sha)
        else:
            result = {"status": "RECEIPT_VERIFIED", "head_sha": receipt["head_sha"]}
        sys.stdout.buffer.write(canonical(result))
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError) as exc:
        print(f"Candidate verification failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
