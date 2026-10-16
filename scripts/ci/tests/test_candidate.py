import hashlib
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location("candidate", ROOT / "scripts/ci/candidate.py")
candidate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(candidate)


def receipt():
    return {"schema_version": 1, "repository": candidate.REPOSITORY, "pr": 123,
            "head_sha": "a" * 40, "tree_sha": "b" * 40, "base_sha": "c" * 40,
            "inputs": {"toolchain": "1.99.0", "profile": "linux-gnu-sentinel", "image": "d" * 64, "input_digest": "e" * 64},
            "gates": [{"name": "rust-tests", "source_digest": "e" * 64, "command": ["cargo", "test"],
                       "exit_code": 0, "counts": {"passed": 5, "failed": 0}, "evidence_sha": "f" * 64}],
            "test_inventory_digest": "1" * 64,
            "shards": [{"partition": 0, "total": 2, "expected_count": 3, "observed_count": 3, "missing": [], "duplicates": []},
                       {"partition": 1, "total": 2, "expected_count": 2, "observed_count": 2, "missing": [], "duplicates": []}],
            "artifacts": [{"path": "release/daemon", "size": 5, "sha256": hashlib.sha256(b"built").hexdigest(), "build_origin": "a" * 40}]}


class CandidateTests(unittest.TestCase):
    def validate(self, value):
        return candidate.validate(value, required_gates=["rust-tests"], expected_head="a" * 40, expected_base="c" * 40)

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
                             ("missing", ["test-a"]), ("duplicates", ["test-b"])):
            data = receipt()
            data["shards"][1][field] = value
            with self.assertRaises(ValueError, msg=field):
                self.validate(data)
        data = receipt()
        data["shards"].pop()
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
