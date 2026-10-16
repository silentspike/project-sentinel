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
ARCHIVE_OWNERS = {"lab-normal", "lab-ignored", "builder-compiler", "protected-diagnostic", "live-session", "manual-benchmark"}
LAB_OWNERS = {"lab-normal", "lab-ignored"}
NEXTEST_VERSION = "0.9.145"


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


def compact_archive(summary, summary_digest):
    # The controller runs the archive verifier on full exact-ID/JUnit evidence.
    # Transport only its compact, identity-bound assertion, never 5000+ IDs or
    # blocked diagnostic definitions in workflow_dispatch inputs.
    fields = ("schema_version", "aggregate_pass", "identity", "cargo_profile", "nextest_version", "plan_digest",
              "manifest_sha256", "archive_sha256", "source_bundle_sha256", "source_inventory_sha256", "test_inventory_digest",
              "test_count", "archive_shards_verified", "observed_count", "expected_owners", "observed_owners",
              "owner_inventory", "pending_non_sharded")
    return {**{key: summary[key] for key in fields}, "summary_sha256": summary_digest,
            "not_applicable_owners": summary.get("not_applicable_owners", {})}


def validate_archive(receipt, expected_plan_digest):
    plan = receipt["qualification_plan"]
    require(digest(expected_plan_digest), "independently frozen qualification plan missing")
    require(hashlib.sha256(canonical(plan)).hexdigest() == expected_plan_digest, "qualification plan changed")
    require(set(plan) == {"schema_version", "repository", "pr", "head_sha", "tree_sha", "base_sha", "input_digest",
                          "test_inventory_digest", "archive_plan_digest", "owners"}, "qualification plan shape")
    require(type(plan["schema_version"]) is int and plan["schema_version"] == 1, "qualification plan version")
    for name in ("repository", "pr", "head_sha", "tree_sha", "base_sha", "test_inventory_digest"):
        require(plan[name] == receipt[name], "qualification plan identity mismatch")
    require(plan["input_digest"] == receipt["inputs"]["input_digest"], "qualification plan inputs mismatch")
    archive = receipt["archive"]
    require(type(archive["schema_version"]) is int and archive["schema_version"] == 1 and archive["aggregate_pass"] is False, "archive summary is mechanism-only")
    require(archive["identity"] == {k: receipt[k] for k in ("repository", "pr", "head_sha", "tree_sha", "base_sha", "inputs")}, "archive identity mismatch")
    require(archive["cargo_profile"] == "ci" and archive["nextest_version"] == NEXTEST_VERSION, "archive tool/profile mismatch")
    for name in ("plan_digest", "manifest_sha256", "archive_sha256", "source_bundle_sha256", "source_inventory_sha256", "test_inventory_digest", "summary_sha256"):
        require(digest(archive[name]), "archive digest missing")
    require(archive["plan_digest"] == plan["archive_plan_digest"], "archive plan mismatch")
    require(archive["test_inventory_digest"] == receipt["test_inventory_digest"], "archive inventory mismatch")
    owners = plan["owners"]
    require(isinstance(owners, list) and len(owners) == len(ARCHIVE_OWNERS), "incomplete applicability owner inventory")
    require({o["owner"] for o in owners} == ARCHIVE_OWNERS, "unknown or duplicate applicability owner")
    require(set(archive["owner_inventory"]) == ARCHIVE_OWNERS, "archive owner inventory missing")
    for field in ("expected_owners", "observed_owners"):
        require(set(archive[field]) <= ARCHIVE_OWNERS, "unknown archive owner")
        require(all(type(n) is int and n >= 0 for n in archive[field].values()), "invalid archive owner counts")
    require(type(archive["test_count"]) is int and archive["test_count"] > 0 and
            archive["test_count"] == sum(archive["expected_owners"].values()), "incomplete or empty archive owner inventory")
    require(set(archive["pending_non_sharded"]) <= ARCHIVE_OWNERS - LAB_OWNERS, "unexpected pending owner")
    require(all(type(n) is int and n >= 0 for n in archive["pending_non_sharded"].values()), "invalid pending owner count")
    exclusions = archive["not_applicable_owners"]
    require(isinstance(exclusions, dict) and set(exclusions) <= {"lab-ignored"}, "unsupported archive owner exclusion")
    require(all(inventory == archive["owner_inventory"][name] for name, inventory in exclusions.items()), "excluded owner inventory changed")
    shards = receipt["shards"]
    require(isinstance(shards, list) and shards, "missing archive shards")
    require(all(s["owner"] in LAB_OWNERS for s in shards), "compiler or protected tests cannot be lab shards")
    for owner in owners:
        name = owner["owner"]
        require(set(owner) == {"owner", "required", "reason", "expected_count", "inventory_id_digest"}, "applicability owner shape")
        require(type(owner["required"]) is bool and isinstance(owner["reason"], str) and owner["reason"].strip(), "missing applicability rationale")
        require(type(owner["expected_count"]) is int and owner["expected_count"] >= 0 and digest(owner["inventory_id_digest"]), "invalid applicability inventory")
        inventory = archive["owner_inventory"][name]
        require(set(inventory) == {"test_count", "test_inventory_digest"} and type(inventory["test_count"]) is int, "invalid owner inventory")
        require(inventory["test_count"] == owner["expected_count"] == archive["expected_owners"].get(name, 0) and
                inventory["test_inventory_digest"] == owner["inventory_id_digest"], "applicability inventory mismatch")
        observed = archive["observed_owners"].get(name, 0)
        if name not in LAB_OWNERS:
            require(not any(s["owner"] == name for s in shards), "non-lab owner in shard receipt")
            require(archive["pending_non_sharded"].get(name, 0) == owner["expected_count"], "non-sharded inventory mismatch")
            require(observed == 0, "archive cannot claim non-sharded execution")
            if owner["required"] and owner["expected_count"]:
                gate_name = "rust-compiler-tests" if name == "builder-compiler" else f"{name}-tests"
                gate = next((g for g in receipt["gates"] if g["name"] == gate_name), None)
                require(gate is not None and gate["counts"]["passed"] == owner["expected_count"], "required non-sharded evidence missing")
            # No claim that an unexecuted diagnostic/manual group passed.
            continue
        owned = [s for s in shards if s["owner"] == name]
        if not owner["required"]:
            require(not owned and observed == 0, "not-applicable tests reported as executed")
            if owner["expected_count"]:
                require(name in exclusions, "archive owner exclusion not explicit")
            continue
        require(name not in exclusions, "required owner excluded by archive verifier")
        total = len(owned)
        require(observed == owner["expected_count"], "owner coverage incomplete")
        require(sorted(s["partition"] for s in owned) == list(range(total)), "missing or duplicate owner shard")
        require(sum(s["expected_count"] for s in owned) == owner["expected_count"], "owner shard counts mismatch")
        for shard in owned:
            require(set(shard) == {"id", "receipt_sha256", "owner", "partition", "total", "expected_count", "observed_count", "missing", "duplicates", "extra",
                                   "exit_code", "command", "evidence_sha", "junit_sha256", "nextest_version", "plan_digest", "test_inventory_digest"}, "shard shape")
            require(type(shard["partition"]) is int and type(shard["total"]) is int and shard["total"] == total, "shard partition mismatch")
            require(type(shard["expected_count"]) is int and shard["expected_count"] > 0, "empty shard")
            require(type(shard["observed_count"]) is int and shard["expected_count"] == shard["observed_count"], "shard count mismatch")
            require(shard["missing"] == [] and shard["duplicates"] == [] and shard["extra"] == [], "incomplete test coverage")
            require(type(shard["exit_code"]) is int and shard["exit_code"] == 0, "shard failed")
            require(shard["nextest_version"] == NEXTEST_VERSION and shard["plan_digest"] == archive["plan_digest"] and
                    shard["test_inventory_digest"] == receipt["test_inventory_digest"], "stale shard identity")
            require(shard["id"] == f'{name}-{shard["partition"]}', "shard ID mismatch")
            require(digest(shard["evidence_sha"]) and digest(shard["junit_sha256"]) and digest(shard["receipt_sha256"]), "unbound shard evidence")
            require(isinstance(shard["command"], list) and shard["command"] and all(isinstance(p, str) and p for p in shard["command"]), "missing shard command")
    require(type(archive["archive_shards_verified"]) is int and archive["archive_shards_verified"] == len(shards), "unverified archive shards")
    require(type(archive["observed_count"]) is int and archive["observed_count"] > 0 and
            archive["observed_count"] == sum(s["observed_count"] for s in shards), "archive observed total mismatch")
    require(archive["observed_count"] == sum(archive["observed_owners"].values()), "archive observed owner mismatch")


