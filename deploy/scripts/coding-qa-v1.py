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
import shutil
import socket
import stat
import sys
import tempfile
import time


MAX_FILES = 64
MAX_FILE_BYTES = 8 * 1024 * 1024
MAX_TOTAL_BYTES = 64 * 1024 * 1024
MAX_OUTPUT_BYTES = 65536
MAX_WIRE_BYTES = 512 * 1024
BROKER_SOCKET = "/run/sentinel-qa-broker.sock"
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


def stage_inputs(values: list[str], root: Path, progress=None) -> dict[str, object]:
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
            if progress is not None:
                progress["completed"] += 1
    encoded = json.dumps(inventory, separators=(",", ":")).encode()
    bound_inputs = json.dumps([[path, digest] for path, _, digest in inventory],
                              separators=(",", ":"), ensure_ascii=False).encode()
    return {"files": len(values), "bytes": total,
            "input_inventory_sha256": hashlib.sha256(bound_inputs).hexdigest(),
            "inventory_sha256": hashlib.sha256(encoded).hexdigest()}


def run_tool(args: list[str], root: Path, deadline: float,
             input_bytes: bytes = b"") -> tuple[int, bytes, bytes]:
    if time.monotonic() >= deadline:
        raise QaError("tool_timeout", "error")
    token = os.environ.get("SENTINEL_QA_BROKER_TOKEN", "")
    if (len(token) != 64 or any(char not in "0123456789abcdef" for char in token)
            or not args or args[0] not in {"python3", "node"} or len(args) > 66
            or any(not isinstance(arg, str) or len(arg.encode()) > 4096 or "\x00" in arg for arg in args)
            or len(input_bytes) > MAX_OUTPUT_BYTES):
        raise QaError("io_or_tool_error", "error")
    scratch = root.parent / "scratch"
    scratch.mkdir(mode=0o700, exist_ok=True)
    metadata = scratch.lstat()
    if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != 0o700):
        raise QaError("io_or_tool_error", "error")
    request = {
        "kind": "qa", "version": 1, "token": token, "program": args[0], "args": args[1:],
        "workspace": str(root), "scratch": str(scratch), "inputBytes": list(input_bytes),
        "wallTimeMs": min(25000, max(1, int((deadline - time.monotonic()) * 1000))),
        "stdoutBytes": MAX_OUTPUT_BYTES, "stderrBytes": MAX_OUTPUT_BYTES,
    }
    encoded = json.dumps(request, separators=(",", ":")).encode()
    if len(encoded) + 4 > MAX_WIRE_BYTES:
        raise QaError("io_or_tool_error", "error")
    # The trusted pre-Landlock sibling alone constructs mounts. Its readonly
    # children inherit command accounting but cannot reach this socket/token.
    received = 0
    streams = {"stdout": bytearray(), "stderr": bytearray()}
    ready = False
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as channel:
            def timeout():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise QaError("tool_timeout", "error")
                channel.settimeout(remaining)

            def read_exact(size):
                chunks = bytearray()
                while len(chunks) < size:
                    timeout()
                    part = channel.recv(size - len(chunks))
                    if not part:
                        raise QaError("io_or_tool_error", "error")
                    chunks.extend(part)
                return chunks

            timeout()
            channel.connect(BROKER_SOCKET)
            timeout()
            channel.sendall(len(encoded).to_bytes(4, "big") + encoded)
            while True:
                size = int.from_bytes(read_exact(4), "big")
                received += size + 4
                if size == 0 or received > MAX_WIRE_BYTES:
                    raise QaError("tool_output_limit", "error")
                event = json.loads(read_exact(size), object_pairs_hook=unique_object)
                if not isinstance(event, dict) or type(event.get("version")) is not int or event["version"] != 1:
                    raise QaError("io_or_tool_error", "error")
                kind = event.get("kind")
                if kind == "ready" and set(event) == {"kind", "version"} and not ready:
                    ready = True
                elif kind in streams and set(event) == {"kind", "version", "dataHex"} and ready:
                    value = event["dataHex"]
                    if (not isinstance(value, str) or len(value) % 2
                            or len(value) > MAX_OUTPUT_BYTES * 2
                            or any(char not in "0123456789abcdef" for char in value)):
                        raise QaError("io_or_tool_error", "error")
                    chunk = bytes.fromhex(value)
                    if len(streams[kind]) + len(chunk) > MAX_OUTPUT_BYTES:
                        raise QaError("tool_output_limit", "error")
                    streams[kind].extend(chunk)
                elif kind == "exit" and set(event) == {"kind", "version", "code", "signal"} and ready:
                    if event["signal"] is not None:
                        raise QaError("tool_terminated", "error")
                    if type(event["code"]) is not int or not 0 <= event["code"] <= 255:
                        raise QaError("io_or_tool_error", "error")
                    timeout()
                    if channel.recv(1):
                        raise QaError("io_or_tool_error", "error")
                    return event["code"], bytes(streams["stdout"]), bytes(streams["stderr"])
                else:
                    raise QaError("io_or_tool_error", "error")
    except (TimeoutError, socket.timeout):
        raise QaError("tool_timeout", "error") from None
    except (OSError, ValueError, TypeError, KeyError):
        raise QaError("io_or_tool_error", "error") from None


