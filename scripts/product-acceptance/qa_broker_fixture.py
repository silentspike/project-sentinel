"""TEST ONLY: Unix QA wire fixture, not a production broker or Landlock proof.

Positive events come from observed read-only bwrap children. The runner's socket
is injected into a loaded module only; no production alternate-socket setting
or direct subprocess fallback is provided.
"""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import selectors
import shutil
import socket
import struct
import subprocess
import sys
import threading
import time
from types import SimpleNamespace


TOKEN = "a" * 64
MAX_FRAME_BYTES = 512 * 1024
MAX_OUTPUT_BYTES = 65536
ENVIRONMENT = {"LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "PATH": "/usr/bin:/bin"}


def unique_object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate wire key")
        value[key] = item
    return value


def frame(value):
    payload = value if isinstance(value, bytes) else json.dumps(value, separators=(",", ":")).encode()
    return struct.pack("!I", len(payload)) + payload


def event(kind, **fields):
    return dict(kind=kind, version=1, **fields)


def receive_exact(connection, size):
    result = bytearray()
    while len(result) < size:
        chunk = connection.recv(size - len(result))
        if not chunk:
            raise EOFError("truncated fixture request")
        result.extend(chunk)
    return bytes(result)


def load_test_runner(path, broker_socket):
    spec = importlib.util.spec_from_file_location("coding_qa_test_subject", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.BROKER_SOCKET = str(broker_socket)
    if hasattr(module, "subprocess"):
        def forbidden(*args, **kwargs):
            raise AssertionError("production evaluator attempted direct subprocess fallback")
        # Do not patch the shared subprocess module used by the fixture server.
        module.subprocess = SimpleNamespace(TimeoutExpired=subprocess.TimeoutExpired,
                                            Popen=forbidden, run=forbidden)
    return module


class QaBrokerFixture:
    """Bounded, test-scoped responder with optional adversarial raw replies."""

    def __init__(self, root, response=None):
        self.root = Path(root).resolve()
        if not self.root.is_relative_to(Path("/work/tmp")):
            raise ValueError("fixture must remain under /work/tmp")
        self.path = self.root / "broker.sock"
        self.response = response
        self.requests = []
        self.errors = []
        self.stop = threading.Event()
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(str(self.path))
        self.path.chmod(0o600)
        self.listener.listen(4)
        self.listener.settimeout(0.1)
        self.thread = threading.Thread(target=self._serve, name="test-only-qa-broker")
        self.thread.start()

    def close(self):
        self.stop.set()
        self.listener.close()
        self.thread.join(timeout=3)
        self.path.unlink(missing_ok=True)
        if self.thread.is_alive():
            raise AssertionError("test fixture did not quiesce")
        if self.errors:
            raise AssertionError(f"test fixture failed: {self.errors!r}")

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def _request(self, connection):
        length = struct.unpack("!I", receive_exact(connection, 4))[0]
        if not 0 < length <= MAX_FRAME_BYTES:
            raise ValueError("fixture request frame limit")
        request = json.loads(receive_exact(connection, length), object_pairs_hook=unique_object)
        if not isinstance(request, dict) or set(request) != {
            "kind", "version", "token", "program", "args", "workspace", "scratch",
            "inputBytes", "wallTimeMs", "stdoutBytes", "stderrBytes",
        }:
            raise ValueError("fixture request fields")
        if (request["kind"] != "qa" or type(request["version"]) is not int or request["version"] != 1
                or request["token"] != TOKEN or request["program"] not in {"python3", "node"}
                or not isinstance(request["args"], list) or len(request["args"]) > 65
                or any(not isinstance(arg, str) or len(arg.encode()) > 4096 or "\x00" in arg
                       for arg in request["args"])
                or not isinstance(request["inputBytes"], list) or len(request["inputBytes"]) > MAX_OUTPUT_BYTES
                or any(type(value) is not int or not 0 <= value <= 255 for value in request["inputBytes"])
                or any(type(request[field]) is not int or not 0 < request[field] <= ceiling
                       for field, ceiling in (("wallTimeMs", 25000), ("stdoutBytes", MAX_OUTPUT_BYTES),
                                              ("stderrBytes", MAX_OUTPUT_BYTES)))):
            raise ValueError("fixture request bounds or binding")
        workspace, scratch = Path(request["workspace"]), Path(request["scratch"])
        if (not workspace.is_absolute() or not workspace.resolve().is_relative_to(self.root)
                or not workspace.is_dir() or scratch != workspace.parent / "scratch"
                or not scratch.resolve().is_relative_to(self.root)):
            raise ValueError("fixture workspace scope")
        self.requests.append(request)
        return request

    def _serve(self):
        while not self.stop.is_set():
            try:
                connection, _ = self.listener.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            with connection:
                connection.settimeout(1)
                try:
                    if len(self.requests) >= 256:
                        raise ValueError("fixture request count limit")
                    request = self._request(connection)
                    if self.response is not None:
                        self.response(connection, request, self.stop)
                    else:
                        self._observe_child(connection, request)
                except (BrokenPipeError, ConnectionResetError):
                    pass
                except Exception as error:
                    if not self.stop.is_set():
                        self.errors.append(f"{type(error).__name__}: {error}")

    def _observe_child(self, connection, request):
        root, scratch = Path(request["workspace"]), Path(request["scratch"])
        scratch.mkdir(mode=0o700, exist_ok=True)
        environment = dict(ENVIRONMENT, HOME=str(scratch), TMPDIR=str(scratch), PYTHONDONTWRITEBYTECODE="1")
        program = shutil.which(request["program"])
        if program is None:
            raise ValueError("fixture interpreter unavailable")
        isolated = ["bwrap", "--unshare-user", "--unshare-pid", "--unshare-net",
                    "--cap-drop", "ALL", "--ro-bind", "/", "/", "--proc", "/proc",
                    "--dev", "/dev", "--bind", str(scratch), str(scratch),
                    "--chdir", str(root), "--die-with-parent", "--", program, *request["args"]]
        process = subprocess.Popen(isolated, cwd=root, env=environment, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, shell=False)
        streams = {process.stdout: ("stdout", 0), process.stderr: ("stderr", 0)}
        deadline = time.monotonic() + request["wallTimeMs"] / 1000
        try:
            connection.sendall(frame(event("ready")))
            with selectors.DefaultSelector() as selector:
                for stream in streams:
                    selector.register(stream, selectors.EVENT_READ)
                pending_input = memoryview(bytes(request["inputBytes"]))
                if pending_input:
                    selector.register(process.stdin, selectors.EVENT_WRITE)
                else:
                    process.stdin.close()
                while selector.get_map():
                    if self.stop.is_set():
                        return
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        connection.sendall(frame(event("error", code="tool_timeout")))
                        return
                    for key, _ in selector.select(min(remaining, 0.05)):
                        if key.fileobj is process.stdin:
                            try:
                                written = os.write(process.stdin.fileno(), pending_input[:4096])
                                pending_input = pending_input[written:]
                            except BrokenPipeError:
                                pending_input = memoryview(b"")
                            if not pending_input:
                                selector.unregister(process.stdin)
                                process.stdin.close()
                            continue
                        chunk = os.read(key.fileobj.fileno(), 4096)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        kind, total = streams[key.fileobj]
                        total += len(chunk)
                        streams[key.fileobj] = (kind, total)
                        # Forward at most one over-budget chunk so the actual
                        # client must reject aggregate output, then stop the child.
                        connection.sendall(frame(event(kind, dataHex=chunk.hex())))
                        if total > request[kind + "Bytes"]:
                            return
                status = process.wait(timeout=max(0.001, deadline - time.monotonic()))
            connection.sendall(frame(event("exit", code=status if status >= 0 else None,
                                          signal=-status if status < 0 else None)))
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=1)
            for stream in (*streams, process.stdin):
                stream.close()


if __name__ == "__main__":
    if len(sys.argv) < 6 or sys.argv[1] != "--invoke":
        raise SystemExit("test helper requires --invoke RUNNER SOCKET TOKEN ARGS")
    os.environ["SENTINEL_QA_BROKER_TOKEN"] = sys.argv[4]
    subject = load_test_runner(Path(sys.argv[2]), Path(sys.argv[3]))
    raise SystemExit(subject.main(sys.argv[5:]))
