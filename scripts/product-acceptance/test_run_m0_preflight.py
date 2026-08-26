from __future__ import annotations

import copy
from contextlib import contextmanager
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock


sys.path.insert(0, str(Path(__file__).parent))
import run_m0_preflight as preflight  # noqa: E402


SECRET = "operator-secret-that-is-never-evidence"
EVENT_DB = Path("/opt/sentinel/data/events.db")
PROJECTION_DB = Path("/opt/sentinel/data/projection.db")
TEST_CERTIFICATE = b"""-----BEGIN CERTIFICATE-----
AA==
-----END CERTIFICATE-----
"""


def encoded(value: object) -> bytes:
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode("ascii")


class TransportState:
    def __init__(self) -> None:
        self.paths: list[str] = []
        self.authorization: list[str | None] = []
        self.certificate_hash: str | None = None


class TransportHandler(BaseHTTPRequestHandler):
    server: "TransportServer"

    def do_GET(self) -> None:
        self.server.state.paths.append(self.path)
        self.server.state.authorization.append(self.headers.get("Authorization"))
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
        if self.path == "/api/cert-hash":
            self.write_response(
                encoded(
                    {
                        "algorithm": "sha-256",
                        "hash": self.server.state.certificate_hash,
                    }
                ),
                "application/json",
            )
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
    allow_reuse_address = True

    def __init__(self, port: int = 0) -> None:
        super().__init__(("127.0.0.1", port), TransportHandler)
        self.state = TransportState()

    def handle_error(self, request: object, client_address: object) -> None:
        del request, client_address


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


@contextmanager
def tls_transport_server() -> object:
    def generate_certificate(root: Path, stem: str) -> tuple[Path, Path]:
        certificate = root / f"{stem}-cert.pem"
        key = root / f"{stem}-key.pem"
        subprocess.run(
            [
                shutil.which("openssl") or "/usr/bin/openssl",
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-sha256",
                "-days",
                "1",
                "-nodes",
                "-keyout",
                str(key),
                "-out",
                str(certificate),
                "-subj",
                "/CN=127.0.0.1",
                "-addext",
                "subjectAltName=IP:127.0.0.1",
                "-addext",
                "basicConstraints=critical,CA:FALSE",
            ],
            check=True,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        return certificate, key

    runner_temp = Path(os.environ["RUNNER_TEMP"])
    runner_temp.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="cdx1-650-tls-", dir=runner_temp) as raw:
        root = Path(raw)
        certificate, key = generate_certificate(root, "server")
        server = TransportServer(8001)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(certificate, key)
        server.socket = context.wrap_socket(server.socket, server_side=True)
        pem = certificate.read_bytes()
        der = ssl.PEM_cert_to_DER_cert(pem.decode("ascii"))
        digest = hashlib.sha256(der)
        server.state.certificate_hash = base64.b64encode(digest.digest()).decode("ascii")
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield server, pem, digest.hexdigest(), root, generate_certificate
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


