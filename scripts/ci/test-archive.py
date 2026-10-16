#!/usr/bin/env python3
"""Seal builder outputs and run counted nextest partitions without a compiler.

This is mechanism evidence, not product acceptance. The controller owns target
authorization, mounts, credentials, scheduling and aggregate qualification.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tarfile
import tomllib
import urllib.request
import xml.etree.ElementTree as ET


ROOT = Path(__file__).resolve().parents[2]
PIN = json.loads(Path(__file__).with_name("test-archive-tool.json").read_text())
SCHEMA = 1
COMPILER = "sentinel-daemon::runtime_lifecycle_visibility"


class ArchiveError(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise ArchiveError(message)


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def object_digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def load(path):
    return json.loads(Path(path).read_text())


def write(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(path.name + ".new")
    with temp.open("x") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")
    temp.replace(path)


def relative(value):
    path = PurePosixPath(value)
    require(not path.is_absolute() and path.parts and ".." not in path.parts,
            "unsafe relative path: " + str(value))
    return path


def test_rows(listing):
    rows = []
    for binary_id, suite in sorted(listing["rust-suites"].items()):
        require(suite.get("status", "listed") == "listed", "unlisted suite: " + binary_id)
        for name, case in sorted(suite["testcases"].items()):
            require(isinstance(case["ignored"], bool), "invalid ignored flag")
            rows.append({"binary_id": binary_id, "name": name, "ignored": case["ignored"]})
    require(len(rows) == listing["test-count"], "nextest inventory count mismatch")
    return rows


def key(row):
    return row["binary_id"], row["name"]


def selected(listing):
    result = []
    for binary_id, suite in listing["rust-suites"].items():
        for name, case in suite.get("testcases", {}).items():
            if case["filter-match"]["status"] == "matches":
                result.append((binary_id, name))
    return sorted(result)


def source_inventory(root):
    """Source hints cover conditional/excluded code, not compiled test counts."""
    manifests, hints, ignored = [], [], {}
    for parent in ("crates", "services"):
        for path in sorted((root / parent).rglob("Cargo.toml")):
            if "target" in path.parts:
                continue
            value = tomllib.loads(path.read_text())
            manifests.append({"path": path.relative_to(root).as_posix(),
                              "package": value.get("package", {}).get("name"),
                              "features": value.get("features", {}),
                              "targets": {k: value[k] for k in ("test", "bin", "bench", "lib") if k in value}})
        for path in sorted((root / parent).rglob("*.rs")):
            if "target" in path.parts:
                continue
            text = path.read_text()
            rel = path.relative_to(root).as_posix()
            markers = {name: len(re.findall(pattern, text, re.M)) for name, pattern in {
                "test_attributes": r"^\s*#\[(?:test|tokio::test|test_case::test_case)\b",
                "ignored_attributes": r"^\s*#\[ignore(?:\s|\])",
                "doc_code_fences": r"^\s*///\s*```",
                "compiler_cases": r"trybuild::|\.compile_fail\(",
                "snapshots": r"assert_(?:yaml_|json_|debug_)?snapshot!",
                "embedded_paths": r'CARGO_MANIFEST_DIR|CARGO_BIN_EXE_|include_(?:bytes|str)!',
            }.items()}
            if any(markers.values()):
                hints.append({"path": rel, **markers})
            for match in re.finditer(r'^\s*#\[ignore[^\n]*\n(?:(?!\s*(?:async\s+)?fn\b)[^\n]*\n)*?\s*(?:async\s+)?fn\s+(\w+)', text, re.M):
                name = match.group(1)
                if "sentinel-workflow/" in rel:
                    owner = "protected-diagnostic"
                elif "live_wt_topics" in rel:
                    owner = "live-session"
                elif "sentinel-runtime/src/lib.rs" in rel:
                    owner = "manual-benchmark"
                else:
                    owner = "lab-ignored"
                ignored.setdefault(name, []).append({"path": rel, "owner": owner})
    workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
    return {"schema_version": SCHEMA, "count_kind": "source hints only; nextest list is authoritative",
            "members": workspace["members"], "excluded": workspace["exclude"],
            "manifests": manifests, "source_hints": hints, "ignored_definitions": ignored,
            "external_gates": {
                "doctests": {"owner": "builder", "command": ["test", "--workspace", "--profile", "ci", "--doc", "--locked"]},
                "compiler": {"owner": "builder", "binary_id": COMPILER},
                "snapshots": {"owner": "lab-normal", "mode": "INSTA_UPDATE=no; compiled assertions in archive"},
                "wasm": {"owner": "lab-normal+lab-ignored", "fixtures": "crates/sentinel-wasm/tests/fixtures/*.wasm", "rebuild_owner": "builder-wasm-profile"},
                "ebpf": {"owner": "builder+lab", "compile": ["test", "-p", "sentinel-ebpf", "--features", "ebpf", "--no-run", "--locked"], "kernel": "lab must load and verify probes; compilation is not kernel acceptance"},
                "ebpf-probes": {"owner": "builder-specialized", "status": "excluded workspace; separately audited nightly bpfel target required"},
                "inference": {"owner": "builder-specialized", "status": "excluded optional workspace; explicit GPU/model gate required if affected"},
                "privileged": {"owner": "lab-ignored", "requires": "authorized kernel, cgroups, FUSE, Landlock, KVM and immutable wrappers"},
            }}


def classify(row, inventory):
    if row["binary_id"] == COMPILER:
        return "builder-compiler"
    if not row["ignored"]:
        return "lab-normal"
    definitions = inventory["ignored_definitions"].get(row["name"].rsplit("::", 1)[-1], [])
    owners = {entry["owner"] for entry in definitions}
    require(len(owners) == 1, "unassigned/ambiguous ignored test: " + str(key(row)))
    return owners.pop()


def seal(args):
    snapshot, result, identity = load(args.snapshot), load(args.result), load(args.identity)
    snapshot = snapshot.get("snapshot", snapshot)
    entries = snapshot["sources"]
    require(object_digest(entries) == snapshot["source_sha256"], "invalid source manifest digest")
    require(result["exit_code"] == 0 and result["source_sha256"] == snapshot["source_sha256"], "builder result input mismatch")
    require(result["commit"] == snapshot["commit"] == identity["head_sha"], "builder head mismatch")
    for field in ("head_sha", "tree_sha", "base_sha"):
        require(re.fullmatch(r"[0-9a-f]{40}", identity[field]), "invalid commit/tree identity")
    require(identity["inputs"]["input_digest"] == snapshot["source_sha256"], "identity input mismatch")
    require(identity["inputs"]["toolchain"] == "1.99.0" and identity["inputs"]["profile"] == "linux-gnu-sentinel", "wrong builder profile")
    require(identity["inputs"]["image"] == result["image"]["image"].removeprefix("sha256:"), "builder image mismatch")
    archive = Path(args.archive).resolve()
    archive_entry = next((e for e in result["files"] if e["path"] == "target/" + archive.name), None)
    require(archive_entry and digest(archive) == archive_entry["sha256"], "archive missing from verified export")
    require(result["args"][:2] == ["nextest", "archive"] and "--cargo-profile" in result["args"] and result["args"][result["args"].index("--cargo-profile") + 1] == "ci", "not a CI-profile nextest archive")
    out = Path(args.output).resolve()
    out.mkdir(parents=True, exist_ok=False)
    base = Path(args.source_root).resolve()
    require(subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=base, text=True).strip() == identity["head_sha"], "checkout head mismatch")
    require(not subprocess.check_output(["git", "status", "--porcelain"], cwd=base), "seal requires a clean frozen checkout")
    require(subprocess.check_output(["git", "rev-parse", "HEAD^{tree}"], cwd=base, text=True).strip() == identity["tree_sha"], "tree identity mismatch")
    # The builder's filtered manifest is the transfer allowlist, never git or a
    # recursive checkout copy. Credential-bearing private files are not inputs.
    with tarfile.open(out / "sources.tar.gz", "w:gz") as tar:
        for entry in entries:
            rel = relative(entry["path"])
            require(not ({".git", ".orc", ".ssh", ".claude"} & set(rel.parts)), "private source path")
            require(not any(p.startswith(".env") for p in rel.parts), "environment source path")
            path = base / str(rel)
            require(not path.is_symlink() and path.resolve().is_relative_to(base) and digest(path) == entry["sha256"], "source changed/symlink: " + str(rel))
            member = tar.gettarinfo(path, arcname=str(rel))
            member.uid = member.gid = 0
            member.uname = member.gname = ""
            with path.open("rb") as content:
                tar.addfile(member, content)
    shutil.copyfile(archive, out / "tests.tar.zst")
    inventory = source_inventory(base)
    write(out / "source-inventory.json", inventory)
    manifest = {**identity, "schema_version": SCHEMA, "nextest_version": PIN["version"], "cargo_profile": "ci",
                "archive_sha256": digest(out / "tests.tar.zst"), "source_bundle_sha256": digest(out / "sources.tar.gz"),
                "source_inventory_sha256": digest(out / "source-inventory.json"), "sources": entries,
                "build_origin": result["commit"], "build_job": result["id"], "build_args": result["args"]}
    write(out / "manifest.json", manifest)
    print(json.dumps({"manifest_sha256": digest(out / "manifest.json"), "files": len(entries)}))


def check_bundle(bundle):
    manifest = load(bundle / "manifest.json")
    require(manifest["schema_version"] == SCHEMA and manifest["nextest_version"] == PIN["version"], "unsupported bundle")
    for filename, field in (("tests.tar.zst", "archive_sha256"), ("sources.tar.gz", "source_bundle_sha256"), ("source-inventory.json", "source_inventory_sha256")):
        require(digest(bundle / filename) == manifest[field], "corrupt bundle: " + filename)
    return manifest


def nextest(executable):
    output = subprocess.check_output([executable, "nextest", "--version"], text=True)
    require(re.match(r"cargo-nextest " + re.escape(PIN["version"]) + r"(?:\s|$)", output), "nextest version differs from pin")
    return [executable, "nextest"]


def extract(args):
    bundle, destination = Path(args.bundle).resolve(), Path(args.destination).resolve()
    manifest = check_bundle(bundle)
    destination.mkdir(parents=True, exist_ok=False)
    source = destination / "source"
    source.mkdir()
    with tarfile.open(bundle / "sources.tar.gz") as tar:
        expected = {entry["path"]: entry["sha256"] for entry in manifest["sources"]}
        members = tar.getmembers()
        require(len(members) == len(expected) and {m.name for m in members} == set(expected), "source bundle membership mismatch")
        for member in members:
            relative(member.name)
            require(member.isfile(), "source bundle must contain regular files only")
        tar.extractall(source, members=members, filter="data")
    for name, sha in expected.items():
        require(digest(source / name) == sha, "source bundle content mismatch")
    command = nextest(args.nextest) + ["list", "--archive-file", str(bundle / "tests.tar.zst"), "--extract-to", str(destination / "build"), "--workspace-remap", str(source), "--message-format", "json", "--run-ignored", "all", "--user-config-file", "none"]
    listing = subprocess.check_output(command, cwd=source, text=True)
    write(destination / "test-list.json", json.loads(listing))
    require(test_rows(json.loads(listing)), "empty archive")
    (source / "target").symlink_to(destination / "build/target", target_is_directory=True)
    # Historical breakout tests use target/debug even under a custom profile.
    target = destination / "build/target"
    if (target / "ci/breakout-helper").is_file() and not (target / "debug").exists():
        (target / "debug").symlink_to("ci", target_is_directory=True)
    write(destination / "extracted.json", {"manifest_sha256": digest(bundle / "manifest.json"), "command": command,
          "test_inventory_digest": object_digest(test_rows(json.loads(listing)))})


def plan(args):
    bundle, extracted = Path(args.bundle).resolve(), Path(args.extracted).resolve()
    manifest = check_bundle(bundle)
    require(load(extracted / "extracted.json")["manifest_sha256"] == digest(bundle / "manifest.json"), "extraction belongs to another bundle")
    rows = test_rows(load(extracted / "test-list.json"))
    require(object_digest(rows) == load(extracted / "extracted.json")["test_inventory_digest"], "changed extracted test inventory")
    inventory = load(bundle / "source-inventory.json")
    require(1 <= args.shards <= 64, "shard count must be 1..64")
    groups = {name: [] for name in ("lab-normal", "lab-ignored", "builder-compiler", "protected-diagnostic", "live-session", "manual-benchmark")}
    for row in rows:
        groups[classify(row, inventory)].append(row)
    shards = []
    for owner in ("lab-normal", "lab-ignored"):
        owned = groups[owner]
        total = min(args.shards, len(owned)) if owner.startswith("lab-") else min(1, len(owned))
        # Sorted round-robin gives disjoint, nonempty partitions; exact IDs are
        # independently checked against nextest's selected list before running.
        for partition in range(total):
            tests = owned[partition::total]
            shards.append({"id": f"{owner}-{partition}", "partition": partition, "total": total,
                           "owner": owner, "expected_count": len(tests), "tests": tests})
    data = {"schema_version": SCHEMA, "manifest_sha256": digest(bundle / "manifest.json"),
          "archive_sha256": manifest["archive_sha256"], "source_bundle_sha256": manifest["source_bundle_sha256"],
          "source_inventory_sha256": manifest["source_inventory_sha256"],
          "identity": {k: manifest[k] for k in ("repository", "pr", "head_sha", "tree_sha", "base_sha", "inputs")},
          "test_inventory_digest": object_digest(rows), "test_count": len(rows), "shards": shards,
          "non_sharded": {k: v for k, v in groups.items() if k not in ("lab-normal", "lab-ignored")},
          "external_gates": inventory["external_gates"]}
    validate_plan(data)
    write(args.output, data)


def validate_plan(data):
    require(data["schema_version"] == SCHEMA, "unsupported plan")
    ids, rows, owners = set(), [], {}
    for shard in data["shards"]:
        require(shard["id"] not in ids, "duplicate shard id")
        ids.add(shard["id"])
        require(shard["owner"] in ("lab-normal", "lab-ignored"), "invalid shard owner")
        require(len(shard["tests"]) == shard["expected_count"] > 0, "invalid expected count")
        require(all(r["ignored"] == (shard["owner"] == "lab-ignored") for r in shard["tests"]), "ignored ownership mismatch")
        owners.setdefault(shard["owner"], []).append(shard)
        rows.extend(shard["tests"])
    for shards in owners.values():
        require(all(s["total"] == len(shards) for s in shards), "invalid partition total")
        require(sorted(s["partition"] for s in shards) == list(range(len(shards))), "incomplete partitions")
    for name, tests in data["non_sharded"].items():
        require(name in ("builder-compiler", "protected-diagnostic", "live-session", "manual-benchmark"), "unknown non-sharded owner")
        rows.extend(tests)
    require(len(rows) == len({key(r) for r in rows}) == data["test_count"], "duplicate/missing tests in plan")
    require(object_digest(sorted(rows, key=key)) == data["test_inventory_digest"], "plan inventory digest mismatch")


def escape(value):
    return "".join("\\" + c if c in "\\)," else c for c in value)


def filterset(rows):
    return " | ".join(f"(binary_id(={escape(r['binary_id'])}) & test(={escape(r['name'])}))" for r in rows)


def batches(rows):
    batch = []
    for row in rows:
        if batch and len(filterset(batch + [row]).encode()) > 24000:
            yield batch
            batch = []
        batch.append(row)
    if batch:
        yield batch


def junit_tests(path):
    tree = ET.parse(path)
    rows = []
    for suite in tree.getroot().iter("testsuite"):
        for case in suite.findall("testcase"):
            require(case.find("skipped") is None and case.find("failure") is None and case.find("error") is None,
                    "failed/skipped JUnit testcase")
            rows.append((suite.attrib["name"], case.attrib["name"]))
    return rows


def run(args):
    plan_data = load(args.plan)
    validate_plan(plan_data)
    shard = next(s for s in plan_data["shards"] if s["id"] == args.shard)
    require(shard["owner"].startswith("lab-"), "compiler tests must execute on the builder")
    extracted, bundle = Path(args.extracted).resolve(), Path(args.bundle).resolve()
    check_bundle(bundle)
    require(plan_data["manifest_sha256"] == digest(bundle / "manifest.json"), "plan bundle mismatch")
    require(load(extracted / "extracted.json")["manifest_sha256"] == plan_data["manifest_sha256"], "extraction mismatch")
    # Compiler-injected paths cannot be repaired by --workspace-remap alone.
    # The lab controller mounts these exact private clone-local paths.
    require(os.path.samefile("/workspace", extracted / "source"), "controller must mount this source at /workspace")
    require(os.path.samefile("/target/current", extracted / "build/target"), "controller must mount extracted target at /target/current")
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    common = nextest(args.nextest)
    metadata = ["--cargo-metadata", str(extracted / "build/target/nextest/cargo-metadata.json"),
                "--binaries-metadata", str(extracted / "build/target/nextest/binaries-metadata.json"),
                "--workspace-remap", "/workspace", "--target-dir-remap", "/target/current", "--profile", "ci",
                "--config-file", "/workspace/.config/nextest.toml", "--user-config-file", "none", "--run-ignored", "all"]
    env = {**os.environ, "INSTA_UPDATE": "no", "RUST_TEST_THREADS": "1", "RAYON_NUM_THREADS": "2"}
    expected = sorted(key(r) for r in shard["tests"])
    require(expected, "empty partition")
    junit = Path("/workspace/target/nextest/ci/junit.xml")
    combined = ET.Element("testsuites")
    commands, observed, code = [], [], 0
    with (output / "run.log").open("wb") as log:
        for index, batch in enumerate(batches(shard["tests"])):
            options = [*metadata, "-E", filterset(batch)]
            listing = subprocess.check_output(common + ["list", *options, "--message-format", "json"], cwd="/workspace", env=env, text=True)
            write(output / f"selected-{index}.json", json.loads(listing))
            require(selected(json.loads(listing)) == sorted(key(r) for r in batch), "selected tests differ from frozen partition")
            junit.unlink(missing_ok=True)
            command = common + ["run", *options, "--no-tests", "fail"]
            commands.append(command)
            batch_code = subprocess.run(command, cwd="/workspace", env=env, stdout=log, stderr=subprocess.STDOUT).returncode
            code = code or batch_code
            if junit.exists():
                shutil.copyfile(junit, output / f"junit-{index}.xml")
                for suite in ET.parse(junit).getroot().iter("testsuite"):
                    combined.append(suite)
                if batch_code == 0:
                    observed.extend(junit_tests(junit))
    ET.ElementTree(combined).write(output / "junit.xml", encoding="utf-8", xml_declaration=True)
    counts = Counter(observed)
    missing = sorted(set(expected) - set(observed))
    duplicates = sorted(k for k, n in counts.items() if n > 1)
    extra = sorted(set(observed) - set(expected))
    receipt = {**plan_data["identity"], "schema_version": SCHEMA, "plan_digest": digest(args.plan),
               "test_inventory_digest": plan_data["test_inventory_digest"], "id": shard["id"],
               "partition": shard["partition"], "total": shard["total"], "owner": shard["owner"],
               "expected_count": len(expected), "observed_count": len(observed), "missing": missing,
               "duplicates": duplicates, "extra": extra, "exit_code": code, "command": commands[0], "commands": commands,
               "evidence_sha": digest(output / "run.log"), "observed": observed,
               "nextest_version": PIN["version"]}
    if (output / "junit.xml").exists():
        receipt["junit_sha256"] = digest(output / "junit.xml")
    write(output / "receipt.json", receipt)
    require(code == 0 and not missing and not duplicates and not extra and len(expected) == len(observed), "shard failed or count mismatch")


def verify(args):
    data = load(args.plan)
    validate_plan(data)
    not_applicable = set(getattr(args, "not_applicable_owner", []) or [])
    require(not_applicable <= {"lab-ignored"}, "only whole lab-ignored owner can be declared not applicable")
    expected = {s["id"]: s for s in data["shards"] if s["owner"] not in not_applicable}
    require(expected, "empty plan")
    receipts = [load(Path(p) / "receipt.json") for p in args.receipts]
    require(Counter(r["id"] for r in receipts) == Counter({k: 1 for k in expected}), "missing or duplicate shard receipts")
    for folder, receipt in zip(args.receipts, receipts):
        shard = expected[receipt["id"]]
        require(receipt["plan_digest"] == digest(args.plan), "receipt from different plan")
        require(all(receipt[k] == v for k, v in data["identity"].items()), "receipt identity mismatch")
        require(receipt["test_inventory_digest"] == data["test_inventory_digest"], "receipt inventory mismatch")
        require(receipt["nextest_version"] == PIN["version"], "receipt nextest version mismatch")
        require(receipt["exit_code"] == 0 and receipt["expected_count"] == receipt["observed_count"] == shard["expected_count"] > 0, "failed/empty/count-mismatched shard")
        require(not receipt["missing"] and not receipt["duplicates"] and not receipt["extra"], "invalid test coverage")
        require(receipt["partition"] == shard["partition"] and receipt["total"] == shard["total"] and receipt["owner"] == shard["owner"], "partition mismatch")
        require(digest(Path(folder) / "run.log") == receipt["evidence_sha"], "log checksum mismatch")
        require(digest(Path(folder) / "junit.xml") == receipt["junit_sha256"], "JUnit checksum mismatch")
        require(sorted(junit_tests(Path(folder) / "junit.xml")) == sorted(key(r) for r in shard["tests"]), "JUnit test identity mismatch")
    pending = {k: len(v) for k, v in data["non_sharded"].items() if v}
    expected_owners = Counter()
    observed_owners = Counter()
    for shard in data["shards"]:
        expected_owners[shard["owner"]] += shard["expected_count"]
    expected_owners.update(pending)
    for receipt in receipts:
        observed_owners[receipt["owner"]] += receipt["observed_count"]
    summary = {**data["identity"], "schema_version": SCHEMA, "plan_digest": digest(args.plan),
               "identity": data["identity"], "cargo_profile": "ci", "nextest_version": PIN["version"],
               **{k: data[k] for k in ("manifest_sha256", "archive_sha256", "source_bundle_sha256", "source_inventory_sha256", "test_inventory_digest", "test_count")},
               "archive_shards_verified": len(receipts), "observed_count": sum(observed_owners.values()),
               "expected_owners": dict(expected_owners), "observed_owners": dict(observed_owners),
               "owners": {owner: {"expected_count": expected_owners[owner], "observed_count": observed_owners[owner],
                                  "partitions": sum(r["owner"] == owner for r in receipts)} for owner in ("lab-normal", "lab-ignored")},
               "owner_inventory": {owner: {"test_count": expected_owners[owner],
                                           "test_inventory_digest": object_digest(sorted(
                                               [r for s in data["shards"] if s["owner"] == owner for r in s["tests"]]
                                               + data["non_sharded"].get(owner, []), key=key))}
                                   for owner in ("lab-normal", "lab-ignored", "builder-compiler", "protected-diagnostic", "live-session", "manual-benchmark")},
               "shards": [{**{k: r[k] for k in ("id", "owner", "partition", "total", "expected_count", "observed_count", "missing", "duplicates", "extra", "nextest_version", "plan_digest", "test_inventory_digest", "junit_sha256", "evidence_sha", "command", "exit_code")},
                           "receipt_sha256": digest(Path(folder) / "receipt.json")} for folder, r in zip(args.receipts, receipts)],
               "pending_non_sharded": pending,
               "blocked": [{"owner": name, "tests": data["non_sharded"][name]} for name in pending if name != "builder-compiler"],
               "external_gates": data["external_gates"], "aggregate_pass": False}
    summary["not_applicable_owners"] = {owner: summary["owner_inventory"][owner] for owner in sorted(not_applicable)}
    if getattr(args, "output", None):
        write(args.output, summary)
    print(json.dumps(summary, sort_keys=True))


def install_tool(args):
    directory = Path(args.directory).resolve()
    directory.mkdir(parents=True, exist_ok=False)
    path = directory / "cargo-nextest.tar.gz"
    with urllib.request.urlopen(PIN["url"], timeout=120) as source, path.open("xb") as output:
        shutil.copyfileobj(source, output)
    require(digest(path) == PIN["sha256"], "nextest download checksum mismatch")
    with tarfile.open(path) as tar:
        member = tar.getmember(PIN["binary"])
        require(member.isfile(), "nextest binary must be a regular file")
        with tar.extractfile(member) as source, (directory / PIN["binary"]).open("xb") as output:
            shutil.copyfileobj(source, output)
    (directory / PIN["binary"]).chmod(0o755)
    nextest(str(directory / PIN["binary"]))
    print(json.dumps({"version": PIN["version"], "sha256": PIN["sha256"]}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    installing = commands.add_parser("install-tool")
    installing.add_argument("--directory", required=True)
    installing.set_defaults(action=install_tool)
    inv = commands.add_parser("inventory")
    inv.add_argument("--source-root", default=str(ROOT))
    inv.add_argument("--output", required=True)
    inv.set_defaults(action=lambda a: write(a.output, source_inventory(Path(a.source_root))))
    sealing = commands.add_parser("seal")
    for flag in ("snapshot", "result", "identity", "archive", "source-root", "output"):
        sealing.add_argument("--" + flag, required=True)
    sealing.set_defaults(action=seal)
    extracting = commands.add_parser("extract")
    for flag in ("bundle", "destination"):
        extracting.add_argument("--" + flag, required=True)
    extracting.add_argument("--nextest", default="cargo-nextest")
    extracting.set_defaults(action=extract)
    planning = commands.add_parser("plan")
    for flag in ("bundle", "extracted", "output"):
        planning.add_argument("--" + flag, required=True)
    planning.add_argument("--shards", type=int, default=2)
    planning.set_defaults(action=plan)
    running = commands.add_parser("run")
    for flag in ("plan", "bundle", "extracted", "shard", "output"):
        running.add_argument("--" + flag, required=True)
    running.add_argument("--nextest", default="cargo-nextest")
    running.set_defaults(action=run)
    verifying = commands.add_parser("verify")
    verifying.add_argument("--plan", required=True)
    verifying.add_argument("--output")
    verifying.add_argument("--not-applicable-owner", action="append", choices=["lab-ignored"],
                           help="Omit only this whole owner; acceptance still requires a separately frozen applicability decision")
    verifying.add_argument("receipts", nargs="+")
    verifying.set_defaults(action=verify)
    args = parser.parse_args()
    try:
        args.action(args)
    except (ArchiveError, OSError, KeyError, StopIteration, subprocess.CalledProcessError, ET.ParseError) as error:
        print("test-archive: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