def validate(receipt, *, required_gates, expected_head, expected_base, expected_plan_digest):
    require(set(receipt) == {"schema_version", "repository", "pr", "head_sha", "tree_sha", "base_sha",
                             "inputs", "gates", "test_inventory_digest", "shards", "artifacts", "archive", "qualification_plan"}, "receipt shape")
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
    validate_archive(receipt, expected_plan_digest)
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
    parser.add_argument("command", choices=("assemble", "verify", "promote"))
    parser.add_argument("--receipt", required=True, type=Path)
    parser.add_argument("--head", required=True)
    parser.add_argument("--base", required=True)
    parser.add_argument("--required-gate", action="append", required=True)
    binding = parser.add_mutually_exclusive_group(required=True)
    binding.add_argument("--plan", type=Path, help="Separately frozen canonical applicability plan")
    binding.add_argument("--plan-sha256", help="Independently enrolled applicability plan digest")
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--artifact-root", type=Path)
    parser.add_argument("--merge-sha")
    parser.add_argument("--archive-summary", type=Path)
    args = parser.parse_args()
    try:
        expected_plan_digest = args.plan_sha256
        if args.plan is not None:
            plan_raw = canonical(load(args.plan))
            require(args.plan.read_bytes() == plan_raw, "frozen plan must be canonical JSON")
            expected_plan_digest = hashlib.sha256(plan_raw).hexdigest()
        receipt = load(args.receipt)
        if args.command == "assemble":
            require(args.plan is not None and args.archive_summary is not None, "assembly requires separate plan and archive verifier summary")
            summary = load(args.archive_summary)
            receipt["qualification_plan"] = load(args.plan)
            receipt["archive"] = compact_archive(summary, hashlib.sha256(args.archive_summary.read_bytes()).hexdigest())
            receipt["shards"] = summary["shards"]
            receipt["test_inventory_digest"] = summary["test_inventory_digest"]
        receipt = validate(receipt, required_gates=args.required_gate, expected_head=args.head, expected_base=args.base,
                           expected_plan_digest=expected_plan_digest)
        if args.command == "assemble":
            require(len(canonical(receipt)) * 4 // 3 + 4 <= 60000, "compact receipt exceeds reporter transport limit")
            result = receipt
        elif args.command == "promote":
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
