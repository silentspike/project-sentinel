"""Local mechanism checks only; not deployment or kernel-isolation evidence."""

from __future__ import annotations

import contextlib
import ast
import hashlib
import io
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest
from unittest import mock

from qa_broker_fixture import QaBrokerFixture, TOKEN, event, frame, load_test_runner


REPO = Path(__file__).resolve().parents[2]
RUNNER = Path(os.environ.get("SENTINEL_QA_TEST_RUNNER", REPO / "deploy/scripts/coding-qa-v1.py"))
TEST_LOADER = Path(__file__).with_name("qa_broker_fixture.py")
TEMP_ROOT = Path(os.environ.get("RUNNER_TEMP", "/work/tmp"))
PYTHON_FILES = {
    "lib/__init__.py": "",
    "lib/math_ops.py": "def add(a, b):\n    return a + b\n",
    "tests/__init__.py": "",
    "tests/test_math.py": (
        "import unittest\nfrom lib.math_ops import add\n"
        "class Addition(unittest.TestCase):\n"
        "    def test_add(self):\n        print('private test log')\n"
        "        self.assertEqual(add(2, 3), 5)\n"
    ),
    "data/config.json": '{"value": 5}\n',
    "app.py": "from lib.math_ops import add\nprint(add(2, 3))\n",
    "sentinel-qa.json": json.dumps({"schema_version": 1, "cases": [{
        "id": "addition", "script": "app.py", "args": [], "stdin": "",
        "expected_stdout": "5\n", "expected_stderr": "", "expected_exit": 0,
    }]}),
}
NODE_FILES = {
    "package.json": '{"type":"module"}\n',
    "lib/math.mjs": "export const add = (a, b) => a + b;\n",
    "test/math.test.mjs": (
        "import test from 'node:test';\nimport assert from 'node:assert/strict';\n"
        "import { add } from '../lib/math.mjs';\n"
        "test('addition', () => assert.equal(add(2, 3), 5));\n"
    ),
    "data/config.json": '{"value": 5}\n',
    "app.mjs": "import { add } from './lib/math.mjs';\nconsole.log(add(2, 3));\n",
    "sentinel-qa.json": json.dumps({"schema_version": 1, "cases": [{
        "id": "addition", "script": "app.mjs", "args": [], "stdin": "",
        "expected_stdout": "5\n", "expected_stderr": "", "expected_exit": 0,
    }]}),
}


def load_runner(broker_socket):
    return load_test_runner(RUNNER, broker_socket)


def expected_input_inventory(files, ensure_ascii=False):
    pairs = [[relative, hashlib.sha256(content if isinstance(content, bytes) else content.encode("utf-8")).hexdigest()]
             for relative, content in sorted(files.items())]
    encoded = json.dumps(pairs, separators=(",", ":"), ensure_ascii=ensure_ascii).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


class CodingQaTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="sentinel-coding-qa-test-", dir=TEMP_ROOT)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.workspace = self.root / "qa"
        self.workspace.mkdir()
        self.broker = QaBrokerFixture(self.root)
        self.addCleanup(self.broker.close)
        self.module = load_runner(self.broker.path)
        token = mock.patch.dict(os.environ, {"SENTINEL_QA_BROKER_TOKEN": TOKEN})
        token.start()
        self.addCleanup(token.stop)

    def stage(self, files, artifact="candidate", real_contract=False):
        base = (self.root / ".inputs/project/work" if real_contract
                else self.root / "inputs" / artifact)
        paths = []
        for relative, content in files.items():
            path = base / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content if isinstance(content, bytes) else content.encode())
            path.chmod(0o444)
            paths.append(str(path))
        return paths

    def invoke(self, family, paths):
        result = subprocess.run([sys.executable, "-I", "-B", str(TEST_LOADER), "--invoke",
                                 str(RUNNER), str(self.broker.path), TOKEN, family, *paths],
                                cwd=self.workspace, capture_output=True, timeout=30)
        self.assertEqual(result.stderr, b"")
        payload = json.loads(result.stdout)
        self.assertEqual(result.returncode, {"pass": 0, "fail": 1, "error": 2}[payload["outcome"]])
        self.assertLess(len(result.stdout), 1024)
        self.assertNotIn(str(self.root), result.stdout.decode())
        self.assertNotIn("private test log", result.stdout.decode())
        self.assertEqual(list(self.workspace.iterdir()), [])
        return payload

    def mechanism(self, family, paths):
        output = io.StringIO()
        previous = Path.cwd()
        try:
            os.chdir(self.workspace)
            with contextlib.redirect_stdout(output):
                code = self.module.main([family, *paths])
        finally:
            os.chdir(previous)
        return code, json.loads(output.getvalue())

    def test_multifile_python_local_imports_and_deterministic_inventory(self):
        paths = self.stage(PYTHON_FILES)
        before = {path: Path(path).read_bytes() for path in paths}
        first = self.invoke("python-project-v1", paths)
        self.assertEqual(first, self.invoke("python-project-v1", list(reversed(paths))))
        self.assertEqual(first["outcome"], "pass")
        self.assertEqual(first["tests"], 1)
        self.assertEqual(first["syntax_files"], 5)
        self.assertEqual(first["files"], len(PYTHON_FILES))
        self.assertEqual(first["input_inventory_sha256"], expected_input_inventory(PYTHON_FILES))
        progress = first["native_status"]
        self.assertEqual(progress["outcome"], "pass")
        self.assertEqual(progress["inventory"], {"outcome": "pass", "planned": 7, "completed": 7})
        self.assertEqual(progress["syntax"], {"outcome": "pass", "planned": 5, "completed": 5})
        self.assertEqual(progress["tests"], {"outcome": "pass", "planned": 1, "completed": 1})
        self.assertEqual(before, {path: Path(path).read_bytes() for path in paths})

    def test_actual_workbench_staging_tree(self):
        result = self.invoke("python-project-v1", self.stage(PYTHON_FILES, real_contract=True))
        self.assertEqual(result["outcome"], "pass")
        self.assertEqual(result["input_inventory_sha256"], expected_input_inventory(PYTHON_FILES))

    def test_python_syntax_batches_64_or_more_sources_through_broker(self):
        # Exercise syntax directly: main's unchanged 64-input limit must also
        # reserve an input for the behavioral plan, leaving at most 63 sources.
        for count in (64, 125):
            with self.subTest(sources=count):
                root = self.workspace / f"sources-{count}"
                root.mkdir(mode=0o700)
                sources = [f"source_{index:03d}.py" for index in range(count)]
                for index, name in enumerate(sources):
                    path = root / name
                    path.write_text(f"VALUE = {index}\n", encoding="utf-8")
                    path.chmod(0o444)
                before = len(self.broker.requests)
                progress = {"outcome": "not_run", "planned": 0, "completed": 0}
                self.assertEqual(self.module.python_qa(root, time.monotonic() + 5, progress),
                                 {"syntax_files": count})
                self.assertEqual(progress["planned"], count)
                self.assertEqual(progress["completed"], count)
                requests = self.broker.requests[before:]
                expected_sizes = [61, 3] if count == 64 else [61, 61, 3]
                self.assertEqual([len(request["args"]) - 4 for request in requests], expected_sizes)
                self.assertEqual([name for request in requests for name in request["args"][4:]], sources)
                for request in requests:
                    self.assertEqual(request["program"], "python3")
                    self.assertEqual(request["args"][:4], ["-E", "-s", "-c", self.module.PYTHON_COMPILE])
                    self.assertLessEqual(len(request["args"]), 65)
                    self.assertEqual(request["workspace"], str(root))

    def test_python_later_syntax_batch_failure_retains_observed_progress(self):
        sources = [f"source_{index:03d}.py" for index in range(64)]
        for index, name in enumerate(sources):
            path = self.workspace / name
            path.write_text("def broken(\n" if index == 62 else f"VALUE = {index}\n", encoding="utf-8")
            path.chmod(0o444)
        progress = {"outcome": "not_run", "planned": 0, "completed": 0}
        with self.assertRaises(self.module.QaError) as caught:
            self.module.python_qa(self.workspace, time.monotonic() + 5, progress)
        self.assertEqual(caught.exception.code, "python_compile_failed")
        self.assertEqual(progress["planned"], 64)
        self.assertEqual(progress["completed"], 63)
        self.assertEqual([len(request["args"]) - 4 for request in self.broker.requests], [61, 3])

    def test_full_python_qa_batches_within_64_declared_input_limit(self):
        files = {f"source_{index:03d}.py": f"VALUE = {index}\n" for index in range(61)}
        files.update({"app.py": "print(5)\n", "sentinel-qa.json": PYTHON_FILES["sentinel-qa.json"],
                      "test_smoke.py": "import unittest\nclass Smoke(unittest.TestCase):\n    def test_value(self):\n        self.assertEqual(2 + 3, 5)\n"})
        self.assertEqual(len(files), 64)
        result = self.invoke("python-project-v1", self.stage(files))
        self.assertEqual(result["outcome"], "pass", result)
        self.assertEqual(result["syntax_files"], 63)
        self.assertEqual(result["input_inventory_sha256"], expected_input_inventory(files))
        self.assertEqual(result["native_status"]["syntax"], {"outcome": "pass", "planned": 63, "completed": 63})
        syntax_requests = [request for request in self.broker.requests if request["args"][:3] == ["-E", "-s", "-c"]]
        self.assertEqual([len(request["args"]) - 4 for request in syntax_requests], [61, 2])
        self.assertTrue(all(len(request["args"]) <= 65 for request in self.broker.requests))

    def test_multifile_node_with_actual_tests_and_package_json(self):
        result = self.invoke("node-project-v1", self.stage(NODE_FILES))
        self.assertEqual(result["outcome"], "pass")
        self.assertEqual(result["tests"], 1)
        self.assertEqual(result["syntax_files"], 3)
        self.assertEqual(result["files"], len(NODE_FILES))
        self.assertEqual(result["input_inventory_sha256"], expected_input_inventory(NODE_FILES))

    def test_fixture_resolves_hosted_interpreter_without_changing_production_path(self):
        with mock.patch("qa_broker_fixture.shutil.which", side_effect=lambda name: {
                "node": "/opt/hostedtoolcache/node/bin/node",
                "python3": "/usr/bin/python3",
        }[name]):
            subject = load_runner(self.broker.path)
        self.assertIn("/opt/hostedtoolcache/node/bin", subject.ENVIRONMENT["PATH"].split(os.pathsep))
        self.assertIn('"PATH": "/usr/bin:/bin"', RUNNER.read_text())

    def test_input_inventory_uses_utf8_relative_paths_and_canonical_compact_pairs(self):
        files = {"data/z.bin": b"\x00\xff", "data/caf\u00e9.txt": "na\u00efve\n", "empty.txt": b""}
        paths = self.stage(files)
        first = self.invoke("--inventory-only", paths)
        self.assertEqual(first["input_inventory_sha256"], expected_input_inventory(files))
        self.assertNotEqual(first["input_inventory_sha256"], expected_input_inventory(files, ensure_ascii=True))
        self.assertEqual(first, self.invoke("--inventory-only", list(reversed(paths))))
        full_inventory = [[relative, len(content if isinstance(content, bytes) else content.encode("utf-8")),
                           hashlib.sha256(content if isinstance(content, bytes) else content.encode("utf-8")).hexdigest()]
                          for relative, content in sorted(files.items())]
        self.assertEqual(first["inventory_sha256"], hashlib.sha256(
            json.dumps(full_inventory, separators=(",", ":")).encode("utf-8")).hexdigest())

    def test_observed_source_sha_tamper_cannot_match_declared_input_inventory(self):
        paths = self.stage(PYTHON_FILES)
        declared_digest = expected_input_inventory(PYTHON_FILES)
        source = next(Path(path) for path in paths if path.endswith("/lib/math_ops.py"))
        before = source.read_bytes()
        changed = before.replace(b"return a + b", b"return a - b")
        self.assertNotEqual(hashlib.sha256(before).digest(), hashlib.sha256(changed).digest())
        self.assertEqual(len(before), len(changed))
        source.chmod(0o644)
        source.write_bytes(changed)
        source.chmod(0o444)
        result = self.invoke("python-project-v1", paths)
        observed = dict(PYTHON_FILES, **{"lib/math_ops.py": changed})
        self.assertEqual(result["input_inventory_sha256"], expected_input_inventory(observed))
        self.assertNotEqual(result["input_inventory_sha256"], declared_digest)
        self.assertEqual(result["files"], len(PYTHON_FILES))
        self.assertEqual(result["bytes"], sum(len(content.encode("utf-8")) for content in PYTHON_FILES.values()))
        self.assertEqual(result["outcome"], "fail")
        self.assertEqual(source.read_bytes(), changed)

    def test_input_inventory_binds_relative_paths_not_only_content_hashes(self):
        first = {"data/first.bin": b"same source"}
        second = {"data/second.bin": b"same source"}
        result_a = self.invoke("--inventory-only", self.stage(first, "first"))
        result_b = self.invoke("--inventory-only", self.stage(second, "second"))
        self.assertEqual(result_a["input_inventory_sha256"], expected_input_inventory(first))
        self.assertEqual(result_b["input_inventory_sha256"], expected_input_inventory(second))
        self.assertNotEqual(result_a["input_inventory_sha256"], result_b["input_inventory_sha256"])

    def test_intentional_test_failure_for_both_families(self):
        for family, files, path, old, new in [
            ("python", PYTHON_FILES, "tests/test_math.py", "5)", "6)"),
            ("node", NODE_FILES, "test/math.test.mjs", "5)", "6)"),
        ]:
            with self.subTest(family=family):
                candidate = dict(files)
                candidate[path] = candidate[path].replace(old, new)
                result = self.invoke(family + "-project-v1", self.stage(candidate, family))
                self.assertEqual(result["code"], "tests_failed")

    def test_no_tests_is_not_a_pass(self):
        for family, files in [
            ("python", {"app.py": "print('not a test')\n"}),
            ("node", {"app.js": "console.log('not a test');\n"}),
            ("node", {"empty.test.js": "console.log('not a test');\n"}),
        ]:
            with self.subTest(files=files):
                result = self.invoke(family + "-project-v1", self.stage(files, next(iter(files))))
                self.assertEqual(result["code"], "tests_missing")

    def test_all_skipped_tests_are_not_a_pass(self):
        candidates = [
            ("python", {"test_skip.py": "import unittest\n@unittest.skip('skip')\n"
                        "class Skip(unittest.TestCase):\n    def test_skip(self): pass\n"}),
            ("node", {"skip.test.js": "require('node:test')('skip', {skip:true}, () => {});\n"}),
        ]
        for family, files in candidates:
            result = self.invoke(family + "-project-v1", self.stage(files, family))
            self.assertEqual(result["code"], "tests_missing")

    def test_node_wrapper_cannot_mask_skipped_tests_or_empty_suite(self):
        for index, source in enumerate([
            "require('node:test')('skip', {skip:true}, () => {});\n",
            "require('node:test').describe('empty suite', () => {});\n",
            "require('node:test')('todo', {todo:true}, () => {});\n",
        ]):
            paths = self.stage({"empty.test.js": "", "other.test.js": source}, f"case{index}")
            result = self.invoke("node-project-v1", paths)
            self.assertEqual(result["code"], "tests_missing")

    def test_node_nested_actual_tests_and_console_logs(self):
        files = dict(NODE_FILES)
        files["nested.test.cjs"] = (
            "const {describe, it} = require('node:test');\n"
            "describe('suite', () => {\n"
            "  it('works', () => { console.log('private test log'); });\n"
            "  it.skip('skip', () => {});\n});\n"
        )
        paths = self.stage(files)
        result = self.invoke("node-project-v1", paths)
        self.assertEqual(result["outcome"], "pass")
        self.assertEqual(result["tests"], 1)
        self.assertEqual(result["skipped"], 0)

    def test_compile_does_not_import_candidate_stdlib_shadow(self):
        files = dict(PYTHON_FILES, **{"pathlib.py": "raise RuntimeError('must not execute at compile')\n"})
        result = self.invoke("python-project-v1", self.stage(files))
        self.assertEqual(result["outcome"], "pass")

    def test_maximum_declared_file_count_is_accepted(self):
        files = dict(PYTHON_FILES)
        files.update({f"data/file{number}.txt": "data\n" for number in range(64 - len(files))})
        result = self.invoke("python-project-v1", self.stage(files))
        self.assertEqual(result["outcome"], "pass")
        self.assertEqual(result["files"], 64)

    def test_real_syntax_failure(self):
        for family, files, code in [
            ("python", {"broken.py": "def broken(\n"}, "python_compile_failed"),
            ("node", {"broken.js": "const broken = ;\n"}, "node_syntax_failed"),
        ]:
            candidate = dict(PYTHON_FILES if family == "python" else NODE_FILES)
            candidate.update(files)
            result = self.invoke(family + "-project-v1", self.stage(candidate, family))
            self.assertEqual(result["code"], code)

    def test_behavioral_assertions_are_executed_by_the_parent_not_candidate_reports(self):
        for family, template, script, program in [
            ("python", PYTHON_FILES, "app.py", "import sys\nprint(sys.stdin.read().upper(), end='')\n"),
            ("node", NODE_FILES, "app.mjs", "import fs from 'node:fs';\nprocess.stdout.write(fs.readFileSync(0, 'utf8').toUpperCase());\n"),
        ]:
            candidate = dict(template)
            candidate[script] = program
            cases = [{"id": f"case-{index}", "script": script, "args": [], "stdin": text,
                      "expected_stdout": text.upper(), "expected_stderr": "", "expected_exit": 0}
                     for index, text in enumerate(["alpha\\nbeta", "mixed case\n"])]
            candidate["sentinel-qa.json"] = json.dumps({"schema_version": 1, "cases": cases})
            result = self.invoke(family + "-project-v1", self.stage(candidate, family))
            self.assertEqual(result["outcome"], "pass")
            self.assertEqual(result["tests"], 2)
            cases[0]["expected_stdout"] = "wrong expectation"
            candidate["sentinel-qa.json"] = json.dumps({"schema_version": 1, "cases": cases})
            result = self.invoke(family + "-project-v1", self.stage(candidate, family + "-bad"))
            self.assertEqual(result["code"], "behavioral_assertion_failed")

    def test_behavioral_plan_rejects_empty_cross_family_and_ambiguous_cases(self):
        valid = json.loads(PYTHON_FILES["sentinel-qa.json"])
        for index, plan in enumerate([
            {"schema_version": 1, "cases": []},
            {"schema_version": 1, "cases": valid["cases"] * 2},
            {"schema_version": 1, "cases": [dict(valid["cases"][0], script="../app.py")]},
            {"schema_version": 1, "cases": [dict(valid["cases"][0], script="app.js")]},
            {"schema_version": 1, "cases": [dict(valid["cases"][0], expected_stdout="")]},
            {"schema_version": 1, "cases": [dict(valid["cases"][0], expected_exit=False)]},
        ]):
            files = dict(PYTHON_FILES, **{"sentinel-qa.json": json.dumps(plan)})
            result = self.invoke("python-project-v1", self.stage(files, f"invalid{index}"))
            self.assertEqual(result["code"], "test_plan_invalid")

    def test_failure_outcome_has_no_candidate_content(self):
        paths = self.stage({"test_private.py": "raise RuntimeError('secret-customer-data')\n"})
        result = self.invoke("python-project-v1", paths)
        self.assertEqual(result["outcome"], "error")
        self.assertNotIn("secret-customer-data", json.dumps(result))

    def test_malformed_family_and_argument_limits(self):
        paths = self.stage({"app.py": "pass\n"})
        for family in ["web-project-v1", "Python-project-v1", "../python-project-v1", "--help"]:
            self.assertEqual(self.invoke(family, paths)["code"], "family_denied")
        for inputs in [[], paths * 65]:
            self.assertEqual(self.invoke("python-project-v1", inputs)["code"], "arguments_denied")

    def test_writable_input_rejected(self):
        paths = self.stage({"app.py": "pass\n"})
        Path(paths[0]).chmod(0o644)
        self.assertEqual(self.invoke("python-project-v1", paths)["code"], "input_file_contract")

    def test_symlink_file_and_ancestor_rejected(self):
        paths = self.stage({"app.py": "pass\n"})
        alias = Path(paths[0]).with_name("alias.py")
        alias.symlink_to(paths[0])
        self.assertNotEqual(self.invoke("python-project-v1", [str(alias)])["outcome"], "pass")
        directory_alias = self.root / "inputs/alias"
        directory_alias.symlink_to(Path(paths[0]).parent, target_is_directory=True)
        self.assertNotEqual(self.invoke("python-project-v1", [str(directory_alias / "app.py")])["outcome"], "pass")

    def test_hardlink_rejected(self):
        paths = self.stage({"app.py": "pass\n"})
        os.link(paths[0], Path(paths[0]).with_name("alias.py"))
        self.assertEqual(self.invoke("python-project-v1", paths)["code"], "input_file_contract")

    def test_colliding_relative_files_and_file_directory_overlap(self):
        a = self.stage({"app.py": "pass\n"}, "one")
        b = self.stage({"app.py": "pass\n"}, "two")
        self.assertEqual(self.invoke("python-project-v1", a + b)["code"], "input_collision")
        self.assertEqual(self.invoke("python-project-v1", a * 2)["code"], "input_collision")
        c = self.stage({"app.py/child.py": "pass\n"}, "three")
        self.assertEqual(self.invoke("python-project-v1", a + c)["code"], "input_collision")

    def test_distinct_artifact_trees_can_be_combined_without_flattening(self):
        sources = {key: value for key, value in PYTHON_FILES.items() if not key.startswith("tests/")}
        tests = {key: value for key, value in PYTHON_FILES.items() if key.startswith("tests/")}
        result = self.invoke("python-project-v1", self.stage(sources, "source") + self.stage(tests, "tests"))
        self.assertEqual(result["outcome"], "pass")

    def test_traversal_ambiguous_markers_noncanonical_and_unstaged_paths(self):
        paths = self.stage({"app.py": "pass\n"})
        base = str(Path(paths[0]).parent)
        for value in [base + "/../candidate/app.py", base + "/./app.py", base + "//app.py",
                      "relative.py", str(self.root / "app.py"), base + "/inputs/other/app.py",
                      base + "/back\\slash.py", base + "/newline\n.py"]:
            self.assertEqual(self.invoke("python-project-v1", [value])["code"], "input_path_contract")

    def test_oversized_file_and_total_limit(self):
        paths = self.stage({"big.py": b""})
        path = Path(paths[0])
        path.chmod(0o644)
        with path.open("wb") as handle:
            handle.truncate(self.module.MAX_FILE_BYTES + 1)
        path.chmod(0o444)
        self.assertEqual(self.invoke("python-project-v1", paths)["code"], "input_file_contract")
        total_paths = self.stage({f"data{i}.bin": b"" for i in range(9)}, "total")
        for name in total_paths:
            path = Path(name)
            path.chmod(0o644)
            with path.open("wb") as handle:
                handle.truncate(self.module.MAX_FILE_BYTES)
            path.chmod(0o444)
        self.assertEqual(self.invoke("python-project-v1", total_paths)["code"], "input_total_limit")

    def test_directory_fifo_and_missing_input_fail_closed(self):
        paths = self.stage({"app.py": "pass\n"})
        fifo = Path(paths[0]).with_name("fifo.py")
        os.mkfifo(fifo, 0o444)
        for value in [str(Path(paths[0]).parent), str(fifo), str(fifo.with_name("missing.py"))]:
            self.assertNotEqual(self.invoke("python-project-v1", [value])["outcome"], "pass")

    def test_changed_file_identity_rejected_before_tools(self):
        paths = self.stage(PYTHON_FILES)
        original = self.module.open_input
        calls = 0

        def replace_on_recheck(value):
            nonlocal calls
            calls += 1
            if calls == 2:
                path = Path(value)
                content = path.read_bytes()
                path.unlink()
                path.write_bytes(content)
                path.chmod(0o444)
            return original(value)

        with mock.patch.object(self.module, "open_input", side_effect=replace_on_recheck):
            _, result = self.mechanism("python-project-v1", paths)
        self.assertEqual(result["code"], "input_changed")

    def test_unavailable_broker_is_error(self):
        paths = self.stage(PYTHON_FILES)
        with mock.patch.object(self.module, "BROKER_SOCKET", str(self.root / "unavailable.sock")):
            code, result = self.mechanism("python-project-v1", paths)
        self.assertEqual(code, 2)
        self.assertEqual(result["code"], "io_or_tool_error")
        self.assertEqual(result["native_status"]["syntax"],
                         {"outcome": "error", "planned": 5, "completed": 0})
        self.assertEqual(result["native_status"]["tests"]["outcome"], "not_run")

    def test_duplicate_plan_keys_fail_before_any_tools(self):
        valid = PYTHON_FILES["sentinel-qa.json"]
        fixtures = [
            valid.replace('"schema_version": 1', '"schema_version": 1, "schema_version": 1'),
            valid.replace('"cases": [', '"cases": [], "cases": ['),
            valid.replace('"expected_exit": 0', '"expected_exit": 1, "expected_exit": 0'),
            valid.replace('"script": "app.py"', '"script": "bad.py", "script": "app.py"'),
        ]
        for index, plan in enumerate(fixtures):
            with self.subTest(index=index):
                files = dict(PYTHON_FILES, **{"sentinel-qa.json": plan})
                with mock.patch.object(self.module, "run_tool") as tools:
                    code, result = self.mechanism("python-project-v1", self.stage(files, f"duplicate{index}"))
                tools.assert_not_called()
                self.assertEqual(code, 2)
                self.assertEqual(result["code"], "test_plan_invalid")
                self.assertEqual(result["native_status"]["syntax"]["outcome"], "not_run")
                self.assertEqual(result["native_status"]["tests"],
                                 {"outcome": "error", "planned": 0, "completed": 0})

    def test_missing_interpreter_preflight_is_not_an_assertion_failure(self):
        with mock.patch.object(self.module.shutil, "which", return_value=None), \
                mock.patch.object(self.module, "run_tool") as tools:
            code, result = self.mechanism("python-project-v1", self.stage(PYTHON_FILES))
        tools.assert_not_called()
        self.assertEqual(code, 2)
        self.assertEqual(result["native_status"]["syntax"],
                         {"outcome": "error", "planned": 0, "completed": 0})
        self.assertEqual(result["native_status"]["tests"]["outcome"], "not_run")

    def test_missing_and_invalid_plans_never_run_tools(self):
        for index, content in enumerate([None, "{", '{"schema_version":1,"cases":[]}']):
            files = dict(PYTHON_FILES)
            if content is None:
                del files["sentinel-qa.json"]
            else:
                files["sentinel-qa.json"] = content
            with mock.patch.object(self.module, "run_tool") as tools:
                code, result = self.mechanism("python-project-v1", self.stage(files, f"missing{index}"))
            tools.assert_not_called()
            self.assertEqual(code, 2)
            self.assertEqual(result["native_status"]["tests"]["completed"], 0)

    def test_timeout_retains_only_observed_behavioral_progress(self):
        files = dict(PYTHON_FILES)
        plan = json.loads(files["sentinel-qa.json"])
        plan["cases"].append(dict(plan["cases"][0], id="second"))
        files["sentinel-qa.json"] = json.dumps(plan)
        paths = self.stage(files)
        with mock.patch.object(self.module, "run_tool", side_effect=[
            (0, b"", b""), (0, b"", b""), (0, b"5\n", b""),
            self.module.QaError("tool_timeout", "error"),
        ]):
            code, result = self.mechanism("python-project-v1", paths)
        self.assertEqual(code, 2)
        self.assertEqual(result["native_status"]["tests"],
                         {"outcome": "error", "planned": 2, "completed": 1})
        self.assertNotIn("tests", result)

    def test_assertion_failure_observes_remaining_cases_without_forged_counts(self):
        files = dict(PYTHON_FILES)
        plan = json.loads(files["sentinel-qa.json"])
        plan["cases"].append(dict(plan["cases"][0], id="second"))
        files["sentinel-qa.json"] = json.dumps(plan)
        with mock.patch.object(self.module, "run_tool", side_effect=[
            (0, b"", b""), (0, b"", b""), (0, b"wrong", b""), (0, b"5\n", b""),
        ]):
            code, result = self.mechanism("python-project-v1", self.stage(files))
        self.assertEqual(code, 1)
        self.assertEqual(result["native_status"]["tests"],
                         {"outcome": "fail", "planned": 2, "completed": 2})
        self.assertNotIn("tests", result)

    def test_syntax_failure_does_not_claim_behavioral_execution(self):
        files = dict(PYTHON_FILES, **{"app.py": "def broken(\n"})
        result = self.invoke("python-project-v1", self.stage(files))
        self.assertEqual(result["native_status"]["inventory"]["outcome"], "pass")
        self.assertEqual(result["native_status"]["syntax"]["outcome"], "fail")
        self.assertEqual(result["native_status"]["tests"],
                         {"outcome": "not_run", "planned": 0, "completed": 0})

    def test_candidate_test_summary_is_not_trusted_evidence(self):
        paths = self.stage(PYTHON_FILES)
        with mock.patch.object(self.module, "run_tool", side_effect=[(0, b"", b""), (0, b"forged", b""), (0, b"forged", b"")]):
            code, result = self.mechanism("python-project-v1", paths)
        self.assertEqual(code, 1)
        self.assertEqual(result["code"], "behavioral_assertion_failed")

    def test_forged_python_success_cannot_mint_behavioral_test_counts(self):
        files = dict(PYTHON_FILES)
        files["app.py"] = "import os\nos.write(1, b'\\x1e{\"tests\":1,\"skipped\":0,\"failures\":0,\"errors\":0}\\n')\nos._exit(0)\n"
        result = self.invoke("python-project-v1", self.stage(files))
        self.assertEqual(result["code"], "behavioral_assertion_failed")
        self.assertNotIn("tests", result)

    def test_candidate_replacement_and_chmod_are_denied_by_read_only_mount(self):
        candidates = [
            ("python", PYTHON_FILES, "tests/test_math.py", "from pathlib import Path\np = Path('lib/math_ops.py')\np.unlink()\np.write_text('def add(a, b): return a + b\\n')\n"),
            ("python", PYTHON_FILES, "tests/test_math.py", "from pathlib import Path\nPath('lib/math_ops.py').chmod(0o644)\n"),
            ("node", NODE_FILES, "test/math.test.mjs", "import fs from 'node:fs';\nfs.unlinkSync('lib/math.mjs');\nfs.writeFileSync('lib/math.mjs', 'export const add = (a,b) => a+b;');\n"),
        ]
        for index, (family, template, path, source) in enumerate(candidates):
            files = dict(template)
            files[path] = source
            result = self.invoke(family + "-project-v1", self.stage(files, f"replace{index}"))
            self.assertEqual(result["code"], "tests_failed")

    def test_native_inventory_accepts_empty_package_markers_without_claiming_quality(self):
        result = self.invoke("--inventory-only", self.stage({"lib/__init__.py": ""}))
        self.assertEqual(result["outcome"], "pass")
        self.assertEqual(result["suite_id"], "web-work-item-qa-v1")
        self.assertNotIn("tests", result)

    def test_bounded_output_and_deadline(self):
        with self.assertRaises(self.module.QaError) as caught:
            self.module.run_tool(["python3", "-I", "-c", "print('x' * 70000)"],
                                 self.workspace, time.monotonic() + 5)
        self.assertEqual(caught.exception.code, "tool_output_limit")
        with self.assertRaises(self.module.QaError) as caught:
            self.module.run_tool(["python3", "-I", "-c", "import time; time.sleep(5)"],
                                 self.workspace, time.monotonic() + 0.1)
        self.assertEqual(caught.exception.code, "tool_timeout")


