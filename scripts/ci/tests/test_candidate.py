import hashlib
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("candidate", ROOT / "scripts/ci/candidate.py")
candidate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(candidate)


def receipt():
    result = {"schema_version": 1, "repository": candidate.REPOSITORY, "pr": 123,
            "head_sha": "a" * 40, "tree_sha": "b" * 40, "base_sha": "c" * 40,
            "inputs": {"toolchain": "1.99.0", "profile": "linux-gnu-sentinel", "image": "d" * 64, "input_digest": "e" * 64},
            "gates": [{"name": "rust-tests", "source_digest": "e" * 64, "command": ["cargo", "test"],
                       "exit_code": 0, "counts": {"passed": 5, "failed": 0}, "evidence_sha": "f" * 64}],
            "test_inventory_digest": "1" * 64,
            "shards": [{"partition": 0, "total": 2, "expected_count": 3, "observed_count": 3, "missing": [], "duplicates": []},
                       {"partition": 1, "total": 2, "expected_count": 2, "observed_count": 2, "missing": [], "duplicates": []}],
            "artifacts": [{"path": "release/daemon", "size": 5, "sha256": hashlib.sha256(b"built").hexdigest(), "build_origin": "a" * 40}]}
    for shard in result["shards"]:
        shard.update(id=f'lab-normal-{shard["partition"]}', receipt_sha256="8" * 64,
                     owner="lab-normal", extra=[], exit_code=0, command=["cargo-nextest", "run"],
                     evidence_sha="f" * 64, junit_sha256="2" * 64, nextest_version="0.9.145",
                     plan_digest="3" * 64, test_inventory_digest=result["test_inventory_digest"])
    owners = [{"owner": name, "required": name == "lab-normal", "reason": "affected source" if name == "lab-normal" else "not in affected contract",
               "expected_count": 5 if name == "lab-normal" else 0, "inventory_id_digest": "4" * 64}
              for name in sorted(candidate.ARCHIVE_OWNERS)]
    result["qualification_plan"] = {"schema_version": 1, **{k: result[k] for k in ("repository", "pr", "head_sha", "tree_sha", "base_sha", "test_inventory_digest")},
                                    "input_digest": result["inputs"]["input_digest"], "archive_plan_digest": "3" * 64, "owners": owners}
    result["archive"] = {"schema_version": 1, "aggregate_pass": False, "cargo_profile": "ci", "nextest_version": "0.9.145",
                         "identity": {k: result[k] for k in ("repository", "pr", "head_sha", "tree_sha", "base_sha", "inputs")},
                         "plan_digest": "3" * 64, "manifest_sha256": "5" * 64, "archive_sha256": "6" * 64, "source_bundle_sha256": "7" * 64,
                         "source_inventory_sha256": "9" * 64, "summary_sha256": "a" * 64, "test_count": 5,
                         "test_inventory_digest": result["test_inventory_digest"], "archive_shards_verified": 2, "observed_count": 5,
                         "pending_non_sharded": {}, "not_applicable_owners": {},
                         "expected_owners": {o["owner"]: o["expected_count"] for o in owners},
                         "observed_owners": {"lab-normal": 5},
                         "owner_inventory": {o["owner"]: {"test_count": o["expected_count"], "test_inventory_digest": o["inventory_id_digest"]} for o in owners}}
    return result


def plan_digest(value):
    return hashlib.sha256(candidate.canonical(value["qualification_plan"])).hexdigest()


