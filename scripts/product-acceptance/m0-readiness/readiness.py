#!/usr/bin/env python3
"""Bounded boot-readiness probes for the single-node M0 systemd topology."""

from __future__ import annotations

import argparse
import hashlib
import hmac
import http.client
import json
import os
from pathlib import Path
import re
import stat
import sys
import time
from typing import Any, Callable


SCHEMA_VERSION = 1
NATS_HOST = "127.0.0.1"
NATS_PORT = 8222
NATS_PATH = "/healthz?js-enabled-only=true"
DAEMON_HOST = "127.0.0.1"
DAEMON_PORT = 8084
DAEMON_READINESS_PATH = "/operator/runtime-health"
NIGHTRUN_PATH = "/operator/nightrun"
MAX_HTTP_BYTES = 256 * 1024
MAX_CREDENTIAL_BYTES = 512
MIN_CREDENTIAL_BYTES = 32
MAX_DIAGNOSTIC_BYTES = 128
MAX_TIMEOUT_SECONDS = 300.0
SAFE_CREDENTIAL_ROOTS = (Path("/run/credentials"), Path("/work/tmp/project-sentinel"))
CONTROL_RE = re.compile(r"[\x00-\x1f\x7f]")


class ReadinessError(Exception):
    def __init__(self, code: str) -> None:
        super().__init__(code)
        self.code = code


def fail(code: str) -> None:
    raise ReadinessError(code)


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode(
        "ascii"
    )


def strict_json(data: bytes) -> Any:
    def pairs(values: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in values:
            if key in result:
                fail("json_duplicate_key")
            result[key] = value
        return result

    try:
        return json.loads(data.decode("utf-8"), object_pairs_hook=pairs)
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise ReadinessError("json_invalid") from exc


def require_object(value: Any, code: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        fail(code)
    return value


def require_int(value: Any, code: str, *, minimum: int = 0) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        fail(code)
    return value


def require_bool(value: Any, code: str) -> bool:
    if not isinstance(value, bool):
        fail(code)
    return value


def validate_timeout(value: float) -> float:
    if not 0.1 <= value <= MAX_TIMEOUT_SECONDS:
        fail("timeout_invalid")
    return value


def _safe_credential_path(path: Path) -> None:
    if not path.is_absolute() or ".." in path.parts:
        fail("credential_path_invalid")
    if not any(path.is_relative_to(root) for root in SAFE_CREDENTIAL_ROOTS):
        fail("credential_path_invalid")


def _open_absolute(path: Path) -> int:
    parts = path.parts[1:]
    if not parts:
        fail("credential_path_invalid")
    directory = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for part in parts[:-1]:
            next_directory = os.open(
                part,
                os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=directory,
            )
            info = os.fstat(next_directory)
            if (
                not stat.S_ISDIR(info.st_mode)
                or info.st_uid not in {0, os.geteuid()}
                or stat.S_IMODE(info.st_mode) & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX | 0o022)
            ):
                os.close(next_directory)
                fail("credential_path_authority_invalid")
            os.close(directory)
            directory = next_directory
        return os.open(
            parts[-1],
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
            dir_fd=directory,
        )
    finally:
        os.close(directory)


def _credential_identity(info: os.stat_result) -> tuple[int, ...]:
    return (
        info.st_dev,
        info.st_ino,
        info.st_uid,
        info.st_mode,
        info.st_nlink,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
    )


def _validate_credential_metadata(info: os.stat_result) -> None:
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_nlink != 1
        or info.st_uid not in {0, os.geteuid()}
        or stat.S_IMODE(info.st_mode) not in {0o400, 0o600}
        or not MIN_CREDENTIAL_BYTES <= info.st_size <= MAX_CREDENTIAL_BYTES
    ):
        fail("credential_authority_invalid")


def _read_declared(
    descriptor: int,
    expected_size: int,
    reader: Callable[[int, int], bytes] | None = None,
) -> bytes:
    read = os.read if reader is None else reader
    chunks: list[bytes] = []
    remaining = expected_size
    while remaining:
        chunk = read(descriptor, remaining)
        if not chunk:
            fail("credential_short_read")
        if len(chunk) > remaining:
            fail("credential_trailing_data")
        chunks.append(chunk)
        remaining -= len(chunk)
    if read(descriptor, 1):
        fail("credential_trailing_data")
    return b"".join(chunks)


def _require_unchanged(before: os.stat_result, current: os.stat_result) -> None:
    try:
        _validate_credential_metadata(current)
    except ReadinessError as exc:
        raise ReadinessError("credential_changed") from exc
    if _credential_identity(before) != _credential_identity(current):
        fail("credential_changed")


