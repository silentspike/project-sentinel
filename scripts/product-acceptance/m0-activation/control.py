#!/usr/bin/env python3
"""Fail-closed activation and restart control for the single-node M0 run."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from dataclasses import dataclass
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import sys
import time
from typing import Any, Callable


SCHEMA_VERSION = 1
SAFE_ROOT = Path("/work/tmp/project-sentinel")
CONTROL_LOCK = SAFE_ROOT / ".m0-activation-control.lock"
MAX_JSON_BYTES = 4 * 1024 * 1024
MAX_OUTPUT_BYTES = 4 * 1024 * 1024
MAX_TIMEOUT_SECONDS = 30.0
MAX_ACTIVATION_DEADLINE_SECONDS = 900.0
DEFAULT_ACTIVATION_DEADLINE_SECONDS = 300.0
ACTIVATION_POLL_SECONDS = 1.0
DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
SAFE_UNIT_RE = re.compile(r"^[a-z0-9][a-z0-9@_.-]{0,127}\.(?:service|timer|target)$")
CREDENTIAL_RE = re.compile(r"^(?:agent|customer|operator)=[A-Z][A-Z0-9_]{0,127}$")
HERE = Path(__file__).resolve().parent
PRODUCT_ACCEPTANCE = HERE.parent
PREFLIGHT_PROGRAM = PRODUCT_ACCEPTANCE / "run_m0_preflight.py"
JOURNEY_PROGRAM = PRODUCT_ACCEPTANCE / "run_m0_journey.py"
SYSTEMCTL = Path("/usr/bin/systemctl")
PYTHON = Path("/usr/bin/python3")
TARGET = "sentinel.target"
SERVICES = (
    "nats-server.service",
    "sentinel-daemon.service",
    "sentinel-dashboard-backend.service",
    "sentinel-gaia-loop.service",
    "sentinel-gateway.service",
    "sentinel-judge.service",
    "sentinel-nats-bridge.service",
    "sentinel-projection.service",
)
TIMERS = ("sentinel-health-monitor.timer", "sentinel-nightrun.timer")
ONESHOTS = ("sentinel-health-monitor.service", "sentinel-nightrun.service")
TOPOLOGY = (
    "sentinel-daemon.service",
    "sentinel-gateway.service",
    "sentinel-dashboard-backend.service",
    "sentinel-projection.service",
    "sentinel-gaia-loop.service",
    "sentinel-nightrun.timer",
    "nats-server.service",
    "sentinel-nats-bridge.service",
    "sentinel-judge.service",
    "sentinel-health-monitor.timer",
)
ALL_UNITS = (*TOPOLOGY, TARGET)
INSPECT_UNITS = (*ALL_UNITS, *ONESHOTS)
ROLLBACK_ORDER = (TARGET, *ONESHOTS, *tuple(reversed(TOPOLOGY)))
BASE_CHILD_ENV = {
    "LC_ALL": "C",
    "PATH": "/usr/bin:/bin",
    "PYTHONDONTWRITEBYTECODE": "1",
    "PYTHONNOUSERSITE": "1",
}
MAX_CREDENTIAL_BYTES = 4096
TEMPORAL_PREFLIGHT_REASONS = frozenset({
    "episode_projection_blocked",
    "episode_projection_not_ready",
    "event_cut_changed",
    "event_projection_lag",
    "http_readiness_failed",
    "http_timeout",
    "identity_dependency_failed",
    "listener_contract_mismatch",
    "platform_unresolved",
    "publication_or_recovery_backlog",
    "read_model_projection_lag",
    "runtime_agent_not_ready",
    "runtime_count_mismatch",
    "runtime_drift",
    "runtime_queue_backlog",
    "runtime_repair_unresolved",
    "runtime_roster_mismatch",
    "runtime_worker_not_ready",
    "store_snapshot_dependency_failed",
    "systemd_dependency_failed",
    "systemd_target_not_ready",
    "systemd_timer_not_ready",
    "systemd_unit_not_ready",
})


class ControlError(RuntimeError):
    """A typed public-safe control failure."""

    def __init__(self, code: str) -> None:
        super().__init__(code)
        self.code = code


def fail(code: str) -> None:
    raise ControlError(code)


def reject_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            fail("json_duplicate_key")
        result[key] = value
    return result


def canonical(value: Any) -> bytes:
    try:
        return (json.dumps(
            value, allow_nan=False, ensure_ascii=True, separators=(",", ":"),
            sort_keys=True,
        ) + "\n").encode("ascii")
    except (TypeError, ValueError) as exc:
        raise ControlError("json_not_canonical") from exc


def digest_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def digest(value: Any) -> str:
    return digest_bytes(canonical(value))


def public_failure(code: str) -> bytes:
    return canonical({"schema_version": SCHEMA_VERSION, "status": "FAIL", "reason": code})


def safe_output_path(value: Path, label: str) -> Path:
    if not value.is_absolute() or ".." in value.parts:
        fail(f"{label}_path_invalid")
    try:
        value.relative_to(SAFE_ROOT)
    except ValueError as exc:
        raise ControlError(f"{label}_path_invalid") from exc
    return value


def stat_identity(value: os.stat_result) -> tuple[int, int, int, int, int, int]:
    return (
        value.st_dev, value.st_ino, value.st_mode, value.st_uid,
        value.st_size, value.st_mtime_ns,
    )


def directory_identity(value: os.stat_result) -> tuple[int, int, int, int]:
    return value.st_dev, value.st_ino, value.st_mode, value.st_uid


def directory_parts(path: Path) -> tuple[str, ...]:
    if not path.is_absolute() or ".." in path.parts:
        fail("path_component_unsafe")
    return tuple(part for part in path.parts if part != "/")


def validate_directory(value: os.stat_result) -> None:
    mode = stat.S_IMODE(value.st_mode)
    if (
        not stat.S_ISDIR(value.st_mode)
        or value.st_uid not in {0, os.geteuid()}
        or mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX | 0o022)
    ):
        fail("path_component_unsafe")


def open_directory_chain(parts: tuple[str, ...]) -> list[int]:
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    descriptors = [os.open("/", flags)]
    try:
        validate_directory(os.fstat(descriptors[0]))
        for part in parts:
            descriptor = os.open(part, flags, dir_fd=descriptors[-1])
            validate_directory(os.fstat(descriptor))
            descriptors.append(descriptor)
        return descriptors
    except Exception:
        for descriptor in reversed(descriptors):
            os.close(descriptor)
        raise


@contextmanager
def pinned_directory(path: Path):
    parts = directory_parts(path)
    descriptors: list[int] = []
    try:
        descriptors = open_directory_chain(parts)
        expected = tuple(directory_identity(os.fstat(item)) for item in descriptors)
        yield descriptors[-1]
        observed_descriptors = open_directory_chain(parts)
        try:
            observed = tuple(directory_identity(os.fstat(item)) for item in observed_descriptors)
            if observed != expected:
                fail("path_component_changed")
        finally:
            for descriptor in reversed(observed_descriptors):
                os.close(descriptor)
    except ControlError:
        raise
    except OSError as exc:
        raise ControlError("path_component_unsafe") from exc
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


def read_regular(path: Path, label: str, maximum: int = MAX_JSON_BYTES) -> bytes:
    if path.name in {"", ".", ".."}:
        fail(f"{label}_file_unsafe")
    with pinned_directory(path.parent) as parent_fd:
        fd = -1
        try:
            before = os.stat(path.name, dir_fd=parent_fd, follow_symlinks=False)
            mode = stat.S_IMODE(before.st_mode)
            if (
                not stat.S_ISREG(before.st_mode)
                or before.st_nlink != 1
                or before.st_uid not in {0, os.geteuid()}
                or mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX | 0o022)
            ):
                fail(f"{label}_file_unsafe")
            fd = os.open(
                path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC,
                dir_fd=parent_fd,
            )
            opened = os.fstat(fd)
            after = os.stat(path.name, dir_fd=parent_fd, follow_symlinks=False)
            if len({stat_identity(before), stat_identity(opened), stat_identity(after)}) != 1:
                fail(f"{label}_file_changed")
            chunks: list[bytes] = []
            total = 0
            while True:
                chunk = os.read(fd, min(1024 * 1024, maximum + 1 - total))
                if not chunk:
                    break
                total += len(chunk)
                if total > maximum:
                    fail(f"{label}_file_oversized")
                chunks.append(chunk)
            final = os.fstat(fd)
            path_final = os.stat(path.name, dir_fd=parent_fd, follow_symlinks=False)
            if len({stat_identity(opened), stat_identity(final), stat_identity(path_final)}) != 1:
                fail(f"{label}_file_changed")
            return b"".join(chunks)
        except ControlError:
            raise
        except OSError as exc:
            raise ControlError(f"{label}_file_unavailable") from exc
        finally:
            if fd >= 0:
                os.close(fd)


def strict_json(raw: bytes, label: str) -> Any:
    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=reject_duplicates)
    except ControlError:
        raise
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise ControlError(f"{label}_json_invalid") from exc


def load_json(path: Path, label: str) -> tuple[bytes, Any]:
    raw = read_regular(path, label)
    return raw, strict_json(raw, label)


def atomic_json(path: Path, value: Any) -> str:
    safe_output_path(path, "receipt")
    data = canonical(value)
    with pinned_directory(path.parent) as parent_fd:
        if stat.S_IMODE(os.fstat(parent_fd).st_mode) != 0o700:
            fail("receipt_parent_unsafe")
        temp_name = f".{path.name}.new"
        try:
            os.stat(temp_name, dir_fd=parent_fd, follow_symlinks=False)
            fail("receipt_temp_exists")
        except FileNotFoundError:
            pass
        fd = os.open(
            temp_name,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
            0o600,
            dir_fd=parent_fd,
        )
        try:
            written = 0
            while written < len(data):
                written += os.write(fd, data[written:])
            os.fsync(fd)
        finally:
            os.close(fd)
        try:
            os.link(
                temp_name, path.name, src_dir_fd=parent_fd,
                dst_dir_fd=parent_fd, follow_symlinks=False,
            )
        except FileExistsError as exc:
            os.unlink(temp_name, dir_fd=parent_fd)
            raise ControlError("receipt_exists") from exc
        except OSError as exc:
            os.unlink(temp_name, dir_fd=parent_fd)
            raise ControlError("receipt_write_failed") from exc
        os.unlink(temp_name, dir_fd=parent_fd)
        os.fsync(parent_fd)
    return digest_bytes(data)


@contextmanager
def controller_lock():
    safe_output_path(CONTROL_LOCK, "control_lock")
    with pinned_directory(CONTROL_LOCK.parent) as parent_fd:
        fd = -1
        try:
            fd = os.open(
                CONTROL_LOCK.name,
                os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC,
                0o600,
                dir_fd=parent_fd,
            )
            info = os.fstat(fd)
            if (
                not stat.S_ISREG(info.st_mode)
                or info.st_nlink != 1
                or info.st_uid != os.geteuid()
                or stat.S_IMODE(info.st_mode) != 0o600
            ):
                fail("control_lock_unsafe")
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as exc:
                raise ControlError("controller_busy") from exc
            yield
        except ControlError:
            raise
        except OSError as exc:
            raise ControlError("control_lock_unsafe") from exc
        finally:
            if fd >= 0:
                os.close(fd)


def write_receipt(path: Path, value: dict[str, Any]) -> str:
    unsigned_digest = digest(value)
    value["receipt_sha256"] = unsigned_digest
    atomic_json(path, value)
    return unsigned_digest


@dataclass(frozen=True)
class Result:
    returncode: int
    stdout: bytes = b""


Runner = Callable[[tuple[str, ...], float, dict[str, str]], Result]


def validate_command(argv: tuple[str, ...]) -> None:
    if not argv:
        fail("executable_not_allowed")
    if argv[0] == str(PYTHON):
        if len(argv) < 2 or argv[1] not in {
            str(PREFLIGHT_PROGRAM), str(JOURNEY_PROGRAM)
        }:
            fail("executable_not_allowed")
        return
    if argv[0] != str(SYSTEMCTL) or len(argv) < 2:
        fail("executable_not_allowed")
    verb = argv[1]
    if verb == "daemon-reload" and len(argv) == 2:
        return
    if verb == "start" and argv[2:] == ("--no-block", TARGET):
        return
    if verb == "show" and len(argv) == 5 and argv[2] in INSPECT_UNITS:
        return
    if verb == "stop" and len(argv) == 3 and argv[2] in INSPECT_UNITS:
        return
    if verb == "restart" and len(argv) == 3 and argv[2] in SERVICES:
        return
    fail("command_not_allowed")


def credential_environment_names(argv: tuple[str, ...]) -> tuple[str, ...]:
    if argv[:2] != (str(PYTHON), str(JOURNEY_PROGRAM)):
        return ()
    names: list[str] = []
    roles: set[str] = set()
    index = 2
    while index < len(argv):
        if argv[index] != "--credential":
            index += 1
            continue
        if index + 1 >= len(argv):
            fail("credential_reference_invalid")
        reference = argv[index + 1]
        if not CREDENTIAL_RE.fullmatch(reference):
            fail("credential_reference_invalid")
        role, name = reference.split("=", 1)
        if role in roles or name in names:
            fail("credential_reference_invalid")
        roles.add(role)
        names.append(name)
        index += 2
    return tuple(names)


def child_environment(argv: tuple[str, ...]) -> dict[str, str]:
    validate_command(argv)
    result = dict(BASE_CHILD_ENV)
    for name in credential_environment_names(argv):
        value = os.environ.get(name)
        if (
            value is None
            or not 1 <= len(value.encode("utf-8")) <= MAX_CREDENTIAL_BYTES
            or any(ord(character) < 0x20 or ord(character) == 0x7f for character in value)
        ):
            fail("credential_value_invalid")
        result[name] = value
    return result


def invoke(runner: Runner, argv: tuple[str, ...], timeout: float) -> Result:
    return runner(argv, timeout, child_environment(argv))


def production_runner(
    argv: tuple[str, ...], timeout: float, environment: dict[str, str]
) -> Result:
    validate_command(argv)
    if environment != child_environment(argv):
        fail("child_environment_invalid")
    try:
        process = subprocess.Popen(
            argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, shell=False, env=environment,
            start_new_session=True,
        )
    except OSError as exc:
        raise ControlError("command_failed") from exc
    selector = selectors.DefaultSelector()
    assert process.stdout is not None and process.stderr is not None
    selector.register(process.stdout, selectors.EVENT_READ, "stdout")
    selector.register(process.stderr, selectors.EVENT_READ, "stderr")
    chunks: list[bytes] = []
    total = 0
    deadline = time.monotonic() + timeout
    try:
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                fail("command_timeout")
            events = selector.select(remaining)
            if not events:
                fail("command_timeout")
            for key, _ in events:
                block = os.read(key.fileobj.fileno(), 65536)
                if not block:
                    selector.unregister(key.fileobj)
                    continue
                total += len(block)
                if total > MAX_OUTPUT_BYTES:
                    fail("command_output_oversized")
                if key.data == "stdout":
                    chunks.append(block)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            fail("command_timeout")
        return Result(process.wait(timeout=remaining), b"".join(chunks))
    except ControlError:
        terminate_process_group(process)
        raise
    except subprocess.TimeoutExpired as exc:
        terminate_process_group(process)
        raise ControlError("command_timeout") from exc
    except OSError as exc:
        terminate_process_group(process)
        raise ControlError("command_failed") from exc
    finally:
        selector.close()
        process.stdout.close()
        process.stderr.close()


def terminate_process_group(process: subprocess.Popen[bytes]) -> None:
    group = process.pid
    try:
        os.killpg(group, signal.SIGTERM)
    except ProcessLookupError:
        pass
    time.sleep(0.05)
    try:
        os.killpg(group, signal.SIGKILL)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=0.5)
    except subprocess.TimeoutExpired as exc:
        raise ControlError("command_tree_kill_failed") from exc


def validate_executables(systemctl: Path, python: Path, preflight: Path, journey: Path) -> None:
    if (systemctl, python, preflight, journey) != (
        SYSTEMCTL, PYTHON, PREFLIGHT_PROGRAM, JOURNEY_PROGRAM
    ):
        fail("executable_not_allowed")


def systemctl_show(runner: Runner, unit: str, timeout: float) -> dict[str, str]:
    if unit not in INSPECT_UNITS:
        fail("unit_not_allowed")
    properties = ("LoadState", "ActiveState", "SubState")
    if unit != TARGET:
        properties += ("Result",)
    result = invoke(runner, (
        str(SYSTEMCTL), "show", unit, f"--property={','.join(properties)}",
        "--no-pager",
    ), timeout)
    if result.returncode != 0:
        fail("unit_readback_failed")
    try:
        lines = result.stdout.decode("ascii").splitlines()
    except UnicodeError as exc:
        raise ControlError("unit_readback_invalid") from exc
    values: dict[str, str] = {}
    for line in lines:
        if line.count("=") != 1:
            fail("unit_readback_invalid")
        key, value = line.split("=", 1)
        if key in values:
            fail("unit_readback_invalid")
        values[key] = value
    if set(values) != set(properties):
        fail("unit_readback_invalid")
    return values


def unit_ready(unit: str, values: dict[str, str]) -> bool:
    if values["LoadState"] != "loaded":
        return False
    if unit == TARGET:
        return values["ActiveState"] == "active" and values["SubState"] == "active"
    if values["Result"] != "success":
        return False
    if unit in TIMERS:
        return values["ActiveState"] == "active" and values["SubState"] == "waiting"
    return values["ActiveState"] == "active" and values["SubState"] == "running"


def validate_provision_authority(
    receipt_path: Path, expected_receipt_sha: str, manifest_path: Path,
    expected_manifest_sha: str, expected_git_sha: str,
) -> dict[str, Any]:
    if not DIGEST_RE.fullmatch(expected_receipt_sha):
        fail("provision_receipt_digest_invalid")
    if not DIGEST_RE.fullmatch(expected_manifest_sha):
        fail("manifest_digest_invalid")
    if not SHA_RE.fullmatch(expected_git_sha):
        fail("git_sha_invalid")
    receipt_raw, receipt = load_json(receipt_path, "provision_receipt")
    manifest_raw = read_regular(manifest_path, "manifest")
    if digest_bytes(receipt_raw) != expected_receipt_sha:
        fail("provision_receipt_digest_mismatch")
    if digest_bytes(manifest_raw) != expected_manifest_sha:
        fail("manifest_digest_mismatch")
    if not isinstance(receipt, dict) or set(receipt) != {
        "schema_version", "status", "git_sha", "manifest_sha256",
        "artifact_count", "changed_count", "artifact_set_digest",
        "services_started",
    }:
        fail("provision_receipt_shape")
    if (
        receipt["schema_version"] != 1
        or receipt["status"] != "COMPLETE"
        or receipt["git_sha"] != expected_git_sha
        or receipt["manifest_sha256"] != expected_manifest_sha
        or receipt["artifact_count"] != 111
        or not isinstance(receipt["changed_count"], int)
        or not 0 <= receipt["changed_count"] <= 111
        or receipt["services_started"] is not False
        or not isinstance(receipt["artifact_set_digest"], str)
        or not DIGEST_RE.fullmatch(receipt["artifact_set_digest"])
    ):
        fail("provision_receipt_authority_mismatch")
    manifest = strict_json(manifest_raw, "manifest")
    if (
        not isinstance(manifest, dict)
        or manifest.get("version") != "1.0"
        or manifest.get("git_sha") != expected_git_sha
        or not isinstance(manifest.get("artifacts"), list)
        or len(manifest["artifacts"]) != 111
    ):
        fail("manifest_authority_mismatch")
    return receipt


@dataclass(frozen=True)
class PreflightArgs:
    manifest: Path
    contract: Path
    profile: Path
    agents_dir: Path
    operator_credential_file: Path
    expected_git_sha: str
    expected_manifest_sha256: str
    timeout: float


def preflight_command(value: PreflightArgs) -> tuple[str, ...]:
    return (
        str(PYTHON), str(PREFLIGHT_PROGRAM), "--manifest", str(value.manifest),
        "--contract", str(value.contract), "--profile", str(value.profile),
        "--agents-dir", str(value.agents_dir), "--operator-credential-file",
        str(value.operator_credential_file), "--expected-git-sha",
        value.expected_git_sha, "--expected-manifest-sha256",
        value.expected_manifest_sha256, "--timeout-seconds", str(value.timeout),
    )


def run_preflight_attempt(runner: Runner, value: PreflightArgs) -> tuple[str | None, bool]:
    result = invoke(runner, preflight_command(value), value.timeout)
    output = strict_json(result.stdout, "preflight")
    expected_keys = {
        "schema_version", "claim", "runtime_preflight_pass",
        "m0_acceptance_pass", "checks", "result_digest",
    }
    if (
        not isinstance(output, dict)
        or frozenset(output) not in {
            frozenset(expected_keys), frozenset({*expected_keys, "fatal_reason"})
        }
        or output.get("schema_version") != 1
        or output.get("m0_acceptance_pass") is not False
        or not isinstance(output.get("checks"), list)
        or not isinstance(output.get("result_digest"), str)
        or not DIGEST_RE.fullmatch(output["result_digest"])
    ):
        fail("readiness_failed")
    passed = output.get("runtime_preflight_pass") is True
    if passed:
        if result.returncode != 0 or output.get("claim") != "runtime_preflight_pass":
            fail("readiness_failed")
        return output["result_digest"], False
    if result.returncode == 0 or output.get("claim") != "runtime_preflight_fail":
        fail("readiness_failed")
    if "fatal_reason" in output:
        fail("readiness_failed")
    failed_reasons: list[str] = []
    for check in output["checks"]:
        if (
            not isinstance(check, dict)
            or set(check) != {"id", "status", "reason", "evidence_digest", "evidence"}
            or check.get("status") not in {"PASS", "FAIL"}
            or not isinstance(check.get("id"), str)
            or not isinstance(check.get("reason"), str)
            or not isinstance(check.get("evidence_digest"), str)
            or not DIGEST_RE.fullmatch(check["evidence_digest"])
            or not isinstance(check.get("evidence"), dict)
        ):
            fail("readiness_failed")
        if check["status"] == "FAIL":
            failed_reasons.append(check["reason"])
    if (
        not failed_reasons
        or any(reason not in TEMPORAL_PREFLIGHT_REASONS for reason in failed_reasons)
    ):
        fail("readiness_failed")
    return None, True


def run_preflight(runner: Runner, value: PreflightArgs) -> str:
    result, _ = run_preflight_attempt(runner, value)
    if result is None:
        fail("readiness_failed")
    return result


def unit_terminal_failure(unit: str, values: dict[str, str]) -> bool:
    return (
        values["LoadState"] != "loaded"
        or values["ActiveState"] == "failed"
        or (unit != TARGET and values.get("Result") == "failed")
    )


def oneshot_terminal_success(values: dict[str, str]) -> bool:
    return (
        values["LoadState"] == "loaded"
        and values["ActiveState"] == "inactive"
        and values["SubState"] == "dead"
        and values["Result"] == "success"
    )


def wait_for_activation(
    runner: Runner, preflight: PreflightArgs, command_timeout: float,
    deadline: float, monotonic: Callable[[], float],
    sleeper: Callable[[float], None],
) -> str:
    while True:
        remaining = deadline - monotonic()
        if remaining <= 0:
            fail("activation_timeout")
        per_command_timeout = min(command_timeout, remaining)
        all_ready = True
        for unit in ALL_UNITS:
            values = systemctl_show(runner, unit, per_command_timeout)
            if unit_terminal_failure(unit, values):
                fail("activation_unit_failed")
            if not unit_ready(unit, values):
                all_ready = False
        for unit in ONESHOTS:
            values = systemctl_show(runner, unit, per_command_timeout)
            if unit_terminal_failure(unit, values):
                fail("activation_oneshot_failed")
            if not oneshot_terminal_success(values):
                all_ready = False
        if all_ready:
            readiness_digest, retryable = run_preflight_attempt(runner, preflight)
            if readiness_digest is not None:
                return readiness_digest
            if not retryable:
                fail("readiness_failed")
        remaining = deadline - monotonic()
        if remaining <= 0:
            fail("activation_timeout")
        sleeper(min(ACTIVATION_POLL_SECONDS, remaining))


def _activate(
    runner: Runner, receipt_path: Path, expected_receipt_sha: str,
    manifest_path: Path, expected_manifest_sha: str, expected_git_sha: str,
    preflight: PreflightArgs, output_path: Path, timeout: float,
    activation_deadline: float, monotonic: Callable[[], float],
    sleeper: Callable[[float], None],
) -> dict[str, Any]:
    if not 0 < timeout <= MAX_TIMEOUT_SECONDS:
        fail("timeout_invalid")
    if (
        not timeout <= activation_deadline <= MAX_ACTIVATION_DEADLINE_SECONDS
    ):
        fail("activation_deadline_invalid")
    validate_executables(SYSTEMCTL, PYTHON, PREFLIGHT_PROGRAM, JOURNEY_PROGRAM)
    if (
        preflight.manifest != manifest_path
        or preflight.expected_manifest_sha256 != expected_manifest_sha
        or preflight.expected_git_sha != expected_git_sha
    ):
        fail("preflight_authority_mismatch")
    provision = validate_provision_authority(
        receipt_path, expected_receipt_sha, manifest_path,
        expected_manifest_sha, expected_git_sha,
    )
    for unit in INSPECT_UNITS:
        values = systemctl_show(runner, unit, timeout)
        if values["ActiveState"] != "inactive" or values.get("Result") == "failed":
            fail("unit_not_stopped_cleanly")
    reload_result = invoke(runner, (str(SYSTEMCTL), "daemon-reload"), timeout)
    if reload_result.returncode != 0:
        fail("daemon_reload_failed")
    invocation_units = list(ROLLBACK_ORDER)
    failure: str | None = None
    readiness_digest: str | None = None
    rollback_failed = False
    try:
        result = invoke(
            runner, (str(SYSTEMCTL), "start", "--no-block", TARGET), timeout
        )
        start_failed = result.returncode != 0
        if start_failed:
            fail("target_start_failed")
        readiness_digest = wait_for_activation(
            runner, preflight, timeout, monotonic() + activation_deadline,
            monotonic, sleeper,
        )
    except ControlError as exc:
        failure = exc.code
    if failure is not None:
        for unit in ROLLBACK_ORDER:
            try:
                result = invoke(runner, (str(SYSTEMCTL), "stop", unit), timeout)
            except ControlError:
                rollback_failed = True
                continue
            if result.returncode != 0:
                rollback_failed = True
            else:
                try:
                    values = systemctl_show(runner, unit, timeout)
                    if values["ActiveState"] != "inactive":
                        rollback_failed = True
                except ControlError:
                    rollback_failed = True
        receipt = {
            "schema_version": SCHEMA_VERSION,
            "status": "ROLLBACK_FAILED" if rollback_failed else "ROLLED_BACK",
            "reason": failure,
            "git_sha": expected_git_sha,
            "manifest_sha256": expected_manifest_sha,
            "provision_receipt_sha256": expected_receipt_sha,
            "started_unit_count": len(invocation_units),
            "readiness_digest": None,
            "m0_acceptance_pass": False,
        }
        write_receipt(output_path, receipt)
        raise ControlError("activation_rollback_failed" if rollback_failed else failure)
    receipt = {
        "schema_version": SCHEMA_VERSION, "status": "ACTIVE",
        "git_sha": expected_git_sha, "manifest_sha256": expected_manifest_sha,
        "provision_receipt_sha256": expected_receipt_sha,
        "provision_artifact_set_digest": provision["artifact_set_digest"],
        "started_unit_count": len(ALL_UNITS),
        "readiness_digest": readiness_digest,
        "m0_acceptance_pass": False,
    }
    write_receipt(output_path, receipt)
    return receipt


def activate(
    runner: Runner, receipt_path: Path, expected_receipt_sha: str,
    manifest_path: Path, expected_manifest_sha: str, expected_git_sha: str,
    preflight: PreflightArgs, output_path: Path, timeout: float,
    activation_deadline: float = DEFAULT_ACTIVATION_DEADLINE_SECONDS,
    monotonic: Callable[[], float] = time.monotonic,
    sleeper: Callable[[float], None] = time.sleep,
) -> dict[str, Any]:
    safe_output_path(output_path, "activation_receipt")
    if output_path.exists() or output_path.is_symlink():
        fail("activation_receipt_exists")
    try:
        with controller_lock():
            return _activate(
                runner, receipt_path, expected_receipt_sha, manifest_path,
                expected_manifest_sha, expected_git_sha, preflight, output_path,
                timeout, activation_deadline, monotonic, sleeper,
            )
    except Exception as exc:
        code = exc.code if isinstance(exc, ControlError) else "internal_failure"
        if not output_path.exists() and not output_path.is_symlink():
            failure_receipt = {
                "schema_version": SCHEMA_VERSION, "status": "FAILED",
                "reason": code, "started_unit_count": 0,
                "readiness_digest": None, "m0_acceptance_pass": False,
            }
            try:
                write_receipt(output_path, failure_receipt)
            except Exception:
                pass
        raise ControlError(code) from exc


def load_control_plan(path: Path, expected_sha: str, journey_plan_sha: str,
                      checkpoints: list[str]) -> dict[str, str]:
    if not DIGEST_RE.fullmatch(expected_sha) or not DIGEST_RE.fullmatch(journey_plan_sha):
        fail("control_digest_invalid")
    raw, value = load_json(path, "restart_control")
    if digest_bytes(raw) != expected_sha:
        fail("restart_control_digest_mismatch")
    if not isinstance(value, dict) or set(value) != {
        "schema_version", "journey_plan_sha256", "checkpoint_services"
    }:
        fail("restart_control_shape")
    mapping = value["checkpoint_services"]
    if (
        value["schema_version"] != 1
        or value["journey_plan_sha256"] != journey_plan_sha
        or not isinstance(mapping, dict)
        or set(mapping) != set(checkpoints)
        or any(not isinstance(unit, str) or unit not in SERVICES for unit in mapping.values())
    ):
        fail("restart_control_authority_mismatch")
    return {checkpoint: mapping[checkpoint] for checkpoint in checkpoints}


@dataclass(frozen=True)
class JourneyContract:
    raw_sha256: str
    module: Any
    plan: dict[str, Any]
    checkpoints: tuple[str, ...]
    step_ids: tuple[str, ...]


def load_journey_contract(plan_path: Path) -> JourneyContract:
    raw, plan = load_json(plan_path, "journey_plan")
    if not isinstance(plan, dict) or not isinstance(plan.get("steps"), list):
        fail("journey_plan_shape")
    try:
        spec = importlib.util.spec_from_file_location(
            "m0_activation_journey_ssot", JOURNEY_PROGRAM
        )
        if spec is None or spec.loader is None:
            fail("journey_contract_unavailable")
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        module.validate_plan(plan)
    except ControlError:
        raise
    except Exception as exc:
        raise ControlError("journey_plan_invalid") from exc
    checkpoints: list[str] = []
    seen: set[str] = set()
    for step in plan["steps"]:
        if not isinstance(step, dict):
            fail("journey_plan_shape")
        checkpoint = step.get("checkpoint")
        if checkpoint is not None:
            if not isinstance(checkpoint, str) or checkpoint in seen:
                fail("journey_checkpoint_invalid")
            seen.add(checkpoint)
            checkpoints.append(checkpoint)
    if not checkpoints:
        fail("journey_checkpoints_missing")
    return JourneyContract(
        digest_bytes(raw), module, plan, tuple(checkpoints),
        tuple(step["id"] for step in plan["steps"]),
    )


@dataclass(frozen=True)
class JourneyArgs:
    plan: Path
    base_url: str
    credentials: tuple[str, ...]
    ledger: Path
    evidence: Path
    timeout: float


def journey_command(value: JourneyArgs, checkpoint: str | None) -> tuple[str, ...]:
    argv = [
        str(PYTHON), str(JOURNEY_PROGRAM), "--plan", str(value.plan),
        "--base-url", value.base_url, "--ledger", str(value.ledger),
        "--evidence", str(value.evidence), "--timeout", str(value.timeout),
    ]
    for credential in value.credentials:
        if not CREDENTIAL_RE.fullmatch(credential):
            fail("credential_reference_invalid")
        argv.extend(("--credential", credential))
    if checkpoint is not None:
        argv.extend(("--stop-after-checkpoint", checkpoint))
    return tuple(argv)


def validate_journey_state(
    contract: JourneyContract, journey: JourneyArgs, expected_result: str,
    checkpoint: str | None, expected_completed: tuple[str, ...],
    expected_replayed: tuple[str, ...],
) -> tuple[str, str, dict[str, Any]]:
    plan_raw, plan_value = load_json(journey.plan, "journey_plan")
    ledger_raw, ledger_value = load_json(journey.ledger, "journey_ledger")
    evidence_raw, evidence_value = load_json(journey.evidence, "journey_evidence")
    if (
        digest_bytes(plan_raw) != contract.raw_sha256
        or plan_value != contract.plan
        or ledger_raw != canonical(ledger_value)
        or evidence_raw != canonical(evidence_value)
    ):
        fail("journey_state_noncanonical")
    module = contract.module
    original_load_json = module.load_json

    def pinned_load_json(path: Path, _label: str) -> dict[str, Any]:
        if path == journey.ledger and isinstance(ledger_value, dict):
            return ledger_value
        if path == journey.evidence and isinstance(evidence_value, dict):
            return evidence_value
        raise module.JourneyError("controller supplied an unknown state path")

    try:
        module.load_json = pinned_load_json
        normalized_origin = module.validate_base_url(journey.base_url)
        ledger = module.load_ledger(
            journey.ledger, contract.plan["schema_version"],
            module.digest(contract.plan), contract.plan["journey_id"],
            normalized_origin,
        )
        module.validate_completed_prefix(
            contract.plan, ledger["completed"], ledger["chain_tip"]
        )
        module.validate_evidence_binding(journey.evidence, ledger, contract.plan)
        if (
            len(ledger["completed"]) != len(expected_completed)
            or set(ledger["completed"]) != set(expected_completed)
        ):
            fail("journey_completed_prefix_mismatch")
        expected_evidence = module.build_evidence(
            contract.plan, ledger, expected_result, checkpoint,
            set(expected_replayed),
        )
        if evidence_value != expected_evidence:
            fail("journey_evidence_mismatch")
    except ControlError:
        raise
    except Exception as exc:
        raise ControlError("journey_state_invalid") from exc
    finally:
        module.load_json = original_load_json
    return digest_bytes(ledger_raw), digest_bytes(evidence_raw), evidence_value


def wait_service(runner: Runner, unit: str, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while True:
        values = systemctl_show(runner, unit, min(timeout, 5.0))
        if unit_ready(unit, values):
            return
        if time.monotonic() >= deadline:
            fail("restart_timeout")
        time.sleep(min(0.05, max(0.0, deadline - time.monotonic())))


def _restart_journey(
    runner: Runner, journey: JourneyArgs, control_path: Path,
    expected_control_sha: str, preflight: PreflightArgs, output_path: Path,
) -> dict[str, Any]:
    validate_executables(SYSTEMCTL, PYTHON, PREFLIGHT_PROGRAM, JOURNEY_PROGRAM)
    if not 0 < journey.timeout <= MAX_TIMEOUT_SECONDS:
        fail("timeout_invalid")
    safe_output_path(output_path, "restart_receipt")
    if output_path.exists() or output_path.is_symlink():
        fail("restart_receipt_exists")
    safe_output_path(journey.ledger, "ledger")
    safe_output_path(journey.evidence, "evidence")
    contract = load_journey_contract(journey.plan)
    checkpoints = list(contract.checkpoints)
    mapping = load_control_plan(
        control_path, expected_control_sha, contract.raw_sha256, checkpoints
    )
    records: list[dict[str, str]] = []
    previously_completed: tuple[str, ...] = ()
    for checkpoint in checkpoints:
        result = invoke(runner, journey_command(journey, checkpoint), journey.timeout)
        if result.returncode != 0:
            fail("journey_checkpoint_failed")
        checkpoint_index = next(
            index for index, step in enumerate(contract.plan["steps"])
            if step.get("checkpoint") == checkpoint
        )
        expected_completed = contract.step_ids[:checkpoint_index + 1]
        ledger_before, evidence_before, _ = validate_journey_state(
            contract, journey, "checkpoint_reached", checkpoint,
            expected_completed, previously_completed,
        )
        unit = mapping[checkpoint]
        result = invoke(runner, (str(SYSTEMCTL), "restart", unit), journey.timeout)
        if result.returncode != 0:
            fail("restart_failed")
        wait_service(runner, unit, journey.timeout)
        readiness = run_preflight(runner, preflight)
        if digest_bytes(read_regular(journey.ledger, "journey_ledger")) != ledger_before:
            fail("ledger_changed_during_restart")
        if digest_bytes(read_regular(journey.evidence, "journey_evidence")) != evidence_before:
            fail("evidence_changed_during_restart")
        records.append({"checkpoint": checkpoint, "unit": unit, "readiness_digest": readiness,
                        "ledger_digest": ledger_before, "evidence_digest": evidence_before})
        previously_completed = expected_completed
    result = invoke(runner, journey_command(journey, None), journey.timeout)
    if result.returncode != 0:
        fail("journey_resume_failed")
    validate_journey_state(
        contract, journey, "complete", None, contract.step_ids,
        previously_completed,
    )
    result = invoke(runner, journey_command(journey, None), journey.timeout)
    if result.returncode != 0:
        fail("journey_replay_failed")
    _, _, final = validate_journey_state(
        contract, journey, "complete", None, contract.step_ids,
        contract.step_ids,
    )
    receipt = {
        "schema_version": SCHEMA_VERSION, "status": "COMPLETE",
        "journey_plan_sha256": contract.raw_sha256,
        "restart_control_sha256": expected_control_sha,
        "checkpoint_count": len(records), "checkpoint_digest": digest(records),
        "final_record_chain_tip": final["record_chain_tip"],
        "authoritative_replay_verified": True, "m0_acceptance_pass": False,
    }
    write_receipt(output_path, receipt)
    return receipt


def restart_journey(
    runner: Runner, journey: JourneyArgs, control_path: Path,
    expected_control_sha: str, preflight: PreflightArgs, output_path: Path,
) -> dict[str, Any]:
    with controller_lock():
        return _restart_journey(
            runner, journey, control_path, expected_control_sha, preflight,
            output_path,
        )


def add_preflight(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--contract", type=Path, required=True)
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--agents-dir", type=Path, required=True)
    parser.add_argument("--operator-credential-file", type=Path, required=True)
    parser.add_argument("--expected-git-sha", required=True)
    parser.add_argument("--expected-manifest-sha256", required=True)
    parser.add_argument("--timeout", type=float, default=5.0)


def parsed_preflight(args: argparse.Namespace) -> PreflightArgs:
    return PreflightArgs(
        args.manifest, args.contract, args.profile, args.agents_dir,
        args.operator_credential_file, args.expected_git_sha,
        args.expected_manifest_sha256, args.timeout,
    )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    activation = commands.add_parser("activate")
    add_preflight(activation)
    activation.add_argument("--provision-receipt", type=Path, required=True)
    activation.add_argument("--expected-provision-receipt-sha256", required=True)
    activation.add_argument("--output", type=Path, required=True)
    activation.add_argument(
        "--activation-deadline-seconds", type=float,
        default=DEFAULT_ACTIVATION_DEADLINE_SECONDS,
    )
    restart = commands.add_parser("restart-journey")
    add_preflight(restart)
    restart.add_argument("--plan", type=Path, required=True)
    restart.add_argument("--base-url", required=True)
    restart.add_argument("--credential", action="append", default=[])
    restart.add_argument("--ledger", type=Path, required=True)
    restart.add_argument("--evidence", type=Path, required=True)
    restart.add_argument("--restart-control", type=Path, required=True)
    restart.add_argument("--expected-restart-control-sha256", required=True)
    restart.add_argument("--output", type=Path, required=True)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None, runner: Runner = production_runner) -> int:
    try:
        args = parse_args(sys.argv[1:] if argv is None else argv)
        preflight = parsed_preflight(args)
        if args.command == "activate":
            result = activate(
                runner, args.provision_receipt,
                args.expected_provision_receipt_sha256, args.manifest,
                args.expected_manifest_sha256, args.expected_git_sha,
                preflight, args.output, args.timeout,
                args.activation_deadline_seconds,
            )
        else:
            journey = JourneyArgs(
                args.plan, args.base_url, tuple(args.credential), args.ledger,
                args.evidence, args.timeout,
            )
            result = restart_journey(
                runner, journey, args.restart_control,
                args.expected_restart_control_sha256, preflight, args.output,
            )
    except Exception as exc:
        code = exc.code if isinstance(exc, ControlError) else "internal_failure"
        sys.stderr.buffer.write(public_failure(code))
        return 1
    sys.stdout.buffer.write(canonical(result))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
