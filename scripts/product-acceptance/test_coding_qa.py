"""Local mechanism checks only; not deployment or kernel-isolation evidence."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest
from unittest import mock


REPO = Path(__file__).resolve().parents[2]
RUNNER = REPO / "deploy/scripts/coding-qa-v1.py"
TEMP_ROOT = Path("/work/tmp")
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
}


def load_runner():
    spec = importlib.util.spec_from_file_location("coding_qa_v1", RUNNER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CodingQaTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="sentinel-coding-qa-test-", dir=TEMP_ROOT)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.workspace = self.root / "qa"
        self.workspace.mkdir()
        self.module = load_runner()

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
        result = subprocess.run([sys.executable, "-I", str(RUNNER), family, *paths],
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
        self.assertEqual(first["syntax_files"], 4)
        self.assertEqual(first["files"], len(PYTHON_FILES))
        self.assertEqual(before, {path: Path(path).read_bytes() for path in paths})

    def test_actual_workbench_staging_tree(self):
        result = self.invoke("python-project-v1", self.stage(PYTHON_FILES, real_contract=True))
        self.assertEqual(result["outcome"], "pass")

    def test_multifile_node_with_actual_tests_and_package_json(self):
        result = self.invoke("node-project-v1", self.stage(NODE_FILES))
        self.assertEqual(result["outcome"], "pass")
        self.assertEqual(result["tests"], 1)
        self.assertEqual(result["syntax_files"], 2)
        self.assertEqual(result["files"], len(NODE_FILES))

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
        paths = self.stage({"nested.test.js": (
            "const {describe, it} = require('node:test');\n"
            "describe('suite', () => {\n"
            "  it('works', () => { console.log('private test log'); });\n"
            "  it.skip('skip', () => {});\n});\n"
        )})
        result = self.invoke("node-project-v1", paths)
        self.assertEqual(result["outcome"], "pass")
        self.assertEqual(result["tests"], 2)
        self.assertEqual(result["skipped"], 1)

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
            result = self.invoke(family + "-project-v1", self.stage(files, family))
            self.assertEqual(result["code"], code)

    def test_failure_outcome_has_no_candidate_content(self):
        paths = self.stage({"test_private.py": "raise RuntimeError('secret-customer-data')\n"})
        result = self.invoke("python-project-v1", paths)
        self.assertEqual(result["outcome"], "fail")
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
        sources = {key: value for key, value in PYTHON_FILES.items() if key.startswith("lib/")}
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

    def test_unavailable_interpreter_is_error(self):
        paths = self.stage(PYTHON_FILES)
        with mock.patch.object(self.module.subprocess, "Popen", side_effect=FileNotFoundError):
            code, result = self.mechanism("python-project-v1", paths)
        self.assertEqual(code, 2)
        self.assertEqual(result["code"], "io_or_tool_error")

    def test_invalid_test_result_is_error(self):
        paths = self.stage(PYTHON_FILES)
        with mock.patch.object(self.module, "run_tool", side_effect=[(0, b""), (0, b"forged")]):
            code, result = self.mechanism("python-project-v1", paths)
        self.assertEqual(code, 2)
        self.assertEqual(result["code"], "test_result_invalid")

    def test_bounded_output_and_deadline(self):
        with self.assertRaises(self.module.QaError) as caught:
            self.module.run_tool(["python3", "-I", "-c", "print('x' * 70000)"],
                                 self.workspace, time.monotonic() + 5)
        self.assertEqual(caught.exception.code, "tool_output_limit")
        with self.assertRaises(self.module.QaError) as caught:
            self.module.run_tool(["python3", "-I", "-c", "import time; time.sleep(5)"],
                                 self.workspace, time.monotonic() + 0.1)
        self.assertEqual(caught.exception.code, "tool_timeout")


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
        for field in ("runtime_key", "environment", "network", "resource_ceilings", "output_artifact_kinds"):
            self.assertEqual(profile[field], web[field])
        self.assertEqual(profile["command_rules"], [
            {"program": "sentinel-coding-qa", "required_arg_prefix": [], "max_args": 65},
            {"program": "sentinel-work-item-gate", "required_arg_prefix": [], "max_args": 64},
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
