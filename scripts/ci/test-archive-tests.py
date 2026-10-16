"""Mechanism tests; synthetic listings are not compiled/runtime acceptance."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("archive", Path(__file__).with_name("test-archive.py"))
archive = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(archive)


def listing(tests):
    return {"test-count": len(tests), "rust-suites": {"sentinel-bio": {
        "status": "listed", "testcases": {name: {"ignored": False, "filter-match": {"status": "matches"}} for name in tests}}}}


class ArchiveTests(unittest.TestCase):
    def test_compiled_listing_has_exact_count(self):
        self.assertEqual(len(archive.test_rows(listing(["a", "b"]))), 2)
        bad = listing(["a"])
        bad["test-count"] = 2
        with self.assertRaisesRegex(archive.ArchiveError, "count"):
            archive.test_rows(bad)

    def test_unlisted_suite_cannot_hide_tests(self):
        bad = listing(["a"])
        bad["rust-suites"]["sentinel-bio"]["status"] = "skipped"
        with self.assertRaisesRegex(archive.ArchiveError, "unlisted"):
            archive.test_rows(bad)

    def test_selected_excludes_partition_mismatches(self):
        data = listing(["a", "b"])
        data["rust-suites"]["sentinel-bio"]["testcases"]["b"]["filter-match"] = {"status": "mismatch", "reason": "partition"}
        self.assertEqual(archive.selected(data), [("sentinel-bio", "a")])

    def test_unknown_ignored_test_blocks(self):
        with self.assertRaisesRegex(archive.ArchiveError, "unassigned"):
            archive.classify({"binary_id": "sentinel-bio", "name": "x", "ignored": True}, {"ignored_definitions": {}})

    def test_compiler_tests_never_route_to_lab(self):
        row = {"binary_id": archive.COMPILER, "name": "test", "ignored": False}
        self.assertEqual(archive.classify(row, {}), "builder-compiler")

    def test_protected_diagnostic_is_not_generic_ignored_test(self):
        row = {"binary_id": "sentinel-workflow", "name": "x", "ignored": True}
        self.assertEqual(archive.classify(row, {"ignored_definitions": {"x": [{"owner": "protected-diagnostic"}]}}), "protected-diagnostic")

    def test_paths_fail_closed(self):
        for path in ("/secret", "../secret", "a/../secret", ""):
            with self.subTest(path=path), self.assertRaises(archive.ArchiveError):
                archive.relative(path)

    def test_exact_filterset_escapes_nextest_dsl(self):
        expr = archive.filterset([{"binary_id": "pkg::target", "name": "case(a,b)\\end"}])
        self.assertEqual(expr, r"(binary_id(=pkg::target) & test(=case(a\,b\)\\end))")

    def test_batches_bound_single_argument_without_losing_tests(self):
        rows = [{"binary_id": "pkg", "name": f"test_{i}_" + "a" * 100} for i in range(2000)]
        batches = list(archive.batches(rows))
        self.assertGreater(len(batches), 1)
        self.assertEqual([r for b in batches for r in b], rows)
        self.assertTrue(all(len(archive.filterset(b).encode()) <= 24000 for b in batches))

    def test_bad_tool_version_is_rejected(self):
        with patch.object(archive.subprocess, "check_output", return_value="cargo-nextest 0.9.14\n"):
            with self.assertRaisesRegex(archive.ArchiveError, "version"):
                archive.nextest("cargo-nextest")

    def test_junit_failures_and_skips_cannot_pass(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "junit.xml"
            for marker in ("<failure/>", "<error/>", "<skipped/>"):
                path.write_text(f'<testsuites><testsuite name="pkg"><testcase name="x">{marker}</testcase></testsuite></testsuites>')
                with self.subTest(marker=marker), self.assertRaises(archive.ArchiveError):
                    archive.junit_tests(path)

    def fixture_plan(self, root, with_ignored=False):
        bundle, extracted = root / "bundle", root / "extracted"
        bundle.mkdir()
        extracted.mkdir()
        manifest = {"schema_version": 1, "repository": "silentspike/project-sentinel", "pr": 1,
                    "archive_sha256": "f" * 64, "source_bundle_sha256": "1" * 64, "source_inventory_sha256": "2" * 64,
                    "head_sha": "a" * 40, "tree_sha": "b" * 40, "base_sha": "c" * 40,
                    "inputs": {"toolchain": "1.99.0", "profile": "linux-gnu-sentinel", "image": "d" * 64, "input_digest": "e" * 64}}
        archive.write(bundle / "manifest.json", manifest)
        archive.write(bundle / "source-inventory.json", {"ignored_definitions": {"f_ignored": [{"owner": "lab-ignored"}]}, "external_gates": {"doctests": {"owner": "builder"}}})
        test_listing = listing(["a", "b", "c", "d", "e"])
        if with_ignored:
            test_listing["rust-suites"]["sentinel-bio"]["testcases"]["f_ignored"] = {"ignored": True, "filter-match": {"status": "matches"}}
            test_listing["test-count"] += 1
        archive.write(extracted / "test-list.json", test_listing)
        archive.write(extracted / "extracted.json", {"manifest_sha256": archive.digest(bundle / "manifest.json"), "test_inventory_digest": archive.object_digest(archive.test_rows(test_listing))})
        args = type("Args", (), {"bundle": str(bundle), "extracted": str(extracted), "shards": 2, "output": str(root / "plan.json")})
        with patch.object(archive, "check_bundle", return_value=manifest):
            archive.plan(args)
        return archive.load(args.output), args.output

    def test_plan_is_disjoint_complete_and_nonempty(self):
        with tempfile.TemporaryDirectory() as temporary:
            plan, _ = self.fixture_plan(Path(temporary))
            rows = [archive.key(r) for s in plan["shards"] for r in s["tests"]]
            self.assertEqual(len(rows), len(set(rows)))
            self.assertEqual(len(rows), plan["test_count"])
            self.assertEqual([s["expected_count"] for s in plan["shards"]], [3, 2])
            self.assertEqual(plan["non_sharded"]["builder-compiler"], [])

    def fixture_receipts(self, root, data, plan_path):
        folders = []
        for shard in data["shards"]:
            folder = root / shard["id"]
            folder.mkdir()
            (folder / "run.log").write_text("synthetic fixture, not runtime acceptance\n")
            cases = "".join(f'<testcase name="{r["name"]}"/>' for r in shard["tests"])
            (folder / "junit.xml").write_text(f'<testsuites><testsuite name="sentinel-bio">{cases}</testsuite></testsuites>')
            archive.write(folder / "receipt.json", {**data["identity"], **{k: shard[k] for k in ("id", "partition", "total", "owner", "expected_count")},
                          "observed_count": shard["expected_count"], "plan_digest": archive.digest(plan_path),
                          "test_inventory_digest": data["test_inventory_digest"], "exit_code": 0,
                          "nextest_version": archive.PIN["version"], "command": ["synthetic-fixture"],
                          "missing": [], "duplicates": [], "extra": [],
                          "evidence_sha": archive.digest(folder / "run.log"), "junit_sha256": archive.digest(folder / "junit.xml")})
            folders.append(str(folder))
        return folders

    def test_verify_accepts_counted_shards_but_never_aggregate_pass(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            data, path = self.fixture_plan(root)
            folders = self.fixture_receipts(root, data, path)
            args = type("Args", (), {"plan": path, "receipts": folders})
            with patch("builtins.print") as output:
                archive.verify(args)
            self.assertFalse(json.loads(output.call_args.args[0])["aggregate_pass"])

    def test_not_applicable_ignored_owner_retains_inventory_without_pass(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            data, path = self.fixture_plan(root, with_ignored=True)
            folders = [p for p in self.fixture_receipts(root, data, path) if "lab-normal-" in p]
            args = type("Args", (), {"plan": path, "receipts": folders, "not_applicable_owner": ["lab-ignored"]})
            with patch("builtins.print") as output:
                archive.verify(args)
            summary = json.loads(output.call_args.args[0])
            self.assertEqual(summary["test_count"], 6)
            self.assertEqual(summary["observed_count"], 5)
            self.assertEqual(summary["owners"]["lab-ignored"], {"expected_count": 1, "observed_count": 0, "partitions": 0})
            self.assertEqual(summary["not_applicable_owners"]["lab-ignored"], summary["owner_inventory"]["lab-ignored"])
            self.assertFalse(summary["aggregate_pass"])
            self.assertEqual(summary["test_inventory_digest"], data["test_inventory_digest"])
            args.not_applicable_owner = []
            with self.assertRaisesRegex(archive.ArchiveError, "missing"):
                archive.verify(args)

    def test_normal_owner_cannot_be_skipped_or_partial_ignored_shard_waived(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            data, path = self.fixture_plan(root, with_ignored=True)
            folders = self.fixture_receipts(root, data, path)
            args = type("Args", (), {"plan": path, "receipts": folders, "not_applicable_owner": ["lab-normal"]})
            with self.assertRaisesRegex(archive.ArchiveError, "only whole"):
                archive.verify(args)
            args.not_applicable_owner = ["lab-ignored"]
            with self.assertRaisesRegex(archive.ArchiveError, "missing or duplicate"):
                archive.verify(args)

    def test_verify_rejects_missing_duplicate_corrupt_receipts(self):
        for defect in ("missing", "duplicate", "log", "junit", "head", "count", "inventory", "partition", "version"):
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                data, path = self.fixture_plan(root)
                folders = self.fixture_receipts(root, data, path)
                if defect == "missing":
                    folders.pop()
                elif defect == "duplicate":
                    folders.append(folders[0])
                elif defect in ("log", "junit"):
                    (Path(folders[0]) / ("run.log" if defect == "log" else "junit.xml")).write_text("tampered")
                else:
                    receipt_path = Path(folders[0]) / "receipt.json"
                    receipt = archive.load(receipt_path)
                    field = {"head": "head_sha", "count": "observed_count", "inventory": "test_inventory_digest", "partition": "partition", "version": "nextest_version"}[defect]
                    receipt[field] = 9000 if defect in ("count", "partition") else "0" * 64
                    archive.write(receipt_path, receipt)
                args = type("Args", (), {"plan": path, "receipts": folders})
                with self.assertRaises(archive.ArchiveError):
                    archive.verify(args)

    def test_plan_tampering_cannot_hide_unexecuted_tests(self):
        for defect in ("missing", "duplicate", "partition", "total", "count", "owner"):
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as temporary:
                data, _ = self.fixture_plan(Path(temporary))
                if defect == "missing":
                    data["shards"][0]["tests"].pop()
                elif defect == "duplicate":
                    data["shards"][0]["tests"][0] = data["shards"][1]["tests"][0]
                else:
                    data["shards"][0][{"partition": "partition", "total": "total", "count": "expected_count", "owner": "owner"}[defect]] = "builder-compiler" if defect == "owner" else 99
                with self.assertRaises(archive.ArchiveError):
                    archive.validate_plan(data)

    def test_ci_profile_preserves_checks_and_does_not_change_release(self):
        data = archive.tomllib.loads((archive.ROOT / "Cargo.toml").read_text())
        self.assertEqual(data["profile"]["ci"]["inherits"], "dev")
        self.assertTrue(data["profile"]["ci"]["debug-assertions"])
        self.assertTrue(data["profile"]["ci"]["overflow-checks"])
        self.assertNotIn("release", data["profile"])

    def test_source_inventory_covers_ignored_and_excluded(self):
        data = archive.source_inventory(archive.ROOT)
        self.assertIn("crates/sentinel-ebpf-probes", data["excluded"])
        self.assertIn("crates/sentinel-inference", data["excluded"])
        self.assertTrue(data["ignored_definitions"])
        owners = {v["owner"] for entries in data["ignored_definitions"].values() for v in entries}
        self.assertEqual(owners, {"lab-ignored", "protected-diagnostic", "manual-benchmark", "live-session"})

    def test_seal_binds_exported_archive_and_filters_source_transfer(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source"
            source.mkdir()
            (source / "Cargo.toml").write_text('[workspace]\nmembers=[]\nexclude=[]\n')
            (source / "fixture.rs").write_text("// synthetic fixture\n")
            (source / ".env").write_text("UNRELATED_PRIVATE_FIXTURE=not-a-real-secret\n")
            files = [{"path": name, "sha256": archive.digest(source / name)} for name in ("Cargo.toml", "fixture.rs")]
            sha = archive.object_digest(files)
            snapshot = {"sources": files, "source_sha256": sha, "commit": "a" * 40}
            archive.write(root / "snapshot.json", {"snapshot": snapshot, "token": "capability-must-not-transfer"})
            (root / "archive.tar.zst").write_bytes(b"synthetic archive export; not a Rust build")
            identity = {"repository": "silentspike/project-sentinel", "pr": 1, "head_sha": "a" * 40,
                        "tree_sha": "b" * 40, "base_sha": "c" * 40,
                        "inputs": {"toolchain": "1.99.0", "profile": "linux-gnu-sentinel", "image": "d" * 64, "input_digest": sha}}
            archive.write(root / "identity.json", identity)
            result = {"id": "fixture", "exit_code": 0, "commit": "a" * 40, "source_sha256": sha,
                      "image": {"image": "d" * 64}, "args": ["nextest", "archive", "--cargo-profile", "ci"],
                      "files": [{"path": "target/archive.tar.zst", "sha256": archive.digest(root / "archive.tar.zst")}]}
            archive.write(root / "result.json", result)
            args = type("Args", (), {"snapshot": str(root / "snapshot.json"), "result": str(root / "result.json"),
                                    "identity": str(root / "identity.json"), "archive": str(root / "archive.tar.zst"),
                                    "source_root": str(source), "output": str(root / "bundle")})
            def git_result(command, **kwargs):
                return {"HEAD": "a" * 40 + "\n", "HEAD^{tree}": "b" * 40 + "\n", "--porcelain": b""}[command[-1]]
            with patch.object(archive.subprocess, "check_output", side_effect=git_result), patch("builtins.print"):
                archive.seal(args)
            manifest = archive.check_bundle(root / "bundle")
            self.assertNotIn("token", manifest)
            with archive.tarfile.open(root / "bundle/sources.tar.gz") as tar:
                self.assertEqual(tar.getnames(), ["Cargo.toml", "fixture.rs"])
                self.assertTrue(all(m.uname == m.gname == "" for m in tar.getmembers()))
            (root / "bundle/tests.tar.zst").write_bytes(b"corrupt")
            with self.assertRaisesRegex(archive.ArchiveError, "corrupt"):
                archive.check_bundle(root / "bundle")


if __name__ == "__main__":
    unittest.main()