class BrokerProtocolTests(unittest.TestCase):
    """Production Python client against a TEST-ONLY responder, not runtime proof."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="sentinel-qa-wire-", dir=TEMP_ROOT)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.workspace = self.root / "candidate"
        self.workspace.mkdir(mode=0o700)
        token = mock.patch.dict(os.environ, {"SENTINEL_QA_BROKER_TOKEN": TOKEN})
        token.start()
        self.addCleanup(token.stop)

    def call(self, module, deadline=2, args=None, input_bytes=b""):
        return module.run_tool(args or ["python3", "-I", "-c", "pass"], self.workspace,
                               time.monotonic() + deadline, input_bytes)

    def reject(self, wire, code="io_or_tool_error", pause=False, deadline=1):
        def reply(connection, request, stop):
            connection.sendall(wire)
            if pause:
                stop.wait(0.5)
        with QaBrokerFixture(self.root, reply) as broker:
            module = load_runner(broker.path)
            with self.assertRaises(module.QaError) as caught:
                self.call(module, deadline=deadline)
            self.assertEqual(caught.exception.outcome, "error")
            self.assertEqual(caught.exception.code, code)
            self.assertEqual(len(broker.requests), 1)

    def test_observed_child_binary_stdin_stdout_stderr_and_exact_request(self):
        with QaBrokerFixture(self.root) as broker:
            module = load_runner(broker.path)
            program = "import sys; data=sys.stdin.buffer.read(); sys.stdout.buffer.write(data); sys.stderr.buffer.write(b'err\\x00')"
            data = b"hello\x00\xff\n"
            self.assertEqual(self.call(module, args=["python3", "-I", "-c", program], input_bytes=data),
                             (0, data, b"err\x00"))
            self.assertEqual(len(broker.requests), 1)
            request = broker.requests[0]
            self.assertEqual(request["inputBytes"], list(data))
            self.assertEqual(request["token"], TOKEN)
            self.assertEqual(request["workspace"], str(self.workspace))
            self.assertEqual(request["scratch"], str(self.root / "scratch"))
            self.assertEqual(request["stdoutBytes"], 65536)
            self.assertEqual(request["stderrBytes"], 65536)
            self.assertLessEqual(request["wallTimeMs"], 2000)

    def test_fragmented_frames_and_nonzero_exit_are_observed(self):
        wire = frame(event("ready")) + frame(event("stdout", dataHex="00ff")) + frame(event("exit", code=1, signal=None))
        def reply(connection, request, stop):
            for byte in wire:
                connection.sendall(bytes([byte]))
        with QaBrokerFixture(self.root, reply) as broker:
            module = load_runner(broker.path)
            self.assertEqual(self.call(module), (1, b"\x00\xff", b""))

    def test_malformed_payloads_and_exact_event_keys(self):
        ready = frame(event("ready"))
        fixtures = [b"{", b"\xff", b"[]", b"null", b'"ready"', b"true",
                    frame(event("unknown")), frame(event("ready", extra=True)),
                    frame({"kind": "ready"}),
                    ready + frame(event("stdout", data_hex="00")),
                    ready + frame(event("stdout", dataHex="00", extra=1)),
                    ready + frame(event("exit", code=0)),
                    ready + frame(event("exit", code=True, signal=None)),
                    ready + frame(event("exit", code=-1, signal=None)),
                    ready + frame(event("exit", code=256, signal=None)),
                    ready + frame(event("exit", code="0", signal=None)),
                    frame(event("error", code="broker_denied"))]
        for index, wire in enumerate(fixtures):
            with self.subTest(index=index):
                self.reject(frame(wire) if index < 6 else wire)

    def test_invalid_versions_are_rejected_for_every_event_kind(self):
        for kind in ("ready", "stdout", "stderr", "exit", "error"):
            for version in (0, 2, True, "1", None):
                with self.subTest(kind=kind, version=version):
                    value = event(kind)
                    value["version"] = version
                    if kind in {"stdout", "stderr"}:
                        value["dataHex"] = ""
                    if kind == "exit":
                        value.update(code=0, signal=None)
                    if kind == "error":
                        value["code"] = "tool_timeout"
                    self.reject((b"" if kind == "ready" else frame(event("ready"))) + frame(value))

    def test_broker_timeout_before_client_deadline_remains_terminal_error(self):
        ready = frame(event("ready"))
        timeout_event = frame(event("error", code="tool_timeout"))
        for prefix in (ready, ready + frame(event("stdout", dataHex="00ff"))):
            with self.subTest(partial_output=prefix != ready):
                self.reject(prefix + timeout_event, "tool_timeout", deadline=5)

    def test_timeout_event_requires_ready_exact_fields_code_and_terminal_eof(self):
        ready = frame(event("ready"))
        timeout_event = frame(event("error", code="tool_timeout"))
        fixtures = [
            timeout_event,
            ready + frame(event("error")),
            ready + frame(event("error", code="tool_timeout", extra=True)),
            ready + timeout_event + b"x",
            ready + timeout_event + timeout_event,
            ready + timeout_event + frame(event("exit", code=0, signal=None)),
            ready + frame(b'{"kind":"error","version":1,"code":"tool_timeout","code":"tool_timeout"}'),
        ]
        for code in ("broker_denied", "pass", "tests_failed", "tool_output_limit",
                     "", True, 1, None, [], {}):
            fixtures.append(ready + frame(event("error", code=code)))
        for index, wire in enumerate(fixtures):
            with self.subTest(index=index):
                self.reject(wire)

    def test_timeout_event_waits_for_terminal_eof_only_until_client_deadline(self):
        started = time.monotonic()
        self.reject(frame(event("ready")) + frame(event("error", code="tool_timeout")),
                    "tool_timeout", pause=True, deadline=0.05)
        self.assertGreaterEqual(time.monotonic() - started, 0.05)
        self.assertLess(time.monotonic() - started, 1)

    def test_ready_order_and_post_exit_bytes_are_rejected(self):
        ready, exit_event = frame(event("ready")), frame(event("exit", code=0, signal=None))
        for wire in [frame(event("stdout", dataHex="00")), frame(event("stderr", dataHex="00")),
                     exit_event, ready + ready, ready + exit_event + b"x",
                     ready + exit_event + exit_event, ready + exit_event + frame(event("stdout", dataHex="00"))]:
            with self.subTest(wire=wire):
                self.reject(wire)

    def test_hex_requires_bounded_lowercase_even_length_strings(self):
        for kind in ("stdout", "stderr"):
            for value in ("0", "FF", "gg", "00 00", "00\n", 12, None, [], "00" * 65537):
                with self.subTest(kind=kind, value_type=type(value).__name__, length=len(value) if isinstance(value, str) else None):
                    self.reject(frame(event("ready")) + frame(event(kind, dataHex=value)))

    def test_duplicate_keys_in_every_event_fail_closed(self):
        fixtures = [
            b'{"kind":"ready","kind":"ready","version":1}',
            b'{"kind":"ready","version":1,"version":1}',
            b'{"kind":"stdout","version":1,"dataHex":"00","dataHex":"00"}',
            b'{"kind":"stderr","version":1,"dataHex":"00","dataHex":"00"}',
            b'{"kind":"exit","version":1,"code":0,"code":0,"signal":null}',
            b'{"kind":"exit","version":1,"code":0,"signal":null,"signal":null}',
        ]
        for index, payload in enumerate(fixtures):
            with self.subTest(index=index):
                self.reject((b"" if index < 2 else frame(event("ready"))) + frame(payload))

    def test_zero_oversized_and_aggregate_wire_limits(self):
        self.reject(struct.pack("!I", 0), "tool_output_limit")
        self.reject(struct.pack("!I", 512 * 1024), "tool_output_limit")
        self.reject(struct.pack("!I", 0xffffffff), "tool_output_limit")
        empty = frame(event("stdout", dataHex=""))
        wire = frame(event("ready")) + empty * (512 * 1024 // len(empty) + 1)
        self.reject(wire, "tool_output_limit", deadline=2)

    def test_aggregate_stream_output_limits_are_independent(self):
        for kind in ("stdout", "stderr"):
            with self.subTest(kind=kind):
                self.reject(frame(event("ready")) + frame(event(kind, dataHex="00" * 65536))
                            + frame(event(kind, dataHex="00")), "tool_output_limit")

    def test_eof_before_complete_header_payload_ready_or_exit_is_error(self):
        for wire in (b"", b"\x00", b"\x00\x00\x00", struct.pack("!I", 8) + b"{",
                     frame(event("ready")), frame(event("ready")) + frame(event("stdout", dataHex="00"))):
            with self.subTest(wire=wire):
                self.reject(wire)

    def test_deadline_covers_idle_partial_header_and_post_exit_eof(self):
        for wire in (b"", b"\x00\x00", frame(event("ready")),
                     frame(event("ready")) + frame(event("exit", code=0, signal=None))):
            with self.subTest(wire=wire):
                started = time.monotonic()
                self.reject(wire, "tool_timeout", pause=True, deadline=0.05)
                self.assertLess(time.monotonic() - started, 1)

    def test_signal_is_not_an_assertion_exit(self):
        self.reject(frame(event("ready")) + frame(event("exit", code=None, signal=9)), "tool_terminated")

    def test_request_preflight_denies_invalid_token_program_args_and_input(self):
        with QaBrokerFixture(self.root) as broker:
            module = load_runner(broker.path)
            for value in ("", "a" * 63, "a" * 65, "A" * 64, "g" * 64):
                with self.subTest(token_length=len(value)), mock.patch.dict(os.environ, {"SENTINEL_QA_BROKER_TOKEN": value}):
                    with self.assertRaises(module.QaError) as caught:
                        self.call(module)
                    self.assertEqual(caught.exception.code, "io_or_tool_error")
            for args, input_bytes in [(["sh"], b""), (["python3"] + ["x"] * 66, b""),
                                      (["python3", "\x00"], b""), (["python3", "x" * 4097], b""),
                                      (["python3"], b"x" * 65537),
                                      (["python3"] + ["x" * 4096] * 65, b"\xff" * 65536)]:
                with self.subTest(arg_count=len(args), input_length=len(input_bytes)):
                    with self.assertRaises(module.QaError) as caught:
                        self.call(module, args=args, input_bytes=input_bytes)
                    self.assertEqual(caught.exception.code, "io_or_tool_error")
            self.assertEqual(broker.requests, [])

    def test_scratch_permissions_owner_and_symlinks_fail_before_connect(self):
        with QaBrokerFixture(self.root) as broker:
            module = load_runner(broker.path)
            scratch = self.root / "scratch"
            scratch.mkdir(mode=0o755)
            for mode in (0o755, 0o777, 0o500):
                scratch.chmod(mode)
                with self.assertRaises(module.QaError) as caught:
                    self.call(module)
                self.assertEqual(caught.exception.code, "io_or_tool_error")
            scratch.chmod(0o700)
            with mock.patch.object(module.os, "geteuid", return_value=os.geteuid() + 1):
                with self.assertRaises(module.QaError) as caught:
                    self.call(module)
                self.assertEqual(caught.exception.code, "io_or_tool_error")
            scratch.rmdir()
            scratch.symlink_to(self.workspace, target_is_directory=True)
            with self.assertRaises(module.QaError) as caught:
                self.call(module)
            self.assertEqual(caught.exception.code, "io_or_tool_error")
            self.assertEqual(broker.requests, [])

    def test_production_runner_has_fixed_socket_and_no_subprocess_fallback(self):
        tree = ast.parse(RUNNER.read_text())
        imported = {alias.name for node in ast.walk(tree) if isinstance(node, ast.Import) for alias in node.names}
        imported.update(node.module for node in ast.walk(tree) if isinstance(node, ast.ImportFrom))
        self.assertNotIn("subprocess", imported)
        self.assertNotIn("selectors", imported)
        assignments = [node for node in tree.body if isinstance(node, ast.Assign)
                       and any(isinstance(target, ast.Name) and target.id == "BROKER_SOCKET" for target in node.targets)]
        self.assertEqual(len(assignments), 1)
        self.assertEqual(ast.literal_eval(assignments[0].value), "/run/sentinel-qa-broker.sock")


class ProfileContractTests(unittest.TestCase):
    def read(self, path):
        return tomllib.loads((REPO / path).read_text())

    def test_project_families_preserve_governance_without_web_requirements(self):
        web = self.read("config/work-profiles/web-project-v1.toml")
        for language in ("python", "node"):
            profile = self.read(f"config/work-profiles/{language}-project-v1.toml")
            self.assertEqual(profile["id"], language + "-project-v1")
            self.assertEqual(profile["generation"], 1)
            for section in ("lifecycle", "customer_contract", "project", "cost", "runtime",
                            "security", "recovery", "memory", "required_artifacts", "acceptance"):
                self.assertEqual(profile[section], web[section], section)
            tools = {item["id"]: item for item in profile["tool_profiles"]}
            self.assertEqual(tools["web-authoring-v1"]["roles"], ["designer"])
            self.assertEqual(tools[language + "-coding-v1"]["roles"], ["developer"])
            self.assertEqual(tools["web-review-v1"]["tools"], ["file.inspect", "file.write", "artifact.commit"])
            self.assertEqual(tools["coding-qa-v1"]["tools"], ["file.inspect", "test.run_profile"])
            self.assertNotIn("delivery.publish_preview", tools["web-release-v1"]["tools"])
            gates = {item["id"]: item["runner"] for item in profile["quality_gates"]}
            self.assertEqual(gates["agreement_acceptance_criteria"], "web-review-v1")
            for gate in ("coding-input-integrity", "coding-syntax", "coding-tests"):
                self.assertEqual(gates[gate], "coding-qa-v1")
            self.assertFalse({"html_structure", "local_link_integrity", "browser_smoke", "static_security"} & gates.keys())
            self.assertNotIn("browser_validation", str(profile))
            self.assertTrue(all(item["immutable"] for item in profile["required_artifacts"]))

    def test_qa_profile_exact_tools_environment_and_bounded_fixed_suites(self):
        profile = self.read("config/workbench-profiles/coding-qa-v1.toml")
        web = self.read("config/workbench-profiles/web-qa-v1.toml")
        self.assertEqual(profile["capabilities"], ["file.inspect", "test.run_profile"])
        for field in ("runtime_key", "environment", "network", "output_artifact_kinds"):
            self.assertEqual(profile[field], web[field])
        ceilings = dict(web["resource_ceilings"], process_count=32)
        self.assertEqual(profile["resource_ceilings"], ceilings)
        self.assertEqual(profile["command_rules"], [
            {"program": "sentinel-coding-qa", "required_arg_prefix": [], "max_args": 65},
            {"program": "sentinel-coding-qa", "required_arg_prefix": ["--inventory-only"], "max_args": 65},
        ])
        suites = {item["id"]: item for item in profile["test_suites"]}
        self.assertEqual(set(suites), {"python-qa-v1", "node-qa-v1", "web-work-item-qa-v1"})
        for language in ("python", "node"):
            self.assertEqual(suites[language + "-qa-v1"], {
                "id": language + "-qa-v1", "program": "sentinel-coding-qa",
                "required_arg_prefix": [language + "-project-v1"], "max_args": 65})

    def test_runner_is_executable_for_provisioning(self):
        self.assertEqual(RUNNER.stat().st_mode & 0o777, 0o755)


if __name__ == "__main__":
    unittest.main()
