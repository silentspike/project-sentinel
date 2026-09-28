#!/usr/bin/env python3
"""Bounded native QA. Process/network isolation must be supplied by Workbench.

Only declared read-only files are staged. Inventory checks are not a security
assessment of arbitrary candidate code. No shell, installation, or network
operation is performed by this runner; candidate tests inherit runtime isolation.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import selectors
import stat
import subprocess
import sys
import tempfile
import time


MAX_FILES = 64
MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_TOTAL_BYTES = 64 * 1024 * 1024
MAX_OUTPUT_BYTES = 65536
WALL_SECONDS = 25
FAMILIES = {"python-project-v1": "python-qa-v1", "node-project-v1": "node-qa-v1"}
ENVIRONMENT = {"HOME": "/workspace", "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
               "PATH": "/usr/bin:/bin"}


class QaError(Exception):
    def __init__(self, code: str, outcome: str = "fail") -> None:
        self.code = code
        self.outcome = outcome


def relative_input(value: str) -> Path:
    parts = value.split("/")
    if (not value.startswith("/") or len(value) > 4096
            or any(part in {"", ".", ".."} for part in parts[1:])
            or any(ord(char) < 32 or ord(char) == 127 for char in value)
            or "\\" in value):
        raise QaError("input_path_contract")
    # Real Workbench: /workspace/.inputs/<project>/<work-item>/<relative>.
    # Artifact-mounted callers: .../inputs/<artifact-id>/<relative>.
    markers = [i for i, part in enumerate(parts) if part in {"inputs", ".inputs"}]
    if len(markers) != 1:
        raise QaError("input_path_contract")
    index = markers[0]
    start = index + (3 if parts[index] == ".inputs" else 2)
    if start >= len(parts):
        raise QaError("input_path_contract")
    relative = Path(*parts[start:])
    if len(relative.parts) > 64:
        raise QaError("input_path_contract")
    return relative


def open_input(value: str) -> int:
    # Pin every ancestor with no-follow directory descriptors, not resolve().
    parent = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        parts = value.split("/")[1:]
        for part in parts[:-1]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
                            | os.O_CLOEXEC, dir_fd=parent)
            os.close(parent)
            parent = child
        return os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
                       | os.O_CLOEXEC, dir_fd=parent)
    finally:
        os.close(parent)


def signature(info: os.stat_result) -> tuple[int, ...]:
    return (info.st_dev, info.st_ino, info.st_mode, info.st_nlink, info.st_size,
            info.st_mtime_ns, info.st_ctime_ns)


def stage_inputs(values: list[str], root: Path) -> dict[str, object]:
    relatives = [relative_input(value) for value in values]
    inventory_paths = set(relatives)
    if (len(inventory_paths) != len(values)
            or any(parent in inventory_paths for path in relatives for parent in path.parents)):
        raise QaError("input_collision")
    total = 0
    inventory = []
    identities = set()
    for value, relative in sorted(zip(values, relatives), key=lambda pair: pair[1].as_posix()):
        fd = open_input(value)
        with os.fdopen(fd, "rb") as source:
            before = os.fstat(source.fileno())
            if (not stat.S_ISREG(before.st_mode) or before.st_mode & 0o222
                    or before.st_nlink != 1 or before.st_size > MAX_FILE_BYTES):
                raise QaError("input_file_contract")
            identity = (before.st_dev, before.st_ino)
            if identity in identities:
                raise QaError("input_collision")
            identities.add(identity)
            total += before.st_size
            if total > MAX_TOTAL_BYTES:
                raise QaError("input_total_limit")
            destination = root / relative
            destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            digest = hashlib.sha256()
            size = 0
            with destination.open("xb") as target:
                while chunk := source.read(65536):
                    size += len(chunk)
                    if size > before.st_size:
                        raise QaError("input_changed")
                    digest.update(chunk)
                    target.write(chunk)
            if size != before.st_size or signature(os.fstat(source.fileno())) != signature(before):
                raise QaError("input_changed")
            check = open_input(value)
            try:
                if signature(os.fstat(check)) != signature(before):
                    raise QaError("input_changed")
            finally:
                os.close(check)
            destination.chmod(0o444)
            inventory.append([relative.as_posix(), size, digest.hexdigest()])
    encoded = json.dumps(inventory, separators=(",", ":")).encode()
    return {"files": len(values), "bytes": total,
            "inventory_sha256": hashlib.sha256(encoded).hexdigest()}


def run_tool(args: list[str], root: Path, deadline: float) -> tuple[int, bytes]:
    if time.monotonic() >= deadline:
        raise QaError("tool_timeout", "error")
    # Never create a new session/process group: preserve Workbench accounting
    # and full-tree cancellation. Bounded reads avoid untrusted output capture.
    process = subprocess.Popen(args, cwd=root, env=ENVIRONMENT, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, shell=False)
    streams = {process.stdout: bytearray(), process.stderr: bytearray()}
    try:
        with selectors.DefaultSelector() as selector:
            for stream in streams:
                selector.register(stream, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise QaError("tool_timeout", "error")
                for key, _ in selector.select(min(remaining, 0.1)):
                    chunk = os.read(key.fileobj.fileno(), 4096)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    output = streams[key.fileobj]
                    if len(output) + len(chunk) > MAX_OUTPUT_BYTES:
                        raise QaError("tool_output_limit", "error")
                    output.extend(chunk)
            status = process.wait(timeout=max(0.001, deadline - time.monotonic()))
        if status < 0:
            raise QaError("tool_terminated", "error")
        return status, bytes(streams[process.stdout])
    finally:
        if process.poll() is None:
            process.kill()
        process.wait()
        for stream in streams:
            stream.close()


PYTHON_COMPILE = """
import sys
# -E/-s allow ordinary local scripts; compilation itself must not import them.
sys.path.pop(0)
import pathlib
for name in sys.argv[1:]:
    path = pathlib.Path(name)
    compile(path.read_bytes(), name, 'exec', dont_inherit=True)
