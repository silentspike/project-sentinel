from __future__ import annotations

import copy
from contextlib import contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import sys
import threading
import time
import unittest


sys.path.insert(0, str(Path(__file__).parent))
import run_m0_preflight as preflight  # noqa: E402


SECRET = "operator-secret-that-is-never-evidence"
EVENT_DB = Path("/opt/sentinel/data/events.db")
PROJECTION_DB = Path("/opt/sentinel/data/projection.db")


def encoded(value: object) -> bytes:
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode("ascii")


class TransportState:
    def __init__(self) -> None:
        self.paths: list[str] = []


class TransportHandler(BaseHTTPRequestHandler):
    server: "TransportServer"

    def do_GET(self) -> None:
        self.server.state.paths.append(self.path)
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("Location", "/credential-sink")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if self.path == "/wrong-content":
            self.write_response(b'{"status":"ok"}', "text/plain")
            return
        if self.path == "/oversized":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", "65")
            self.end_headers()
            return
        if self.path == "/chunked":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            for chunk in (b"x" * 40, b"y" * 40):
                self.wfile.write(f"{len(chunk):x}\r\n".encode("ascii") + chunk + b"\r\n")
                self.wfile.flush()
            self.wfile.write(b"0\r\n\r\n")
            return
        if self.path == "/slow":
            body = b'{"status":"ok"}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body[:1])
            self.wfile.flush()
            time.sleep(0.15)
            try:
                self.wfile.write(body[1:])
            except BrokenPipeError:
                pass
            return
        self.write_response(b'{"status":"ok"}', "application/json")

    def write_response(self, body: bytes, content_type: str) -> None:
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        del format, args


class TransportServer(ThreadingHTTPServer):
    def __init__(self) -> None:
        super().__init__(("127.0.0.1", 0), TransportHandler)
        self.state = TransportState()


@contextmanager
def transport_server() -> object:
    server = TransportServer()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