PYTHON_COMPILE = """
import sys
# -E/-s allow ordinary local scripts; compilation itself must not import them.
sys.path.pop(0)
import pathlib
completed = 0
try:
    for name in sys.argv[1:]:
        path = pathlib.Path(name)
        completed += 1
        compile(path.read_bytes(), name, 'exec', dont_inherit=True)
finally:
    print(completed)
"""

def python_qa(root: Path, deadline: float, progress) -> dict[str, int]:
    sources = sorted(path.relative_to(root).as_posix() for path in root.rglob("*.py"))
    progress["planned"] = len(sources)
    if not sources:
        raise QaError("source_missing")
    # Four fixed argv entries leave 61 paths within the child envelope. A
    # 64-file inventory must not overflow that separate argument bound.
    for offset in range(0, len(sources), 61):
        batch = sources[offset:offset + 61]
        status, stdout, _ = run_tool(["python3", "-E", "-s", "-c", PYTHON_COMPILE, *batch], root, deadline)
        if status:
            # Only the fixed supervisor emits the count; source is not imported.
            if stdout.strip().isdigit() and 0 < int(stdout) <= len(batch):
                progress["completed"] += int(stdout)
            raise QaError("python_compile_failed")
        progress["completed"] += len(batch)
    return {"syntax_files": len(sources)}


def node_qa(root: Path, deadline: float, progress) -> dict[str, int]:
    sources = sorted(path.relative_to(root) for path in root.rglob("*")
                     if path.is_file() and path.suffix in {".js", ".mjs", ".cjs"})
    progress["planned"] = len(sources)
    if not sources:
        raise QaError("source_missing")
    for path in sources:
        status, _, _ = run_tool(["node", "--check", "--", path.as_posix()], root, deadline)
        progress["completed"] += 1
        if status:
            raise QaError("node_syntax_failed")
    return {"syntax_files": len(sources)}


def unique_object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate JSON key")
        value[key] = item
    return value


def load_plan(root: Path, family: str):
    plan_path = root / "sentinel-qa.json"
    if not plan_path.is_file():
        raise QaError("tests_missing", "error")
    try:
        plan = json.loads(plan_path.read_text(encoding="utf-8"), object_pairs_hook=unique_object)
        if (not isinstance(plan, dict) or set(plan) != {"schema_version", "cases"}
                or type(plan["schema_version"]) is not int or plan["schema_version"] != 1
                or not isinstance(plan["cases"], list) or not 1 <= len(plan["cases"]) <= 64):
            raise ValueError()
        identifiers = set()
        for case in plan["cases"]:
            if (not isinstance(case, dict) or set(case) != {
                    "id", "script", "args", "stdin", "expected_stdout", "expected_stderr", "expected_exit"}
                    or not isinstance(case["id"], str) or not 1 <= len(case["id"]) <= 128
                    or case["id"] in identifiers
                    or not isinstance(case["script"], str)
                    or not isinstance(case["args"], list) or len(case["args"]) > 32
                    or any(not isinstance(arg, str) or len(arg.encode()) > 4096 or "\x00" in arg for arg in case["args"])
                    or any(not isinstance(case[field], str) or len(case[field].encode()) > MAX_OUTPUT_BYTES
                           for field in ("stdin", "expected_stdout", "expected_stderr"))
                    or type(case["expected_exit"]) is not int or not 0 <= case["expected_exit"] <= 255
                    or not (case["expected_stdout"] or case["expected_stderr"] or case["expected_exit"])):
                raise ValueError()
            identifiers.add(case["id"])
            script = Path(case["script"])
            if (script.is_absolute() or any(part in {"", ".", ".."} for part in case["script"].split("/"))
                    or script.suffix not in ({".py"} if family == "python-project-v1" else {".js", ".mjs", ".cjs"})
                    or not (root / script).is_file()):
                raise ValueError()
    except (ValueError, TypeError, OSError):
        raise QaError("test_plan_invalid", "error") from None
    return plan