"""

PYTHON_TEST = """
import json, os, sys, unittest
root = os.getcwd()
sys.path.insert(0, root)
suite = unittest.defaultTestLoader.discover(root, pattern='test*.py', top_level_dir=root)
result = unittest.TextTestRunner(stream=sys.stderr).run(suite)
print('\x1e' + json.dumps({'tests': result.testsRun, 'skipped': len(result.skipped),
                  'failures': len(result.failures), 'errors': len(result.errors)}))
sys.exit(0 if result.wasSuccessful() else 1)
"""


def python_qa(root: Path, deadline: float) -> dict[str, int]:
    sources = sorted(path.relative_to(root).as_posix() for path in root.rglob("*.py"))
    if not sources:
        raise QaError("source_missing")
    status, _ = run_tool(["python3", "-E", "-s", "-c", PYTHON_COMPILE, *sources], root, deadline)
    if status:
        raise QaError("python_compile_failed")
    # -I disables environment/user-site imports; only the private staged root
    # is deliberately added for package-local imports during discovery.
    status, output = run_tool(["python3", "-I", "-c", PYTHON_TEST], root, deadline)
    try:
        counts = json.loads(output.rsplit(b"\x1e", 1)[1])
        if (set(counts) != {"tests", "skipped", "failures", "errors"}
                or any(type(value) is not int or not 0 <= value <= 1000000 for value in counts.values())
                or sum(counts[key] for key in ("skipped", "failures", "errors")) > counts["tests"]):
            raise ValueError()
    except (ValueError, TypeError, IndexError):
        raise QaError("test_result_invalid", "error") from None
    if status or counts["failures"] or counts["errors"]:
        raise QaError("tests_failed")
    if counts["tests"] <= counts["skipped"]:
        raise QaError("tests_missing")
    return {"syntax_files": len(sources), "tests": counts["tests"], "skipped": counts["skipped"]}


NODE_REPORTER = """
const wrappers = new Set(WRAPPER_NAMES);
module.exports = async function* (events) {
  const counts = {tests: 0, passed: 0, failed: 0, skipped: 0, todo: 0, unknown: 0};
  let outputBytes = 0;
  for await (const event of events) {
    const data = event.data;
    if (event.type === 'test:stdout' || event.type === 'test:stderr') {
      outputBytes += Buffer.byteLength(data.message);
      if (outputBytes > 65536) throw new Error('test_output_limit');
    }
    if (event.type !== 'test:pass' && event.type !== 'test:fail') continue;
    if (wrappers.has(data.name)) continue;
    if (!data.details || !['suite', 'test'].includes(data.details.type)) {
      counts.unknown++;
      continue;
    }
    if (data.details.type === 'suite') continue;
    counts.tests++;
    if (data.skip) counts.skipped++;
    else if (data.todo) counts.todo++;
    else if (event.type === 'test:fail') counts.failed++;
    else counts.passed++;
  }
  yield JSON.stringify(counts);
};
"""


def node_qa(root: Path, deadline: float) -> dict[str, int]:
    sources = sorted(path.relative_to(root) for path in root.rglob("*")
                     if path.is_file() and path.suffix in {".js", ".mjs", ".cjs"})
    if not sources:
        raise QaError("source_missing")
    for path in sources:
        status, _ = run_tool(["node", "--check", "--", path.as_posix()], root, deadline)
        if status:
            raise QaError("node_syntax_failed")
    tests = [path.as_posix() for path in sources if
             "test" in path.parts[:-1] or "tests" in path.parts[:-1]
             or path.stem == "test" or path.stem.startswith("test-")
             or path.stem.endswith((".test", ".spec"))]
    if not tests:
        raise QaError("tests_missing")
    # Node treats a test-named file with no test() calls as one passing file.
    # A trusted event reporter distinguishes suites, skips and file wrappers;
    # parsing console TAP text would miscount skipped-only mixed candidates.
    wrappers = sorted(set(tests) | {str(root / name) for name in tests})
    reporter = root.parent / "node-reporter.cjs"
    with reporter.open("x", encoding="ascii") as handle:
        handle.write(NODE_REPORTER.replace("WRAPPER_NAMES", json.dumps(wrappers)))
    reporter.chmod(0o444)
    status, output = run_tool(["node", "--test", "--test-concurrency=1",
                               "--test-reporter=" + str(reporter), "--", *tests], root, deadline)
    try:
        counts = json.loads(output)
        if (set(counts) != {"tests", "passed", "failed", "skipped", "todo", "unknown"}
                or any(type(value) is not int or not 0 <= value <= 1000000 for value in counts.values())
                or counts["unknown"]
                or counts["tests"] != sum(counts[key] for key in ("passed", "failed", "skipped", "todo"))):
            raise ValueError()
    except (ValueError, TypeError):
        raise QaError("test_result_invalid", "error") from None
    if status or counts["failed"]:
        raise QaError("tests_failed")
    if not counts["passed"]:
        raise QaError("tests_missing")
    return {"syntax_files": len(sources), "tests": counts["tests"], "skipped": counts["skipped"]}


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    result: dict[str, object] = {"schema_version": 1}
    try:
        if not 2 <= len(args) <= MAX_FILES + 1:
            raise QaError("arguments_denied")
        family = args[0]
        if family not in FAMILIES:
            raise QaError("family_denied")
        result.update(family=family, suite_id=FAMILIES[family])
        deadline = time.monotonic() + WALL_SECONDS
        # The current directory is the already scoped writable QA workspace;
        # neither /tmp nor the retained input tree is used for working files.
        with tempfile.TemporaryDirectory(prefix=".coding-qa-", dir=Path.cwd()) as directory:
            root = Path(directory) / "candidate"
            root.mkdir(mode=0o700)
            result.update(stage_inputs(args[1:], root))
            result.update(python_qa(root, deadline) if family == "python-project-v1"
                          else node_qa(root, deadline))
        result.update(outcome="pass", code="checks_passed")
    except QaError as error:
        result.update(outcome=error.outcome, code=error.code)
    except subprocess.TimeoutExpired:
        result.update(outcome="error", code="tool_timeout")
    except OSError:
        result.update(outcome="error", code="io_or_tool_error")
    except Exception:
        result.update(outcome="error", code="runner_error")
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return {"pass": 0, "fail": 1, "error": 2}[result["outcome"]]


if __name__ == "__main__":
    raise SystemExit(main())