def read_credential(path: Path) -> str:
    _safe_credential_path(path)
    try:
        descriptor = _open_absolute(path)
    except OSError as exc:
        raise ReadinessError("credential_unavailable") from exc
    try:
        before = os.fstat(descriptor)
        _validate_credential_metadata(before)
        data = _read_declared(descriptor, before.st_size)
        after = os.fstat(descriptor)
        _require_unchanged(before, after)
    finally:
        os.close(descriptor)
    try:
        verify_descriptor = _open_absolute(path)
    except OSError as exc:
        raise ReadinessError("credential_changed") from exc
    try:
        verified = os.fstat(verify_descriptor)
        _require_unchanged(before, verified)
        verified_data = _read_declared(verify_descriptor, verified.st_size)
        verified_after = os.fstat(verify_descriptor)
        _require_unchanged(verified, verified_after)
        if not hmac.compare_digest(data, verified_data):
            fail("credential_changed")
    finally:
        os.close(verify_descriptor)
    try:
        value = data.decode("utf-8")
    except UnicodeError as exc:
        raise ReadinessError("credential_invalid") from exc
    if (
        not MIN_CREDENTIAL_BYTES <= len(data) <= MAX_CREDENTIAL_BYTES
        or value != value.strip()
        or CONTROL_RE.search(value)
    ):
        fail("credential_invalid")
    return value


def request_json(
    method: str,
    host: str,
    port: int,
    path: str,
    timeout: float,
    *,
    credential: str | None = None,
    body: bytes | None = None,
    expected_status: int = 200,
) -> dict[str, Any]:
    if host not in {NATS_HOST, DAEMON_HOST} or method not in {"GET", "POST"}:
        fail("http_authority_invalid")
    timeout = validate_timeout(timeout)
    headers = {"Accept": "application/json", "Connection": "close"}
    if credential is not None:
        headers["x-sentinel-operator-key"] = credential
    if body is not None:
        headers["Content-Type"] = "application/json"
        headers["Content-Length"] = str(len(body))
    started = time.monotonic()
    connection = http.client.HTTPConnection(host, port, timeout=timeout)
    try:
        connection.request(method, path, body=body, headers=headers)
        response = connection.getresponse()
        if response.status != expected_status:
            if 500 <= response.status <= 599:
                fail("http_status_transient")
            if response.status in {401, 403}:
                fail("http_auth_rejected")
            if response.status == 404:
                fail("http_endpoint_missing")
            fail("http_status_fatal")
        if response.headers.get_content_type() != "application/json":
            fail("http_content_type")
        declared = response.headers.get("Content-Length")
        if declared is not None:
            try:
                if int(declared) > MAX_HTTP_BYTES:
                    fail("http_body_oversized")
            except ValueError as exc:
                raise ReadinessError("http_length_invalid") from exc
        chunks: list[bytes] = []
        total = 0
        while True:
            remaining = timeout - (time.monotonic() - started)
            if remaining <= 0:
                fail("http_timeout")
            if connection.sock is not None:
                connection.sock.settimeout(remaining)
            block = response.read(min(65536, MAX_HTTP_BYTES + 1 - total))
            if not block:
                break
            total += len(block)
            if total > MAX_HTTP_BYTES:
                fail("http_body_oversized")
            chunks.append(block)
        return require_object(strict_json(b"".join(chunks)), "http_json_shape")
    except ReadinessError:
        raise
    except TimeoutError as exc:
        raise ReadinessError("http_timeout") from exc
    except (OSError, http.client.HTTPException) as exc:
        raise ReadinessError("http_failed") from exc
    finally:
        connection.close()


HttpCall = Callable[..., dict[str, Any]]
Clock = Callable[[], float]
Sleeper = Callable[[float], None]


def wait_for_ready(
    timeout: float,
    attempt: Callable[[float], dict[str, Any]],
    retryable: set[str],
    *,
    clock: Clock = time.monotonic,
    sleeper: Sleeper = time.sleep,
) -> dict[str, Any]:
    deadline = clock() + validate_timeout(timeout)
    last_code = "readiness_timeout"
    while True:
        remaining = deadline - clock()
        if remaining <= 0:
            raise ReadinessError(last_code)
        try:
            return attempt(min(5.0, remaining))
        except ReadinessError as exc:
            if exc.code not in retryable:
                raise
            last_code = exc.code
        remaining = deadline - clock()
        if remaining <= 0:
            raise ReadinessError(last_code)
        sleeper(min(1.0, remaining))


