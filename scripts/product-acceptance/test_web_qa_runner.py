from __future__ import annotations

import contextlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]
RUNNER = REPO_ROOT / "deploy" / "scripts" / "web-qa-v1.py"
WORK_ITEM_RUNNER = REPO_ROOT / "deploy" / "scripts" / "work-item-gate-v1.py"


def load_runner():
    spec = importlib.util.spec_from_file_location("sentinel_web_qa_v1", RUNNER)
    if spec is None or spec.loader is None:
        raise RuntimeError("web QA runner module cannot be loaded")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class WebQaRunnerTests(unittest.TestCase):
    def run_candidate(self, files: dict[str, str | bytes]) -> tuple[int, dict[str, object]]:
        module = load_runner()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative, content in files.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                if isinstance(content, bytes):
                    path.write_bytes(content)
                else:
                    path.write_text(content, encoding="utf-8")
            previous_argv = sys.argv
            output = io.StringIO()
            try:
                sys.argv = [str(RUNNER), *[str(root / path) for path in sorted(files)]]
                with contextlib.redirect_stdout(output):
                    try:
                        module.main()
                    except SystemExit as error:
                        code = int(error.code)
                    else:
                        code = 0
            finally:
                sys.argv = previous_argv
        return code, json.loads(output.getvalue())

    def test_valid_candidate_is_accepted_deterministically(self) -> None:
        candidate = {
            "index.html": (
                "<!doctype html><html><head><title>M0</title>"
                '<link href="styles.css" rel="stylesheet"></head>'
                '<body><img src="assets/logo.txt" alt="logo"></body></html>'
            ),
            "styles.css": "body { color: black; }\n",
            "assets/logo.txt": "sentinel\n",
        }
        first = self.run_candidate(candidate)
        second = self.run_candidate(candidate)
        self.assertEqual(first, second)
        self.assertEqual(first[0], 0)
        self.assertEqual(first[1]["outcome"], "pass")
        self.assertEqual(first[1]["references"], 2)

    def test_network_reference_is_rejected_without_leaking_it(self) -> None:
        code, result = self.run_candidate({
            "index.html": (
                "<!doctype html><html><head><title>M0</title></head>"
                '<body><img src="https://example.invalid/tracker"></body></html>'
            )
        })
        self.assertEqual(code, 1)
        self.assertEqual(result["outcome"], "fail")
        self.assertEqual(result["code"], "external_or_unsafe_reference")
        self.assertNotIn("example.invalid", json.dumps(result))

    def test_missing_local_reference_is_rejected(self) -> None:
        code, result = self.run_candidate({
            "index.html": (
                "<!doctype html><html><head><title>M0</title></head>"
                '<body><script src="missing.js"></script></body></html>'
            )
        })
        self.assertEqual(code, 1)
        self.assertEqual(result["code"], "local_reference_missing")

    def test_contact_actions_are_anchor_only_and_do_not_require_local_files(self) -> None:
        code, result = self.run_candidate({
            "index.html": (
                "<html><head><title>Contact</title></head><body>"
                '<a href="mailto:office@example.invalid?subject=Question">Email</a>'
                '<a href="TEL:+49123456789">Phone</a></body></html>'
            )
        })
        self.assertEqual(code, 0)
        self.assertEqual(result["references"], 2)

    def test_contact_resources_unknown_schemes_and_unsafe_actions_stay_denied(self) -> None:
        for element in [
            '<img src="mailto:office@example.invalid">',
            '<link href="tel:+49123456789">',
            '<a src="mailto:office@example.invalid">Email</a>',
            '<a href="custom:local.txt">Custom</a>',
            '<a href="mailto:">Empty</a>',
            '<a href="tel://example.invalid/123">Remote</a>',
            '<a href="mailto:office@example.invalid%0d%0aBCC:other@example.invalid">Email</a>',
            '<a href="http://[invalid">Invalid</a>',
            '<a href="javascript:alert(1)">Script</a>',
            '<a href="//example.invalid/tracker">Network</a>',
            '<a href="file:/private">File</a>',
        ]:
            with self.subTest(element=element):
                code, result = self.run_candidate({
                    "index.html": f"<html><head><title>Contact</title></head><body>{element}</body></html>"
                })
                self.assertEqual(code, 1)
                self.assertEqual(result["code"], "external_or_unsafe_reference")
                self.assertNotIn("example.invalid", json.dumps(result))

    def test_invalid_utf8_is_reported_without_parser_exception_or_content(self) -> None:
        code, result = self.run_candidate({"index.html": b"<html>\xffprivate</html>"})
        self.assertEqual(code, 1)
        self.assertEqual(result["code"], "html_invalid")
        self.assertNotIn("private", json.dumps(result))


class WorkItemGateRunnerTests(unittest.TestCase):
    def run_inputs(self, contents: list[bytes]) -> tuple[int, dict[str, object]]:
        spec = importlib.util.spec_from_file_location(
            "sentinel_work_item_gate_v1", WORK_ITEM_RUNNER
        )
        if spec is None or spec.loader is None:
            raise RuntimeError("work-item gate runner module cannot be loaded")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            paths = []
            for index, content in enumerate(contents):
                path = Path(directory) / f"artifact-{index}.bin"
                path.write_bytes(content)
                path.chmod(0o444)
                paths.append(path)
            previous_argv = sys.argv
            output = io.StringIO()
            try:
                sys.argv = [str(WORK_ITEM_RUNNER), *map(str, paths)]
                with contextlib.redirect_stdout(output):
                    try:
                        module.main()
                    except SystemExit as error:
                        code = int(error.code)
                    else:
                        code = 0
            finally:
                sys.argv = previous_argv
        return code, json.loads(output.getvalue())

    def test_read_only_inventory_is_deterministic(self) -> None:
        first = self.run_inputs([b"source-tree", b"artifact"])
        second = self.run_inputs([b"source-tree", b"artifact"])
        self.assertEqual(first, second)
        self.assertEqual(first[0], 0)
        self.assertEqual(first[1]["outcome"], "pass")
        self.assertEqual(first[1]["files"], 2)

    def test_empty_artifact_is_rejected(self) -> None:
        code, result = self.run_inputs([b""])
        self.assertEqual(code, 1)
        self.assertEqual(result["code"], "input_file_contract")


if __name__ == "__main__":
    unittest.main()