def behavioral_qa(root: Path, family: str, deadline: float, progress, plan) -> dict[str, int]:
    """The supervisor, not imported candidate code, owns assertions and counts."""
    progress["planned"] = len(plan["cases"])
    # Developer checks are diagnostics, never trusted behavioral test counts.
    if family == "python-project-v1":
        status, _, _ = run_tool(["python3", "-I", "-m", "unittest", "discover"], root, deadline)
    else:
        tests = sorted(path.relative_to(root).as_posix() for path in root.rglob("*")
                       if path.is_file() and path.suffix in {".js", ".mjs", ".cjs"}
                       and ("test" in path.relative_to(root).parts[:-1]
                            or "tests" in path.relative_to(root).parts[:-1]
                            or path.stem == "test" or path.stem.startswith("test-")
                            or path.stem.endswith((".test", ".spec"))))
        status = run_tool(["node", "--test", "--test-concurrency=1", "--", *tests], root, deadline)[0] if tests else 0
    diagnostics_failed = bool(status)
    failed = False
    for case in plan["cases"]:
        command = (["python3", "-E", "-s", "--"] if family == "python-project-v1" else ["node", "--"])
        status, stdout, stderr = run_tool(command + [case["script"], *case["args"]], root, deadline, case["stdin"].encode())
        progress["completed"] += 1
        if (status != case["expected_exit"] or stdout != case["expected_stdout"].encode()
                or stderr != case["expected_stderr"].encode()):
            failed = True
    if diagnostics_failed:
        raise QaError("tests_failed")
    if failed:
        raise QaError("behavioral_assertion_failed")
    return {"tests": len(plan["cases"]), "skipped": 0}


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    result: dict[str, object] = {"schema_version": 1}
    stages = {name: {"outcome": "not_run", "planned": 0, "completed": 0}
              for name in ("inventory", "syntax", "tests")}
    active = "inventory"
    try:
        if not 2 <= len(args) <= MAX_FILES + 1:
            raise QaError("arguments_denied")
        family = args[0]
        inventory_only = family == "--inventory-only"
        if not inventory_only and family not in FAMILIES:
            raise QaError("family_denied")
        result.update(family=family, suite_id="web-work-item-qa-v1" if inventory_only else FAMILIES[family])
        stages["inventory"]["planned"] = len(args) - 1
        deadline = time.monotonic() + WALL_SECONDS
        # The current directory is the already scoped writable QA workspace;
        # neither /tmp nor the retained input tree is used for working files.
        with tempfile.TemporaryDirectory(prefix=".coding-qa-", dir=Path.cwd()) as directory:
            root = Path(directory) / "candidate"
            root.mkdir(mode=0o700)
            result.update(stage_inputs(args[1:], root, stages["inventory"]))
            stages["inventory"]["outcome"] = "pass"
            if not inventory_only:
                active = "tests"
                plan = load_plan(root, family)
                active = "syntax"
                if shutil.which("python3" if family == "python-project-v1" else "node",
                                path=ENVIRONMENT["PATH"]) is None:
                    raise QaError("io_or_tool_error", "error")
                result.update(python_qa(root, deadline, stages[active]) if family == "python-project-v1"
                              else node_qa(root, deadline, stages[active]))
                stages[active]["outcome"] = "pass"
                active = "tests"
                result.update(behavioral_qa(root, family, deadline, stages[active], plan))
                stages[active]["outcome"] = "pass"
        result.update(outcome="pass", code="checks_passed")
    except QaError as error:
        result.update(outcome=error.outcome, code=error.code)
    except TimeoutError:
        result.update(outcome="error", code="tool_timeout")
    except OSError:
        result.update(outcome="error", code="io_or_tool_error")
    except Exception:
        result.update(outcome="error", code="runner_error")
    if result["outcome"] != "pass":
        stages[active]["outcome"] = result["outcome"]
    if result.get("family") in FAMILIES:
        result["native_status"] = dict(schema_version=1, family=result["family"],
                                       suite_id=result["suite_id"], outcome=result["outcome"], **stages)
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return {"pass": 0, "fail": 1, "error": 2}[result["outcome"]]


if __name__ == "__main__":
    raise SystemExit(main())
