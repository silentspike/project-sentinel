#!/usr/bin/env python3
"""Deterministic tests for the M0 boot-readiness helper and unit topology."""

from __future__ import annotations

from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
from types import SimpleNamespace
import unittest
from unittest import mock


HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parents[2]


def load_helper():
    spec = importlib.util.spec_from_file_location("m0_boot_readiness", HERE / "readiness.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


readiness = load_helper()
SECRET = "m0-operator-credential-with-safe-length"


class FakeClock:
    def __init__(self) -> None:
        self.now = 0.0

    def __call__(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.now += seconds


def valid_agent(agent_id: int) -> dict[str, object]:
    return {
        "agent_id": agent_id,
        "aggregate_id": f"AGENT-{agent_id:02}",
        "runtime_present": True,
        "projection_present": True,
        "tracked_pid": 1000 + agent_id,
        "tracked_pid_alive": True,
        "tracked_pid_state": "S",
        "cgroup_live_pid_count": 1,
        "security_runtime_present": True,
        "adapter_handle_present": True,
        "adapter_instance_matches": True,
        "runtime_resources_healthy": True,
        "adapter_health_state": "healthy",
        "adapter_observation_error": None,
        "logical_status": "Active",
        "last_repair_status": "healthy",
    }


def valid_runtime() -> dict[str, object]:
    return {
        "expected_active_agents": 2,
        "runtime_agents": 2,
        "projection_agents": 2,
        "security_runtime_entries": 2,
        "tracked_processes": 2,
        "live_cgroup_dirs": 2,
        "sandbox_handles": 2,
        "stale_runtime_entries": 0,
        "orphan_cgroups": 0,
        "zombie_tracked_pids": 0,
        "projection_drift_detected": False,
        "projection_drift_agents": 0,
        "respawn_failures": 0,
        "operator_auth_required": True,
        "repair_last_status": "healthy",
        "last_repair_error": None,
        "worker_states": {
            "ecs_tick_loop": {"running": True, "restart_count": 0, "last_error": None},
            "service_health": {"running": True, "restart_count": 0, "last_error": None},
        },
        "agents": [valid_agent(1), valid_agent(2)],
    }


def validate_topology(units: dict[str, str], health_script: str) -> None:
    nats = units["nats-server.service"]
    daemon = units["sentinel-daemon.service"]
    bridge = units["sentinel-nats-bridge.service"]
    judge = units["sentinel-judge.service"]
    health_service = units["sentinel-health-monitor.service"]
    nightrun_service = units["sentinel-nightrun.service"]
    nightrun_timer = units["sentinel-nightrun.timer"]
    if "m0-readiness.py nats --timeout-seconds 285" not in nats:
        raise ValueError("nats_gate_missing")
    if "TimeoutStartSec=300" not in nats:
        raise ValueError("nats_deadline_missing")
    if "After=network-online.target nats-server.service" not in daemon:
        raise ValueError("daemon_nats_order_missing")
    if "Type=exec" not in daemon:
        raise ValueError("daemon_exec_readiness_race")
    if "Requires=nats-server.service" not in daemon:
        raise ValueError("daemon_nats_requirement_missing")
    if "Environment=SENTINEL_OPERATOR_CREDENTIAL_FILE=%d/operator-api" not in daemon:
        raise ValueError("daemon_operator_credential_binding_missing")
    if (
        "m0-readiness.py daemon --credential-environment "
        "SENTINEL_OPERATOR_CREDENTIAL_FILE"
    ) not in daemon:
        raise ValueError("daemon_operator_credential_gate_missing")
    if "sentinel-projection.service" in daemon:
        raise ValueError("daemon_projection_cycle")
    if "Requires=nats-server.service sentinel-daemon.service" not in bridge:
        raise ValueError("bridge_readiness_dependency_missing")
    if "Requires=nats-server.service sentinel-daemon.service" not in judge:
        raise ValueError("judge_readiness_dependency_missing")
    expected_health_after = (
        "After=nats-server.service sentinel-daemon.service "
        "sentinel-projection.service sentinel-gateway.service "
        "sentinel-dashboard-backend.service sentinel-nats-bridge.service "
        "sentinel-judge.service"
    )
    if expected_health_after not in health_service:
        raise ValueError("health_service_order_missing")
    if expected_health_after not in units["sentinel-health-monitor.timer"]:
        raise ValueError("health_timer_order_missing")
    if "ExecCondition=" in health_service:
        raise ValueError("health_monitor_blind_spot")
    if (
        "m0-readiness.py nightrun --credential-environment "
        "SENTINEL_OPERATOR_CREDENTIAL_FILE"
    ) not in nightrun_service:
        raise ValueError("nightrun_credential_gate_missing")
    if "ExecCondition=/usr/bin/systemctl --quiet is-active sentinel-daemon.service" not in nightrun_service:
        raise ValueError("nightrun_boot_gate_missing")
    if "After=sentinel-daemon.service" not in nightrun_timer:
        raise ValueError("nightrun_timer_order_missing")
    if any("After=sentinel.target" in text for text in units.values()):
        raise ValueError("target_cycle")
    if "nats:nats-server.service:nats:::5:observe" not in health_script:
        raise ValueError("nats_restart_policy_invalid")


class ResponseServer(ThreadingHTTPServer):
    response_status = 200
    response_statuses: list[int]
    response_type = "application/json"
    response_body = b'{"status":"ok"}'
    request_count = 0


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 - stdlib hook
        self.server.request_count += 1
        status = (
            self.server.response_statuses.pop(0)
            if self.server.response_statuses
            else self.server.response_status
        )
        self.send_response(status)
        self.send_header("Content-Type", self.server.response_type)
        self.send_header("Content-Length", str(len(self.server.response_body)))
        self.end_headers()
        self.wfile.write(self.server.response_body)

    def log_message(self, _format: str, *_args: object) -> None:
        return


@contextmanager
def response_server():
    server = ResponseServer(("127.0.0.1", 0), Handler)
    server.response_statuses = []
    server.request_count = 0
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


class ReadinessTests(unittest.TestCase):
    def setUp(self) -> None:
        root = Path(os.environ.get("RUNNER_TEMP", "/work/tmp/project-sentinel"))
        root.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="cdx1-650-readiness-", dir=root)
        self.credential = Path(self.temp.name) / "operator-api"
        self.credential.write_text(SECRET, encoding="utf-8")
        self.credential.chmod(0o600)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def assert_code(self, code: str, action) -> None:
        with self.assertRaises(readiness.ReadinessError) as context:
            action()
        self.assertEqual(context.exception.code, code)
        self.assertNotIn(SECRET, str(context.exception))

    def test_nats_uses_exact_jetstream_readiness_endpoint(self) -> None:
        observed: list[tuple[object, ...]] = []

        def fake(*args, **_kwargs):
            observed.append(args)
            return {"status": "ok"}

        result = readiness.check_nats(285, fake)
        self.assertEqual(observed, [("GET", "127.0.0.1", 8222, "/healthz?js-enabled-only=true", 5.0)])
        self.assertRegex(result["endpoint_digest"], r"^[0-9a-f]{64}$")

    def test_nats_rejects_non_ready_payload(self) -> None:
        clock = FakeClock()
        self.assert_code(
            "nats_not_ready",
            lambda: readiness.check_nats(
                2,
                lambda *_args, **_kwargs: {"status": "error"},
                clock=clock,
                sleeper=clock.sleep,
            ),
        )

    def test_nats_waits_through_observed_recovery_without_real_sleep(self) -> None:
        clock = FakeClock()

        def fake(*_args, **_kwargs):
            if clock.now < 149:
                raise readiness.ReadinessError("http_status_transient")
            return {"status": "ok"}

        result = readiness.check_nats(285, fake, clock=clock, sleeper=clock.sleep)
        self.assertGreaterEqual(clock.now, 149)
        self.assertRegex(result["endpoint_digest"], r"^[0-9a-f]{64}$")

    def test_daemon_accepts_authenticated_nonzero_local_runtime(self) -> None:
        observed: dict[str, object] = {}

        def fake(*args, **kwargs):
            observed["args"] = args
            observed["credential"] = kwargs["credential"]
            return valid_runtime()

        result = readiness.check_daemon(120, self.credential, fake)
        self.assertEqual(result["expected_active_agents"], 2)
        self.assertEqual(observed["credential"], SECRET)
        self.assertEqual(
            observed["args"],
            ("GET", "127.0.0.1", 8084, "/operator/runtime-health", 5.0),
        )

    def test_daemon_retries_zero_or_mismatched_local_counts(self) -> None:
        for field, value in (
            ("expected_active_agents", 0),
            ("runtime_agents", 1),
            ("security_runtime_entries", 1),
            ("tracked_processes", 1),
            ("live_cgroup_dirs", 1),
            ("sandbox_handles", 1),
        ):
            with self.subTest(field=field):
                payload = valid_runtime()
                payload[field] = value
                self.assert_code(
                    "daemon_not_initialized",
                    lambda payload=payload: readiness.validate_daemon_payload(payload),
                )

    def test_daemon_local_gate_ignores_projection_and_composite_drift(self) -> None:
        payload = valid_runtime()
        payload.update(
            projection_agents=0,
            projection_drift_detected=True,
            projection_drift_agents=2,
            stale_runtime_entries=2,
            repair_last_status="drift_detected",
        )
        for agent in payload["agents"]:
            agent["projection_present"] = False
        self.assertEqual(
            readiness.validate_daemon_payload(payload)["expected_active_agents"], 2
        )

    def test_daemon_waits_for_local_runtime_convergence(self) -> None:
        clock = FakeClock()

        def fake(*_args, **_kwargs):
            payload = valid_runtime()
            if clock.now < 17:
                payload["tracked_processes"] = 1
            return payload

        result = readiness.check_daemon(
            120,
            self.credential,
            fake,
            clock=clock,
            sleeper=clock.sleep,
        )
        self.assertGreaterEqual(clock.now, 17)
        self.assertEqual(result["expected_active_agents"], 2)

    def test_daemon_retries_local_orphan_process_and_repair_failures(self) -> None:
        for field, value in (
            ("orphan_cgroups", 1),
            ("zombie_tracked_pids", 1),
            ("respawn_failures", 1),
        ):
            with self.subTest(field=field):
                payload = valid_runtime()
                payload[field] = value
                self.assert_code(
                    "daemon_not_initialized",
                    lambda payload=payload: readiness.validate_daemon_payload(payload),
                )
        payload = valid_runtime()
        payload["last_repair_error"] = "bounded-error"
        self.assert_code(
            "daemon_not_initialized",
            lambda: readiness.validate_daemon_payload(payload),
        )

    def test_daemon_rejects_auth_schema_and_unknown_worker_but_retries_partial(self) -> None:
        cases = []
        auth = valid_runtime()
        auth["operator_auth_required"] = False
        cases.append((auth, "daemon_auth_disabled"))
        schema = valid_runtime()
        schema["expected_active_agents"] = "two"
        cases.append((schema, "daemon_count_invalid"))
        unknown = valid_runtime()
        unknown["worker_states"]["foreign"] = {
            "running": True,
            "restart_count": 0,
            "last_error": None,
        }
        cases.append((unknown, "daemon_worker_state_invalid"))
        for payload, code in cases:
            with self.subTest(code=code):
                self.assert_code(code, lambda payload=payload: readiness.validate_daemon_payload(payload))

        for workers in ({}, {"ecs_tick_loop": valid_runtime()["worker_states"]["ecs_tick_loop"]}):
            payload = valid_runtime()
            payload["worker_states"] = workers
            self.assert_code(
                "daemon_not_initialized",
                lambda payload=payload: readiness.validate_daemon_payload(payload),
            )

    def test_daemon_accepts_only_runtime_core_status_pairs(self) -> None:
        sleeping = valid_runtime()
        sleeping["agents"][0]["logical_status"] = "Sleeping"
        self.assertEqual(readiness.validate_daemon_payload(sleeping)["expected_active_agents"], 2)

        suspended = valid_runtime()
        suspended["agents"][0].update(
            logical_status="Suspended",
            adapter_health_state="degraded",
            last_repair_status="suspended",
        )
        self.assertEqual(readiness.validate_daemon_payload(suspended)["expected_active_agents"], 2)

        for logical_status, health, repair in (
            ("Errored", "healthy", "healthy"),
            (None, "healthy", "healthy"),
            ("Active", "degraded", "degraded"),
            ("Suspended", "degraded", "healthy"),
        ):
            with self.subTest(logical_status=logical_status, health=health, repair=repair):
                payload = valid_runtime()
                payload["agents"][0].update(
                    logical_status=logical_status,
                    adapter_health_state=health,
                    last_repair_status=repair,
                )
                self.assert_code(
                    "daemon_not_initialized",
                    lambda payload=payload: readiness.validate_daemon_payload(payload),
                )

    def test_daemon_temporal_initialization_retries_but_schema_fails_once(self) -> None:
        clock = FakeClock()
        calls = 0

        def temporal(*_args, **_kwargs):
            nonlocal calls
            calls += 1
            payload = valid_runtime()
            if calls == 1:
                payload["expected_active_agents"] = 0
                payload["worker_states"] = {}
            return payload

        self.assertEqual(
            readiness.check_daemon(
                5, self.credential, temporal, clock=clock, sleeper=clock.sleep
            )["expected_active_agents"],
            2,
        )
        self.assertEqual(calls, 2)

        permanent_clock = FakeClock()
        self.assert_code(
            "daemon_not_initialized",
            lambda: readiness.check_daemon(
                2,
                self.credential,
                lambda *_args, **_kwargs: {**valid_runtime(), "expected_active_agents": 0},
                clock=permanent_clock,
                sleeper=permanent_clock.sleep,
            ),
        )

        fatal_calls = 0

        def fatal(*_args, **_kwargs):
            nonlocal fatal_calls
            fatal_calls += 1
            return {**valid_runtime(), "expected_active_agents": "two"}

        self.assert_code(
            "daemon_count_invalid",
            lambda: readiness.check_daemon(5, self.credential, fatal),
        )
        self.assertEqual(fatal_calls, 1)

    def test_nightrun_reads_credential_and_never_places_value_in_request_path(self) -> None:
        observed: dict[str, object] = {}

        def fake(*args, **kwargs):
            observed.update(args=args, kwargs=kwargs)
            return {"accepted": True, "agents_queued": 2, "message": "queued"}

        self.assertEqual(
            readiness.trigger_nightrun(45, self.credential, fake), {"agents_queued": 2}
        )
        self.assertEqual(observed["args"][:4], ("POST", "127.0.0.1", 8084, "/operator/nightrun"))
        self.assertEqual(observed["kwargs"]["credential"], SECRET)
        self.assertNotIn(SECRET, " ".join(str(item) for item in observed["args"]))

    def test_credential_rejects_symlink_permissive_mode_and_control_bytes(self) -> None:
        link = Path(self.temp.name) / "link"
        link.symlink_to(self.credential)
        self.assert_code("credential_unavailable", lambda: readiness.read_credential(link))
        self.credential.chmod(0o644)
        self.assert_code("credential_authority_invalid", lambda: readiness.read_credential(self.credential))
        self.credential.chmod(0o600)
        self.credential.write_bytes(b"x" * 32 + b"\n")
        self.assert_code("credential_invalid", lambda: readiness.read_credential(self.credential))

    def test_systemd_credential_path_resolves_from_fixed_environment_binding(self) -> None:
        resolved = readiness.resolve_credential_path(
            None,
            "SENTINEL_OPERATOR_CREDENTIAL_FILE",
            environment={"SENTINEL_OPERATOR_CREDENTIAL_FILE": str(self.credential)},
        )
        self.assertEqual(resolved, self.credential)
        self.assertEqual(readiness.read_credential(resolved), SECRET)

        self.assert_code(
            "credential_unavailable",
            lambda: readiness.resolve_credential_path(
                None, "SENTINEL_OPERATOR_CREDENTIAL_FILE", environment={}
            ),
        )
        self.assert_code(
            "credential_path_invalid",
            lambda: readiness.resolve_credential_path(
                None,
                "SENTINEL_OPERATOR_CREDENTIAL_FILE",
                environment={"SENTINEL_OPERATOR_CREDENTIAL_FILE": "relative"},
            ),
        )
        self.assert_code(
            "arguments_invalid",
            lambda: readiness.resolve_credential_path(
                self.credential,
                "SENTINEL_OPERATOR_CREDENTIAL_FILE",
                environment={"SENTINEL_OPERATOR_CREDENTIAL_FILE": str(self.credential)},
            ),
        )
        self.assert_code(
            "arguments_invalid",
            lambda: readiness.resolve_credential_path(
                None,
                "UNTRUSTED_CREDENTIAL_FILE",
                environment={"UNTRUSTED_CREDENTIAL_FILE": str(self.credential)},
            ),
        )

    def test_credential_rejects_fifo_short_trailing_owner_and_parent_mode(self) -> None:
        fifo = Path(self.temp.name) / "credential-fifo"
        os.mkfifo(fifo, 0o600)
        self.assert_code("credential_authority_invalid", lambda: readiness.read_credential(fifo))

        reads = iter((b"x", b""))
        self.assert_code(
            "credential_short_read",
            lambda: readiness._read_declared(7, 2, lambda _fd, _size: next(reads)),
        )
        reads = iter((b"xx", b"y"))
        self.assert_code(
            "credential_trailing_data",
            lambda: readiness._read_declared(7, 2, lambda _fd, _size: next(reads)),
        )

        actual = os.stat(self.credential)
        foreign_owner = SimpleNamespace(
            st_mode=actual.st_mode,
            st_nlink=actual.st_nlink,
            st_uid=actual.st_uid + 1,
            st_size=actual.st_size,
        )
        self.assert_code(
            "credential_authority_invalid",
            lambda: readiness._validate_credential_metadata(foreign_owner),
        )
        before = SimpleNamespace(
            st_dev=actual.st_dev,
            st_ino=actual.st_ino,
            st_uid=actual.st_uid,
            st_mode=actual.st_mode,
            st_nlink=actual.st_nlink,
            st_size=actual.st_size,
            st_mtime_ns=actual.st_mtime_ns,
            st_ctime_ns=actual.st_ctime_ns,
        )
        changed_owner = SimpleNamespace(**vars(before))
        changed_owner.st_uid += 1
        self.assert_code(
            "credential_changed",
            lambda: readiness._require_unchanged(before, changed_owner),
        )

        unsafe_parent = Path(self.temp.name) / "unsafe-parent"
        unsafe_parent.mkdir(mode=0o700)
        child = unsafe_parent / "operator-api"
        child.write_text(SECRET, encoding="utf-8")
        child.chmod(0o600)
        unsafe_parent.chmod(0o777)
        self.assert_code(
            "credential_path_authority_invalid",
            lambda: readiness.read_credential(child),
        )

    def test_credential_rejects_same_inode_content_mode_and_link_changes(self) -> None:
        original_read = readiness._read_declared

        def assert_mutation_rejected(mutate) -> None:
            calls = 0

            def mutating_read(descriptor, expected_size, reader=None):
                nonlocal calls
                data = original_read(descriptor, expected_size, reader)
                calls += 1
                if calls == 1:
                    mutate()
                return data

            with mock.patch.object(readiness, "_read_declared", side_effect=mutating_read):
                self.assert_code(
                    "credential_changed",
                    lambda: readiness.read_credential(self.credential),
                )

        replacement = "z" * len(SECRET)
        assert_mutation_rejected(
            lambda: self.credential.write_text(replacement, encoding="utf-8")
        )
        self.credential.write_text(SECRET, encoding="utf-8")
        self.credential.chmod(0o600)

        assert_mutation_rejected(lambda: self.credential.chmod(0o400))
        self.credential.chmod(0o600)

        hardlink = Path(self.temp.name) / "credential-hardlink"
        assert_mutation_rejected(lambda: os.link(self.credential, hardlink))
        hardlink.unlink()

        sealed_reads = iter((SECRET.encode("utf-8"), b"z" * len(SECRET)))
        with mock.patch.object(
            readiness, "_read_declared", side_effect=lambda *_args: next(sealed_reads)
        ):
            self.assert_code(
                "credential_changed",
                lambda: readiness.read_credential(self.credential),
            )

    def test_credential_rejects_symlinked_parent_and_cli_error_is_public_safe(self) -> None:
        real = Path(self.temp.name) / "real"
        real.mkdir(mode=0o700)
        nested = real / "operator-api"
        nested.write_text(SECRET, encoding="utf-8")
        nested.chmod(0o600)
        alias = Path(self.temp.name) / "alias"
        alias.symlink_to(real, target_is_directory=True)
        self.assert_code("credential_unavailable", lambda: readiness.read_credential(alias / "operator-api"))

        self.credential.chmod(0o644)
        result = subprocess.run(
            [
                sys.executable,
                str(HERE / "readiness.py"),
                "daemon",
                "--credential-file",
                str(self.credential),
                "--timeout-seconds",
                "1",
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=5,
            check=False,
        )
        self.assertEqual(result.returncode, 1)
        failure = json.loads(result.stderr)
        self.assertEqual(failure["reason"], "credential_authority_invalid")
        self.assertNotIn(SECRET.encode(), result.stdout + result.stderr)
        self.assertNotIn(str(self.credential).encode(), result.stdout + result.stderr)

    def test_http_rejects_redirect_wrong_type_oversize_and_duplicate_json(self) -> None:
        with response_server() as server:
            port = server.server_address[1]
            server.response_status = 302
            self.assert_code(
                "http_status_fatal",
                lambda: readiness.request_json("GET", "127.0.0.1", port, "/", 2),
            )
            server.response_status = 200
            server.response_type = "text/plain"
            self.assert_code(
                "http_content_type",
                lambda: readiness.request_json("GET", "127.0.0.1", port, "/", 2),
            )
            server.response_type = "application/json"
            server.response_body = b"x" * (readiness.MAX_HTTP_BYTES + 1)
            self.assert_code(
                "http_body_oversized",
                lambda: readiness.request_json("GET", "127.0.0.1", port, "/", 2),
            )
            server.response_body = b'{"status":"ok","status":"ok"}'
            self.assert_code(
                "json_duplicate_key",
                lambda: readiness.request_json("GET", "127.0.0.1", port, "/", 2),
            )

    def test_http_status_categories_and_retry_policy_are_exact(self) -> None:
        with response_server() as server:
            port = server.server_address[1]
            for status, code in (
                (401, "http_auth_rejected"),
                (403, "http_auth_rejected"),
                (404, "http_endpoint_missing"),
                (409, "http_status_fatal"),
                (503, "http_status_transient"),
            ):
                with self.subTest(status=status):
                    before = server.request_count
                    server.response_status = status
                    self.assert_code(
                        code,
                        lambda: readiness.request_json(
                            "GET", "127.0.0.1", port, "/", 2
                        ),
                    )
                    self.assertEqual(server.request_count, before + 1)

        daemon_calls = 0

        def daemon_unauthorized(*_args, **_kwargs):
            nonlocal daemon_calls
            daemon_calls += 1
            raise readiness.ReadinessError("http_auth_rejected")

        self.assert_code(
            "http_auth_rejected",
            lambda: readiness.check_daemon(5, self.credential, daemon_unauthorized),
        )
        self.assertEqual(daemon_calls, 1)

        nats_calls = 0

        def nats_missing(*_args, **_kwargs):
            nonlocal nats_calls
            nats_calls += 1
            raise readiness.ReadinessError("http_endpoint_missing")

        self.assert_code(
            "http_endpoint_missing", lambda: readiness.check_nats(5, nats_missing)
        )
        self.assertEqual(nats_calls, 1)

        clock = FakeClock()
        calls = 0

        def recovering(*_args, **_kwargs):
            nonlocal calls
            calls += 1
            if calls < 3:
                raise readiness.ReadinessError("http_status_transient")
            return {"status": "ok"}

        readiness.check_nats(5, recovering, clock=clock, sleeper=clock.sleep)
        self.assertEqual(calls, 3)


class TopologyTests(unittest.TestCase):
    def unit(self, name: str) -> str:
        return (REPO_ROOT / "deploy/systemd" / name).read_text(encoding="utf-8")

    def units(self) -> dict[str, str]:
        names = (
            "nats-server.service",
            "sentinel-daemon.service",
            "sentinel-nats-bridge.service",
            "sentinel-judge.service",
            "sentinel-nightrun.service",
            "sentinel-nightrun.timer",
            "sentinel-health-monitor.service",
            "sentinel-health-monitor.timer",
            "sentinel.target",
        )
        return {name: self.unit(name) for name in names}

    def test_nats_and_daemon_have_acyclic_local_readiness_gates(self) -> None:
        nats = self.unit("nats-server.service")
        daemon = self.unit("sentinel-daemon.service")
        self.assertIn("m0-readiness.py nats --timeout-seconds 285", nats)
        self.assertIn("TimeoutStartSec=300", nats)
        self.assertIn("After=network-online.target nats-server.service", daemon)
        self.assertIn("Requires=nats-server.service", daemon)
        self.assertIn(
            "m0-readiness.py daemon --credential-environment "
            "SENTINEL_OPERATOR_CREDENTIAL_FILE",
            daemon,
        )
        self.assertIn("LoadCredential=operator-api:/etc/sentinel/credentials/operator-api", daemon)
        self.assertIn("Environment=SENTINEL_OPERATOR_CREDENTIAL_FILE=%d/operator-api", daemon)
        self.assertNotIn("sentinel-projection.service", daemon)

    def test_bridge_and_judge_require_ready_nats_and_daemon(self) -> None:
        bridge = self.unit("sentinel-nats-bridge.service")
        judge = self.unit("sentinel-judge.service")
        self.assertIn("Requires=nats-server.service sentinel-daemon.service", bridge)
        self.assertIn("Requires=nats-server.service sentinel-daemon.service", judge)

    def test_timers_do_not_order_after_target_and_actions_are_readiness_guarded(self) -> None:
        health_timer = self.unit("sentinel-health-monitor.timer")
        nightrun_timer = self.unit("sentinel-nightrun.timer")
        health_service = self.unit("sentinel-health-monitor.service")
        nightrun_service = self.unit("sentinel-nightrun.service")
        for text in (health_timer, nightrun_timer, health_service, nightrun_service):
            self.assertNotIn("After=sentinel.target", text)
        self.assertNotIn("ExecCondition=", health_service)
        self.assertIn("After=nats-server.service sentinel-daemon.service", health_service)
        self.assertIn("After=nats-server.service sentinel-daemon.service", health_timer)
        self.assertIn("After=sentinel-daemon.service", nightrun_timer)
        self.assertNotIn("Requires=sentinel-daemon.service", nightrun_timer)
        self.assertIn("ExecCondition=/usr/bin/systemctl --quiet is-active sentinel-daemon.service", nightrun_service)
        self.assertIn(
            "Environment=SENTINEL_OPERATOR_CREDENTIAL_FILE=%d/operator-api",
            nightrun_service,
        )
        self.assertIn(
            "m0-readiness.py nightrun --credential-environment "
            "SENTINEL_OPERATOR_CREDENTIAL_FILE",
            nightrun_service,
        )

    def test_health_monitor_observes_but_never_restarts_nats(self) -> None:
        script = (REPO_ROOT / "deploy/scripts/sentinel-health-monitor.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn("nats:nats-server.service:nats:::5:observe", script)
        self.assertIn('if [ "$restart_policy" = "restart" ]', script)
        self.assertIn("healthz?js-enabled-only=true", script)

        harness = f'''
source "{REPO_ROOT / "deploy/scripts/sentinel-health-monitor.sh"}"
alert_down() {{ :; }}
check_systemd_unit() {{ return 1; }}
try_restart() {{ printf 'restart:%s\n' "$1"; }}
check_service judge sentinel-judge.service systemd '' '' 3 restart
check_service nats nats-server.service nats '' '' 5 observe
'''
        result = subprocess.run(
            ["/bin/bash", "-c", harness],
            cwd=REPO_ROOT,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=5,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["restart:sentinel-judge.service"])

    def test_current_boot_failure_ordering_mutations_are_rejected(self) -> None:
        health_script = (REPO_ROOT / "deploy/scripts/sentinel-health-monitor.sh").read_text(
            encoding="utf-8"
        )
        validate_topology(self.units(), health_script)
        mutations = []
        no_nats_gate = self.units()
        no_nats_gate["nats-server.service"] = no_nats_gate["nats-server.service"].replace(
            "ExecStartPost=/usr/bin/python3 /opt/sentinel/scripts/m0-readiness.py nats --timeout-seconds 285\n",
            "",
        )
        mutations.append(no_nats_gate)
        no_daemon = self.units()
        no_daemon["sentinel-nats-bridge.service"] = no_daemon["sentinel-nats-bridge.service"].replace(
            "Requires=nats-server.service sentinel-daemon.service", "Requires=nats-server.service"
        )
        mutations.append(no_daemon)
        leaked_nightrun = self.units()
        leaked_nightrun["sentinel-nightrun.service"] = leaked_nightrun[
            "sentinel-nightrun.service"
        ].replace(
            "ExecStart=/usr/bin/python3 /opt/sentinel/scripts/m0-readiness.py nightrun --credential-environment SENTINEL_OPERATOR_CREDENTIAL_FILE --timeout-seconds 45",
            'ExecStart=/usr/bin/curl -H "Authorization: Bearer secret"',
        )
        mutations.append(leaked_nightrun)
        target_cycle = self.units()
        target_cycle["sentinel-nightrun.timer"] += "\nAfter=sentinel.target\n"
        mutations.append(target_cycle)
        blind_monitor = self.units()
        blind_monitor["sentinel-health-monitor.service"] += (
            "\nExecCondition=/usr/bin/systemctl --quiet is-active sentinel-judge.service\n"
        )
        mutations.append(blind_monitor)
        for index, units in enumerate(mutations):
            with self.subTest(index=index), self.assertRaises(ValueError):
                validate_topology(units, health_script)


if __name__ == "__main__":
    unittest.main()