def check_nats(
    timeout: float,
    http: HttpCall = request_json,
    *,
    clock: Clock = time.monotonic,
    sleeper: Sleeper = time.sleep,
) -> dict[str, Any]:
    def attempt(attempt_timeout: float) -> dict[str, Any]:
        payload = http("GET", NATS_HOST, NATS_PORT, NATS_PATH, attempt_timeout)
        if payload.get("status") != "ok":
            fail("nats_not_ready")
        return {
            "endpoint_digest": hashlib.sha256(NATS_PATH.encode("ascii")).hexdigest()
        }

    return wait_for_ready(
        timeout,
        attempt,
        {"http_failed", "http_status_transient", "http_timeout", "nats_not_ready"},
        clock=clock,
        sleeper=sleeper,
    )


def _validate_worker_states(value: Any) -> None:
    workers = require_object(value, "daemon_worker_state_invalid")
    required = {"ecs_tick_loop", "service_health"}
    if not set(workers).issubset(required):
        fail("daemon_worker_state_invalid")
    for raw in workers.values():
        worker = require_object(raw, "daemon_worker_state_invalid")
        running = require_bool(worker.get("running"), "daemon_worker_state_invalid")
        restart_count = require_int(
            worker.get("restart_count"), "daemon_worker_state_invalid"
        )
        last_error = worker.get("last_error")
        if last_error is not None and not isinstance(last_error, str):
            fail("daemon_worker_state_invalid")
        if (
            not running
            or restart_count != 0
            or last_error is not None
        ):
            fail("daemon_not_initialized")
    if set(workers) != required:
        fail("daemon_not_initialized")


def validate_daemon_payload(payload: dict[str, Any]) -> dict[str, Any]:
    expected = require_int(payload.get("expected_active_agents"), "daemon_count_invalid")
    if expected == 0:
        fail("daemon_not_initialized")
    for field in (
        "runtime_agents",
        "security_runtime_entries",
        "tracked_processes",
        "live_cgroup_dirs",
        "sandbox_handles",
    ):
        if require_int(payload.get(field), "daemon_count_invalid") != expected:
            fail("daemon_not_initialized")
    for field in (
        "orphan_cgroups",
        "zombie_tracked_pids",
        "respawn_failures",
    ):
        if require_int(payload.get(field), "daemon_drift_invalid") != 0:
            fail("daemon_not_initialized")
    if require_bool(payload.get("operator_auth_required"), "daemon_auth_invalid") is not True:
        fail("daemon_auth_disabled")
    last_repair_error = payload.get("last_repair_error")
    if last_repair_error is not None and not isinstance(last_repair_error, str):
        fail("daemon_repair_shape_invalid")
    if last_repair_error is not None:
        fail("daemon_not_initialized")
    _validate_worker_states(payload.get("worker_states"))
    agents = payload.get("agents")
    if not isinstance(agents, list):
        fail("daemon_agent_shape_invalid")
    if len(agents) != expected:
        fail("daemon_not_initialized")
    identities: set[int] = set()
    for raw in agents:
        agent = require_object(raw, "daemon_agent_invalid")
        agent_id = require_int(agent.get("agent_id"), "daemon_agent_invalid", minimum=1)
        if agent_id in identities or agent.get("aggregate_id") != f"AGENT-{agent_id:02}":
            fail("daemon_agent_roster_mismatch")
        identities.add(agent_id)
        for field in (
            "runtime_present",
            "tracked_pid_alive",
            "security_runtime_present",
            "adapter_handle_present",
            "adapter_instance_matches",
            "runtime_resources_healthy",
        ):
            if not require_bool(agent.get(field), "daemon_agent_shape_invalid"):
                fail("daemon_not_initialized")
        tracked_pid = agent.get("tracked_pid")
        if tracked_pid is None:
            fail("daemon_not_initialized")
        if require_int(tracked_pid, "daemon_agent_shape_invalid") < 1:
            fail("daemon_not_initialized")
        cgroup_live_pid_count = require_int(
            agent.get("cgroup_live_pid_count"), "daemon_agent_shape_invalid"
        )
        if cgroup_live_pid_count < 1:
            fail("daemon_not_initialized")
        tracked_pid_state = agent.get("tracked_pid_state")
        if tracked_pid_state is None:
            fail("daemon_not_initialized")
        if not isinstance(tracked_pid_state, str):
            fail("daemon_agent_shape_invalid")
        if tracked_pid_state in {"X", "Z"}:
            fail("daemon_not_initialized")
        observation_error = agent.get("adapter_observation_error")
        if observation_error is not None and not isinstance(observation_error, str):
            fail("daemon_agent_shape_invalid")
        if observation_error is not None:
            fail("daemon_not_initialized")
        status_pair = (agent.get("logical_status"), agent.get("adapter_health_state"))
        repair_status = agent.get("last_repair_status")
        if any(value is not None and not isinstance(value, str) for value in status_pair):
            fail("daemon_agent_shape_invalid")
        if repair_status is not None and not isinstance(repair_status, str):
            fail("daemon_agent_shape_invalid")
        if status_pair in {("Active", "healthy"), ("Sleeping", "healthy")}:
            if repair_status != "healthy":
                fail("daemon_not_initialized")
        elif status_pair == ("Suspended", "degraded"):
            if repair_status != "suspended":
                fail("daemon_not_initialized")
        else:
            fail("daemon_not_initialized")
    return {
        "expected_active_agents": expected,
        "roster_digest": hashlib.sha256(
            canonical_json(sorted(identities))
        ).hexdigest(),
    }