class Fixture:
    def __init__(self) -> None:
        self.files: dict[Path, bytes] = {}
        self.commands: list[list[str]] = []
        self.http_calls: list[tuple[str, str | None]] = []
        self.http_overrides: dict[str, bytes | Exception] = {}
        self.command_overrides: dict[tuple[str, ...], bytes | Exception] = {}
        self.manifest_path = Path("/fixture/release-manifest.json")
        self.contract_path = Path("/fixture/m0-contract.toml")
        self.profile_path = Path("/opt/sentinel/config/work-profiles/web-project-v1.toml")
        self.agents_dir = Path("/opt/sentinel/config/agents")
        self.credential_path = Path("/fixture/operator.secret")
        self.agent_paths = [
            self.agents_dir / "AGENT-01-ONE.toml",
            self.agents_dir / "AGENT-02-TWO.toml",
        ]
        self.files[self.contract_path] = b'''schema_version = 1
profile = "web-project-v1"
profile_path = "config/work-profiles/web-project-v1.toml"
'''
        self.files[self.profile_path] = b'''schema_version = 1
id = "web-project-v1"
runtime_mode = "single_node"
cluster_required = false
[runtime]
tool_runtime = "bwrap"
runtime_registry_required = true
allow_secure_runtime_fallback = false
'''
        self.files[self.agent_paths[0]] = b'''[identity]
id = 1
name = "Agent One"
role = "Developer"
shift_set = 1
[runtime]
nano_runtime = "bwrap-landlock"
'''
        self.files[self.agent_paths[1]] = b'''[identity]
id = 2
name = "Agent Two"
role = "Operations"
shift_set = 0
'''
        self.files[self.credential_path] = (SECRET + "\n").encode("ascii")
        self.manifest = self._manifest()
        self.files[self.manifest_path] = encoded(self.manifest)
        self.unit_facts = self._unit_facts()
        self.listeners = self._listeners()
        self.http_payloads = self._http_payloads()
        self.event_store = {
            "latest_event_id": 41,
            "pending_outbox": 0,
            "orphan_outbox": 0,
            "unresolved_llm": 0,
            "runtime_recovery": 0,
            "config_apply_recovery": 0,
            "projection_offset": 41,
            "hierarchy_offset": 41,
        }
        self.projection_store = {"projection_watermark": 41}
        self.projection_agents = [
            {
                "agent_id": 1,
                "name": "Agent One",
                "role": "Developer",
                "shift_set": 1,
                "status": "active",
            },
            {
                "agent_id": 2,
                "name": "Agent Two",
                "role": "Operations",
                "shift_set": 0,
                "status": "active",
            },
        ]

    def _manifest(self) -> dict[str, object]:
        artifacts = []
        for index, path in enumerate(sorted(preflight.REQUIRED_MANIFEST_PATHS)):
            content = self.files.get(Path(path), f"artifact-{index}".encode("ascii"))
            self.files[Path(path)] = content
            artifacts.append(
                {
                    "path": path,
                    "source": f"source/artifact-{index}",
                    "sha256": hashlib.sha256(content).hexdigest(),
                    "type": (
                        "binary"
                        if "/bin/" in path
                        else "systemd"
                        if path.endswith((".service", ".timer", ".target"))
                        else "config"
                    ),
                }
            )
        return {
            "version": "1.0",
            "created_at": "2026-08-13T00:00:00Z",
            "git_sha": "a" * 40,
            "artifacts": artifacts,
        }

    def _unit_facts(self) -> dict[str, dict[str, str]]:
        facts = {
            preflight.TARGET_UNIT: {
                "Id": preflight.TARGET_UNIT,
                "LoadState": "loaded",
                "ActiveState": "active",
                "SubState": "active",
                "Result": "success",
                "NRestarts": "0",
                "FragmentPath": "/etc/systemd/system/sentinel.target",
                "Wants": " ".join(sorted(preflight.REQUIRED_UNITS)),
            }
        }
        for unit, (active, sub) in preflight.REQUIRED_UNITS.items():
            facts[unit] = {
                "Id": unit,
                "LoadState": "loaded",
                "ActiveState": active,
                "SubState": sub,
                "Result": "success",
                "NRestarts": "0",
                "FragmentPath": f"/etc/systemd/system/{unit}",
                "Wants": "",
            }
        return facts

    def _listeners(self) -> bytes:
        lines = []
        for protocol, host, port in sorted(preflight.EXPECTED_LISTENERS):
            state = "LISTEN" if protocol == "tcp" else "UNCONN"
            lines.append(f"{protocol} {state} 0 128 {host}:{port} 0.0.0.0:*")
        return ("\n".join(lines) + "\n").encode("ascii")

    @staticmethod
    def _runtime_agent(agent_id: int, name: str) -> dict[str, object]:
        return {
            "agent_id": agent_id,
            "aggregate_id": f"AGENT-{agent_id:02}",
            "name": name,
            "runtime_key": "bwrap-landlock",
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
            "adapter_health_state": "Healthy",
            "adapter_observation_error": None,
            "logical_status": "Active",
            "last_repair_status": "healthy",
        }

    def _http_payloads(self) -> dict[str, dict[str, object]]:
        payloads: dict[str, dict[str, object]] = {}
        for name, _, _, field, expected in preflight.HTTP_CONTRACTS:
            payloads[name] = {field: expected} if field is not None else {}
        payloads["runtime_health"] = {
            "current_shift": 1,
            "expected_active_agents": 2,
            "runtime_agents": 2,
            "projection_agents": 2,
            "projection_drift_detected": False,
            "projection_drift_agents": 0,
            "security_runtime_entries": 2,
            "sandbox_handles": 2,
            "tracked_processes": 2,
            "live_cgroup_dirs": 2,
            "stale_runtime_entries": 0,
            "orphan_cgroups": 0,
            "zombie_tracked_pids": 0,
            "worker_states": {
                "ecs_tick_loop": {"running": True, "restart_count": 0, "last_error": None},
                "service_health": {"running": True, "restart_count": 0, "last_error": None},
            },
            "analysis_queue_depth": 0,
            "analysis_queue_dropped_total": 0,
            "analysis_queue_coalesced_total": 0,
            "reconcile_runs_total": 1,
            "reconcile_repairs_total": 0,
            "respawn_failures": 0,
            "last_repair_error": None,
            "repair_last_status": "healthy",
            "operator_auth_required": True,
            "agents": [self._runtime_agent(1, "Agent One"), self._runtime_agent(2, "Agent Two")],
        }
        payloads["platform_state"] = {
            "unresolved_counts": {},
            "resource_profiles": {"AGENT-01": "normal", "AGENT-02": "normal"},
            "agents": [
                {"agent_id": 1, "aggregate_id": "AGENT-01", "name": "Agent One", "current_profile": "normal"},
                {"agent_id": 2, "aggregate_id": "AGENT-02", "name": "Agent Two", "current_profile": "normal"},
            ],
        }
        payloads["episode_projection"] = {
            "initialized": True,
            "integrity_error": False,
            "global_frontier_source_row_id": 41,
            "global_blockers": [],
            "agents": [
                {"agent_id": 1, "ready": True, "frontier_source_row_id": 41, "lag_rows": 0, "blockers": []},
                {"agent_id": 2, "ready": True, "frontier_source_row_id": 41, "lag_rows": 0, "blockers": []},
            ],
        }
        return payloads

    def inputs(self) -> preflight.Inputs:
        return preflight.Inputs(
            manifest=self.manifest_path,
            contract=self.contract_path,
            profile=self.profile_path,
            agents_dir=self.agents_dir,
            operator_credential=self.credential_path,
            expected_git_sha="a" * 40,
            event_store=EVENT_DB,
            projection_store=PROJECTION_DB,
        )

    def read_file(self, path: Path, limit: int) -> bytes:
        if path not in self.files:
            raise preflight.PreflightError("file_unavailable")
        data = self.files[path]
        if len(data) > limit:
            raise preflight.PreflightError("oversized_file")
        return data

    def list_agents(self, path: Path) -> list[Path]:
        if path != self.agents_dir:
            raise preflight.PreflightError("agents_unavailable")
        return list(self.agent_paths)

    def command(self, argv: list[str], timeout: float, limit: int) -> bytes:
        del timeout, limit
        self.commands.append(list(argv))
        key = tuple(argv)
        override = self.command_overrides.get(key)
        if isinstance(override, Exception):
            raise override
        if isinstance(override, bytes):
            return override
        if argv[:2] == ["/usr/bin/systemctl", "show"]:
            facts = self.unit_facts[argv[2]]
            return ("\n".join(f"{key}={value}" for key, value in facts.items()) + "\n").encode("ascii")
        if argv == ["/usr/bin/ss", "-H", "-lntu"]:
            return self.listeners
        if argv[:3] == ["/usr/bin/sqlite3", "-readonly", "-json"]:
            if Path(argv[3]) == EVENT_DB:
                return encoded([self.event_store])
            if Path(argv[3]) == PROJECTION_DB:
                if argv[4] == preflight.PROJECTION_AGENTS_SQL:
                    return encoded(self.projection_agents)
                return encoded([self.projection_store])
        raise AssertionError(f"unexpected command: {argv!r}")

    def http(self, url: str, credential: str | None, timeout: float, limit: int) -> bytes:
        del timeout, limit
        self.http_calls.append((url, credential))
        override = self.http_overrides.get(url)
        if isinstance(override, Exception):
            raise override
        if isinstance(override, bytes):
            return override
        for name, expected_url, role, _, _ in preflight.HTTP_CONTRACTS:
            if expected_url == url:
                if role == "operator" and credential != SECRET:
                    raise preflight.PreflightError("http_failed")
                if role is None and credential is not None:
                    raise preflight.PreflightError("credential_leak")
                return encoded(self.http_payloads[name])
        raise AssertionError(f"unexpected URL: {url}")

    def deps(self) -> preflight.Dependencies:
        return preflight.Dependencies(
            self.command,
            self.http,
            self.read_file,
            self.list_agents,
            lambda path: None,
        )


class PreflightTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def run_fixture(self) -> dict[str, object]:
        return preflight.evaluate(self.fixture.inputs(), self.fixture.deps())

    @staticmethod
    def check(result: dict[str, object], check_id: str) -> dict[str, object]:
        return next(
            item for item in result["checks"] if item["id"] == check_id
        )  # type: ignore[index]

    def test_positive_fixture_is_canonical_runtime_preflight_only(self) -> None:
        result = self.run_fixture()
        self.assertTrue(result["runtime_preflight_pass"])
        self.assertEqual(result["claim"], "runtime_preflight_pass")
        self.assertFalse(result["m0_acceptance_pass"])
        self.assertEqual(
            preflight.canonical_json(result),
            preflight.canonical_json(
                preflight.strict_json(preflight.canonical_json(result))
            ),
        )
        self.assertNotIn(SECRET, preflight.canonical_json(result).decode("ascii"))

    def test_inactive_unit_fails(self) -> None:
        self.fixture.unit_facts["sentinel-daemon.service"]["ActiveState"] = "inactive"
        result = self.run_fixture()
        self.assertEqual(self.check(result, "systemd_units")["reason"], "systemd_unit_not_ready")

    def test_restart_count_fails(self) -> None:
        self.fixture.unit_facts["sentinel-gateway.service"]["NRestarts"] = "1"
        result = self.run_fixture()
        self.assertEqual(self.check(result, "systemd_units")["status"], "FAIL")

    def test_missing_or_duplicate_required_unit_fails(self) -> None:
        wants = self.fixture.unit_facts[preflight.TARGET_UNIT]["Wants"].split()
        self.fixture.unit_facts[preflight.TARGET_UNIT]["Wants"] = " ".join(wants[:-1] + [wants[0]])
        result = self.run_fixture()
        self.assertEqual(self.check(result, "systemd_units")["reason"], "systemd_required_set_mismatch")

    def test_runtime_drift_and_stale_entry_fail(self) -> None:
        runtime = self.fixture.http_payloads["runtime_health"]
        runtime["projection_drift_detected"] = True
        runtime["stale_runtime_entries"] = 1
        result = self.run_fixture()
        self.assertEqual(self.check(result, "identity_readiness")["reason"], "runtime_drift")

    def test_runtime_roster_mismatch_fails(self) -> None:
        runtime = self.fixture.http_payloads["runtime_health"]
        runtime["agents"] = runtime["agents"][:-1]  # type: ignore[index]
        result = self.run_fixture()
        self.assertEqual(self.check(result, "identity_readiness")["reason"], "runtime_roster_mismatch")

    def test_episode_roster_mismatch_and_blocker_fail(self) -> None:
        projection = self.fixture.http_payloads["episode_projection"]
        projection["agents"] = projection["agents"][:-1]  # type: ignore[index]
        projection["global_blockers"] = [{"kind": "global"}]
        result = self.run_fixture()
        self.assertEqual(self.check(result, "identity_readiness")["reason"], "episode_projection_blocked")

    def test_artifact_hash_mismatch_fails(self) -> None:
        path = Path(self.fixture.manifest["artifacts"][0]["path"])  # type: ignore[index]
        self.fixture.files[path] = b"changed"
        result = self.run_fixture()
        self.assertEqual(self.check(result, "release_manifest_identity")["reason"], "artifact_hash_mismatch")

    def test_expected_release_identity_mismatch_fails(self) -> None:
        inputs = self.fixture.inputs()
        inputs = preflight.Inputs(**{**inputs.__dict__, "expected_git_sha": "b" * 40})
        result = preflight.evaluate(inputs, self.fixture.deps())
        self.assertEqual(self.check(result, "release_manifest_identity")["reason"], "manifest_git_sha_mismatch")

    def test_manifest_duplicate_and_missing_required_artifact_fail(self) -> None:
        duplicate = copy.deepcopy(self.fixture.manifest["artifacts"][0])  # type: ignore[index]
        self.fixture.manifest["artifacts"].append(duplicate)  # type: ignore[union-attr]
        self.fixture.files[self.fixture.manifest_path] = encoded(self.fixture.manifest)
        result = self.run_fixture()
        self.assertEqual(self.check(result, "release_manifest_identity")["reason"], "manifest_artifact_duplicate")

        fixture = Fixture()
        fixture.manifest["artifacts"].pop()  # type: ignore[union-attr]
        fixture.files[fixture.manifest_path] = encoded(fixture.manifest)
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "release_manifest_identity")["reason"],
            "manifest_required_artifact_missing",
        )

    def test_missing_listener_fails(self) -> None:
        self.fixture.listeners = b"\n".join(self.fixture.listeners.splitlines()[:-1]) + b"\n"
        result = self.run_fixture()
        self.assertEqual(self.check(result, "required_listeners")["reason"], "listener_contract_mismatch")

    def test_http_malformed_and_duplicate_json_fail(self) -> None:
        url = "http://127.0.0.1:8080/health"
        for body in (b"not-json", b'{"status":"ok","status":"ok"}'):
            with self.subTest(body=body):
                fixture = Fixture()
                fixture.http_overrides[url] = body
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(self.check(result, "loopback_health")["reason"], "invalid_json")

    def test_http_oversized_and_timeout_fail(self) -> None:
        url = "http://127.0.0.1:8080/health"
        for failure in (
            preflight.PreflightError("http_body_oversized"),
            preflight.PreflightError("http_timeout"),
        ):
            with self.subTest(code=failure.code):
                fixture = Fixture()
                fixture.http_overrides[url] = failure
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(self.check(result, "loopback_health")["reason"], failure.code)

    def test_partial_http_readback_fails(self) -> None:
        url = "http://127.0.0.1:8082/ready"
        self.fixture.http_overrides[url] = b"{}"
        result = self.run_fixture()
        self.assertEqual(self.check(result, "loopback_health")["reason"], "http_readiness_failed")

    def test_http_content_type_and_command_failure_are_fail_closed(self) -> None:
        gateway_url = "http://127.0.0.1:8080/health"
        self.fixture.http_overrides[gateway_url] = preflight.PreflightError("http_content_type")
        result = self.run_fixture()
        self.assertEqual(self.check(result, "loopback_health")["reason"], "http_content_type")

        fixture = Fixture()
        ss_argv = ("/usr/bin/ss", "-H", "-lntu")
        fixture.command_overrides[ss_argv] = preflight.PreflightError("command_failed")
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(self.check(result, "required_listeners")["reason"], "command_failed")

    def test_secret_never_reaches_public_evidence_or_unprotected_routes(self) -> None:
        url = "http://127.0.0.1:8080/health"
        self.fixture.http_overrides[url] = RuntimeError(f"hostile {SECRET} diagnostic")
        result = self.run_fixture()
        serialized = preflight.canonical_json(result).decode("ascii")
        self.assertNotIn(SECRET, serialized)
        self.assertTrue(
            all(
                credential is None
                for target, credential in self.fixture.http_calls
                if ":8084/" not in target
            )
        )

    def test_store_path_command_injection_is_rejected_before_command(self) -> None:
        inputs = copy.copy(self.fixture.inputs())
        inputs = preflight.Inputs(
            manifest=inputs.manifest,
            contract=inputs.contract,
            profile=inputs.profile,
            agents_dir=inputs.agents_dir,
            operator_credential=inputs.operator_credential,
            expected_git_sha=inputs.expected_git_sha,
            event_store=Path("/opt/sentinel/data/events.db;touch-owned"),
            projection_store=inputs.projection_store,
        )
        with self.assertRaisesRegex(preflight.PreflightError, "store_path_invalid"):
            preflight.evaluate(inputs, self.fixture.deps())
        self.assertEqual(self.fixture.commands, [])

    def test_store_backlog_and_projection_lag_fail(self) -> None:
        self.fixture.event_store["pending_outbox"] = 1
        result = self.run_fixture()
        self.assertEqual(self.check(result, "store_projection_backlog")["reason"], "publication_or_recovery_backlog")
        fixture = Fixture()
        fixture.projection_store["projection_watermark"] = 40
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(self.check(result, "store_projection_backlog")["reason"], "read_model_projection_lag")

    def test_projection_store_identity_mismatch_fails(self) -> None:
        self.fixture.projection_agents[0]["name"] = "Wrong Agent"
        result = self.run_fixture()
        self.assertEqual(
            self.check(result, "projection_store_identity")["reason"],
            "projection_store_identity_mismatch",
        )

    def test_store_duplicate_key_and_partial_row_fail(self) -> None:
        event_argv = (
            "/usr/bin/sqlite3",
            "-readonly",
            "-json",
            str(EVENT_DB),
            preflight.EVENT_STORE_SQL,
        )
        self.fixture.command_overrides[event_argv] = b'[{"latest_event_id":1,"latest_event_id":1}]'
        result = self.run_fixture()
        self.assertEqual(self.check(result, "store_projection_backlog")["reason"], "invalid_json")

    def test_duplicate_agent_id_or_name_fails(self) -> None:
        self.fixture.files[self.fixture.agent_paths[1]] = b'''[identity]
id = 1
name = "Agent One"
role = "Developer"
shift_set = 0
'''
        result = self.run_fixture()
        self.assertEqual(self.check(result, "contract_profile_roster")["reason"], "agent_roster_ambiguous")

    def test_zero_mutation_uses_only_bounded_read_operations(self) -> None:
        result = self.run_fixture()
        self.assertTrue(result["runtime_preflight_pass"])
        self.assertTrue(self.fixture.commands)
        for argv in self.fixture.commands:
            self.assertIn(argv[0], {"/usr/bin/systemctl", "/usr/bin/ss", "/usr/bin/sqlite3"})
            self.assertNotIn("sh", argv)
            self.assertNotIn("sudo", argv)
            self.assertNotIn("ssh", argv)
        self.assertTrue(all(url.startswith("http://127.0.0.1:") for url, _ in self.fixture.http_calls))

    def test_input_timeout_and_credential_controls_fail_closed(self) -> None:
        inputs = self.fixture.inputs()
        bad_timeout = preflight.Inputs(**{**inputs.__dict__, "timeout_seconds": 100.0})
        with self.assertRaisesRegex(preflight.PreflightError, "timeout_invalid"):
            preflight.evaluate(bad_timeout, self.fixture.deps())
        self.fixture.files[self.fixture.credential_path] = b"short"
        result = self.run_fixture()
        self.assertEqual(self.check(result, "operator_credential_reference")["reason"], "credential_invalid")
        self.assertEqual(self.check(result, "loopback_health")["reason"], "credential_dependency_failed")

    def test_credential_permissions_are_checked_before_readback(self) -> None:
        deps = self.fixture.deps()
        denied = preflight.Dependencies(
            deps.command,
            deps.http,
            deps.read_file,
            deps.list_agents,
            lambda path: (_ for _ in ()).throw(
                preflight.PreflightError("credential_permissions_invalid")
            ),
        )
        result = preflight.evaluate(self.fixture.inputs(), denied)
        self.assertEqual(
            self.check(result, "operator_credential_reference")["reason"],
            "credential_permissions_invalid",
        )
        self.assertFalse(self.fixture.http_calls)

    def test_default_http_denies_redirect_and_ignores_proxy_environment(self) -> None:
        with transport_server() as server:
            base = f"http://127.0.0.1:{server.server_port}"
            previous = os.environ.get("HTTP_PROXY")
            os.environ["HTTP_PROXY"] = "http://127.0.0.1:1"
            try:
                self.assertEqual(
                    preflight.strict_json(preflight.default_http(f"{base}/ok", None, 1.0, 64)),
                    {"status": "ok"},
                )
                with self.assertRaisesRegex(preflight.PreflightError, "http_status"):
                    preflight.default_http(f"{base}/redirect", SECRET, 1.0, 64)
            finally:
                if previous is None:
                    os.environ.pop("HTTP_PROXY", None)
                else:
                    os.environ["HTTP_PROXY"] = previous
            self.assertNotIn("/credential-sink", server.state.paths)

    def test_default_http_enforces_type_size_and_complete_deadline(self) -> None:
        with transport_server() as server:
            base = f"http://127.0.0.1:{server.server_port}"
            cases = (
                ("/wrong-content", 1.0, "http_content_type"),
                ("/oversized", 1.0, "http_body_oversized"),
                ("/chunked", 1.0, "http_body_oversized"),
                ("/slow", 0.05, "http_timeout"),
            )
            for path, timeout, reason in cases:
                with self.subTest(path=path):
                    with self.assertRaisesRegex(preflight.PreflightError, reason):
                        preflight.default_http(f"{base}{path}", None, timeout, 64)

    def test_default_command_rejects_unapproved_executable(self) -> None:
        with self.assertRaisesRegex(preflight.PreflightError, "command_not_allowed"):
            preflight.default_command(["/bin/sh", "-c", "true"], 1.0, 64)


if __name__ == "__main__":
    unittest.main()