class CandidateTests(unittest.TestCase):
    def validate(self, value):
        return candidate.validate(value, required_gates=["rust-tests"], expected_head="a" * 40, expected_base="c" * 40,
                                  expected_plan_digest=plan_digest(receipt()))

    def test_complete_receipt(self):
        self.validate(receipt())

    def test_identity_environment_and_provenance_negatives(self):
        cases = [("repository", "foreign/repository"), ("pr", True), ("head_sha", "2" * 40),
                 ("base_sha", "2" * 40), ("tree_sha", "not-a-sha"), ("schema_version", True)]
        for field, value in cases:
            data = receipt()
            data[field] = value
            with self.assertRaises(ValueError, msg=field):
                self.validate(data)
        for field, value in (("toolchain", "latest"), ("profile", "desktop-linux"), ("image", "latest"), ("input_digest", "bad")):
            data = receipt()
            data["inputs"][field] = value
            with self.assertRaises(ValueError, msg=field):
                self.validate(data)
        data = receipt()
        data["artifacts"][0]["build_origin"] = "2" * 40
        with self.assertRaisesRegex(ValueError, "relabeling"):
            self.validate(data)

    def test_malformed_missing_failed_stale_and_zero_test_gates(self):
        for gates in ([], [receipt()["gates"][0]] * 2, [{"name": "other"}], None):
            data = receipt()
            data["gates"] = gates
            with self.assertRaises((ValueError, KeyError, TypeError)):
                self.validate(data)
        for field, value in (("exit_code", 1), ("exit_code", False), ("source_digest", "2" * 64),
                             ("evidence_sha", "bad"), ("command", []),
                             ("counts", {"passed": 0, "failed": 0}), ("counts", {"passed": 4, "failed": 1})):
            data = receipt()
            data["gates"][0][field] = value
            with self.assertRaises(ValueError, msg=field):
                self.validate(data)

    def test_archive_coverage_negative_matrix(self):
        for field, value in (("partition", 0), ("total", 3), ("expected_count", 0), ("observed_count", 1),
                             ("missing", ["test-a"]), ("duplicates", ["test-b"]), ("extra", ["test-c"]),
                             ("exit_code", 1), ("nextest_version", "latest"), ("plan_digest", "0" * 64),
                             ("test_inventory_digest", "0" * 64), ("evidence_sha", "bad"), ("junit_sha256", "bad")):
            data = receipt()
            data["shards"][1][field] = value
            with self.assertRaises(ValueError, msg=field):
                self.validate(data)
        data = receipt()
        data["shards"].pop()
        with self.assertRaises(ValueError):
            self.validate(data)

    def test_plan_must_be_independently_frozen(self):
        data = receipt()
        data["qualification_plan"]["owners"][0]["reason"] = "changed after freeze"
        with self.assertRaisesRegex(ValueError, "plan changed"):
            self.validate(data)
        with self.assertRaisesRegex(ValueError, "frozen qualification plan missing"):
            candidate.validate(receipt(), required_gates=["rust-tests"], expected_head="a" * 40, expected_base="c" * 40,
                               expected_plan_digest=None)

    def test_compact_projection_excludes_large_exact_id_and_blocked_lists(self):
        data = receipt()
        summary = dict(data["archive"], shards=data["shards"],
                       blocked=[{"owner": "protected-diagnostic", "tests": [{"binary_id": "private-diagnostic", "name": f"test-{n}"} for n in range(6000)]}])
        projected = candidate.compact_archive(summary, "a" * 64)
        self.assertNotIn("blocked", projected)
        self.assertNotIn("shards", projected)
        self.assertEqual(projected, data["archive"])
        self.assertLess(len(candidate.canonical(data)) * 4 // 3 + 4, 60000)

    def test_assembly_cli_uses_separate_plan_and_actual_summary(self):
        data = receipt()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan = root / "plan.json"
            summary = root / "summary.json"
            draft = root / "draft.json"
            plan.write_bytes(candidate.canonical(data["qualification_plan"]))
            summary.write_bytes(candidate.canonical(dict(data["archive"], shards=data["shards"])))
            draft.write_bytes(candidate.canonical({k: v for k, v in data.items() if k not in {"archive", "qualification_plan", "shards", "test_inventory_digest"}}))
            command = [sys.executable, str(ROOT / "scripts/ci/candidate.py"), "assemble", "--receipt", str(draft),
                       "--archive-summary", str(summary), "--plan", str(plan), "--head", data["head_sha"], "--base", data["base_sha"],
                       "--required-gate", "rust-tests"]
            result = subprocess.run(command, capture_output=True, cwd=ROOT)
            self.assertEqual(result.returncode, 0, result.stderr)
            assembled = json.loads(result.stdout)
            self.assertEqual(assembled["archive"]["summary_sha256"], hashlib.sha256(summary.read_bytes()).hexdigest())
            self.assertEqual(assembled["qualification_plan"], data["qualification_plan"])
            draft.write_bytes(result.stdout)
            verify = subprocess.run([sys.executable, str(ROOT / "scripts/ci/candidate.py"), "verify", "--receipt", str(draft),
                                     "--plan-sha256", "0" * 64, "--head", data["head_sha"], "--base", data["base_sha"],
                                     "--required-gate", "rust-tests"], capture_output=True, cwd=ROOT)
            self.assertNotEqual(verify.returncode, 0)
            self.assertIn(b"qualification plan changed", verify.stderr)

    def test_missing_owner_or_changed_owner_digest_cannot_be_hidden(self):
        for mutate in (lambda a: a["owner_inventory"].pop("live-session"),
                       lambda a: a["owner_inventory"]["lab-normal"].update(test_inventory_digest="0" * 64)):
            data = receipt()
            mutate(data["archive"])
            with self.assertRaises(ValueError):
                self.validate(data)

    def test_required_compiler_gate_is_separate_from_archive_shards(self):
        data = receipt()
        owner = next(o for o in data["qualification_plan"]["owners"] if o["owner"] == "builder-compiler")
        owner.update(required=True, expected_count=1, reason="compiler-dependent contract")
        data["archive"]["owner_inventory"]["builder-compiler"]["test_count"] = 1
        data["archive"]["expected_owners"]["builder-compiler"] = 1
        data["archive"]["pending_non_sharded"]["builder-compiler"] = 1
        data["archive"]["test_count"] += 1
        with self.assertRaisesRegex(ValueError, "non-sharded evidence missing"):
            candidate.validate(data, required_gates=["rust-tests"], expected_head="a" * 40, expected_base="c" * 40,
                               expected_plan_digest=plan_digest(data))
        gate = copy.deepcopy(data["gates"][0])
        gate.update(name="rust-compiler-tests", counts={"passed": 1, "failed": 0})
        data["gates"].append(gate)
        candidate.validate(data, required_gates=["rust-tests", "rust-compiler-tests"], expected_head="a" * 40, expected_base="c" * 40,
                           expected_plan_digest=plan_digest(data))
        data["gates"].pop(0)
        with self.assertRaisesRegex(ValueError, "gate inventory mismatch"):
            candidate.validate(data, required_gates=["rust-tests", "rust-compiler-tests"], expected_head="a" * 40, expected_base="c" * 40,
                               expected_plan_digest=plan_digest(data))

    def test_all_zero_archive_is_not_qualification_even_with_matching_frozen_plan(self):
        data = receipt()
        data["shards"] = []
        data["archive"].update(test_count=0, archive_shards_verified=0, observed_count=0, observed_owners={})
        for owner in data["qualification_plan"]["owners"]:
            owner["expected_count"] = 0
            data["archive"]["expected_owners"][owner["owner"]] = 0
            data["archive"]["owner_inventory"][owner["owner"]]["test_count"] = 0
        with self.assertRaisesRegex(ValueError, "empty archive owner inventory"):
            candidate.validate(data, required_gates=["rust-tests"], expected_head="a" * 40, expected_base="c" * 40,
                               expected_plan_digest=plan_digest(data))

    def test_static_external_gate_names_cannot_be_omitted(self):
        policy = json.loads((ROOT / "scripts/ci/qualification-policy.json").read_text())
        required = policy["required_gates"]
        self.assertIn("rust-doctests", required)
        self.assertIn("rust-compiler-tests", required)
        self.assertNotIn("doctests", required)
        self.assertNotIn("builder-compiler-tests", required)
        data = receipt()
        with self.assertRaisesRegex(ValueError, "gate inventory mismatch"):
            candidate.validate(data, required_gates=required, expected_head="a" * 40, expected_base="c" * 40,
                               expected_plan_digest=plan_digest(data))

    def test_per_owner_partition_ranges_are_independent(self):
        data = receipt()
        owner = next(o for o in data["qualification_plan"]["owners"] if o["owner"] == "lab-ignored")
        owner.update(required=True, expected_count=1, reason="required ignored kernel regression")
        data["archive"]["owner_inventory"]["lab-ignored"]["test_count"] = 1
        data["archive"]["expected_owners"]["lab-ignored"] = 1
        data["archive"]["observed_owners"]["lab-ignored"] = 1
        data["archive"]["test_count"] += 1
        shard = dict(data["shards"][0], id="lab-ignored-0", owner="lab-ignored", partition=0, total=1, expected_count=1, observed_count=1)
        data["shards"].append(shard)
        data["archive"].update(archive_shards_verified=3, observed_count=6)
        candidate.validate(data, required_gates=["rust-tests"], expected_head="a" * 40, expected_base="c" * 40,
                           expected_plan_digest=plan_digest(data))
        data["shards"].pop()
        with self.assertRaisesRegex(ValueError, "shard counts mismatch"):
            candidate.validate(data, required_gates=["rust-tests"], expected_head="a" * 40, expected_base="c" * 40,
                               expected_plan_digest=plan_digest(data))

    def test_not_applicable_ignored_owner_requires_explicit_frozen_exclusion(self):
        data = receipt()
        owner = next(o for o in data["qualification_plan"]["owners"] if o["owner"] == "lab-ignored")
        owner.update(expected_count=2, reason="optional hardware tests outside this affected contract")
        data["archive"]["owner_inventory"]["lab-ignored"]["test_count"] = 2
        data["archive"]["expected_owners"]["lab-ignored"] = 2
        data["archive"]["test_count"] += 2
        with self.assertRaisesRegex(ValueError, "exclusion not explicit"):
            candidate.validate(data, required_gates=["rust-tests"], expected_head="a" * 40, expected_base="c" * 40,
                               expected_plan_digest=plan_digest(data))
        data["archive"]["not_applicable_owners"] = {"lab-ignored": dict(data["archive"]["owner_inventory"]["lab-ignored"])}
        candidate.validate(data, required_gates=["rust-tests"], expected_head="a" * 40, expected_base="c" * 40,
                           expected_plan_digest=plan_digest(data))
        owner["required"] = True
        with self.assertRaisesRegex(ValueError, "required owner excluded"):
            candidate.validate(data, required_gates=["rust-tests"], expected_head="a" * 40, expected_base="c" * 40,
                               expected_plan_digest=plan_digest(data))

    def test_frozen_not_applicable_protected_tests_are_not_passes(self):
        data = receipt()
        owner = next(o for o in data["qualification_plan"]["owners"] if o["owner"] == "protected-diagnostic")
        owner.update(expected_count=2, reason="requires production identity; unrelated diagnostic contract")
        data["archive"]["owner_inventory"]["protected-diagnostic"]["test_count"] = 2
        data["archive"]["expected_owners"]["protected-diagnostic"] = 2
        data["archive"]["test_count"] += 2
        data["archive"]["pending_non_sharded"]["protected-diagnostic"] = 2
        candidate.validate(data, required_gates=["rust-tests"], expected_head="a" * 40, expected_base="c" * 40,
                           expected_plan_digest=plan_digest(data))
        owner["required"] = True
        with self.assertRaisesRegex(ValueError, "non-sharded evidence missing"):
            candidate.validate(data, required_gates=["rust-tests"], expected_head="a" * 40, expected_base="c" * 40,
                               expected_plan_digest=plan_digest(data))

    def test_archive_cannot_invent_aggregate_pass_or_inventory(self):
        for field, value in (("schema_version", True), ("aggregate_pass", True), ("archive_shards_verified", 1), ("observed_count", 6),
                             ("nextest_version", "latest"), ("cargo_profile", "release"), ("plan_digest", "0" * 64),
                             ("test_inventory_digest", "0" * 64)):
            data = receipt()
            data["archive"][field] = value
            with self.assertRaises(ValueError, msg=field):
                self.validate(data)
        for mutate in (lambda a: a["owner_inventory"]["live-session"].update(test_count=False),
                       lambda a: a["pending_non_sharded"].update({"live-session": False})):
            data = receipt()
            mutate(data["archive"])
            with self.assertRaises(ValueError):
                self.validate(data)
        data["shards"] = []
        with self.assertRaises(ValueError):
            self.validate(data)

    def test_all_top_level_fields_required(self):
        for field in receipt():
            data = receipt()
            del data[field]
            with self.assertRaises(ValueError, msg=field):
                self.validate(data)

    def test_artifact_path_and_digest_negatives(self):
        for field, value in (("path", "/home/private/file"), ("path", "../outside"), ("path", "release//file"),
                             ("sha256", "bad"), ("size", 0), ("size", True)):
            data = receipt()
            data["artifacts"][0][field] = value
            with self.assertRaises(ValueError, msg=field):
                self.validate(data)
        data = receipt()
        data["artifacts"] *= 2
        with self.assertRaises(ValueError):
            self.validate(data)

    def test_duplicate_json_keys_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "receipt.json"
            path.write_text('{"pr": 1,"pr":2}')
            with self.assertRaises(ValueError):
                candidate.load(path)


class PromotionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.env = {**os.environ, "GIT_AUTHOR_NAME": "CI Test", "GIT_COMMITTER_NAME": "CI Test",
                    "GIT_AUTHOR_EMAIL": "ci@example.invalid", "GIT_COMMITTER_EMAIL": "ci@example.invalid",
                    "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1"}
        self.run_git("init", "-b", "main")
        (self.repo / "source").write_text("base")
        self.run_git("add", ".")
        self.run_git("commit", "-m", "base")
        self.base = self.run_git("rev-parse", "HEAD")
        self.run_git("checkout", "-b", "candidate")
        (self.repo / "source").write_text("candidate")
        self.run_git("commit", "-am", "candidate")
        self.head = self.run_git("rev-parse", "HEAD")
        self.run_git("checkout", "main")
        self.run_git("merge", "--no-ff", "candidate", "-m", "qualified merge")
        self.merge = self.run_git("rev-parse", "HEAD")
        self.artifacts = self.root / "artifacts"
        (self.artifacts / "release").mkdir(parents=True)
        self.binary = self.artifacts / "release/daemon"
        self.binary.write_bytes(b"built")
        self.data = receipt()
        self.data.update(head_sha=self.head, base_sha=self.base, tree_sha=self.run_git("rev-parse", "HEAD^{tree}"))
        self.data["artifacts"][0]["build_origin"] = self.head

    def run_git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args], env=self.env, stderr=subprocess.DEVNULL, text=True).strip()

    def promote(self, merge=None):
        return candidate.promote(self.data, root=self.repo, artifact_root=self.artifacts, merge_sha=merge or self.merge)

    def test_tree_equivalent_merge_preserves_original_source(self):
        result = self.promote()
        self.assertEqual(result["source_sha"], self.head)
        self.assertEqual(result["merge_sha"], self.merge)
        self.assertEqual(result["artifacts"][0]["build_origin"], self.head)
        self.assertNotEqual(self.head, self.merge)

    def test_unreviewed_merge_tree_is_not_promoted(self):
        (self.repo / "source").write_text("changed integration")
        self.run_git("commit", "-am", "changed")
        with self.assertRaisesRegex(ValueError, "tree changed"):
            self.promote(self.run_git("rev-parse", "HEAD"))

    def test_tree_identical_commit_without_head_ancestry_is_not_promoted(self):
        arbitrary = self.run_git("commit-tree", self.data["tree_sha"], "-p", self.base, "-m", "unrelated identical tree")
        with self.assertRaisesRegex(ValueError, "head is not an ancestor"):
            self.promote(arbitrary)

    def test_qualified_source_tree_mismatch(self):
        self.data["tree_sha"] = "2" * 40
        with self.assertRaisesRegex(ValueError, "source tree mismatch"):
            self.promote()

    def test_modified_or_symlink_artifacts_are_not_promoted(self):
        self.binary.write_bytes(b"other")
        with self.assertRaisesRegex(ValueError, "bytes changed"):
            self.promote()
        self.binary.unlink()
        self.binary.symlink_to(self.repo / "source")
        with self.assertRaisesRegex(ValueError, "symlink"):
            self.promote()


if __name__ == "__main__":
    unittest.main()