class Fixture:
    def __init__(self) -> None:
        self.files: dict[Path, bytes] = {}
        self.commands: list[list[str]] = []
        self.http_calls: list[tuple[str, str | None]] = []
        self.https_calls: list[tuple[str, str]] = []
        self.http_overrides: dict[str, bytes | Exception] = {}
        self.https_overrides: dict[str, bytes | Exception] = {}
        self.command_overrides: dict[tuple[str, ...], bytes | Exception] = {}
        self.event_store_reads: list[dict[str, int]] = []
        self.hash_calls: list[tuple[Path, int]] = []
        self.running_hash_calls: list[tuple[int, Path, int]] = []
        self.running_hash_overrides: dict[Path, tuple[str, int] | Exception] = {}
        self.unit_fact_reads: dict[str, list[dict[str, str]]] = {}
        self.manifest_path = Path("/fixture/release-manifest.json")
        self.contract_path = preflight.M0_CONTRACT_PATH
        self.profile_path = preflight.M0_PROFILE_PATH
        self.agents_dir = Path("/opt/sentinel/config/agents")
        self.credential_path = Path("/fixture/operator.secret")
        self.agent_paths = [
            self.agents_dir / name for name in sorted(preflight.CANONICAL_AGENT_FILES)
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
        self.files[preflight.M0_WORKBENCH_PROFILE_PATH] = b'''schema_version = 1
id = "web-authoring-v1"
'''
        repository_agents = Path(__file__).resolve().parents[2] / "config" / "agents"
        self.agent_identities: dict[int, dict[str, object]] = {}
        for path in self.agent_paths:
            data = (repository_agents / path.name).read_bytes()
            self.files[path] = data
            identity = preflight.parse_toml(data)["identity"]
            self.agent_identities[identity["id"]] = identity
        self.files[self.credential_path] = (SECRET + "\n").encode("ascii")
        self.files[preflight.DASHBOARD_CERT_PATH] = TEST_CERTIFICATE
        self.manifest = self._manifest()
        self.authorize_manifest()
        self.unit_facts = self._unit_facts()
        self.listeners_v4 = self._listeners("ipv4")
        self.listeners_v6 = self._listeners("ipv6")
        self.http_payloads = self._http_payloads()
        self.event_store = {
            "latest_event_id": 41,
            "unpublished_outbox": 0,
            "orphan_outbox": 0,
            "unresolved_llm": 0,
            "runtime_recovery": 0,
            "config_apply_recovery": 0,
            "projection_offset": 41,
            "hierarchy_offset": 41,
        }
        self.projection_store = [
            {"projection_name": "sentinel-projection", "last_event_id": 41},
            {
                "projection_name": "sentinel-projection-cost-hierarchy-v2",
                "last_event_id": 41,
            },
        ]
        self.projection_agents = [
            {
                "agent_id": agent_id,
                "name": identity["name"],
                "role": identity["role"],
                "shift_set": identity["shift_set"],
                "status": "active",
            }
            for agent_id, identity in sorted(self.agent_identities.items())
            if identity["shift_set"] in {0, 1}
        ]

    def _manifest(self) -> dict[str, object]:
        artifacts = []
        for index, (path, (source, kind)) in enumerate(
            sorted(preflight.CANONICAL_RELEASE_ARTIFACTS.items())
        ):
            content = self.files.get(Path(path), f"artifact-{index}".encode("ascii"))
            self.files[Path(path)] = content
            artifacts.append(
                {
                    "path": path,
                    "source": source,
                    "sha256": hashlib.sha256(content).hexdigest(),
                    "type": kind,
                }
            )
        return {
            "version": "1.0",
            "created_at": "2026-08-13T00:00:00Z",
            "git_sha": "a" * 40,
            "artifacts": artifacts,
        }

    def authorize_manifest(self) -> None:
        raw = encoded(self.manifest)
        self.files[self.manifest_path] = raw
        self.expected_manifest_sha256 = hashlib.sha256(raw).hexdigest()

    def _unit_facts(self) -> dict[str, dict[str, str]]:
        facts = {
            preflight.TARGET_UNIT: {
                "Id": preflight.TARGET_UNIT,
                "LoadState": "loaded",
                "ActiveState": "active",
                "SubState": "active",
                "FragmentPath": "/etc/systemd/system/sentinel.target",
                "Wants": " ".join(sorted(preflight.TARGET_WANTS)),
                "Requires": preflight.AUTH_INIT_UNIT,
                "NeedDaemonReload": "no",
            }
        }
        facts[preflight.AUTH_INIT_UNIT] = {
            "Id": preflight.AUTH_INIT_UNIT,
            "LoadState": "loaded",
            "ActiveState": "active",
            "SubState": "exited",
            "Result": "success",
            "FragmentPath": f"/etc/systemd/system/{preflight.AUTH_INIT_UNIT}",
            "NeedDaemonReload": "no",
            "ExecMainCode": "1",
            "ExecMainStatus": "0",
        }
        self.main_pids = {
            unit: 2000 + index
            for index, unit in enumerate(sorted(preflight.REQUIRED_SERVICES), start=1)
        }
        for unit in preflight.REQUIRED_SERVICES:
            facts[unit] = {
                "Id": unit,
                "LoadState": "loaded",
                "ActiveState": "active",
                "SubState": "running",
                "Result": "success",
                "FragmentPath": f"/etc/systemd/system/{unit}",
                "NRestarts": "0",
                "NeedDaemonReload": "no",
                "MainPID": str(self.main_pids[unit]),
            }
        for index, (timer, service) in enumerate(
            sorted(preflight.TIMER_SERVICES.items()), start=1
        ):
            timer_entered = 900_000 * index
            service_started = 1_000_000 * index
            facts[timer] = {
                "Id": timer,
                "LoadState": "loaded",
                "ActiveState": "active",
                "SubState": "waiting",
                "Result": "success",
                "FragmentPath": f"/etc/systemd/system/{timer}",
                "NeedDaemonReload": "no",
                "Unit": service,
                "ActiveEnterTimestampMonotonic": str(timer_entered),
            }
            facts[service] = {
                "Id": service,
                "LoadState": "loaded",
                "ActiveState": "inactive",
                "SubState": "dead",
                "Result": "success",
                "FragmentPath": f"/etc/systemd/system/{service}",
                "NeedDaemonReload": "no",
                "ExecMainCode": "1",
                "ExecMainStatus": "0",
                "ExecMainStartTimestampMonotonic": str(service_started),
                "ExecMainExitTimestampMonotonic": str(service_started + 10),
            }
        return facts

    def _listeners(self, family: str) -> bytes:
        lines = []
        for protocol, expected_family, host, port in sorted(preflight.EXPECTED_LISTENERS):
            if expected_family != family:
                continue
            state = "LISTEN" if protocol == "tcp" else "UNCONN"
            remote = "0.0.0.0:*" if family == "ipv4" else "[::]:*"
            local = f"{host}:{port}" if family == "ipv4" else f"[{host}]:{port}"
            service = preflight.LISTENER_SERVICES[
                (protocol, expected_family, host, port)
            ]
            pid = self.main_pids[service]
            lines.append(
                f'{protocol} {state} 0 128 {local} {remote} '
                f'users:(("service",pid={pid},fd=7))'
            )
        return (("\n".join(lines) + "\n") if lines else "").encode("ascii")

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
            "adapter_health_state": "healthy",
            "adapter_observation_error": None,
            "logical_status": "Active",
            "last_repair_status": "healthy",
        }

    def _http_payloads(self) -> dict[str, dict[str, object]]:
        payloads: dict[str, dict[str, object]] = {}
        for name, _, _, field, expected in preflight.HTTP_CONTRACTS:
            payloads[name] = {field: expected} if field is not None else {}
        scheduled = {
            agent_id: identity
            for agent_id, identity in self.agent_identities.items()
            if identity["shift_set"] in {0, 1}
        }
        scheduled_count = len(scheduled)
        payloads["runtime_health"] = {
            "current_shift": 1,
            "expected_active_agents": scheduled_count,
            "runtime_agents": scheduled_count,
            "projection_agents": scheduled_count,
            "projection_drift_detected": False,
            "projection_drift_agents": 0,
            "security_runtime_entries": scheduled_count,
            "sandbox_handles": scheduled_count,
            "tracked_processes": scheduled_count,
            "live_cgroup_dirs": scheduled_count,
            "stale_runtime_entries": 0,
            "orphan_cgroups": 0,
            "zombie_tracked_pids": 0,
            "worker_states": {
                "ecs_tick_loop": {"running": True, "restart_count": 0, "last_error": None},
                "episode_projection": {
                    "running": True,
                    "restart_count": 0,
                    "last_error": None,
                },
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
            "agents": [
                self._runtime_agent(agent_id, str(identity["name"]))
                for agent_id, identity in sorted(scheduled.items())
            ],
        }
        payloads["platform_state"] = {
            "unresolved_counts": {},
            "resource_profiles": {
                f"AGENT-{agent_id:02}": "normal"
                for agent_id in scheduled
            },
            "agents": [
                {
                    "agent_id": agent_id,
                    "aggregate_id": f"AGENT-{agent_id:02}",
                    "name": identity["name"],
                    "current_profile": "normal",
                }
                for agent_id, identity in sorted(scheduled.items())
            ],
        }
        payloads["episode_projection"] = {
            "initialized": True,
            "integrity_error": False,
            "global_frontier_source_row_id": 41,
            "global_blockers": [],
            "agents": [
                {
                    "agent_id": agent_id,
                    "ready": True,
                    "frontier_source_row_id": 41,
                    "lag_rows": 0,
                    "blockers": [],
                }
                for agent_id in range(1, preflight.MAX_AGENTS + 1)
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
            expected_manifest_sha256=self.expected_manifest_sha256,
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

    def hash_file(self, path: Path, limit: int) -> tuple[str, int]:
        self.hash_calls.append((path, limit))
        if path not in self.files:
            raise preflight.PreflightError("file_unavailable")
        data = self.files[path]
        if len(data) > limit:
            raise preflight.PreflightError("artifact_oversized")
        return hashlib.sha256(data).hexdigest(), len(data)

    def hash_running_executable(
        self, pid: int, path: Path, limit: int
    ) -> tuple[str, int]:
        self.running_hash_calls.append((pid, path, limit))
        override = self.running_hash_overrides.get(path)
        if isinstance(override, Exception):
            raise override
        if isinstance(override, tuple):
            return override
        expected_pid = self.main_pids[
            next(unit for unit, executable in preflight.SERVICE_EXECUTABLES.items() if executable == path)
        ]
        if pid != expected_pid:
            raise preflight.PreflightError("running_executable_identity_race")
        data = self.files[path]
        return hashlib.sha256(data).hexdigest(), len(data)

    def projection_snapshot(self) -> list[dict[str, object]]:
        return [
            {
                "row_kind": "watermark",
                "projection_name": row["projection_name"],
                "last_event_id": row["last_event_id"],
                "agent_id": None,
                "name": None,
                "role": None,
                "shift_set": None,
                "status": None,
            }
            for row in self.projection_store
        ] + [
            {
                "row_kind": "agent",
                "projection_name": None,
                "last_event_id": None,
                **row,
            }
            for row in self.projection_agents
        ]

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
            unit = argv[2]
            reads = self.unit_fact_reads.get(unit)
            facts = reads.pop(0) if reads else self.unit_facts[unit]
            properties = argv[-1].removeprefix("--property=").split(",")
            selected = {key: facts[key] for key in properties if key in facts}
            return (
                "\n".join(f"{key}={value}" for key, value in selected.items()) + "\n"
            ).encode("ascii")
        if argv == ["/usr/bin/ss", "-H", "-lntup", "-4"]:
            return self.listeners_v4
        if argv == ["/usr/bin/ss", "-H", "-lntup", "-6"]:
            return self.listeners_v6
        if argv[:3] == ["/usr/bin/sqlite3", "-readonly", "-json"]:
            if Path(argv[3]) == EVENT_DB:
                if self.event_store_reads:
                    return encoded([self.event_store_reads.pop(0)])
                return encoded([self.event_store])
            if Path(argv[3]) == PROJECTION_DB:
                if argv[4] == preflight.PROJECTION_SNAPSHOT_SQL:
                    return encoded(self.projection_snapshot())
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

    def https(
        self,
        url: str,
        timeout: float,
        limit: int,
        trusted_pem: bytes,
        expected_peer_digest: str,
    ) -> bytes:
        del timeout, limit
        actual_digest = hashlib.sha256(
            ssl.PEM_cert_to_DER_cert(trusted_pem.decode("ascii"))
        ).hexdigest()
        if expected_peer_digest != actual_digest:
            raise preflight.PreflightError("https_peer_pin_mismatch")
        self.https_calls.append((url, expected_peer_digest))
        override = self.https_overrides.get(url)
        if isinstance(override, Exception):
            raise override
        if isinstance(override, bytes):
            return override
        if url == f"{preflight.DASHBOARD_ORIGIN}/api/health":
            return encoded({"status": "ok"})
        if url == f"{preflight.DASHBOARD_ORIGIN}/api/cert-hash":
            digest = hashlib.sha256(
                ssl.PEM_cert_to_DER_cert(trusted_pem.decode("ascii"))
            ).digest()
            return encoded(
                {
                    "algorithm": "sha-256",
                    "hash": preflight.base64.b64encode(digest).decode("ascii"),
                }
            )
        raise AssertionError(f"unexpected HTTPS URL: {url}")

    def deps(self) -> preflight.Dependencies:
        return preflight.Dependencies(
            self.command,
            self.http,
            self.https,
            self.read_file,
            self.hash_file,
            self.hash_running_executable,
            self.list_agents,
            self.read_file,
        )


class PreflightTests(unittest.TestCase):
    def test_nats_contract_requires_jetstream_readiness(self) -> None:
        contracts = {name: url for name, url, *_rest in preflight.HTTP_CONTRACTS}
        self.assertEqual(
            contracts["nats_health"],
            "http://127.0.0.1:8222/healthz?js-enabled-only=true",
        )

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

    def test_auth_init_must_be_terminal_success_before_consumers_are_ready(self) -> None:
        for field, value in (
            ("ActiveState", "failed"),
            ("SubState", "running"),
            ("Result", "exit-code"),
            ("ExecMainStatus", "1"),
        ):
            with self.subTest(field=field):
                fixture = Fixture()
                fixture.unit_facts[preflight.AUTH_INIT_UNIT][field] = value
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(
                    self.check(result, "systemd_units")["reason"],
                    "systemd_auth_init_not_ready",
                )

    def test_target_and_timer_omit_inapplicable_properties_but_bad_state_fails(self) -> None:
        target = self.fixture.unit_facts[preflight.TARGET_UNIT]
        timer = self.fixture.unit_facts["sentinel-nightrun.timer"]
        self.assertNotIn("Result", target)
        self.assertNotIn("NRestarts", target)
        self.assertNotIn("NRestarts", timer)
        self.assertTrue(self.run_fixture()["runtime_preflight_pass"])

        fixture = Fixture()
        fixture.unit_facts["sentinel-nightrun.timer"]["SubState"] = "elapsed"
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "systemd_units")["reason"], "systemd_timer_not_ready"
        )

        fixture = Fixture()
        fixture.unit_facts["sentinel-nightrun.timer"]["Result"] = "failed"
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "systemd_units")["reason"], "systemd_timer_not_ready"
        )

        fixture = Fixture()
        fixture.unit_facts[preflight.TARGET_UNIT].pop("NeedDaemonReload")
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "systemd_units")["reason"], "systemd_target_shape"
        )

    def test_restart_count_fails(self) -> None:
        self.fixture.unit_facts["sentinel-gateway.service"]["NRestarts"] = "1"
        result = self.run_fixture()
        self.assertEqual(self.check(result, "systemd_units")["status"], "FAIL")

    def test_running_executables_match_manifest_and_main_pids(self) -> None:
        result = self.run_fixture()
        self.assertTrue(result["runtime_preflight_pass"])
        self.assertEqual(
            {path for _, path, _ in self.fixture.running_hash_calls},
            set(preflight.SERVICE_EXECUTABLES.values()),
        )
        self.assertIn(
            "/usr/local/bin/nats-server", preflight.CANONICAL_RELEASE_ARTIFACTS
        )
        self.assertEqual(
            preflight.CANONICAL_RELEASE_ARTIFACTS["/usr/local/bin/nats-server"],
            ("external/nats-server", "binary"),
        )

    def test_running_executable_old_deleted_replaced_and_pid_race_fail(self) -> None:
        unit = "sentinel-daemon.service"
        path = preflight.SERVICE_EXECUTABLES[unit]
        old = self.fixture.files[path]
        replacement = b"replacement-daemon"
        self.fixture.files[path] = replacement
        artifact = next(
            item
            for item in self.fixture.manifest["artifacts"]  # type: ignore[union-attr]
            if item["path"] == str(path)
        )
        artifact["sha256"] = hashlib.sha256(replacement).hexdigest()
        self.fixture.authorize_manifest()
        self.fixture.running_hash_overrides[path] = (
            hashlib.sha256(old).hexdigest(),
            len(old),
        )
        result = self.run_fixture()
        self.assertEqual(
            self.check(result, "systemd_units")["reason"],
            "running_executable_hash_mismatch",
        )

        for reason in (
            "running_executable_identity_mismatch",
            "running_executable_unavailable",
        ):
            with self.subTest(reason=reason):
                fixture = Fixture()
                fixture.running_hash_overrides[path] = preflight.PreflightError(reason)
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(self.check(result, "systemd_units")["reason"], reason)

        fixture = Fixture()
        first = copy.deepcopy(fixture.unit_facts[unit])
        changed = copy.deepcopy(first)
        changed["MainPID"] = str(int(first["MainPID"]) + 100)
        fixture.unit_fact_reads[unit] = [first, changed]
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "systemd_units")["reason"],
            "systemd_service_identity_changed",
        )

    def test_main_pid_reload_and_nats_manifest_omission_fail(self) -> None:
        for pid in ("", "0"):
            with self.subTest(pid=pid):
                fixture = Fixture()
                fixture.unit_facts["sentinel-daemon.service"]["MainPID"] = pid
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(
                    self.check(result, "systemd_units")["reason"],
                    "systemd_main_pid_invalid",
                )

        fixture = Fixture()
        fixture.unit_facts["sentinel-daemon.service"]["NeedDaemonReload"] = "yes"
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "systemd_units")["reason"],
            "systemd_unit_not_ready",
        )

        fixture = Fixture()
        fixture.manifest["artifacts"] = [  # type: ignore[index]
            item
            for item in fixture.manifest["artifacts"]  # type: ignore[union-attr]
            if item["path"] != "/usr/local/bin/nats-server"
        ]
        fixture.authorize_manifest()
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "release_manifest_identity")["reason"],
            "manifest_required_artifact_missing",
        )

    def test_activation_oneshot_outcome_fails_closed(self) -> None:
        timer = "sentinel-health-monitor.timer"
        service = preflight.TIMER_SERVICES[timer]
        mutations = (
            (
                "running",
                lambda fixture: fixture.unit_facts[service].update(
                    {"ActiveState": "active", "SubState": "running"}
                ),
                "systemd_timer_outcome_failed",
            ),
            (
                "failed",
                lambda fixture: fixture.unit_facts[service].update(
                    {"Result": "failed", "ExecMainStatus": "1"}
                ),
                "systemd_timer_outcome_failed",
            ),
            (
                "stale",
                lambda fixture: fixture.unit_facts[service].update(
                    {
                        "ExecMainStartTimestampMonotonic": str(
                            int(fixture.unit_facts[timer]["ActiveEnterTimestampMonotonic"])
                            - 1
                        )
                    }
                ),
                "systemd_timer_outcome_stale",
            ),
            (
                "timer_activation_missing",
                lambda fixture: fixture.unit_facts[timer].__setitem__(
                    "ActiveEnterTimestampMonotonic", "0"
                ),
                "systemd_timer_activation_missing",
            ),
            (
                "mismatched_unit",
                lambda fixture: fixture.unit_facts[timer].__setitem__(
                    "Unit", "sentinel-nightrun.service"
                ),
                "systemd_timer_not_ready",
            ),
            (
                "missing_completion",
                lambda fixture: fixture.unit_facts[service].__setitem__(
                    "ExecMainExitTimestampMonotonic", "0"
                ),
                "systemd_timer_outcome_missing",
            ),
        )
        for name, mutate, reason in mutations:
            with self.subTest(name=name):
                fixture = Fixture()
                mutate(fixture)
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(
                    self.check(result, "systemd_units")["reason"], reason
                )

    def test_missing_or_duplicate_required_unit_fails(self) -> None:
        wants = self.fixture.unit_facts[preflight.TARGET_UNIT]["Wants"].split()
        self.fixture.unit_facts[preflight.TARGET_UNIT]["Wants"] = " ".join(wants[:-1] + [wants[0]])
        result = self.run_fixture()
        self.assertEqual(self.check(result, "systemd_units")["reason"], "systemd_required_set_mismatch")

        fixture = Fixture()
        fixture.unit_facts[preflight.TARGET_UNIT]["Requires"] = ""
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "systemd_units")["reason"],
            "systemd_required_set_mismatch",
        )

    def test_runtime_drift_and_stale_entry_fail(self) -> None:
        runtime = self.fixture.http_payloads["runtime_health"]
        runtime["projection_drift_detected"] = True
        runtime["stale_runtime_entries"] = 1
        result = self.run_fixture()
        self.assertEqual(self.check(result, "identity_readiness")["reason"], "runtime_drift")

    def test_runtime_worker_set_requires_productive_episode_projection(self) -> None:
        for mutate in (
            lambda workers: workers.pop("episode_projection"),
            lambda workers: workers.update({"unknown_worker": workers["episode_projection"]}),
        ):
            fixture = Fixture()
            workers = fixture.http_payloads["runtime_health"]["worker_states"]
            mutate(workers)  # type: ignore[arg-type]
            result = preflight.evaluate(fixture.inputs(), fixture.deps())
            self.assertEqual(
                self.check(result, "identity_readiness")["reason"],
                "runtime_worker_mismatch",
            )

    def test_full_preflight_rejects_projection_absent_during_daemon_local_boot(self) -> None:
        runtime = self.fixture.http_payloads["runtime_health"]
        runtime["projection_agents"] = 0
        runtime["projection_drift_detected"] = True
        runtime["projection_drift_agents"] = runtime["expected_active_agents"]
        runtime["stale_runtime_entries"] = runtime["expected_active_agents"]
        runtime["repair_last_status"] = "drift_detected"
        for agent in runtime["agents"]:
            agent["projection_present"] = False
        result = self.run_fixture()
        self.assertEqual(
            self.check(result, "identity_readiness")["reason"], "runtime_drift"
        )

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

    def test_manifest_raw_digest_and_exact_authority_fail_closed(self) -> None:
        self.fixture.files[self.fixture.manifest_path] += b" "
        result = self.run_fixture()
        self.assertEqual(
            self.check(result, "release_manifest_identity")["reason"],
            "manifest_authority_digest_mismatch",
        )

        for field, value in (("source", "fork/replaced"), ("type", "script")):
            with self.subTest(field=field):
                fixture = Fixture()
                artifact = fixture.manifest["artifacts"][0]  # type: ignore[index]
                if artifact[field] == value:  # type: ignore[index]
                    value = "binary"
                artifact[field] = value  # type: ignore[index]
                fixture.authorize_manifest()
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(
                    self.check(result, "release_manifest_identity")["reason"],
                    "manifest_artifact_authority_mismatch",
                )

        fixture = Fixture()
        fixture.manifest["artifacts"].append(  # type: ignore[union-attr]
            {
                "path": "/opt/sentinel/bin/unapproved",
                "source": "target/release/unapproved",
                "sha256": "0" * 64,
                "type": "binary",
            }
        )
        fixture.authorize_manifest()
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "release_manifest_identity")["reason"],
            "manifest_unexpected_artifact",
        )

    def test_streaming_artifact_hash_accepts_more_than_metadata_limit(self) -> None:
        artifact = self.fixture.manifest["artifacts"][0]  # type: ignore[index]
        path = Path(artifact["path"])
        content = b"x" * (preflight.MAX_FILE_BYTES + 1)
        self.fixture.files[path] = content
        artifact["sha256"] = hashlib.sha256(content).hexdigest()
        self.fixture.authorize_manifest()
        result = self.run_fixture()
        self.assertEqual(self.check(result, "release_manifest_identity")["status"], "PASS")
        self.assertIn((path, preflight.MAX_ARTIFACT_BYTES), self.fixture.hash_calls)

        runner_temp = Path(os.environ["RUNNER_TEMP"])
        runner_temp.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="cdx1-650-large-", dir=runner_temp) as raw:
            large = Path(raw) / "large-artifact"
            large.write_bytes(content)
            large.chmod(0o644)
            self.assertEqual(
                preflight.default_hash_file(large, preflight.MAX_ARTIFACT_BYTES),
                (hashlib.sha256(content).hexdigest(), len(content)),
            )

    def test_streaming_artifact_hash_rejects_oversize_and_replacement(self) -> None:
        runner_temp = Path(os.environ["RUNNER_TEMP"])
        runner_temp.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="cdx1-650-hash-", dir=runner_temp) as raw:
            root = Path(raw)
            oversized = root / "oversized"
            with oversized.open("wb") as stream:
                stream.truncate(preflight.MAX_ARTIFACT_BYTES + 1)
            oversized.chmod(0o644)
            with self.assertRaisesRegex(preflight.PreflightError, "artifact_oversized"):
                preflight.default_hash_file(oversized, preflight.MAX_ARTIFACT_BYTES)

            artifact = root / "artifact"
            artifact.write_bytes(b"a" * (2 * 1024 * 1024))
            artifact.chmod(0o644)
            moved = root / "artifact-old"
            real_read = os.read
            replaced = False

            def replace_during_hash(fd: int, size: int) -> bytes:
                nonlocal replaced
                block = real_read(fd, size)
                if block and not replaced:
                    replaced = True
                    artifact.rename(moved)
                    artifact.write_bytes(b"b" * (2 * 1024 * 1024))
                    artifact.chmod(0o644)
                return block

            with mock.patch.object(preflight.os, "read", side_effect=replace_during_hash):
                with self.assertRaisesRegex(preflight.PreflightError, "unsafe_file"):
                    preflight.default_hash_file(artifact, preflight.MAX_ARTIFACT_BYTES)
            self.assertTrue(replaced)

    def test_expected_release_identity_mismatch_fails(self) -> None:
        inputs = self.fixture.inputs()
        inputs = preflight.Inputs(**{**inputs.__dict__, "expected_git_sha": "b" * 40})
        result = preflight.evaluate(inputs, self.fixture.deps())
        self.assertEqual(self.check(result, "release_manifest_identity")["reason"], "manifest_git_sha_mismatch")

    def test_manifest_duplicate_and_missing_required_artifact_fail(self) -> None:
        duplicate = copy.deepcopy(self.fixture.manifest["artifacts"][0])  # type: ignore[index]
        self.fixture.manifest["artifacts"].append(duplicate)  # type: ignore[union-attr]
        self.fixture.authorize_manifest()
        result = self.run_fixture()
        self.assertEqual(self.check(result, "release_manifest_identity")["reason"], "manifest_artifact_duplicate")

        fixture = Fixture()
        fixture.manifest["artifacts"].pop()  # type: ignore[union-attr]
        fixture.authorize_manifest()
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "release_manifest_identity")["reason"],
            "manifest_required_artifact_missing",
        )

    def test_missing_listener_fails(self) -> None:
        self.fixture.listeners_v4 = (
            b"\n".join(self.fixture.listeners_v4.splitlines()[:-1]) + b"\n"
        )
        result = self.run_fixture()
        self.assertEqual(self.check(result, "required_listeners")["reason"], "listener_contract_mismatch")

    def test_protected_listener_multiset_rejects_extra_family_and_duplicate(self) -> None:
        daemon_pid = self.fixture.main_pids["sentinel-daemon.service"]
        cases = (
            ("wildcard", "ipv4", f'tcp LISTEN 0 128 0.0.0.0:8084 0.0.0.0:* users:(("service",pid={daemon_pid},fd=7))\n'.encode()),
            ("ipv6", "ipv6", f'tcp LISTEN 0 128 [::]:8084 [::]:* users:(("service",pid={daemon_pid},fd=7))\n'.encode()),
            ("duplicate", "ipv4", f'tcp LISTEN 0 128 127.0.0.1:8084 0.0.0.0:* users:(("service",pid={daemon_pid},fd=8))\n'.encode()),
        )
        for name, family, line in cases:
            with self.subTest(name=name):
                fixture = Fixture()
                attribute = f"listeners_{'v4' if family == 'ipv4' else 'v6'}"
                setattr(fixture, attribute, getattr(fixture, attribute) + line)
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(
                    self.check(result, "required_listeners")["reason"],
                    "listener_contract_mismatch",
                )

        fixture = Fixture()
        fixture.listeners_v4 = b"\n".join(
            line
            for line in fixture.listeners_v4.splitlines()
            if b"127.0.0.1:8084" not in line
        ) + b"\n"
        fixture.listeners_v6 += b"tcp LISTEN 0 128 [::1]:8084 [::]:*\n"
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "required_listeners")["reason"],
            "listener_contract_mismatch",
        )

    def test_unrelated_listener_is_allowed(self) -> None:
        self.fixture.listeners_v4 += (
            b"tcp LISTEN 0 128 0.0.0.0:9999 0.0.0.0:*\n"
            b"tcp LISTEN 0 4096 127.0.0.53%lo:53 0.0.0.0:*\n"
        )
        self.assertTrue(self.run_fixture()["runtime_preflight_pass"])

    def test_listener_process_owner_is_required_and_exact(self) -> None:
        unit = "sentinel-daemon.service"
        correct_pid = self.fixture.main_pids[unit]
        marker = f'users:(("service",pid={correct_pid},fd=7))'.encode()
        cases = (
            ("omitted", b"", "listener_process_ambiguous"),
            (
                "foreign",
                f'users:(("foreign",pid={correct_pid + 999},fd=7))'.encode(),
                "listener_process_mismatch",
            ),
            (
                "ambiguous",
                (
                    f'users:(("service",pid={correct_pid},fd=7),'
                    f'("foreign",pid={correct_pid + 1},fd=8))'
                ).encode(),
                "listener_process_ambiguous",
            ),
        )
        for name, replacement, reason in cases:
            with self.subTest(name=name):
                fixture = Fixture()
                fixture.listeners_v4 = fixture.listeners_v4.replace(
                    marker, replacement, 1
                )
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(
                    self.check(result, "required_listeners")["reason"], reason
                )

    def test_default_running_executable_hash_binds_proc_object(self) -> None:
        executable = Path(sys.executable).resolve()
        expected = hashlib.sha256(executable.read_bytes()).hexdigest()
        self.assertEqual(
            preflight.default_hash_running_executable(
                os.getpid(), executable, preflight.MAX_ARTIFACT_BYTES
            ),
            (expected, executable.stat().st_size),
        )
        with self.assertRaisesRegex(
            preflight.PreflightError, "running_executable_identity_mismatch"
        ):
            preflight.default_hash_running_executable(
                os.getpid(), Path("/usr/bin/true"), preflight.MAX_ARTIFACT_BYTES
            )

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
        ss_argv = ("/usr/bin/ss", "-H", "-lntup", "-4")
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
            expected_manifest_sha256=inputs.expected_manifest_sha256,
            event_store=Path("/opt/sentinel/data/events.db;touch-owned"),
            projection_store=inputs.projection_store,
        )
        with self.assertRaisesRegex(preflight.PreflightError, "store_path_invalid"):
            preflight.evaluate(inputs, self.fixture.deps())
        self.assertEqual(self.fixture.commands, [])

    def test_store_backlog_and_projection_lag_fail(self) -> None:
        self.fixture.event_store["unpublished_outbox"] = 1
        result = self.run_fixture()
        self.assertEqual(self.check(result, "store_projection_backlog")["reason"], "publication_or_recovery_backlog")
        fixture = Fixture()
        fixture.projection_store[0]["last_event_id"] = 40
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(self.check(result, "store_projection_backlog")["reason"], "read_model_projection_lag")

        fixture = Fixture()
        fixture.projection_store[1]["last_event_id"] = 40
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "store_projection_backlog")["reason"],
            "read_model_projection_lag",
        )

    def test_failed_outbox_row_is_publication_backlog(self) -> None:
        self.assertIn("status != 'published'", preflight.EVENT_STORE_SQL)
        self.fixture.event_store["unpublished_outbox"] = 1
        result = self.run_fixture()
        self.assertEqual(
            self.check(result, "store_projection_backlog")["reason"],
            "publication_or_recovery_backlog",
        )

    def test_missing_event_store_projection_offsets_are_temporal_but_malformed_fail(self) -> None:
        for key in ("projection_offset", "hierarchy_offset"):
            with self.subTest(key=key, value=None):
                fixture = Fixture()
                fixture.event_store[key] = None
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(
                    self.check(result, "store_projection_backlog")["reason"],
                    "read_model_projection_lag",
                )
            with self.subTest(key=key, value="41"):
                fixture = Fixture()
                fixture.event_store[key] = "41"
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(
                    self.check(result, "store_projection_backlog")["reason"],
                    "store_readback_value",
                )

    def test_projection_watermarks_and_identities_use_one_snapshot(self) -> None:
        result = self.run_fixture()
        self.assertTrue(result["runtime_preflight_pass"])
        projection_commands = [
            argv
            for argv in self.fixture.commands
            if len(argv) > 4 and Path(argv[3]) == PROJECTION_DB
        ]
        self.assertEqual(len(projection_commands), 1)
        self.assertEqual(projection_commands[0][4], preflight.PROJECTION_SNAPSHOT_SQL)

        fixture = Fixture()
        mixed = fixture.projection_snapshot()
        mixed[0]["last_event_id"] = 40
        projection_argv = (
            "/usr/bin/sqlite3",
            "-readonly",
            "-json",
            str(PROJECTION_DB),
            preflight.PROJECTION_SNAPSHOT_SQL,
        )
        fixture.command_overrides[projection_argv] = encoded(mixed)
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "store_projection_backlog")["reason"],
            "read_model_projection_lag",
        )

        for name, mutate, reason in (
            (
                "null_watermark",
                lambda fixture: fixture.projection_store[0].__setitem__(
                    "last_event_id", None
                ),
                "read_model_projection_lag",
            ),
            (
                "missing_watermark",
                lambda fixture: fixture.projection_store.pop(),
                "read_model_projection_lag",
            ),
            (
                "malformed_watermark",
                lambda fixture: fixture.projection_store[0].__setitem__(
                    "last_event_id", "41"
                ),
                "store_readback_value",
            ),
        ):
            with self.subTest(name=name):
                fixture = Fixture()
                mutate(fixture)
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(
                    self.check(result, "store_projection_backlog")["reason"],
                    reason,
                )

    def test_episode_frontier_accepts_exact_subject_local_lag(self) -> None:
        fixture = Fixture()
        agent = fixture.http_payloads["episode_projection"]["agents"][0]  # type: ignore[index]
        agent["frontier_source_row_id"] = 40
        agent["lag_rows"] = 1
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(self.check(result, "identity_readiness")["status"], "PASS")

    def test_episode_frontier_rejects_missing_future_or_inconsistent_lag(self) -> None:
        for frontier, lag_rows, reason in (
            (None, 0, "episode_projection_frontier_missing"),
            (42, 0, "episode_projection_frontier_mismatch"),
            (40, 0, "episode_projection_frontier_mismatch"),
            (40, 2, "episode_projection_frontier_mismatch"),
        ):
            with self.subTest(frontier=frontier, lag_rows=lag_rows):
                fixture = Fixture()
                agent = fixture.http_payloads["episode_projection"]["agents"][0]  # type: ignore[index]
                agent["frontier_source_row_id"] = frontier
                agent["lag_rows"] = lag_rows
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(self.check(result, "identity_readiness")["reason"], reason)

    def test_event_cut_progress_between_projection_reads_fails(self) -> None:
        before = copy.deepcopy(self.fixture.event_store)
        after = copy.deepcopy(before)
        for key in ("latest_event_id", "projection_offset", "hierarchy_offset"):
            after[key] = 42
        self.fixture.event_store_reads = [before, after]
        result = self.run_fixture()
        self.assertEqual(
            self.check(result, "store_projection_backlog")["reason"],
            "event_cut_changed",
        )

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

    def test_canonical_roster_rejects_missing_gap_extra_and_renamed_files(self) -> None:
        cases = [
            ("missing", lambda fixture: fixture.agent_paths.pop()),
            (
                "id_gap",
                lambda fixture: fixture.files.__setitem__(
                    fixture.agent_paths[10],
                    fixture.files[fixture.agent_paths[10]].replace(b"id = 11", b"id = 99"),
                ),
            ),
            (
                "extra",
                lambda fixture: fixture.agent_paths.append(
                    fixture.agents_dir / "AGENT-61-EXTRA.toml"
                ),
            ),
            (
                "renamed",
                lambda fixture: fixture.agent_paths.__setitem__(
                    0, fixture.agents_dir / "AGENT-01-RENAMED.toml"
                ),
            ),
            (
                "duplicate_filename",
                lambda fixture: fixture.agent_paths.__setitem__(
                    1, fixture.agent_paths[0]
                ),
            ),
            (
                "identity_rename",
                lambda fixture: fixture.files.__setitem__(
                    fixture.agent_paths[0],
                    fixture.files[fixture.agent_paths[0]].replace(
                        b'name = "Thomas Mueller"', b'name = "Thomas Renamed"'
                    ),
                ),
            ),
        ]
        for name, mutate in cases:
            with self.subTest(name=name):
                fixture = Fixture()
                mutate(fixture)
                result = preflight.evaluate(fixture.inputs(), fixture.deps())
                self.assertEqual(
                    self.check(result, "contract_profile_roster")["status"], "FAIL"
                )

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
            deps.https,
            deps.read_file,
            deps.hash_file,
            deps.hash_running_executable,
            deps.list_agents,
            lambda path, limit: (_ for _ in ()).throw(
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

    def test_default_https_pins_peer_and_rejects_redirect_proxy_and_wrong_origin(self) -> None:
        previous = os.environ.get("HTTPS_PROXY")
        os.environ["HTTPS_PROXY"] = "http://127.0.0.1:1"
        try:
            with tls_transport_server() as (
                server,
                pem,
                peer_digest,
                tls_root,
                generate_certificate,
            ):
                health = preflight.default_https(
                    f"{preflight.DASHBOARD_ORIGIN}/api/health",
                    2.0,
                    1024,
                    pem,
                    peer_digest,
                )
                self.assertEqual(preflight.strict_json(health), {"status": "ok"})
                cert_hash = preflight.strict_json(
                    preflight.default_https(
                        f"{preflight.DASHBOARD_ORIGIN}/api/cert-hash",
                        2.0,
                        1024,
                        pem,
                        peer_digest,
                    )
                )
                self.assertEqual(cert_hash["hash"], server.state.certificate_hash)
                with self.assertRaisesRegex(
                    preflight.PreflightError, "https_peer_pin_mismatch"
                ):
                    preflight.default_https(
                        f"{preflight.DASHBOARD_ORIGIN}/api/health",
                        2.0,
                        1024,
                        pem,
                        "0" * 64,
                    )
                replacement_cert, _ = generate_certificate(tls_root, "replacement")
                replacement_pem = replacement_cert.read_bytes()
                replacement_der = ssl.PEM_cert_to_DER_cert(
                    replacement_pem.decode("ascii")
                )
                with self.assertRaisesRegex(preflight.PreflightError, "https_failed"):
                    preflight.default_https(
                        f"{preflight.DASHBOARD_ORIGIN}/api/health",
                        2.0,
                        1024,
                        replacement_pem,
                        hashlib.sha256(replacement_der).hexdigest(),
                    )
                with self.assertRaisesRegex(preflight.PreflightError, "http_status"):
                    preflight.default_https(
                        f"{preflight.DASHBOARD_ORIGIN}/redirect",
                        2.0,
                        1024,
                        pem,
                        peer_digest,
                    )
                with self.assertRaisesRegex(preflight.PreflightError, "http_failed"):
                    preflight.default_http(
                        "http://127.0.0.1:8001/api/health", None, 1.0, 1024
                    )
                self.assertNotIn("/credential-sink", server.state.paths)
                self.assertTrue(all(value is None for value in server.state.authorization))
            with self.assertRaisesRegex(
                preflight.PreflightError, "http_origin_not_loopback"
            ):
                preflight.default_https(
                    "https://127.0.0.2:8001/api/health",
                    1.0,
                    1024,
                    TEST_CERTIFICATE,
                    "0" * 64,
                )
            with self.assertRaisesRegex(preflight.PreflightError, "https_origin_invalid"):
                preflight.default_https(
                    "https://127.0.0.1:1/api/health",
                    1.0,
                    1024,
                    TEST_CERTIFICATE,
                    "0" * 64,
                )
        finally:
            if previous is None:
                os.environ.pop("HTTPS_PROXY", None)
            else:
                os.environ["HTTPS_PROXY"] = previous

    def test_dashboard_cert_hash_mismatch_and_replaced_pin_fail(self) -> None:
        url = f"{preflight.DASHBOARD_ORIGIN}/api/cert-hash"
        self.fixture.https_overrides[url] = encoded(
            {"algorithm": "sha-256", "hash": base64.b64encode(b"x" * 32).decode("ascii")}
        )
        result = self.run_fixture()
        self.assertEqual(
            self.check(result, "loopback_health")["reason"],
            "https_certificate_hash_mismatch",
        )

        fixture = Fixture()
        fixture.https_overrides[f"{preflight.DASHBOARD_ORIGIN}/api/health"] = (
            preflight.PreflightError("https_peer_pin_mismatch")
        )
        result = preflight.evaluate(fixture.inputs(), fixture.deps())
        self.assertEqual(
            self.check(result, "loopback_health")["reason"],
            "https_peer_pin_mismatch",
        )

    def test_descriptor_pinned_reader_rejects_symlink_hardlink_and_replacement(self) -> None:
        runner_temp = Path(os.environ["RUNNER_TEMP"])
        runner_temp.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="cdx1-650-files-", dir=runner_temp) as raw:
            root = Path(raw)
            original = root / "original"
            original.write_bytes(b"original")
            original.chmod(0o600)
            symlink = root / "symlink"
            symlink.symlink_to(original)
            hardlink = root / "hardlink"
            os.link(original, hardlink)
            for path in (symlink, hardlink):
                with self.subTest(path=path.name):
                    with self.assertRaisesRegex(preflight.PreflightError, "unsafe_file"):
                        preflight.default_read_file(path, 64)

            replacement = root / "replacement"
            replacement.write_bytes(b"before")
            replacement.chmod(0o600)
            real_open = os.open
            replaced = False

            def replace_before_open(
                path: object, flags: int, *args: object, **kwargs: object
            ) -> int:
                nonlocal replaced
                if path == replacement.name and kwargs.get("dir_fd") is not None and not replaced:
                    replaced = True
                    replacement.unlink()
                    replacement.write_bytes(b"after")
                    replacement.chmod(0o600)
                return real_open(path, flags, *args, **kwargs)

            with mock.patch.object(preflight.os, "open", side_effect=replace_before_open):
                with self.assertRaisesRegex(preflight.PreflightError, "unsafe_file"):
                    preflight.default_read_file(replacement, 64)
            self.assertTrue(replaced)

    def test_descriptor_pinning_rejects_parent_symlink_replacement_and_modes(self) -> None:
        runner_temp = Path(os.environ["RUNNER_TEMP"])
        runner_temp.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="cdx1-650-path-", dir=runner_temp) as raw:
            root = Path(raw)
            real_parent = root / "real"
            real_parent.mkdir(mode=0o755)
            leaf = real_parent / "artifact"
            leaf.write_bytes(b"content")
            leaf.chmod(0o644)
            linked_parent = root / "linked"
            linked_parent.symlink_to(real_parent, target_is_directory=True)
            with self.assertRaisesRegex(preflight.PreflightError, "unsafe_path_component"):
                preflight.default_read_file(linked_parent / "artifact", 64)

            for mode in (0o664, 0o4755):
                with self.subTest(mode=oct(mode)):
                    leaf.chmod(mode)
                    with self.assertRaisesRegex(preflight.PreflightError, "unsafe_file_mode"):
                        preflight.default_read_file(leaf, 64)
            leaf.chmod(0o644)

            parent = root / "replace-parent"
            parent.mkdir(mode=0o755)
            target = parent / "artifact"
            target.write_bytes(b"before")
            target.chmod(0o644)
            old_parent = root / "old-parent"
            real_read = os.read
            replaced = False

            def replace_parent(fd: int, size: int) -> bytes:
                nonlocal replaced
                block = real_read(fd, size)
                if block and not replaced:
                    replaced = True
                    parent.rename(old_parent)
                    parent.mkdir(mode=0o755)
                    replacement = parent / "artifact"
                    replacement.write_bytes(b"after")
                    replacement.chmod(0o644)
                return block

            with mock.patch.object(preflight.os, "read", side_effect=replace_parent):
                with self.assertRaisesRegex(
                    preflight.PreflightError, "unsafe_path_component"
                ):
                    preflight.default_read_file(target, 64)
            self.assertTrue(replaced)

    def test_agent_directory_is_component_pinned(self) -> None:
        runner_temp = Path(os.environ["RUNNER_TEMP"])
        runner_temp.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="cdx1-650-agents-", dir=runner_temp) as raw:
            root = Path(raw)
            agents = root / "agents"
            agents.mkdir(mode=0o755)
            for name in sorted(preflight.CANONICAL_AGENT_FILES):
                item = agents / name
                item.write_bytes(b"")
                item.chmod(0o644)
            self.assertEqual(len(preflight.default_list_agents(agents)), preflight.MAX_AGENTS)

            link = root / "agent-link"
            link.symlink_to(agents, target_is_directory=True)
            with self.assertRaisesRegex(preflight.PreflightError, "unsafe_path_component"):
                preflight.default_list_agents(link)

            moved = root / "agents-old"
            real_listdir = os.listdir
            replaced = False

            def replace_directory(path: object) -> list[str]:
                nonlocal replaced
                names = real_listdir(path)
                if not replaced:
                    replaced = True
                    agents.rename(moved)
                    agents.mkdir(mode=0o755)
                return names

            with mock.patch.object(preflight.os, "listdir", side_effect=replace_directory):
                with self.assertRaisesRegex(
                    preflight.PreflightError, "unsafe_path_component"
                ):
                    preflight.default_list_agents(agents)
            self.assertTrue(replaced)

    def test_secret_reader_uses_same_owner_only_pinned_descriptor(self) -> None:
        runner_temp = Path(os.environ["RUNNER_TEMP"])
        runner_temp.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="cdx1-650-secret-", dir=runner_temp) as raw:
            root = Path(raw)
            secret = root / "secret"
            secret.write_bytes(SECRET.encode("ascii"))
            secret.chmod(0o600)
            self.assertEqual(
                preflight.default_read_secret(secret, 4096), SECRET.encode("ascii")
            )
            secret.chmod(0o640)
            with self.assertRaisesRegex(
                preflight.PreflightError, "credential_permissions_invalid"
            ):
                preflight.default_read_secret(secret, 4096)

            replacement = root / "replacement-secret"
            replacement.write_bytes(SECRET.encode("ascii"))
            replacement.chmod(0o600)
            real_open = os.open
            replaced = False

            def replace_secret(
                path: object, flags: int, *args: object, **kwargs: object
            ) -> int:
                nonlocal replaced
                if path == replacement.name and kwargs.get("dir_fd") is not None and not replaced:
                    replaced = True
                    replacement.unlink()
                    replacement.write_bytes((SECRET + "-changed").encode("ascii"))
                    replacement.chmod(0o600)
                return real_open(path, flags, *args, **kwargs)

            with mock.patch.object(preflight.os, "open", side_effect=replace_secret):
                with self.assertRaisesRegex(
                    preflight.PreflightError, "credential_permissions_invalid"
                ):
                    preflight.default_read_secret(replacement, 4096)
            self.assertTrue(replaced)

    def test_default_command_rejects_unapproved_executable(self) -> None:
        with self.assertRaisesRegex(preflight.PreflightError, "command_not_allowed"):
            preflight.default_command(["/bin/sh", "-c", "true"], 1.0, 64)


if __name__ == "__main__":
    unittest.main()
