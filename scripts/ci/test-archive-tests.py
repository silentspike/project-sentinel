"""Mechanism tests; synthetic listings are not compiled/runtime acceptance."""
import importlib.util
import copy
import hashlib
import json
from pathlib import Path
import tarfile
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

    def test_extract_creates_nextest_destination_before_invocation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bundle = root / "bundle"
            bundle.mkdir()
            fixture = root / "Cargo.toml"
            fixture.write_text("[workspace]\n")
            with tarfile.open(bundle / "sources.tar.gz", "w:gz") as tar:
                tar.add(fixture, arcname="Cargo.toml")
            archive.write(bundle / "manifest.json", {"synthetic": True})
            manifest = {"sources": [{"path": "Cargo.toml", "sha256": archive.digest(fixture)}]}
            destination = root / "extracted"

            def list_archive(command, **kwargs):
                build = Path(command[command.index("--extract-to") + 1])
                self.assertEqual(build, destination / "build")
                self.assertTrue(build.is_dir(), "nextest requires an existing extraction destination")
                self.assertTrue((destination / "source/Cargo.toml").is_file())
                (build / "target/ci").mkdir(parents=True)
                (build / "target/ci/breakout-helper").write_bytes(b"synthetic helper")
                return json.dumps(listing(["synthetic_test"]))

            args = type("Args", (), {"bundle": str(bundle), "destination": str(destination), "nextest": "synthetic-tool"})
            with patch.object(archive, "check_bundle", return_value=manifest), \
                    patch.object(archive, "nextest", return_value=["synthetic-tool", "nextest"]), \
                    patch.object(archive.subprocess, "check_output", side_effect=list_archive):
                archive.extract(args)
            self.assertTrue((destination / "source/target").samefile(destination / "build/target"))
            self.assertTrue((destination / "build/target/debug").samefile(destination / "build/target/ci"))
            self.assertEqual(archive.load(destination / "extracted.json")["test_inventory_digest"],
                             archive.object_digest(archive.test_rows(listing(["synthetic_test"]))))

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

    def test_v1_plan_and_whole_owner_summary_bytes_remain_unchanged(self):
        golden = {
            False: ("7c37d9bfa2f5758b94dedb081183f445007ebe7c623739f6f1c807c8cac29399",
                    "f407291c51938b7d3b48fedad528976734cbeb517c85588bd991ce8e5f920d52"),
            True: ("eff3e0e53ad6284ee02e7d7830d923980fbc7fb9a0cfb88032ad2c2f8bfab435",
                   "453b3596e75b726ef4d1ce76a975a8d7bf6a1d2c7cfee47e36be358fe0d84704")}
        for ignored in (False, True):
            with self.subTest(ignored=ignored), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                data, path = self.fixture_plan(root, with_ignored=ignored)
                folders = self.fixture_receipts(root, data, path)
                args = type("Args", (), {"plan": path, "receipts": [p for p in folders if not ignored or "lab-normal-" in p],
                                        "not_applicable_owner": ["lab-ignored"] if ignored else []})
                with patch("builtins.print") as output:
                    archive.verify(args)
                self.assertEqual((archive.digest(path), hashlib.sha256(output.call_args.args[0].encode()).hexdigest()), golden[ignored])

    def fixture_v2(self, root, required=("f_ignored", "h_ignored"), other_owners=False,
                   ignored_names=("f_ignored", "g_ignored", "h_ignored", "i_ignored")):
        self.fixture_plan(root, with_ignored=True)
        source_inventory = archive.load(root / "bundle/source-inventory.json")
        test_listing = archive.load(root / "extracted/test-list.json")
        for name in ignored_names:
            if name == "f_ignored":
                continue
            source_inventory["ignored_definitions"][name] = [{"owner": "lab-ignored"}]
            test_listing["rust-suites"]["sentinel-bio"]["testcases"][name] = {"ignored": True, "filter-match": {"status": "matches"}}
            test_listing["test-count"] += 1
        if other_owners:
            for binary, name, ignored in ((archive.COMPILER, "compiler", False), ("sentinel-workflow", "diagnostic", True)):
                test_listing["rust-suites"][binary] = {"status": "listed", "testcases": {name: {
                    "ignored": ignored, "filter-match": {"status": "matches"}}}}
                test_listing["test-count"] += 1
            source_inventory["ignored_definitions"]["diagnostic"] = [{"owner": "protected-diagnostic"}]
        archive.write(root / "bundle/source-inventory.json", source_inventory)
        archive.write(root / "extracted/test-list.json", test_listing)
        marker = archive.load(root / "extracted/extracted.json")
        marker["test_inventory_digest"] = archive.object_digest(archive.test_rows(test_listing))
        archive.write(root / "extracted/extracted.json", marker)
        args = type("Args", (), {"bundle": str(root / "bundle"), "extracted": str(root / "extracted"),
                                "output": str(root / "base-plan.json"), "shards": 2})
        with patch.object(archive, "check_bundle", return_value=archive.load(root / "bundle/manifest.json")):
            archive.plan(args)
        original = archive.load(args.output)
        rows = archive.ignored_rows(original)
        applicability = {"schema_version": 2, "identity": copy.deepcopy(original["identity"]),
                         "manifest_sha256": original["manifest_sha256"], "test_inventory_digest": original["test_inventory_digest"],
                         "owner": "lab-ignored", "compiled_owner_count": len(rows), "compiled_owner_digest": archive.object_digest(rows),
                         "decisions": [{"binary_id": r["binary_id"], "name": r["name"], "required": r["name"] in required,
                                        "reason": "Affected contract requires execution" if r["name"] in required else "Reviewed outside the affected contract"}
                                       for r in rows]}
        data = archive.applicable_plan(original, applicability)
        path = root / "v2-plan.json"
        archive.write(path, data)
        return data, path

    def fixture_v2_receipts(self, root, data, path):
        folders = []
        for shard in data["shards"]:
            rows = archive.required_tests(data, shard)
            if not rows:
                continue
            folder = root / shard["id"]
            folder.mkdir()
            (folder / "run.log").write_text("synthetic schema2 mechanism only; not runtime evidence\n")
            cases = "".join(f'<testcase name="{r["name"]}"/>' for r in rows)
            (folder / "junit.xml").write_text(f'<testsuites><testsuite name="sentinel-bio">{cases}</testsuite></testsuites>')
            archive.write(folder / "receipt.json", {**data["identity"], "schema_version": 2,
                          **{k: shard[k] for k in ("id", "partition", "total", "owner")},
                          "expected_count": len(rows), "observed_count": len(rows), "observed": [archive.key(r) for r in rows],
                          "plan_digest": archive.digest(path), "applicability_digest": data["applicability_digest"],
                          "required_digest": archive.object_digest(sorted(rows, key=archive.key)),
                          "test_inventory_digest": data["test_inventory_digest"], "exit_code": 0,
                          "nextest_version": archive.PIN["version"], "command": ["synthetic-not-runtime"],
                          "missing": [], "duplicates": [], "extra": [], "evidence_sha": archive.digest(folder / "run.log"),
                          "junit_sha256": archive.digest(folder / "junit.xml")})
            folders.append(str(folder))
        return folders

    def verify_v2(self, path, folders, frozen=None, **kwargs):
        args = type("Args", (), {"plan": path, "receipts": folders,
                                "frozen_plan_sha256": archive.digest(path) if frozen is None else frozen, **kwargs})
        with patch("builtins.print") as output:
            archive.verify(args)
        return json.loads(output.call_args.args[0])

    def test_schema2_mixed_map_preserves_all_inventory_and_zero_required_partition(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            data, path = self.fixture_v2(root)
            folders = self.fixture_v2_receipts(root, data, path)
            summary = self.verify_v2(path, folders)
            self.assertEqual(summary["schema_version"], 2)
            self.assertEqual(summary["test_count"], 9)
            self.assertEqual(summary["expected_owners"]["lab-ignored"], 4)
            self.assertEqual(summary["observed_owners"]["lab-ignored"], 2)
            owner = summary["owner_applicability"]["lab-ignored"]
            self.assertEqual((owner["compiled_count"], owner["required_count"], owner["not_applicable_count"], owner["observed_required_count"]), (4, 2, 2, 2))
            self.assertEqual(owner["compiled_digest"], summary["owner_inventory"]["lab-ignored"]["test_inventory_digest"])
            self.assertEqual([p["required_count"] for p in owner["partitions"]], [2, 0])
            self.assertEqual(owner["partitions"][1]["required_digest"], archive.object_digest([]))
            self.assertEqual({r["name"] for r in summary["not_applicable_tests"]}, {"g_ignored", "i_ignored"})
            self.assertTrue(all(r["status"] == "NOTEXECUTED_NOTPASS" for r in summary["not_applicable_tests"]))
            self.assertEqual(summary["accounting"], {"compiled_count": 9, "observed_required_count": 7,
                                                     "not_applicable_count": 2, "other_owner_count": 0})
            self.assertFalse(summary["aggregate_pass"])
            self.assertEqual(summary["not_applicable_owners"], {})
            for shard in summary["shards"]:
                self.assertEqual(shard["applicability_digest"], data["applicability_digest"])
                self.assertIn("required_digest", shard)

    def test_schema2_accounting_retains_other_owners_pending(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            data, path = self.fixture_v2(root, other_owners=True)
            summary = self.verify_v2(path, self.fixture_v2_receipts(root, data, path))
            self.assertEqual(summary["test_count"], 11)
            self.assertEqual(summary["accounting"]["other_owner_count"], 2)
            self.assertEqual(summary["pending_non_sharded"], {"builder-compiler": 1, "protected-diagnostic": 1})
            self.assertFalse(summary["aggregate_pass"])

    def test_schema2_full_44_id_owner_remains_inventoried_with_16_required(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            names = ("f_ignored",) + tuple(f"ignored_{n:03}" for n in range(43))
            data, path = self.fixture_v2(root, ignored_names=names, required=names[:16], other_owners=True)
            summary = self.verify_v2(path, self.fixture_v2_receipts(root, data, path))
            owner = summary["owner_applicability"]["lab-ignored"]
            self.assertEqual((owner["compiled_count"], owner["required_count"], owner["not_applicable_count"]), (44, 16, 28))
            self.assertEqual(sum(p["required_count"] for p in owner["partitions"]), 16)
            self.assertEqual(summary["owner_inventory"]["lab-ignored"]["test_count"], 44)
            self.assertEqual(summary["test_count"], 5 + 44 + 2)
            self.assertEqual(summary["accounting"], {"compiled_count": 51, "observed_required_count": 21,
                                                     "not_applicable_count": 28, "other_owner_count": 2})
            self.assertEqual(len(summary["applicability"]["decisions"]), 44)

    def test_schema2_all_required_all_na_and_one_required_per_partition(self):
        for required in ((), ("f_ignored", "g_ignored"), ("f_ignored", "g_ignored", "h_ignored", "i_ignored")):
            with self.subTest(required=required), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                data, path = self.fixture_v2(root, required=required)
                summary = self.verify_v2(path, self.fixture_v2_receipts(root, data, path))
                owner = summary["owner_applicability"]["lab-ignored"]
                self.assertEqual(owner["observed_required_count"], len(required))
                self.assertEqual(owner["not_applicable_count"], 4 - len(required))
                self.assertEqual(summary["owner_inventory"]["lab-ignored"]["test_count"], 4)
                self.assertFalse(summary["aggregate_pass"])

    def test_schema2_exact_map_rejects_malformed_missing_duplicate_and_unknown_ids(self):
        defects = ("missing", "duplicate", "unknown", "wrong-owner", "schema", "required-int", "reason-empty",
                   "reason-control", "reason-unicode", "reason-long", "reason-untrimmed", "extra-field", "identity-type")
        for defect in defects:
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as temporary:
                data, _ = self.fixture_v2(Path(temporary))
                applicability = data["applicability"]
                decisions = applicability["decisions"]
                if defect == "missing":
                    decisions.pop()
                elif defect == "duplicate":
                    decisions[-1] = decisions[0]
                elif defect == "unknown":
                    decisions[0]["name"] = "unknown_compiled_test"
                elif defect == "wrong-owner":
                    applicability["owner"] = "lab-normal"
                elif defect == "schema":
                    applicability["schema_version"] = 1
                elif defect == "required-int":
                    decisions[0]["required"] = 1
                elif defect.startswith("reason-"):
                    decisions[0]["reason"] = {"reason-empty": "", "reason-control": "a\nb", "reason-unicode": "\u00e9",
                                               "reason-long": "x" * 1025, "reason-untrimmed": " reason "}[defect]
                elif defect == "extra-field":
                    decisions[0]["silent_waiver"] = True
                else:
                    decisions[0]["binary_id"] = []
                data["applicability_digest"] = archive.object_digest(applicability)
                with self.assertRaises(archive.ArchiveError):
                    archive.validate_plan(data)

    def test_schema2_source_reason_inventory_and_compiled_count_tampering_rejected(self):
        for defect in ("reason", "source", "head", "tree", "manifest", "inventory", "owner-count", "owner-digest", "plan-inventory"):
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as temporary:
                data, _ = self.fixture_v2(Path(temporary))
                applicability = data["applicability"]
                if defect == "reason":
                    applicability["decisions"][0]["reason"] = "Changed after review"
                elif defect == "source":
                    applicability["identity"]["inputs"]["input_digest"] = "0" * 64
                elif defect in ("head", "tree"):
                    applicability["identity"][defect + "_sha"] = "0" * 40
                elif defect == "plan-inventory":
                    data["test_inventory_digest"] = "0" * 64
                else:
                    field = {"manifest": "manifest_sha256", "inventory": "test_inventory_digest",
                             "owner-count": "compiled_owner_count", "owner-digest": "compiled_owner_digest"}[defect]
                    applicability[field] = 3 if defect == "owner-count" else "0" * 64
                if defect != "reason":
                    data["applicability_digest"] = archive.object_digest(applicability)
                with self.assertRaises(archive.ArchiveError):
                    archive.validate_plan(data)

    def test_schema2_independent_frozen_hash_rejects_self_consistent_late_waiver(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            data, path = self.fixture_v2(root)
            frozen = archive.digest(path)
            folders = self.fixture_v2_receipts(root, data, path)
            data["applicability"]["decisions"][0].update(required=False, reason="Late omission after required failure")
            data["applicability_digest"] = archive.object_digest(data["applicability"])
            archive.validate_plan(data)
            archive.write(path, data)
            with self.assertRaisesRegex(archive.ArchiveError, "frozen plan digest"):
                self.verify_v2(path, folders, frozen=frozen)
            with self.assertRaisesRegex(archive.ArchiveError, "frozen plan SHA"):
                self.verify_v2(path, folders, frozen="")

    def test_schema2_rejects_whole_owner_override_and_v1_receipt(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            data, path = self.fixture_v2(root)
            folders = self.fixture_v2_receipts(root, data, path)
            with self.assertRaisesRegex(archive.ArchiveError, "whole-owner override"):
                self.verify_v2(path, folders, not_applicable_owner=["lab-ignored"])
            receipt_path = Path(folders[0]) / "receipt.json"
            receipt = archive.load(receipt_path)
            receipt["schema_version"] = 1
            archive.write(receipt_path, receipt)
            with self.assertRaisesRegex(archive.ArchiveError, "receipt applicability"):
                self.verify_v2(path, folders)

    def test_schema2_required_receipt_cannot_be_missing_failed_forged_or_duplicated(self):
        for defect in ("missing", "duplicate", "failed", "required-digest", "applicability-digest", "observed", "junit-failure", "na-junit", "reason-plan"):
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                data, path = self.fixture_v2(root)
                folders = self.fixture_v2_receipts(root, data, path)
                required_folder = next(p for p in folders if "lab-ignored-" in p)
                receipt_path = Path(required_folder) / "receipt.json"
                receipt = archive.load(receipt_path)
                frozen = archive.digest(path)
                if defect == "missing":
                    folders.remove(required_folder)
                elif defect == "duplicate":
                    folders.append(required_folder)
                elif defect == "reason-plan":
                    data["applicability"]["decisions"][0]["reason"] = "Changed rationale after execution"
                    data["applicability_digest"] = archive.object_digest(data["applicability"])
                    archive.write(path, data)
                elif defect in ("junit-failure", "na-junit"):
                    junit = Path(required_folder) / "junit.xml"
                    content = junit.read_text()
                    content = content.replace('/>', '><failure/></testcase>', 1) if defect == "junit-failure" else content.replace("f_ignored", "g_ignored")
                    junit.write_text(content)
                    receipt["junit_sha256"] = archive.digest(junit)
                    archive.write(receipt_path, receipt)
                else:
                    field = {"failed": "exit_code", "required-digest": "required_digest", "applicability-digest": "applicability_digest", "observed": "observed"}[defect]
                    receipt[field] = 100 if defect == "failed" else [] if defect == "observed" else "0" * 64
                    archive.write(receipt_path, receipt)
                with self.assertRaises(archive.ArchiveError):
                    self.verify_v2(path, folders, frozen=frozen)

    def test_schema2_zero_required_run_never_calls_nextest(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _, path = self.fixture_v2(root, required=())
            args = type("Args", (), {"plan": path, "shard": "lab-ignored-0", "frozen_plan_sha256": archive.digest(path)})
            with patch.object(archive.subprocess, "run") as execute, self.assertRaisesRegex(archive.ArchiveError, "no required tests"):
                archive.run(args)
            execute.assert_not_called()

    def test_schema2_run_selects_only_required_ids_and_binds_receipt(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            data, path = self.fixture_v2(root, required=("f_ignored",))
            junit = root / "actual-synthetic-junit.xml"
            args = type("Args", (), {"plan": path, "shard": "lab-ignored-0", "frozen_plan_sha256": archive.digest(path),
                                    "bundle": str(root / "bundle"), "extracted": str(root / "extracted"),
                                    "output": str(root / "executed"), "nextest": "synthetic-nextest"})
            def invoke(command, **kwargs):
                expression = command[command.index("-E") + 1]
                self.assertIn("f_ignored", expression)
                self.assertNotIn("h_ignored", expression)
                junit.write_text('<testsuites><testsuite name="sentinel-bio"><testcase name="f_ignored"/></testsuite></testsuites>')
                return type("Result", (), {"returncode": 0})
            samefile = archive.os.path.samefile
            with patch.object(archive, "Path", side_effect=lambda p: junit if str(p) == "/workspace/target/nextest/ci/junit.xml" else Path(p)), \
                    patch.object(archive, "check_bundle", return_value=archive.load(root / "bundle/manifest.json")), \
                    patch.object(archive.os.path, "samefile", side_effect=lambda a, b: True if a in ("/workspace", "/target/current") else samefile(a, b)), \
                    patch.object(archive, "nextest", return_value=["synthetic-nextest", "nextest"]), \
                    patch.object(archive.subprocess, "check_output", return_value=json.dumps(listing(["f_ignored"]))), \
                    patch.object(archive.subprocess, "run", side_effect=invoke):
                archive.run(args)
            receipt = archive.load(root / "executed/receipt.json")
            self.assertEqual(receipt["schema_version"], 2)
            self.assertEqual((receipt["expected_count"], receipt["observed_count"]), (1, 1))
            self.assertEqual(receipt["applicability_digest"], data["applicability_digest"])
            required = archive.required_tests(data, next(s for s in data["shards"] if s["id"] == "lab-ignored-0"))
            self.assertEqual(receipt["required_digest"], archive.object_digest(required))

    def test_schema2_cli_plan_requires_explicit_applicability_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            data, _ = self.fixture_v2(root)
            archive.write(root / "applicability.json", data["applicability"])
            args = type("Args", (), {"bundle": str(root / "bundle"), "extracted": str(root / "extracted"),
                                    "output": str(root / "from-map.json"), "shards": 2, "applicability": str(root / "applicability.json")})
            with patch.object(archive, "check_bundle", return_value=archive.load(root / "bundle/manifest.json")):
                archive.plan(args)
            self.assertEqual(archive.load(args.output), data)

    def test_schema2_rejects_changed_compiled_extraction_before_execution(self):
        for defect in ("inventory", "identity", "missing-frozen-hash"):
            with self.subTest(defect=defect), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                data, path = self.fixture_v2(root)
                manifest = archive.load(root / "bundle/manifest.json")
                if defect == "inventory":
                    marker = archive.load(root / "extracted/extracted.json")
                    marker["test_inventory_digest"] = "0" * 64
                    archive.write(root / "extracted/extracted.json", marker)
                elif defect == "identity":
                    manifest["head_sha"] = "0" * 40
                args = type("Args", (), {"plan": path, "shard": "lab-ignored-0", "bundle": str(root / "bundle"),
                                        "extracted": str(root / "extracted"),
                                        "frozen_plan_sha256": None if defect == "missing-frozen-hash" else archive.digest(path)})
                with patch.object(archive, "check_bundle", return_value=manifest), \
                        patch.object(archive.subprocess, "run") as execute, self.assertRaises(archive.ArchiveError):
                    archive.run(args)
                execute.assert_not_called()

    def test_schema2_numeric_fields_do_not_accept_booleans_or_floats(self):
        for field, value in (("schema_version", 2.0), ("test_count", 9.0), ("partition", False), ("expected_count", 3.0)):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as temporary:
                data, _ = self.fixture_v2(Path(temporary))
                if field in ("schema_version", "test_count"):
                    data[field] = value
                else:
                    data["shards"][0][field] = value
                with self.assertRaises(archive.ArchiveError):
                    archive.validate_plan(data)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            data, path = self.fixture_v2(root)
            folders = self.fixture_v2_receipts(root, data, path)
            receipt_path = Path(folders[0]) / "receipt.json"
            receipt = archive.load(receipt_path)
            receipt["exit_code"] = False
            archive.write(receipt_path, receipt)
            with self.assertRaisesRegex(archive.ArchiveError, "numeric"):
                self.verify_v2(path, folders)

    def test_schema2_rejects_reassigned_original_partition(self):
        for owner in ("lab-normal", "lab-ignored"):
            with self.subTest(owner=owner), tempfile.TemporaryDirectory() as temporary:
                data, _ = self.fixture_v2(Path(temporary))
                first, second = [s for s in data["shards"] if s["owner"] == owner]
                first["tests"][0], second["tests"][0] = second["tests"][0], first["tests"][0]
                with self.assertRaisesRegex(archive.ArchiveError, "round-robin"):
                    archive.validate_plan(data)

    def test_schema2_plan_mutation_during_verification_cannot_publish_summary(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            data, path = self.fixture_v2(root)
            folders = self.fixture_v2_receipts(root, data, path)
            original = archive.junit_tests
            calls = 0
            def observe(junit):
                nonlocal calls
                result = original(junit)
                calls += 1
                if calls == len(folders):
                    path.write_text(path.read_text() + " ")
                return result
            with patch.object(archive, "junit_tests", side_effect=observe), self.assertRaisesRegex(archive.ArchiveError, "frozen plan digest"):
                self.verify_v2(path, folders)


if __name__ == "__main__":
    unittest.main()