def check_daemon(
    timeout: float,
    credential_file: Path,
    http: HttpCall = request_json,
    *,
    clock: Clock = time.monotonic,
    sleeper: Sleeper = time.sleep,
) -> dict[str, Any]:
    credential = read_credential(credential_file)
    def attempt(attempt_timeout: float) -> dict[str, Any]:
        payload = http(
            "GET",
            DAEMON_HOST,
            DAEMON_PORT,
            DAEMON_READINESS_PATH,
            attempt_timeout,
            credential=credential,
        )
        return validate_daemon_payload(payload)

    return wait_for_ready(
        timeout,
        attempt,
        {
            "daemon_not_initialized",
            "http_failed",
            "http_status_transient",
            "http_timeout",
        },
        clock=clock,
        sleeper=sleeper,
    )


def trigger_nightrun(
    timeout: float, credential_file: Path, http: HttpCall = request_json
) -> dict[str, Any]:
    credential = read_credential(credential_file)
    payload = http(
        "POST",
        DAEMON_HOST,
        DAEMON_PORT,
        NIGHTRUN_PATH,
        timeout,
        credential=credential,
        body=b"{}",
        expected_status=202,
    )
    if payload.get("accepted") is not True:
        fail("nightrun_rejected")
    queued = require_int(payload.get("agents_queued"), "nightrun_response_invalid")
    message = payload.get("message")
    if not isinstance(message, str) or len(message.encode("utf-8")) > MAX_DIAGNOSTIC_BYTES:
        fail("nightrun_response_invalid")
    return {"agents_queued": queued}


def result_payload(check: str, details: dict[str, Any]) -> dict[str, Any]:
    result = {
        "check": check,
        "details": details,
        "schema_version": SCHEMA_VERSION,
        "status": "PASS",
    }
    result["result_digest"] = hashlib.sha256(canonical_json(result)).hexdigest()
    return result


class PublicArgumentParser(argparse.ArgumentParser):
    def error(self, _message: str) -> None:
        raise ReadinessError("arguments_invalid")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = PublicArgumentParser(description=__doc__, add_help=False)
    subparsers = parser.add_subparsers(dest="check", required=True)
    for name in ("nats", "daemon", "nightrun"):
        child = subparsers.add_parser(name)
        child.add_argument("--timeout-seconds", required=True, type=float)
        if name != "nats":
            child.add_argument("--credential-file", required=True, type=Path)
    return parser.parse_args(argv)


def run(argv: list[str]) -> int:
    check = "unknown"
    try:
        args = parse_args(argv)
        check = args.check
        timeout = validate_timeout(args.timeout_seconds)
        if check == "nats":
            details = check_nats(timeout)
        elif check == "daemon":
            details = check_daemon(timeout, args.credential_file)
        else:
            details = trigger_nightrun(timeout, args.credential_file)
        sys.stdout.buffer.write(canonical_json(result_payload(check, details)))
        return 0
    except ReadinessError as exc:
        sys.stderr.buffer.write(
            canonical_json(
                {
                    "check": check,
                    "reason": exc.code,
                    "schema_version": SCHEMA_VERSION,
                    "status": "FAIL",
                }
            )
        )
        return 1
    except Exception:
        sys.stderr.buffer.write(
            canonical_json(
                {
                    "check": check,
                    "reason": "internal_failure",
                    "schema_version": SCHEMA_VERSION,
                    "status": "FAIL",
                }
            )
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(run(sys.argv[1:]))
