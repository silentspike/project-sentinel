#!/usr/bin/env python3
"""Fail-closed, token-free runtime preflight for the single-node M0 journey."""

from __future__ import annotations

import argparse
import base64
from collections import Counter
from contextlib import contextmanager
import hmac
from dataclasses import dataclass
import hashlib
import http.client
import ipaddress
import json
import os
from pathlib import Path
import re
import selectors
import ssl
import stat
import subprocess
import sys
import time
import tomllib
from typing import Any, Callable, Iterator
from urllib import parse


SCHEMA_VERSION = 1
MAX_FILE_BYTES = 4 * 1024 * 1024
MAX_ARTIFACT_BYTES = 1024 * 1024 * 1024
MAX_COMMAND_BYTES = 512 * 1024
MAX_HTTP_BYTES = 1024 * 1024
MAX_TIMEOUT_SECONDS = 15.0
MAX_AGENTS = 60
LLM_COMPLETION_IN_FLIGHT_GRACE_MS = 120_000
LLM_COMPLETION_FUTURE_SKEW_MS = 5_000
DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")

TARGET_UNIT = "sentinel.target"
AUTH_INIT_UNIT = "sentinel-auth-init.service"
REQUIRED_SERVICES = {
    "nats-server.service",
    "sentinel-daemon.service",
    "sentinel-dashboard-backend.service",
    "sentinel-gaia-loop.service",
    "sentinel-gateway.service",
    "sentinel-judge.service",
    "sentinel-nats-bridge.service",
    "sentinel-projection.service",
}
REQUIRED_TIMERS = {"sentinel-health-monitor.timer", "sentinel-nightrun.timer"}
TARGET_WANTS = REQUIRED_SERVICES | REQUIRED_TIMERS
REQUIRED_UNITS = TARGET_WANTS | {AUTH_INIT_UNIT}
SERVICE_EXECUTABLES = {
    "nats-server.service": Path("/usr/local/bin/nats-server"),
    "sentinel-daemon.service": Path("/opt/sentinel/bin/sentinel-daemon"),
    "sentinel-dashboard-backend.service": Path(
        "/opt/sentinel/bin/sentinel-dashboard-backend"
    ),
    "sentinel-gaia-loop.service": Path("/opt/sentinel/bin/sentinel-gaia-loop"),
    "sentinel-gateway.service": Path("/opt/sentinel/bin/cortex-gateway"),
    "sentinel-judge.service": Path("/opt/sentinel/bin/sentinel-judge"),
    "sentinel-nats-bridge.service": Path("/opt/sentinel/bin/sentinel-nats-bridge"),
    "sentinel-projection.service": Path("/opt/sentinel/bin/sentinel-projection"),
}
TIMER_SERVICES = {
    "sentinel-health-monitor.timer": "sentinel-health-monitor.service",
    "sentinel-nightrun.timer": "sentinel-nightrun.service",
}
ACTIVATION_ONESHOT_TIMERS = {"sentinel-health-monitor.timer"}
M0_CONTRACT_PATH = Path("/opt/sentinel/config/product-acceptance/m0-contract.toml")
M0_PROFILE_PATH = Path("/opt/sentinel/config/work-profiles/web-project-v1.toml")
M0_WORKBENCH_PROFILE_PATH = Path(
    "/opt/sentinel/config/workbench-profiles/web-authoring-v1.toml"
)
M0_QA_PROFILE_PATH = Path("/opt/sentinel/config/workbench-profiles/web-qa-v1.toml")
M0_JOURNEY_PATH = Path("/opt/sentinel/config/product-acceptance/m0-journey-v2.json")
COLLABORATION_STUDY_PATH = Path(
    "/opt/sentinel/config/product-acceptance/collaboration-admission-study-v1.json"
)
M0_RESTART_CONTROL_PATH = Path(
    "/opt/sentinel/config/product-acceptance/m0-restart-control-v1.json"
)
EXPECTED_LISTENERS = {
    ("tcp", "ipv4", "127.0.0.1", 4222),
    ("tcp", "ipv4", "127.0.0.1", 8001),
    ("udp", "ipv4", "127.0.0.1", 8001),
    ("tcp", "ipv4", "0.0.0.0", 8080),
    ("tcp", "ipv4", "127.0.0.1", 8081),
    ("tcp", "ipv4", "0.0.0.0", 8082),
    ("tcp", "ipv4", "127.0.0.1", 8083),
    ("tcp", "ipv4", "127.0.0.1", 8084),
    ("tcp", "ipv4", "127.0.0.1", 8222),
    ("tcp", "ipv4", "127.0.0.1", 9090),
}
PROTECTED_LISTENER_PORTS = {item[3] for item in EXPECTED_LISTENERS}
LISTENER_SERVICES = {
    ("tcp", "ipv4", "127.0.0.1", 4222): "nats-server.service",
    ("tcp", "ipv4", "127.0.0.1", 8001): "sentinel-dashboard-backend.service",
    ("udp", "ipv4", "127.0.0.1", 8001): "sentinel-dashboard-backend.service",
    ("tcp", "ipv4", "0.0.0.0", 8080): "sentinel-gateway.service",
    ("tcp", "ipv4", "127.0.0.1", 8081): "sentinel-gateway.service",
    ("tcp", "ipv4", "0.0.0.0", 8082): "sentinel-judge.service",
    ("tcp", "ipv4", "127.0.0.1", 8083): "sentinel-nats-bridge.service",
    ("tcp", "ipv4", "127.0.0.1", 8084): "sentinel-daemon.service",
    ("tcp", "ipv4", "127.0.0.1", 8222): "nats-server.service",
    ("tcp", "ipv4", "127.0.0.1", 9090): "sentinel-daemon.service",
}
HTTP_CONTRACTS = (
    ("gateway_health", "http://127.0.0.1:8080/health", None, "status", "ok"),
    ("gateway_ready", "http://127.0.0.1:8080/ready", None, "ready", True),
    ("gateway_control_health", "http://127.0.0.1:8081/health", None, "status", "ok"),
    ("gateway_control_ready", "http://127.0.0.1:8081/ready", None, "status", "ok"),
    ("judge_health", "http://127.0.0.1:8082/health", None, "status", "ok"),
    ("judge_ready", "http://127.0.0.1:8082/ready", None, "ready", True),
    ("bridge_health", "http://127.0.0.1:8083/health", None, "status", "ok"),
    ("bridge_ready", "http://127.0.0.1:8083/ready", None, "status", "ok"),
    (
        "nats_health",
        "http://127.0.0.1:8222/healthz?js-enabled-only=true",
        None,
        "status",
        "ok",
    ),
    ("runtime_health", "http://127.0.0.1:8084/operator/runtime-health", "operator", None, None),
    ("platform_state", "http://127.0.0.1:8084/operator/platform-state", "operator", None, None),
    ("episode_projection", "http://127.0.0.1:8084/operator/episode-projection", "operator", None, None),
)
DASHBOARD_ORIGIN = "https://127.0.0.1:8001"
DASHBOARD_CERT_PATH = Path("/opt/sentinel/data/dashboard-cert/console-cert.pem")
CANONICAL_AGENT_FILES = {
    "AGENT-01-THOMAS-CEO.toml",
    "AGENT-02-LISA-DESIGN.toml",
    "AGENT-03-MAX-DESIGN.toml",
    "AGENT-04-SOPHIE-DESIGN.toml",
    "AGENT-05-ANDREAS-DEV.toml",
    "AGENT-06-JULIA-DEV.toml",
    "AGENT-07-KAI-DEV.toml",
    "AGENT-08-LENA-DEV.toml",
    "AGENT-09-SARAH-PM.toml",
    "AGENT-10-DANIEL-PM.toml",
    "AGENT-11-MARCO-SALES.toml",
    "AGENT-12-NINA-MARKETING.toml",
    "AGENT-13-PETRA-ADMIN.toml",
    "AGENT-14-FLORIAN-IT.toml",
    "AGENT-15-HANNAH-WERKSTUD.toml",
    "AGENT-16-MICHAEL-CEO.toml",
    "AGENT-17-CARLA-DESIGN.toml",
    "AGENT-18-ROBIN-DESIGN.toml",
    "AGENT-19-TIM-DESIGN.toml",
    "AGENT-20-MARTIN-DEV.toml",
    "AGENT-21-FATIMA-DEV.toml",
    "AGENT-22-JONAS-DEV.toml",
    "AGENT-23-ANNA-DEVOPS.toml",
    "AGENT-24-ELENA-PM.toml",
    "AGENT-25-LUKAS-PM.toml",
    "AGENT-26-OLIVER-SALES.toml",
    "AGENT-27-MARA-MARKETING.toml",
    "AGENT-28-GABI-ADMIN.toml",
    "AGENT-29-TOBIAS-IT.toml",
    "AGENT-30-YARA-WERKSTUD.toml",
    "AGENT-31-SANDRA-CEO.toml",
    "AGENT-32-JENS-DESIGN.toml",
    "AGENT-33-PRIYA-DESIGN.toml",
    "AGENT-34-LEA-DESIGN.toml",
    "AGENT-35-KEVIN-DEV.toml",
    "AGENT-36-NILS-DEV.toml",
    "AGENT-37-SELINA-DEV.toml",
    "AGENT-38-PAUL-DEV.toml",
    "AGENT-39-VICTORIA-PM.toml",
    "AGENT-40-DAVID-PM.toml",
    "AGENT-41-FRANK-SALES.toml",
    "AGENT-42-JASMIN-MARKETING.toml",
    "AGENT-43-MONIKA-ADMIN.toml",
    "AGENT-44-MARCUS-IT.toml",
    "AGENT-45-EMILIA-WERKSTUD.toml",
    "AGENT-46-RALF-BETRIEBSRAT.toml",
    "AGENT-47-AYLIN-BETRIEBSRAT.toml",
    "AGENT-48-STEFAN-BETRIEBSRAT.toml",
    "AGENT-49-CARLA-BETRIEBSPSYCH.toml",
    "AGENT-50-KATHARINA-BETRIEBSPSYCH.toml",
    "AGENT-51-HENDRIK-BETRIEBSPSYCH.toml",
    "AGENT-52-WERNER-BETRIEBSARZT.toml",
    "AGENT-53-WIESNER-BETRIEBSARZT.toml",
    "AGENT-54-BRANDT-BETRIEBSARZT.toml",
    "AGENT-55-LAURA-QA.toml",
    "AGENT-56-TOBIAS-DELIVERY.toml",
    "AGENT-57-CHEN-QA.toml",
    "AGENT-58-MARIA-DELIVERY.toml",
    "AGENT-59-AMIR-QA.toml",
    "AGENT-60-KATRIN-DELIVERY.toml",
}
CANONICAL_ROSTER_DIGEST = "6b0c1bb6a52c3c18fa736ce9e763541ba6d15e8619d0e258d99140e6b603784c"
CANONICAL_RELEASE_ARTIFACTS: dict[str, tuple[str, str]] = {
    "/opt/sentinel/bin/sentinel-daemon": ("target/release/sentinel-daemon", "binary"),
    "/usr/bin/agent-runtime": ("target/release/agent-runtime", "binary"),
    "/opt/sentinel/bin/landlock-wrapper": ("target/release/landlock-wrapper", "binary"),
    "/opt/sentinel/bin/sentinel-nightrun": ("target/release/sentinel-nightrun", "binary"),
    "/opt/sentinel/bin/sentinel-projection": ("target/release/sentinel-projection", "binary"),
    "/opt/sentinel/bin/sentinel-dashboard-backend": (
        "target/release/sentinel-dashboard-backend",
        "binary",
    ),
    "/opt/sentinel/bin/sentinel-gaia-loop": ("target/release/sentinel-gaia-loop", "binary"),
    "/opt/sentinel/bin/sentinel-ctl": ("target/release/sentinel-ctl", "binary"),
    "/opt/sentinel/bin/sentinel-gaia": ("target/release/sentinel-gaia", "binary"),
    "/opt/sentinel/bin/cortex-gateway": ("cmd/cortex-gateway/cortex-gateway", "binary"),
    "/opt/sentinel/bin/sentinel-judge": ("services/sentinel-judge/sentinel-judge", "binary"),
    "/opt/sentinel/bin/sentinel-nats-bridge": (
        "services/sentinel-nats-bridge/sentinel-nats-bridge",
        "binary",
    ),
    "/usr/local/bin/nats-server": ("external/nats-server", "binary"),
    "/opt/sentinel/console-dist/index.html": ("console/dist/index.html", "config"),
    "/opt/sentinel/console-dist/assets/app.js": (
        "console/dist/assets/app.js",
        "config",
    ),
    "/opt/sentinel/console-dist/assets/app.js.map": (
        "console/dist/assets/app.js.map",
        "config",
    ),
    "/opt/sentinel/console-dist/assets/index.css": (
        "console/dist/assets/index.css",
        "config",
    ),
    "/opt/sentinel/config/daemon.toml": ("config/daemon.toml", "config"),
    "/opt/sentinel/config/cortex-gateway.toml": ("config/cortex-gateway.toml", "config"),
    "/opt/sentinel/config/nightrun.toml": ("config/nightrun.toml", "config"),
    "/opt/sentinel/config/judge.toml": ("config/judge.toml", "config"),
    "/opt/sentinel/config/nats-bridge.toml": ("config/nats-bridge.toml", "config"),
    "/opt/sentinel/config/simulation.toml": ("config/simulation.toml", "config"),
    "/opt/sentinel/config/rooms.toml": ("config/rooms.toml", "config"),
    "/opt/sentinel/config/company.toml": ("config/company.toml", "config"),
    "/opt/sentinel/config/controlplane.toml": ("config/controlplane.toml", "config"),
    "/etc/nats/nats.conf": ("config/nats.conf", "config"),
    "/etc/systemd/system/sentinel-auth-init.service": (
        "deploy/systemd/sentinel-auth-init.service",
        "systemd",
    ),
    "/etc/systemd/system/sentinel-daemon.service": (
        "deploy/systemd/sentinel-daemon.service",
        "systemd",
    ),
    "/etc/systemd/system/sentinel-gateway.service": (
        "deploy/systemd/sentinel-gateway.service",
        "systemd",
    ),
    "/etc/systemd/system/sentinel-judge.service": (
        "deploy/systemd/sentinel-judge.service",
        "systemd",
    ),
    "/etc/systemd/system/sentinel-nats-bridge.service": (
        "deploy/systemd/sentinel-nats-bridge.service",
        "systemd",
    ),
    "/etc/systemd/system/sentinel-nightrun.service": (
        "deploy/systemd/sentinel-nightrun.service",
        "systemd",
    ),
    "/etc/systemd/system/sentinel-nightrun.timer": (
        "deploy/systemd/sentinel-nightrun.timer",
        "systemd",
    ),
    "/etc/systemd/system/sentinel-projection.service": (
        "deploy/systemd/sentinel-projection.service",
        "systemd",
    ),
    "/etc/systemd/system/sentinel-dashboard-backend.service": (
        "deploy/systemd/sentinel-dashboard-backend.service",
        "systemd",
    ),
    "/etc/systemd/system/sentinel-gaia-loop.service": (
        "deploy/systemd/sentinel-gaia-loop.service",
        "systemd",
    ),
    "/etc/systemd/system/nats-server.service": (
        "deploy/systemd/nats-server.service",
        "systemd",
    ),
    "/etc/systemd/system/sentinel.target": ("deploy/systemd/sentinel.target", "systemd"),
    "/opt/sentinel/scripts/init-cgroups.sh": ("deploy/scripts/init-cgroups.sh", "script"),
    "/opt/sentinel/scripts/init-dirs.sh": ("deploy/scripts/init-dirs.sh", "script"),
    "/opt/sentinel/scripts/init-m0-runtime-dirs.py": (
        "deploy/scripts/init-m0-runtime-dirs.py",
        "script",
    ),
    "/opt/sentinel/scripts/init-runtime-base-dirs.sh": (
        "deploy/scripts/init-runtime-base-dirs.sh",
        "script",
    ),
    "/opt/sentinel/scripts/init-dashboard-auth.sh": (
        "deploy/scripts/init-dashboard-auth.sh",
        "script",
    ),
    "/opt/sentinel/scripts/install-native-claude.sh": (
        "deploy/scripts/install-native-claude.sh",
        "script",
    ),
    "/opt/sentinel/scripts/install-native-codex.sh": (
        "deploy/scripts/install-native-codex.sh",
        "script",
    ),
    "/opt/sentinel/scripts/init-hugepages.sh": ("deploy/scripts/init-hugepages.sh", "script"),
    "/opt/sentinel/scripts/init-sysctl.sh": ("deploy/scripts/init-sysctl.sh", "script"),
    "/opt/sentinel/scripts/init-tmpfs.sh": ("deploy/scripts/init-tmpfs.sh", "script"),
    "/opt/sentinel/share/runtime-base.env": ("deploy/runtime-base.env", "config"),
    "/etc/apt/preferences.d/sentinel-runtime": (
        "deploy/apt/sentinel-runtime.pref",
        "config",
    ),
    "/etc/sysctl.d/99-sentinel-bwrap.conf": (
        "deploy/vm-config/99-sentinel-bwrap.conf",
        "config",
    ),
    "/etc/systemd/system/sentinel-health-monitor.service": (
        "deploy/systemd/sentinel-health-monitor.service",
        "systemd",
    ),
    "/etc/systemd/system/sentinel-health-monitor.timer": (
        "deploy/systemd/sentinel-health-monitor.timer",
        "systemd",
    ),
    "/opt/sentinel/scripts/sentinel-health-monitor.sh": (
        "deploy/scripts/sentinel-health-monitor.sh",
        "script",
    ),
    "/opt/sentinel/scripts/m0-readiness.py": (
        "scripts/product-acceptance/m0-readiness/readiness.py",
        "script",
    ),
    "/opt/sentinel/scripts/init-company-workflow-auth.sh": (
        "deploy/scripts/init-company-workflow-auth.sh",
        "script",
    ),
    "/opt/sentinel/config/company-principals.json": (
        "config/company-principals.json",
        "config",
    ),
    str(M0_PROFILE_PATH): ("config/work-profiles/web-project-v1.toml", "config"),
    str(M0_WORKBENCH_PROFILE_PATH): (
        "config/workbench-profiles/web-authoring-v1.toml",
        "config",
    ),
    str(M0_QA_PROFILE_PATH): ("config/workbench-profiles/web-qa-v1.toml", "config"),
    "/usr/bin/sentinel-web-qa": ("deploy/scripts/web-qa-v1.py", "script"),
    "/usr/bin/sentinel-work-item-gate": (
        "deploy/scripts/work-item-gate-v1.py",
        "script",
    ),
    str(M0_CONTRACT_PATH): ("scripts/product-acceptance/m0-contract.toml", "config"),
    str(COLLABORATION_STUDY_PATH): (
        "scripts/product-acceptance/collaboration-admission-study-v1.json",
        "config",
    ),
    str(M0_JOURNEY_PATH): ("scripts/product-acceptance/m0-journey-v2.json", "config"),
    str(M0_RESTART_CONTROL_PATH): (
        "scripts/product-acceptance/m0-restart-control-v1.json",
        "config",
    ),
    "/opt/sentinel/scripts/product-acceptance/run_m0_preflight.py": (
        "scripts/product-acceptance/run_m0_preflight.py",
        "script",
    ),
    "/opt/sentinel/scripts/product-acceptance/run_m0_journey.py": (
        "scripts/product-acceptance/run_m0_journey.py",
        "script",
    ),
    "/opt/sentinel/scripts/product-acceptance/build_collaboration_admission_journey.py": (
        "scripts/product-acceptance/build_collaboration_admission_journey.py",
        "script",
    ),
    "/opt/sentinel/scripts/product-acceptance/evaluate_collaboration_admission.py": (
        "scripts/product-acceptance/evaluate_collaboration_admission.py",
        "script",
    ),
    "/opt/sentinel/scripts/product-acceptance/m0-activation/control.py": (
        "scripts/product-acceptance/m0-activation/control.py",
        "script",
    ),
}
CANONICAL_RELEASE_ARTIFACTS.update(
    {
        f"/opt/sentinel/config/agents/{name}": (f"config/agents/{name}", "config")
        for name in CANONICAL_AGENT_FILES
    }
)
LLM_COMPLETION_BACKLOG_SQL = f"""
SELECT COUNT(*) FROM llm_completion_outbox
WHERE status IN ('pending_usage', 'ready_for_action', 'failed', 'action_claimed')
   OR (
        status = 'provider_in_flight'
        AND (
             created_at <= CAST(strftime('%s', 'now') AS INTEGER) * 1000
                           - {LLM_COMPLETION_IN_FLIGHT_GRACE_MS}
             OR created_at > CAST(strftime('%s', 'now') AS INTEGER) * 1000
                           + {LLM_COMPLETION_FUTURE_SKEW_MS}
        )
   )
""".strip()

EVENT_STORE_SQL = f"""
SELECT
  COALESCE((SELECT MAX(id) FROM events), 0) AS latest_event_id,
  (SELECT COUNT(*) FROM outbox
     WHERE status IS NULL OR status != 'published') AS unpublished_outbox,
  (SELECT COUNT(*) FROM outbox o
     LEFT JOIN events e ON e.event_id = o.event_id
     WHERE e.event_id IS NULL) AS orphan_outbox,
  ({LLM_COMPLETION_BACKLOG_SQL}) AS unresolved_llm,
  (SELECT COUNT(*) FROM runtime_config_recovery) AS runtime_recovery,
  (SELECT COUNT(*) FROM runtime_config_apply_recovery) AS config_apply_recovery,
  (SELECT last_event_id FROM projection_offsets
   WHERE projection_name = 'sentinel-projection') AS projection_offset,
  (SELECT last_event_id FROM projection_offsets
   WHERE projection_name = 'sentinel-projection-cost-hierarchy-v2')
    AS hierarchy_offset;
""".strip()
PROJECTION_SNAPSHOT_SQL = """
SELECT
  'watermark' AS row_kind,
  projection_name,
  last_event_id,
  NULL AS agent_id,
  NULL AS name,
  NULL AS role,
  NULL AS shift_set,
  NULL AS status
FROM projection_watermarks
WHERE projection_name IN (
  'sentinel-projection',
  'sentinel-projection-cost-hierarchy-v2'
)
UNION ALL
SELECT
  'agent' AS row_kind,
  NULL AS projection_name,
  NULL AS last_event_id,
  agent_id,
  name,
  role,
  shift_set,
  status
FROM agent_live_view
WHERE status = 'active'
ORDER BY row_kind, projection_name, agent_id;
""".strip()


class PreflightError(RuntimeError):
    """Public-safe validation error carrying a stable reason code."""

    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


class DuplicateJsonKey(ValueError):
    pass


CommandRunner = Callable[[list[str], float, int], bytes]
HttpReader = Callable[[str, str | None, float, int], bytes]
HttpsReader = Callable[[str, float, int, bytes, str], bytes]
FileReader = Callable[[Path, int], bytes]
ArtifactHasher = Callable[[Path, int], tuple[str, int]]
RunningExecutableHasher = Callable[[int, Path, int], tuple[str, int]]


@dataclass(frozen=True)
class Inputs:
    manifest: Path
    contract: Path
    profile: Path
    agents_dir: Path
    operator_credential: Path
    expected_git_sha: str
    expected_manifest_sha256: str
    dashboard_cert: Path = DASHBOARD_CERT_PATH
    event_store: Path = Path("/opt/sentinel/data/events.db")
    projection_store: Path = Path("/opt/sentinel/data/projection.db")
    timeout_seconds: float = 5.0


@dataclass(frozen=True)
class Dependencies:
    command: CommandRunner
    http: HttpReader
    https: HttpsReader
    read_file: FileReader
    hash_file: ArtifactHasher
    hash_running_executable: RunningExecutableHasher
    list_agents: Callable[[Path], list[Path]]
    read_secret: FileReader


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateJsonKey(key)
        result[key] = value
    return result


def strict_json(data: bytes) -> Any:
    try:
        return json.loads(data.decode("utf-8"), object_pairs_hook=reject_duplicate_keys)
    except (UnicodeError, json.JSONDecodeError, DuplicateJsonKey) as exc:
        raise PreflightError("invalid_json") from exc


def canonical_json(value: Any) -> bytes:
    try:
        return json.dumps(
            value, ensure_ascii=True, allow_nan=False, sort_keys=True, separators=(",", ":")
        ).encode("ascii")
    except (TypeError, ValueError) as exc:
        raise PreflightError("noncanonical_value") from exc


def evidence_digest(value: Any) -> str:
    return hashlib.sha256(canonical_json(value)).hexdigest()


def _stat_identity(value: os.stat_result) -> tuple[int, int, int, int, int, int]:
    return (
        value.st_dev,
        value.st_ino,
        value.st_mode,
        value.st_nlink,
        value.st_size,
        value.st_mtime_ns,
    )


def _proc_directory_identity(value: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        value.st_dev,
        value.st_ino,
        stat.S_IFMT(value.st_mode),
        value.st_uid,
        value.st_gid,
    )


def _validate_directory_metadata(value: os.stat_result) -> None:
    mode = stat.S_IMODE(value.st_mode)
    if not stat.S_ISDIR(value.st_mode) or mode & (
        stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX | 0o022
    ):
        raise PreflightError("unsafe_path_component")


def _directory_parts(path: Path) -> tuple[str, ...]:
    if not path.is_absolute() or ".." in path.parts:
        raise PreflightError("unsafe_path_component")
    return tuple(part for part in path.parts if part != "/")


def _open_directory_parts(parts: tuple[str, ...]) -> list[int]:
    flags = os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY | os.O_NOFOLLOW
    descriptors = [os.open("/", flags)]
    try:
        _validate_directory_metadata(os.fstat(descriptors[0]))
        for component in parts:
            descriptor = os.open(component, flags, dir_fd=descriptors[-1])
            _validate_directory_metadata(os.fstat(descriptor))
            descriptors.append(descriptor)
        return descriptors
    except Exception:
        for descriptor in reversed(descriptors):
            os.close(descriptor)
        raise


def _revalidate_directory_parts(
    parts: tuple[str, ...], expected: tuple[tuple[int, int, int, int, int, int], ...]
) -> None:
    descriptors = _open_directory_parts(parts)
    try:
        observed = tuple(_stat_identity(os.fstat(descriptor)) for descriptor in descriptors)
        if observed != expected:
            raise PreflightError("unsafe_path_component")
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


@contextmanager
def _pinned_directory(path: Path) -> Iterator[int]:
    parts = _directory_parts(path)
    descriptors: list[int] = []
    try:
        descriptors = _open_directory_parts(parts)
        expected = tuple(_stat_identity(os.fstat(descriptor)) for descriptor in descriptors)
        yield descriptors[-1]
        _revalidate_directory_parts(parts, expected)
    except PreflightError:
        raise
    except OSError as exc:
        raise PreflightError("unsafe_path_component") from exc
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


@contextmanager
def _pinned_process_directory(pid: int) -> Iterator[int]:
    flags = os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY | os.O_NOFOLLOW
    proc_fd = -1
    process_fd = -1
    verify_proc_fd = -1
    try:
        proc_fd = os.open("/proc", flags)
        proc_before = os.fstat(proc_fd)
        _validate_directory_metadata(proc_before)
        process_fd = os.open(str(pid), flags, dir_fd=proc_fd)
        process_before = os.fstat(process_fd)
        _validate_directory_metadata(process_before)
        yield process_fd
        proc_after = os.fstat(proc_fd)
        process_after = os.fstat(process_fd)
        _validate_directory_metadata(proc_after)
        _validate_directory_metadata(process_after)
        if (
            _proc_directory_identity(proc_before)
            != _proc_directory_identity(proc_after)
            or _proc_directory_identity(process_before)
            != _proc_directory_identity(process_after)
        ):
            raise PreflightError("unsafe_path_component")
        verify_proc_fd = os.open("/proc", flags)
        verify_proc = os.fstat(verify_proc_fd)
        _validate_directory_metadata(verify_proc)
        if _proc_directory_identity(proc_before) != _proc_directory_identity(
            verify_proc
        ):
            raise PreflightError("unsafe_path_component")
        verify_process_fd = os.open(str(pid), flags, dir_fd=verify_proc_fd)
        try:
            verify_process = os.fstat(verify_process_fd)
            _validate_directory_metadata(verify_process)
            if _proc_directory_identity(process_before) != _proc_directory_identity(
                verify_process
            ):
                raise PreflightError("unsafe_path_component")
        finally:
            os.close(verify_process_fd)
    except PreflightError:
        raise
    except OSError as exc:
        raise PreflightError("unsafe_path_component") from exc
    finally:
        if verify_proc_fd >= 0:
            os.close(verify_proc_fd)
        if process_fd >= 0:
            os.close(process_fd)
        if proc_fd >= 0:
            os.close(proc_fd)


def _validate_regular_metadata(value: os.stat_result, *, owner_only: bool) -> None:
    mode = stat.S_IMODE(value.st_mode)
    if not stat.S_ISREG(value.st_mode) or value.st_nlink != 1:
        raise PreflightError("unsafe_file")
    if owner_only:
        if value.st_uid != os.geteuid() or mode not in {0o400, 0o600}:
            raise PreflightError("credential_permissions_invalid")
    elif mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX | 0o022):
        raise PreflightError("unsafe_file_mode")


@contextmanager
def _pinned_regular_file(
    path: Path, limit: int, *, owner_only: bool, oversized_code: str
) -> Iterator[tuple[int, int]]:
    if path.name in {"", ".", ".."}:
        raise PreflightError("unsafe_file")
    with _pinned_directory(path.parent) as parent_fd:
        fd = -1
        try:
            before = os.stat(path.name, dir_fd=parent_fd, follow_symlinks=False)
            _validate_regular_metadata(before, owner_only=owner_only)
            if before.st_size > limit:
                raise PreflightError(oversized_code)
            fd = os.open(
                path.name,
                os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=parent_fd,
            )
            opened = os.fstat(fd)
            after = os.stat(path.name, dir_fd=parent_fd, follow_symlinks=False)
            _validate_regular_metadata(opened, owner_only=owner_only)
            _validate_regular_metadata(after, owner_only=owner_only)
            if len({_stat_identity(before), _stat_identity(opened), _stat_identity(after)}) != 1:
                raise PreflightError("unsafe_file")
            yield fd, opened.st_size
            final = os.fstat(fd)
            path_final = os.stat(path.name, dir_fd=parent_fd, follow_symlinks=False)
            _validate_regular_metadata(final, owner_only=owner_only)
            _validate_regular_metadata(path_final, owner_only=owner_only)
            if len({_stat_identity(opened), _stat_identity(final), _stat_identity(path_final)}) != 1:
                raise PreflightError("unsafe_file")
        except PreflightError:
            raise
        except OSError as exc:
            raise PreflightError("file_unavailable") from exc
        finally:
            if fd >= 0:
                os.close(fd)


def _read_pinned_file(path: Path, limit: int, *, owner_only: bool) -> bytes:
    with _pinned_regular_file(
        path, limit, owner_only=owner_only, oversized_code="oversized_file"
    ) as (fd, _):
        chunks: list[bytes] = []
        total = 0
        while True:
            block = os.read(fd, min(65536, limit + 1 - total))
            if not block:
                break
            total += len(block)
            if total > limit:
                raise PreflightError("oversized_file")
            chunks.append(block)
        return b"".join(chunks)


def default_hash_file(path: Path, limit: int) -> tuple[str, int]:
    with _pinned_regular_file(
        path, limit, owner_only=False, oversized_code="artifact_oversized"
    ) as (fd, expected_size):
        digest = hashlib.sha256()
        total = 0
        while True:
            block = os.read(fd, 1024 * 1024)
            if not block:
                break
            total += len(block)
            if total > limit:
                raise PreflightError("artifact_oversized")
            digest.update(block)
        if total != expected_size:
            raise PreflightError("unsafe_file")
        return digest.hexdigest(), total


def _hash_descriptor(fd: int, expected_size: int, limit: int) -> tuple[str, int]:
    digest = hashlib.sha256()
    total = 0
    os.lseek(fd, 0, os.SEEK_SET)
    while True:
        block = os.read(fd, 1024 * 1024)
        if not block:
            break
        total += len(block)
        if total > limit:
            raise PreflightError("artifact_oversized")
        digest.update(block)
    if total != expected_size:
        raise PreflightError("running_executable_identity_mismatch")
    return digest.hexdigest(), total


def default_hash_running_executable(
    pid: int, installed_path: Path, limit: int
) -> tuple[str, int]:
    if pid <= 0:
        raise PreflightError("systemd_main_pid_invalid")
    try:
        with _pinned_regular_file(
            installed_path,
            limit,
            owner_only=False,
            oversized_code="artifact_oversized",
        ) as (installed_fd, installed_size):
            installed = os.fstat(installed_fd)
            installed_digest, _ = _hash_descriptor(installed_fd, installed_size, limit)
            with _pinned_process_directory(pid) as process_fd:
                executable_fd = os.open(
                    "exe", os.O_RDONLY | os.O_CLOEXEC, dir_fd=process_fd
                )
                try:
                    running = os.fstat(executable_fd)
                    _validate_regular_metadata(running, owner_only=False)
                    if (running.st_dev, running.st_ino) != (
                        installed.st_dev,
                        installed.st_ino,
                    ):
                        raise PreflightError("running_executable_identity_mismatch")
                    running_digest, running_size = _hash_descriptor(
                        executable_fd, running.st_size, limit
                    )
                    verify_fd = os.open(
                        "exe", os.O_RDONLY | os.O_CLOEXEC, dir_fd=process_fd
                    )
                    try:
                        verified = os.fstat(verify_fd)
                        if (verified.st_dev, verified.st_ino) != (
                            running.st_dev,
                            running.st_ino,
                        ):
                            raise PreflightError("running_executable_identity_race")
                    finally:
                        os.close(verify_fd)
                finally:
                    os.close(executable_fd)
            if not hmac.compare_digest(running_digest, installed_digest):
                raise PreflightError("running_executable_hash_mismatch")
            return running_digest, running_size
    except PreflightError as exc:
        if exc.code in {
            "unsafe_file",
            "unsafe_file_mode",
            "unsafe_path_component",
            "file_unavailable",
        }:
            raise PreflightError("running_executable_identity_mismatch") from exc
        raise
    except OSError as exc:
        raise PreflightError("running_executable_unavailable") from exc


def default_read_file(path: Path, limit: int) -> bytes:
    return _read_pinned_file(path, limit, owner_only=False)


def default_read_secret(path: Path, limit: int) -> bytes:
    try:
        return _read_pinned_file(path, limit, owner_only=True)
    except PreflightError as exc:
        if exc.code in {"unsafe_file", "file_unavailable"}:
            raise PreflightError("credential_permissions_invalid") from exc
        raise


def default_list_agents(path: Path) -> list[Path]:
    try:
        with _pinned_directory(path) as directory_fd:
            names = sorted(os.listdir(directory_fd))
    except PreflightError:
        raise
    except OSError as exc:
        raise PreflightError("agents_unavailable") from exc
    if len(names) != MAX_AGENTS or any(not name.endswith(".toml") for name in names):
        raise PreflightError("invalid_agent_count")
    return [path / name for name in names]


def default_command(argv: list[str], timeout: float, limit: int) -> bytes:
    if not argv or argv[0] not in {"/usr/bin/systemctl", "/usr/bin/ss", "/usr/bin/sqlite3"}:
        raise PreflightError("command_not_allowed")
    try:
        process = subprocess.Popen(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            shell=False,
            env={"LC_ALL": "C", "PATH": "/usr/bin:/bin"},
        )
    except OSError as exc:
        raise PreflightError("command_failed") from exc
    selector = selectors.DefaultSelector()
    assert process.stdout is not None and process.stderr is not None
    selector.register(process.stdout, selectors.EVENT_READ, "stdout")
    selector.register(process.stderr, selectors.EVENT_READ, "stderr")
    chunks: dict[str, list[bytes]] = {"stdout": [], "stderr": []}
    sizes = {"stdout": 0, "stderr": 0}
    deadline = time.monotonic() + timeout
    try:
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise PreflightError("command_timeout")
            events = selector.select(remaining)
            if not events:
                raise PreflightError("command_timeout")
            for key, _ in events:
                block = os.read(key.fileobj.fileno(), 65536)
                if not block:
                    selector.unregister(key.fileobj)
                    continue
                stream = key.data
                sizes[stream] += len(block)
                if sizes[stream] > limit:
                    raise PreflightError("command_output_oversized")
                chunks[stream].append(block)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise PreflightError("command_timeout")
        return_code = process.wait(timeout=remaining)
        if return_code != 0:
            raise PreflightError("command_failed")
        return b"".join(chunks["stdout"])
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise PreflightError("command_failed") from exc
    finally:
        selector.close()
        if process.poll() is None:
            process.kill()
            process.wait()


def _read_http_response(
    connection: http.client.HTTPConnection,
    *,
    started: float,
    timeout: float,
    limit: int,
) -> bytes:
    response = connection.getresponse()
    if response.status != 200:
        if response.status == 503:
            raise PreflightError("http_readiness_failed")
        raise PreflightError("http_status")
    if response.headers.get_content_type() != "application/json":
        raise PreflightError("http_content_type")
    declared = response.headers.get("Content-Length")
    if declared is not None:
        try:
            if int(declared) > limit:
                raise PreflightError("http_body_oversized")
        except ValueError as exc:
            raise PreflightError("http_length_invalid") from exc
    chunks: list[bytes] = []
    total = 0
    while True:
        remaining = timeout - (time.monotonic() - started)
        if remaining <= 0:
            raise PreflightError("http_timeout")
        if connection.sock is not None:
            connection.sock.settimeout(remaining)
        block = response.read(min(65536, limit + 1 - total))
        if not block:
            break
        total += len(block)
        if total > limit:
            raise PreflightError("http_body_oversized")
        chunks.append(block)
    return b"".join(chunks)


def _validated_url(url: str, scheme: str) -> parse.SplitResult:
    parsed = parse.urlsplit(url)
    if (
        parsed.scheme != scheme
        or parsed.hostname != "127.0.0.1"
        or parsed.port is None
        or parsed.username is not None
        or parsed.password is not None
        or parsed.fragment
    ):
        raise PreflightError("http_origin_not_loopback")
    return parsed


def default_http(url: str, credential: str | None, timeout: float, limit: int) -> bytes:
    parsed = _validated_url(url, "http")
    headers = {"Accept": "application/json"}
    if credential is not None:
        headers["Authorization"] = f"Bearer {credential}"
    started = time.monotonic()
    connection = http.client.HTTPConnection("127.0.0.1", parsed.port, timeout=timeout)
    try:
        target = parsed.path or "/"
        if parsed.query:
            target += f"?{parsed.query}"
        connection.request("GET", target, headers=headers)
        body = _read_http_response(
            connection, started=started, timeout=timeout, limit=limit
        )
    except PreflightError:
        raise
    except TimeoutError as exc:
        raise PreflightError("http_timeout") from exc
    except (OSError, http.client.HTTPException) as exc:
        raise PreflightError("http_failed") from exc
    finally:
        connection.close()
    return body


def default_https(
    url: str,
    timeout: float,
    limit: int,
    trusted_pem: bytes,
    expected_peer_digest: str,
) -> bytes:
    parsed = _validated_url(url, "https")
    if f"https://{parsed.hostname}:{parsed.port}" != DASHBOARD_ORIGIN:
        raise PreflightError("https_origin_invalid")
    if not DIGEST_RE.fullmatch(expected_peer_digest):
        raise PreflightError("https_pin_invalid")
    try:
        pem_text = trusted_pem.decode("ascii")
        context = ssl.create_default_context(cadata=pem_text)
    except (UnicodeError, ValueError, ssl.SSLError) as exc:
        raise PreflightError("https_certificate_invalid") from exc
    context.check_hostname = True
    context.verify_mode = ssl.CERT_REQUIRED
    connection = http.client.HTTPSConnection(
        parsed.hostname, parsed.port, timeout=timeout, context=context
    )
    started = time.monotonic()
    try:
        connection.connect()
        if connection.sock is None:
            raise PreflightError("https_peer_unavailable")
        peer_der = connection.sock.getpeercert(binary_form=True)
        actual_peer_digest = hashlib.sha256(peer_der).hexdigest()
        if not hmac.compare_digest(actual_peer_digest, expected_peer_digest):
            raise PreflightError("https_peer_pin_mismatch")
        target = parsed.path or "/"
        if parsed.query:
            target += f"?{parsed.query}"
        connection.request("GET", target, headers={"Accept": "application/json"})
        return _read_http_response(
            connection, started=started, timeout=timeout, limit=limit
        )
    except PreflightError:
        raise
    except TimeoutError as exc:
        raise PreflightError("http_timeout") from exc
    except (OSError, ssl.SSLError, http.client.HTTPException) as exc:
        raise PreflightError("https_failed") from exc
    finally:
        connection.close()


DEFAULT_DEPENDENCIES = Dependencies(
    default_command,
    default_http,
    default_https,
    default_read_file,
    default_hash_file,
    default_hash_running_executable,
    default_list_agents,
    default_read_secret,
)


def parse_toml(data: bytes) -> dict[str, Any]:
    try:
        value = tomllib.loads(data.decode("utf-8"))
    except (UnicodeError, tomllib.TOMLDecodeError) as exc:
        raise PreflightError("invalid_toml") from exc
    if not isinstance(value, dict):
        raise PreflightError("invalid_toml")
    return value


def require_dict(value: Any, code: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise PreflightError(code)
    return value


def require_int(value: Any, code: str, *, minimum: int = 0) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise PreflightError(code)
    return value


def require_bool(value: Any, code: str) -> bool:
    if not isinstance(value, bool):
        raise PreflightError(code)
    return value


def require_text(value: Any, code: str) -> str:
    if (
        not isinstance(value, str)
        or not 1 <= len(value.encode("utf-8")) <= 160
        or not value.isascii()
        or any(ord(char) < 0x20 or ord(char) > 0x7E for char in value)
    ):
        raise PreflightError(code)
    return value


def load_operator_credential(inputs: Inputs, deps: Dependencies) -> str:
    data = deps.read_secret(inputs.operator_credential, 4096)
    try:
        secret = data.decode("utf-8").strip()
    except UnicodeError as exc:
        raise PreflightError("credential_invalid") from exc
    if not 32 <= len(secret.encode("utf-8")) <= 4096 or any(
        ord(char) < 0x21 for char in secret
    ):
        raise PreflightError("credential_invalid")
    return secret


def validate_contract_profile_roster(
    inputs: Inputs, deps: Dependencies
) -> tuple[dict[str, Any], dict[int, dict[str, Any]]]:
    contract = parse_toml(deps.read_file(inputs.contract, MAX_FILE_BYTES))
    profile = parse_toml(deps.read_file(inputs.profile, MAX_FILE_BYTES))
    if contract.get("schema_version") != 1 or contract.get("profile") != "web-project-v1":
        raise PreflightError("contract_identity_mismatch")
    if contract.get("profile_path") != "config/work-profiles/web-project-v1.toml":
        raise PreflightError("contract_profile_path_mismatch")
    if profile.get("schema_version") != 1 or profile.get("id") != contract["profile"]:
        raise PreflightError("profile_identity_mismatch")
    if profile.get("runtime_mode") != "single_node" or profile.get("cluster_required") is not False:
        raise PreflightError("profile_runtime_mismatch")
    runtime = require_dict(profile.get("runtime"), "profile_runtime_missing")
    if runtime.get("tool_runtime") != "bwrap" or runtime.get("runtime_registry_required") is not True:
        raise PreflightError("profile_runtime_mismatch")
    if runtime.get("allow_secure_runtime_fallback") is not False:
        raise PreflightError("profile_fallback_enabled")

    agent_paths = deps.list_agents(inputs.agents_dir)
    filenames = [path.name for path in agent_paths]
    if (
        len(filenames) != MAX_AGENTS
        or len(set(filenames)) != MAX_AGENTS
        or set(filenames) != CANONICAL_AGENT_FILES
    ):
        raise PreflightError("agent_file_set_mismatch")
    roster: dict[int, dict[str, Any]] = {}
    names: set[str] = set()
    for path in agent_paths:
        agent = parse_toml(deps.read_file(path, MAX_FILE_BYTES))
        identity = require_dict(agent.get("identity"), "agent_identity_missing")
        agent_id = require_int(identity.get("id"), "agent_id_invalid", minimum=1)
        name = require_text(identity.get("name"), "agent_name_invalid")
        role = require_text(identity.get("role"), "agent_role_invalid")
        shift = require_int(identity.get("shift_set"), "agent_shift_invalid")
        if shift not in {0, 1, 2, 3} or agent_id in roster or name in names:
            raise PreflightError("agent_roster_ambiguous")
        runtime_config = agent.get("runtime", {})
        if runtime_config is not None and not isinstance(runtime_config, dict):
            raise PreflightError("agent_runtime_invalid")
        runtime_key = (runtime_config or {}).get("nano_runtime", "bwrap-landlock")
        if runtime_key != "bwrap-landlock":
            raise PreflightError("agent_runtime_fallback")
        roster[agent_id] = {
            "name": name,
            "role": role,
            "shift": shift,
            "runtime_key": runtime_key,
        }
        names.add(name)
    if set(roster) != set(range(1, MAX_AGENTS + 1)):
        raise PreflightError("agent_id_set_mismatch")
    roster_records = [
        {
            "id": key,
            "name": roster[key]["name"],
            "role": roster[key]["role"],
            "shift": roster[key]["shift"],
        }
        for key in sorted(roster)
    ]
    roster_digest = evidence_digest(roster_records)
    if roster_digest != CANONICAL_ROSTER_DIGEST:
        raise PreflightError("canonical_roster_mismatch")
    evidence = {
        "contract_digest": evidence_digest(contract),
        "profile_digest": evidence_digest(profile),
        "profile_id": profile["id"],
        "roster_count": len(roster),
        "roster_digest": roster_digest,
    }
    return evidence, roster


def validate_manifest(
    inputs: Inputs, deps: Dependencies
) -> tuple[dict[str, Any], dict[str, dict[str, str | int]]]:
    manifest_bytes = deps.read_file(inputs.manifest, MAX_FILE_BYTES)
    if not DIGEST_RE.fullmatch(inputs.expected_manifest_sha256):
        raise PreflightError("manifest_authority_digest_invalid")
    manifest_sha256 = hashlib.sha256(manifest_bytes).hexdigest()
    if not hmac.compare_digest(manifest_sha256, inputs.expected_manifest_sha256):
        raise PreflightError("manifest_authority_digest_mismatch")
    manifest = strict_json(manifest_bytes)
    manifest = require_dict(manifest, "manifest_not_object")
    if set(manifest) != {"version", "created_at", "git_sha", "artifacts"}:
        raise PreflightError("manifest_shape")
    if manifest["version"] != "1.0" or not isinstance(manifest["created_at"], str):
        raise PreflightError("manifest_version")
    if not isinstance(manifest["git_sha"], str) or not SHA_RE.fullmatch(manifest["git_sha"]):
        raise PreflightError("manifest_git_sha")
    if manifest["git_sha"] != inputs.expected_git_sha:
        raise PreflightError("manifest_git_sha_mismatch")
    artifacts = manifest["artifacts"]
    if not isinstance(artifacts, list) or not artifacts or len(artifacts) > 512:
        raise PreflightError("manifest_artifacts")
    seen_paths: set[str] = set()
    seen_sources: set[str] = set()
    verified: list[dict[str, str | int]] = []
    artifact_authority: dict[str, dict[str, str | int]] = {}
    for item in artifacts:
        item = require_dict(item, "manifest_artifact_shape")
        if set(item) != {"path", "source", "sha256", "type"}:
            raise PreflightError("manifest_artifact_shape")
        path_text = item.get("path")
        source = item.get("source")
        expected = item.get("sha256")
        kind = item.get("type")
        if (
            not isinstance(path_text, str)
            or not path_text.startswith("/")
            or ".." in Path(path_text).parts
            or not isinstance(source, str)
            or source.startswith("/")
            or ".." in Path(source).parts
            or not isinstance(expected, str)
            or not DIGEST_RE.fullmatch(expected)
            or kind not in {"binary", "config", "systemd", "script"}
        ):
            raise PreflightError("manifest_artifact_invalid")
        if path_text in seen_paths or source in seen_sources:
            raise PreflightError("manifest_artifact_duplicate")
        authority = CANONICAL_RELEASE_ARTIFACTS.get(path_text)
        if authority is None:
            raise PreflightError("manifest_unexpected_artifact")
        if (source, kind) != authority:
            raise PreflightError("manifest_artifact_authority_mismatch")
        actual, size = deps.hash_file(Path(path_text), MAX_ARTIFACT_BYTES)
        if not hmac.compare_digest(actual, expected):
            raise PreflightError("artifact_hash_mismatch")
        seen_paths.add(path_text)
        seen_sources.add(source)
        verified.append(
            {
                "path_digest": hashlib.sha256(path_text.encode("ascii")).hexdigest(),
                "sha256": actual,
                "size": size,
            }
        )
        artifact_authority[path_text] = {
            "sha256": actual,
            "size": size,
            "source": source,
            "type": kind,
        }
    if seen_paths != set(CANONICAL_RELEASE_ARTIFACTS):
        raise PreflightError("manifest_required_artifact_missing")
    return (
        {
            "artifact_count": len(verified),
            "artifact_set_digest": evidence_digest(
                sorted(verified, key=lambda row: row["path_digest"])
            ),
            "git_sha": manifest["git_sha"],
            "manifest_sha256": manifest_sha256,
        },
        artifact_authority,
    )


def parse_key_values(data: bytes) -> dict[str, str]:
    try:
        lines = data.decode("utf-8").splitlines()
    except UnicodeError as exc:
        raise PreflightError("command_output_invalid") from exc
    result: dict[str, str] = {}
    for line in lines:
        if not line or "=" not in line:
            raise PreflightError("command_output_invalid")
        key, value = line.split("=", 1)
        if key in result:
            raise PreflightError("command_output_duplicate")
        result[key] = value
    return result


def systemctl_show(
    unit: str, properties: tuple[str, ...], deps: Dependencies, timeout: float
) -> dict[str, str]:
    return parse_key_values(
        deps.command(
            [
                "/usr/bin/systemctl",
                "show",
                unit,
                "--no-pager",
                f"--property={','.join(properties)}",
            ],
            timeout,
            MAX_COMMAND_BYTES,
        )
    )


def systemd_uint(value: Any, code: str, *, positive: bool = False) -> int:
    if not isinstance(value, str) or not value.isdecimal():
        raise PreflightError(code)
    parsed = int(value)
    if positive and parsed <= 0:
        raise PreflightError(code)
    return parsed


def validate_systemd(
    deps: Dependencies,
    timeout: float,
    artifact_authority: dict[str, dict[str, str | int]],
) -> tuple[dict[str, Any], dict[str, int]]:
    if set(SERVICE_EXECUTABLES) != REQUIRED_SERVICES or set(TIMER_SERVICES) != REQUIRED_TIMERS:
        raise PreflightError("systemd_authority_map_invalid")
    target_properties = (
        "Id",
        "LoadState",
        "ActiveState",
        "SubState",
        "FragmentPath",
        "Wants",
        "Requires",
        "NeedDaemonReload",
    )
    target = systemctl_show(TARGET_UNIT, target_properties, deps, timeout)
    if set(target) != set(target_properties):
        raise PreflightError("systemd_target_shape")
    wants = target.get("Wants", "").split()
    requires = target.get("Requires", "").split()
    if len(wants) != len(set(wants)) or set(wants) != set(TARGET_WANTS):
        raise PreflightError("systemd_required_set_mismatch")
    if len(requires) != len(set(requires)) or set(requires) != {AUTH_INIT_UNIT}:
        raise PreflightError("systemd_required_set_mismatch")
    expected_target = {
        "Id": TARGET_UNIT,
        "LoadState": "loaded",
        "ActiveState": "active",
        "SubState": "active",
        "FragmentPath": "/etc/systemd/system/sentinel.target",
        "NeedDaemonReload": "no",
    }
    for key, value in expected_target.items():
        if target.get(key) != value:
            raise PreflightError("systemd_target_not_ready")
    unit_facts: list[dict[str, str]] = []
    main_pids: dict[str, int] = {}
    auth_properties = (
        "Id",
        "LoadState",
        "ActiveState",
        "SubState",
        "Result",
        "FragmentPath",
        "NeedDaemonReload",
        "ExecMainCode",
        "ExecMainStatus",
    )
    auth_facts = systemctl_show(AUTH_INIT_UNIT, auth_properties, deps, timeout)
    if set(auth_facts) != set(auth_properties):
        raise PreflightError("systemd_auth_init_shape")
    expected_auth = {
        "Id": AUTH_INIT_UNIT,
        "LoadState": "loaded",
        "ActiveState": "active",
        "SubState": "exited",
        "Result": "success",
        "FragmentPath": f"/etc/systemd/system/{AUTH_INIT_UNIT}",
        "NeedDaemonReload": "no",
        "ExecMainCode": "1",
        "ExecMainStatus": "0",
    }
    if any(auth_facts.get(key) != value for key, value in expected_auth.items()):
        raise PreflightError("systemd_auth_init_not_ready")
    unit_facts.append(
        {
            "unit": AUTH_INIT_UNIT,
            "fragment_digest": hashlib.sha256(
                auth_facts["FragmentPath"].encode("ascii")
            ).hexdigest(),
            "executable_digest": artifact_authority[
                "/opt/sentinel/scripts/init-dashboard-auth.sh"
            ]["sha256"],
        }
    )
    for unit in sorted(REQUIRED_SERVICES):
        properties = (
            "Id",
            "LoadState",
            "ActiveState",
            "SubState",
            "Result",
            "NRestarts",
            "FragmentPath",
            "NeedDaemonReload",
            "MainPID",
        )
        facts = systemctl_show(unit, properties, deps, timeout)
        if set(facts) != set(properties):
            raise PreflightError("systemd_unit_shape")
        expected = {
            "Id": unit,
            "LoadState": "loaded",
            "ActiveState": "active",
            "SubState": "running",
            "Result": "success",
            "NRestarts": "0",
            "FragmentPath": f"/etc/systemd/system/{unit}",
            "NeedDaemonReload": "no",
        }
        for key, value in expected.items():
            if facts.get(key) != value:
                raise PreflightError("systemd_unit_not_ready")
        main_pid = systemd_uint(facts.get("MainPID"), "systemd_main_pid_invalid", positive=True)
        executable = SERVICE_EXECUTABLES[unit]
        manifest_record = artifact_authority.get(str(executable))
        if manifest_record is None:
            raise PreflightError("service_executable_manifest_missing")
        expected_digest = manifest_record.get("sha256")
        expected_size = manifest_record.get("size")
        if not isinstance(expected_digest, str) or not isinstance(expected_size, int):
            raise PreflightError("service_executable_manifest_invalid")
        running_digest, running_size = deps.hash_running_executable(
            main_pid, executable, MAX_ARTIFACT_BYTES
        )
        if (
            not hmac.compare_digest(running_digest, expected_digest)
            or running_size != expected_size
        ):
            raise PreflightError("running_executable_hash_mismatch")
        identity_properties = (
            "Id",
            "MainPID",
            "ActiveState",
            "SubState",
            "NeedDaemonReload",
        )
        identity = systemctl_show(unit, identity_properties, deps, timeout)
        if set(identity) != set(identity_properties):
            raise PreflightError("systemd_service_identity_shape")
        if identity != {key: facts[key] for key in identity_properties}:
            raise PreflightError("systemd_service_identity_changed")
        main_pids[unit] = main_pid
        unit_facts.append(
            {
                "unit": unit,
                "fragment_digest": hashlib.sha256(
                    facts["FragmentPath"].encode("ascii")
                ).hexdigest(),
                "executable_digest": running_digest,
            }
        )

    timer_outcomes: list[dict[str, str]] = []
    for timer in sorted(REQUIRED_TIMERS):
        service = TIMER_SERVICES[timer]
        timer_properties = (
            "Id",
            "LoadState",
            "ActiveState",
            "SubState",
            "Result",
            "FragmentPath",
            "NeedDaemonReload",
            "Unit",
            "ActiveEnterTimestampMonotonic",
        )
        timer_facts = systemctl_show(timer, timer_properties, deps, timeout)
        if set(timer_facts) != set(timer_properties):
            raise PreflightError("systemd_timer_shape")
        expected_timer = {
            "Id": timer,
            "LoadState": "loaded",
            "ActiveState": "active",
            "SubState": "waiting",
            "Result": "success",
            "FragmentPath": f"/etc/systemd/system/{timer}",
            "NeedDaemonReload": "no",
            "Unit": service,
        }
        if any(timer_facts.get(key) != value for key, value in expected_timer.items()):
            raise PreflightError("systemd_timer_not_ready")
        timer_entered = systemd_uint(
            timer_facts.get("ActiveEnterTimestampMonotonic"),
            "systemd_timer_activation_missing",
            positive=True,
        )
        outcome_properties = (
            "Id",
            "LoadState",
            "ActiveState",
            "SubState",
            "Result",
            "FragmentPath",
            "NeedDaemonReload",
            "ExecMainCode",
            "ExecMainStatus",
            "ExecMainStartTimestampMonotonic",
            "ExecMainExitTimestampMonotonic",
        )
        outcome = systemctl_show(service, outcome_properties, deps, timeout)
        if set(outcome) != set(outcome_properties):
            raise PreflightError("systemd_timer_outcome_shape")
        expected_outcome = {
            "Id": service,
            "LoadState": "loaded",
            "ActiveState": "inactive",
            "SubState": "dead",
            "Result": "success",
            "FragmentPath": f"/etc/systemd/system/{service}",
            "NeedDaemonReload": "no",
            "ExecMainCode": "1",
            "ExecMainStatus": "0",
        }
        if any(outcome.get(key) != value for key, value in expected_outcome.items()):
            raise PreflightError("systemd_timer_outcome_failed")
        started = systemd_uint(
            outcome.get("ExecMainStartTimestampMonotonic"),
            "systemd_timer_outcome_missing",
            positive=True,
        )
        exited = systemd_uint(
            outcome.get("ExecMainExitTimestampMonotonic"),
            "systemd_timer_outcome_missing",
            positive=True,
        )
        if timer in ACTIVATION_ONESHOT_TIMERS and not timer_entered <= started <= exited:
            raise PreflightError("systemd_timer_outcome_stale")
        timer_outcomes.append(
            {
                "timer": timer,
                "service": service,
                "outcome_digest": evidence_digest(
                    {
                        "timer_entered": timer_entered,
                        "started": started,
                        "exited": exited,
                        "result": outcome["Result"],
                    }
                ),
            }
        )
    return (
        {
            "required_unit_count": len(REQUIRED_UNITS),
            "service_executable_count": len(main_pids),
            "timer_outcome_count": len(timer_outcomes),
            "unit_set_digest": evidence_digest(unit_facts),
            "timer_outcome_digest": evidence_digest(timer_outcomes),
        },
        main_pids,
    )


def split_listener(value: str, family: str) -> tuple[str, int]:
    if value.startswith("["):
        end = value.rfind("]:")
        if end < 0:
            raise PreflightError("listener_invalid")
        host, port = value[1:end], value[end + 2 :]
    else:
        if ":" not in value:
            raise PreflightError("listener_invalid")
        host, port = value.rsplit(":", 1)
    try:
        parsed_port = int(port)
        if not 0 < parsed_port <= 65535:
            raise ValueError
        if parsed_port not in PROTECTED_LISTENER_PORTS:
            return host, parsed_port
        if host == "*":
            host = "0.0.0.0" if family == "ipv4" else "::"
        address = ipaddress.ip_address(host)
        if (family == "ipv4") != isinstance(address, ipaddress.IPv4Address):
            raise ValueError
    except ValueError as exc:
        raise PreflightError("listener_invalid") from exc
    return address.compressed, parsed_port


def validate_listeners(
    deps: Dependencies, timeout: float, main_pids: dict[str, int]
) -> dict[str, Any]:
    if set(LISTENER_SERVICES) != EXPECTED_LISTENERS:
        raise PreflightError("listener_authority_map_invalid")
    observed: Counter[tuple[str, str, str, int]] = Counter()
    ownership: list[dict[str, str | int]] = []
    for family, option in (("ipv4", "-4"), ("ipv6", "-6")):
        raw = deps.command(
            ["/usr/bin/ss", "-H", "-lntup", option], timeout, MAX_COMMAND_BYTES
        )
        try:
            lines = raw.decode("utf-8").splitlines()
        except UnicodeError as exc:
            raise PreflightError("listener_invalid") from exc
        for line in lines:
            fields = line.split()
            if len(fields) < 5 or fields[0] not in {"tcp", "udp"}:
                raise PreflightError("listener_invalid")
            host, port = split_listener(fields[4], family)
            if port in PROTECTED_LISTENER_PORTS:
                listener = (fields[0], family, host, port)
                service = LISTENER_SERVICES.get(listener)
                if service is None:
                    raise PreflightError("listener_contract_mismatch")
                process_ids = {
                    int(value) for value in re.findall(r"\bpid=(\d+)\b", line)
                }
                if len(process_ids) != 1:
                    raise PreflightError("listener_process_ambiguous")
                expected_pid = main_pids.get(service)
                if expected_pid is None or process_ids != {expected_pid}:
                    raise PreflightError("listener_process_mismatch")
                observed[listener] += 1
                ownership.append(
                    {
                        "protocol": fields[0],
                        "family": family,
                        "port": port,
                        "service": service,
                    }
                )
    if observed != Counter(EXPECTED_LISTENERS):
        raise PreflightError("listener_contract_mismatch")
    return {
        "required_listener_count": len(EXPECTED_LISTENERS),
        "listener_set_digest": evidence_digest(sorted(EXPECTED_LISTENERS)),
        "listener_owner_digest": evidence_digest(
            sorted(
                ownership,
                key=lambda item: (
                    item["protocol"], item["family"], item["port"], item["service"]
                ),
            )
        ),
    }


def validate_http(
    inputs: Inputs, deps: Dependencies, credential: str, timeout: float
) -> tuple[dict[str, Any], dict[str, dict[str, Any]]]:
    payloads: dict[str, dict[str, Any]] = {}
    evidence: list[dict[str, str]] = []
    for name, url, role, field, expected in HTTP_CONTRACTS:
        payload = strict_json(
            deps.http(
                url,
                credential if role == "operator" else None,
                timeout,
                MAX_HTTP_BYTES,
            )
        )
        payload = require_dict(payload, "http_payload_not_object")
        if field is not None and payload.get(field) != expected:
            raise PreflightError("http_readiness_failed")
        payloads[name] = payload
        evidence.append({"endpoint": name, "payload_digest": evidence_digest(payload)})
    certificate_pem = deps.read_file(inputs.dashboard_cert, MAX_FILE_BYTES)
    try:
        certificate_der = ssl.PEM_cert_to_DER_cert(certificate_pem.decode("ascii"))
    except (UnicodeError, ValueError, ssl.SSLError) as exc:
        raise PreflightError("https_certificate_invalid") from exc
    peer_digest = hashlib.sha256(certificate_der).hexdigest()
    certificate_hash = base64.b64encode(hashlib.sha256(certificate_der).digest()).decode(
        "ascii"
    )
    dashboard_health = require_dict(
        strict_json(
            deps.https(
                f"{DASHBOARD_ORIGIN}/api/health",
                timeout,
                MAX_HTTP_BYTES,
                certificate_pem,
                peer_digest,
            )
        ),
        "http_payload_not_object",
    )
    if dashboard_health.get("status") != "ok":
        raise PreflightError("http_readiness_failed")
    dashboard_hash = require_dict(
        strict_json(
            deps.https(
                f"{DASHBOARD_ORIGIN}/api/cert-hash",
                timeout,
                MAX_HTTP_BYTES,
                certificate_pem,
                peer_digest,
            )
        ),
        "http_payload_not_object",
    )
    if dashboard_hash != {"algorithm": "sha-256", "hash": certificate_hash}:
        raise PreflightError("https_certificate_hash_mismatch")
    evidence.extend(
        (
            {
                "endpoint": "dashboard_health",
                "payload_digest": evidence_digest(dashboard_health),
            },
            {
                "endpoint": "dashboard_cert_hash",
                "payload_digest": evidence_digest(dashboard_hash),
            },
        )
    )
    return {"endpoint_count": len(evidence), "endpoint_digest": evidence_digest(evidence)}, payloads


def scheduled_roster(roster: dict[int, dict[str, Any]], shift: int) -> dict[int, dict[str, Any]]:
    if shift not in {1, 2, 3}:
        raise PreflightError("runtime_shift_invalid")
    return {agent_id: item for agent_id, item in roster.items() if item["shift"] in {0, shift}}


def unique_by_id(items: Any, code: str) -> dict[int, dict[str, Any]]:
    if not isinstance(items, list) or len(items) > MAX_AGENTS:
        raise PreflightError(code)
    result: dict[int, dict[str, Any]] = {}
    for raw in items:
        item = require_dict(raw, code)
        agent_id = require_int(item.get("agent_id"), code, minimum=1)
        if agent_id in result:
            raise PreflightError(code)
        result[agent_id] = item
    return result


def validate_identity(roster: dict[int, dict[str, Any]], payloads: dict[str, dict[str, Any]]) -> dict[str, Any]:
    runtime = payloads["runtime_health"]
    shift = require_int(runtime.get("current_shift"), "runtime_shape")
    scheduled = scheduled_roster(roster, shift)
    runtime_agents = unique_by_id(runtime.get("agents"), "runtime_agents_ambiguous")
    if set(runtime_agents) != set(scheduled):
        raise PreflightError("runtime_roster_mismatch")
    required_zero = (
        "projection_drift_agents",
        "stale_runtime_entries",
        "orphan_cgroups",
        "zombie_tracked_pids",
        "respawn_failures",
    )
    for field in required_zero:
        if require_int(runtime.get(field), "runtime_shape") != 0:
            raise PreflightError("runtime_drift")
    if require_bool(runtime.get("projection_drift_detected"), "runtime_shape"):
        raise PreflightError("runtime_drift")
    if runtime.get("operator_auth_required") is not True:
        raise PreflightError("operator_auth_disabled")
    if runtime.get("last_repair_error") is not None or runtime.get("repair_last_status") != "healthy":
        raise PreflightError("runtime_repair_unresolved")
    if require_int(runtime.get("analysis_queue_depth"), "runtime_shape") != 0:
        raise PreflightError("runtime_queue_backlog")
    worker_states = require_dict(runtime.get("worker_states"), "runtime_shape")
    if set(worker_states) != {"ecs_tick_loop", "episode_projection", "service_health"}:
        raise PreflightError("runtime_worker_mismatch")
    for worker in worker_states.values():
        worker = require_dict(worker, "runtime_shape")
        if (
            worker.get("running") is not True
            or require_int(worker.get("restart_count"), "runtime_shape") != 0
            or worker.get("last_error") is not None
        ):
            raise PreflightError("runtime_worker_not_ready")
    expected_count = len(scheduled)
    for field in (
        "expected_active_agents",
        "runtime_agents",
        "projection_agents",
        "security_runtime_entries",
        "sandbox_handles",
        "tracked_processes",
        "live_cgroup_dirs",
    ):
        if require_int(runtime.get(field), "runtime_shape") != expected_count:
            raise PreflightError("runtime_count_mismatch")
    for agent_id, item in runtime_agents.items():
        expected = scheduled[agent_id]
        if item.get("aggregate_id") != f"AGENT-{agent_id:02}" or item.get("name") != expected["name"]:
            raise PreflightError("runtime_identity_mismatch")
        if item.get("runtime_key") != "bwrap-landlock":
            raise PreflightError("runtime_fallback_detected")
        for field in (
            "runtime_present",
            "projection_present",
            "tracked_pid_alive",
            "security_runtime_present",
            "adapter_handle_present",
            "adapter_instance_matches",
            "runtime_resources_healthy",
        ):
            if item.get(field) is not True:
                raise PreflightError("runtime_agent_not_ready")
        if item.get("adapter_health_state") != "healthy" or item.get("logical_status") not in {"Active", "Sleeping"}:
            raise PreflightError("runtime_agent_not_ready")
        if item.get("adapter_observation_error") is not None or item.get("last_repair_status") != "healthy":
            raise PreflightError("runtime_agent_not_ready")
        require_int(item.get("tracked_pid"), "runtime_agent_not_ready", minimum=1)
        if require_int(item.get("cgroup_live_pid_count"), "runtime_agent_not_ready", minimum=1) < 1:
            raise PreflightError("runtime_agent_not_ready")
        if item.get("tracked_pid_state") in {None, "X", "Z"}:
            raise PreflightError("runtime_agent_not_ready")

    platform = payloads["platform_state"]
    unresolved = require_dict(platform.get("unresolved_counts"), "platform_shape")
    if any(require_int(value, "platform_shape") != 0 for value in unresolved.values()):
        raise PreflightError("platform_unresolved")
    platform_agents = unique_by_id(platform.get("agents"), "platform_agents_ambiguous")
    if set(platform_agents) != set(scheduled):
        raise PreflightError("platform_roster_mismatch")
    resource_profiles = require_dict(platform.get("resource_profiles"), "platform_shape")
    if set(resource_profiles) != {f"AGENT-{agent_id:02}" for agent_id in scheduled}:
        raise PreflightError("platform_profile_mismatch")
    for agent_id, item in platform_agents.items():
        expected = scheduled[agent_id]
        current_profile = item.get("current_profile")
        if (
            item.get("aggregate_id") != f"AGENT-{agent_id:02}"
            or item.get("name") != expected["name"]
            or current_profile not in {"idle", "normal", "heavy", "suspended"}
            or resource_profiles.get(f"AGENT-{agent_id:02}") != current_profile
        ):
            raise PreflightError("platform_identity_mismatch")

    projection = payloads["episode_projection"]
    if projection.get("initialized") is not True or projection.get("integrity_error") is not False:
        raise PreflightError("episode_projection_not_ready")
    if projection.get("global_blockers") != []:
        raise PreflightError("episode_projection_blocked")
    global_frontier = require_int(
        projection.get("global_frontier_source_row_id"),
        "episode_projection_frontier_missing",
        minimum=0,
    )
    projection_agents = unique_by_id(projection.get("agents"), "episode_agents_ambiguous")
    if set(projection_agents) != set(roster):
        raise PreflightError("episode_roster_mismatch")
    for item in projection_agents.values():
        if item.get("ready") is not True or item.get("blockers") != []:
            raise PreflightError("episode_projection_blocked")
        frontier = require_int(
            item.get("frontier_source_row_id"),
            "episode_projection_frontier_missing",
            minimum=0,
        )
        lag_rows = require_int(
            item.get("lag_rows"),
            "episode_projection_frontier_mismatch",
            minimum=0,
        )
        if frontier > global_frontier or lag_rows != global_frontier - frontier:
            raise PreflightError("episode_projection_frontier_mismatch")
    return {
        "configured_agents": len(roster),
        "scheduled_agents": len(scheduled),
        "current_shift": shift,
        "configured_identity_digest": evidence_digest(
            [
                {
                    "id": agent_id,
                    "name": roster[agent_id]["name"],
                    "role": roster[agent_id]["role"],
                    "shift": roster[agent_id]["shift"],
                }
                for agent_id in sorted(roster)
            ]
        ),
        "scheduled_identity_digest": evidence_digest(
            [
                {
                    "id": agent_id,
                    "name": scheduled[agent_id]["name"],
                    "role": scheduled[agent_id]["role"],
                    "shift": scheduled[agent_id]["shift"],
                }
                for agent_id in sorted(scheduled)
            ]
        ),
    }


def parse_sqlite_json(
    data: bytes,
    expected_keys: set[str],
    *,
    temporal_null_keys: frozenset[str] = frozenset(),
) -> dict[str, int]:
    value = strict_json(data)
    if not isinstance(value, list) or len(value) != 1 or not isinstance(value[0], dict):
        raise PreflightError("store_readback_shape")
    row = value[0]
    if set(row) != expected_keys:
        raise PreflightError("store_readback_shape")
    result: dict[str, int] = {}
    for key in expected_keys:
        if row[key] is None and key in temporal_null_keys:
            raise PreflightError("read_model_projection_lag")
        result[key] = require_int(row[key], "store_readback_value")
    return result


def read_event_cut(inputs: Inputs, deps: Dependencies) -> dict[str, int]:
    return parse_sqlite_json(
        deps.command(
            ["/usr/bin/sqlite3", "-readonly", "-json", str(inputs.event_store), EVENT_STORE_SQL],
            inputs.timeout_seconds,
            MAX_COMMAND_BYTES,
        ),
        {
            "latest_event_id",
            "unpublished_outbox",
            "orphan_outbox",
            "unresolved_llm",
            "runtime_recovery",
            "config_apply_recovery",
            "projection_offset",
            "hierarchy_offset",
        },
        temporal_null_keys=frozenset({"projection_offset", "hierarchy_offset"}),
    )


def parse_projection_snapshot(data: bytes) -> tuple[dict[str, int], list[dict[str, Any]]]:
    value = strict_json(data)
    if not isinstance(value, list) or len(value) > MAX_AGENTS + 2:
        raise PreflightError("store_readback_shape")
    watermarks: dict[str, int] = {}
    identities: list[dict[str, Any]] = []
    expected_keys = {
        "row_kind",
        "projection_name",
        "last_event_id",
        "agent_id",
        "name",
        "role",
        "shift_set",
        "status",
    }
    for raw in value:
        row = require_dict(raw, "store_readback_shape")
        if set(row) != expected_keys:
            raise PreflightError("store_readback_shape")
        if row.get("row_kind") == "watermark":
            if any(row[key] is not None for key in ("agent_id", "name", "role", "shift_set", "status")):
                raise PreflightError("store_readback_shape")
            name = row.get("projection_name")
            if name not in {
                "sentinel-projection",
                "sentinel-projection-cost-hierarchy-v2",
            } or name in watermarks:
                raise PreflightError("store_readback_shape")
            last_event_id = row.get("last_event_id")
            if last_event_id is None:
                raise PreflightError("read_model_projection_lag")
            watermarks[name] = require_int(
                last_event_id, "store_readback_value", minimum=0
            )
        elif row.get("row_kind") == "agent":
            if row.get("projection_name") is not None or row.get("last_event_id") is not None:
                raise PreflightError("store_readback_shape")
            identities.append(
                {
                    key: row[key]
                    for key in ("agent_id", "name", "role", "shift_set", "status")
                }
            )
        else:
            raise PreflightError("store_readback_shape")
    if set(watermarks) != {
        "sentinel-projection",
        "sentinel-projection-cost-hierarchy-v2",
    }:
        raise PreflightError("read_model_projection_lag")
    return watermarks, identities


def capture_store_snapshot(
    inputs: Inputs, deps: Dependencies
) -> tuple[dict[str, int], dict[str, int], Any]:
    event_before = read_event_cut(inputs, deps)
    projection, projection_identity = parse_projection_snapshot(
        deps.command(
            [
                "/usr/bin/sqlite3",
                "-readonly",
                "-json",
                str(inputs.projection_store),
                PROJECTION_SNAPSHOT_SQL,
            ],
            inputs.timeout_seconds,
            MAX_COMMAND_BYTES,
        )
    )
    event_after = read_event_cut(inputs, deps)
    if event_before != event_after:
        raise PreflightError("event_cut_changed")
    return event_before, projection, projection_identity


def validate_stores(
    event: dict[str, int], projection: dict[str, int]
) -> dict[str, Any]:
    for key in ("unpublished_outbox", "orphan_outbox", "unresolved_llm", "runtime_recovery", "config_apply_recovery"):
        if event[key] != 0:
            raise PreflightError("publication_or_recovery_backlog")
    latest = event["latest_event_id"]
    if event["projection_offset"] != latest or event["hierarchy_offset"] != latest:
        raise PreflightError("event_projection_lag")
    if any(watermark != latest for watermark in projection.values()):
        raise PreflightError("read_model_projection_lag")
    return {
        "latest_event_id": latest,
        "projection_offset": event["projection_offset"],
        "hierarchy_offset": event["hierarchy_offset"],
        "projection_watermarks": projection,
        "backlog_count": 0,
        "stable_cut_digest": evidence_digest(event),
    }


def validate_projection_identity_store(
    raw: Any,
    roster: dict[int, dict[str, Any]],
    shift: int,
) -> dict[str, Any]:
    if not isinstance(raw, list) or len(raw) > MAX_AGENTS:
        raise PreflightError("projection_identity_shape")
    observed: dict[int, dict[str, Any]] = {}
    for item in raw:
        item = require_dict(item, "projection_identity_shape")
        if set(item) != {"agent_id", "name", "role", "shift_set", "status"}:
            raise PreflightError("projection_identity_shape")
        agent_id = require_int(item.get("agent_id"), "projection_identity_shape", minimum=1)
        if agent_id in observed:
            raise PreflightError("projection_identity_duplicate")
        observed[agent_id] = item
    expected = scheduled_roster(roster, shift)
    if set(observed) != set(expected):
        raise PreflightError("projection_store_roster_mismatch")
    for agent_id, item in observed.items():
        expected_item = expected[agent_id]
        if (
            item.get("name") != expected_item["name"]
            or item.get("role") != expected_item["role"]
            or item.get("shift_set") != expected_item["shift"]
            or item.get("status") != "active"
        ):
            raise PreflightError("projection_store_identity_mismatch")
    normalized = [
        {
            "agent_id": agent_id,
            "name": observed[agent_id]["name"],
            "role": observed[agent_id]["role"],
            "shift_set": observed[agent_id]["shift_set"],
        }
        for agent_id in sorted(observed)
    ]
    return {
        "active_agent_count": len(normalized),
        "active_identity_digest": evidence_digest(normalized),
    }


def check_result(check_id: str, function: Callable[[], dict[str, Any]]) -> tuple[dict[str, Any], Any]:
    try:
        evidence = function()
        return {
            "id": check_id,
            "status": "PASS",
            "reason": "ok",
            "evidence_digest": evidence_digest(evidence),
            "evidence": evidence,
        }, evidence
    except PreflightError as exc:
        marker = {"id": check_id, "reason": exc.code}
        return {
            "id": check_id,
            "status": "FAIL",
            "reason": exc.code,
            "evidence_digest": evidence_digest(marker),
            "evidence": {},
        }, None
    except Exception:
        marker = {"id": check_id, "reason": "internal_validation_error"}
        return {
            "id": check_id,
            "status": "FAIL",
            "reason": "internal_validation_error",
            "evidence_digest": evidence_digest(marker),
            "evidence": {},
        }, None


def evaluate(inputs: Inputs, deps: Dependencies = DEFAULT_DEPENDENCIES) -> dict[str, Any]:
    if not 0 < inputs.timeout_seconds <= MAX_TIMEOUT_SECONDS:
        raise PreflightError("timeout_invalid")
    if inputs.event_store != Path("/opt/sentinel/data/events.db") or inputs.projection_store != Path(
        "/opt/sentinel/data/projection.db"
    ):
        raise PreflightError("store_path_invalid")
    if inputs.contract != M0_CONTRACT_PATH:
        raise PreflightError("contract_path_invalid")
    if inputs.profile != M0_PROFILE_PATH:
        raise PreflightError("profile_path_invalid")
    if inputs.agents_dir != Path("/opt/sentinel/config/agents"):
        raise PreflightError("agents_path_invalid")
    if inputs.dashboard_cert != DASHBOARD_CERT_PATH:
        raise PreflightError("dashboard_certificate_path_invalid")
    if not SHA_RE.fullmatch(inputs.expected_git_sha):
        raise PreflightError("expected_git_sha_invalid")
    if not DIGEST_RE.fullmatch(inputs.expected_manifest_sha256):
        raise PreflightError("manifest_authority_digest_invalid")
    checks: list[dict[str, Any]] = []

    roster_cache: dict[str, dict[int, dict[str, Any]]] = {}

    def contract_check() -> dict[str, Any]:
        evidence, loaded_roster = validate_contract_profile_roster(inputs, deps)
        roster_cache["value"] = loaded_roster
        return evidence

    contract_result, contract_data = check_result(
        "contract_profile_roster", contract_check
    )
    checks.append(contract_result)
    roster = roster_cache.get("value") if contract_data is not None else None

    manifest_cache: dict[str, Any] = {}

    def manifest_check() -> dict[str, Any]:
        evidence, authority = validate_manifest(inputs, deps)
        manifest_cache["authority"] = authority
        return evidence

    manifest_result, manifest_evidence = check_result(
        "release_manifest_identity", manifest_check
    )
    checks.append(manifest_result)
    systemd_cache: dict[str, Any] = {}

    def systemd_check() -> dict[str, Any]:
        evidence, main_pids = validate_systemd(
            deps, inputs.timeout_seconds, manifest_cache["authority"]
        )
        systemd_cache["main_pids"] = main_pids
        return evidence

    if manifest_evidence is None:
        systemd_result, systemd_evidence = check_result(
            "systemd_units",
            lambda: (_ for _ in ()).throw(
                PreflightError("manifest_dependency_failed")
            ),
        )
    else:
        systemd_result, systemd_evidence = check_result(
            "systemd_units", systemd_check
        )
    checks.append(systemd_result)
    if systemd_evidence is None:
        listener_result, _ = check_result(
            "required_listeners",
            lambda: (_ for _ in ()).throw(
                PreflightError("systemd_dependency_failed")
            ),
        )
    else:
        listener_result, _ = check_result(
            "required_listeners",
            lambda: validate_listeners(
                deps, inputs.timeout_seconds, systemd_cache["main_pids"]
            ),
        )
    checks.append(listener_result)

    secret_cache: dict[str, str] = {}

    def credential_check() -> dict[str, Any]:
        loaded_secret = load_operator_credential(inputs, deps)
        secret_cache["value"] = loaded_secret
        return {"credential_present": True}

    credential_result, credential = check_result("operator_credential_reference", credential_check)
    checks.append(credential_result)
    secret = secret_cache.get("value") if credential is not None else None

    http_result: dict[str, Any]
    payload_cache: dict[str, dict[str, dict[str, Any]]] = {}

    def http_check() -> dict[str, Any]:
        evidence, loaded_payloads = validate_http(
            inputs, deps, secret or "", inputs.timeout_seconds
        )
        payload_cache["value"] = loaded_payloads
        return evidence

    if secret is None:
        http_result, _ = check_result(
            "loopback_health",
            lambda: (_ for _ in ()).throw(
                PreflightError("credential_dependency_failed")
            ),
        )
    else:
        http_result, _ = check_result("loopback_health", http_check)
    checks.append(http_result)
    payloads = payload_cache.get("value")

    if roster is None or payloads is None:
        identity_result, _ = check_result(
            "identity_readiness",
            lambda: (_ for _ in ()).throw(PreflightError("identity_dependency_failed")),
        )
    else:
        identity_result, _ = check_result(
            "identity_readiness",
            lambda: validate_identity(roster or {}, payloads or {}),
        )
    checks.append(identity_result)

    if roster is None or payloads is None:
        store_result, _ = check_result(
            "store_projection_backlog",
            lambda: (_ for _ in ()).throw(
                PreflightError("identity_dependency_failed")
            ),
        )
        projection_identity_result, _ = check_result(
            "projection_store_identity",
            lambda: (_ for _ in ()).throw(
                PreflightError("identity_dependency_failed")
            ),
        )
    else:
        combined_cache: dict[str, Any] = {}

        def combined_store_check() -> dict[str, Any]:
            event, watermarks, identity_rows = capture_store_snapshot(inputs, deps)
            combined_cache["identity_rows"] = identity_rows
            return validate_stores(event, watermarks)

        store_result, store_evidence = check_result(
            "store_projection_backlog", combined_store_check
        )
        if store_evidence is None:
            projection_identity_result, _ = check_result(
                "projection_store_identity",
                lambda: (_ for _ in ()).throw(
                    PreflightError("store_snapshot_dependency_failed")
                ),
            )
        else:
            projection_identity_result, _ = check_result(
                "projection_store_identity",
                lambda: validate_projection_identity_store(
                    combined_cache["identity_rows"],
                    roster or {},
                    require_int(
                        (payloads or {})["runtime_health"].get("current_shift"),
                        "runtime_shape",
                    ),
                ),
            )
    checks.append(store_result)
    checks.append(projection_identity_result)

    passed = all(item["status"] == "PASS" for item in checks)
    summary = {
        "schema_version": SCHEMA_VERSION,
        "claim": "runtime_preflight_pass" if passed else "runtime_preflight_fail",
        "runtime_preflight_pass": passed,
        "m0_acceptance_pass": False,
        "checks": checks,
    }
    summary["result_digest"] = evidence_digest(summary)
    return summary


def parse_args(argv: list[str]) -> Inputs:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--contract", required=True, type=Path)
    parser.add_argument("--profile", required=True, type=Path)
    parser.add_argument("--agents-dir", required=True, type=Path)
    parser.add_argument("--operator-credential-file", required=True, type=Path)
    parser.add_argument("--expected-git-sha", required=True)
    parser.add_argument("--expected-manifest-sha256", required=True)
    parser.add_argument(
        "--event-store",
        type=Path,
        default=Inputs.__dataclass_fields__["event_store"].default,
    )
    parser.add_argument(
        "--projection-store",
        type=Path,
        default=Inputs.__dataclass_fields__["projection_store"].default,
    )
    parser.add_argument("--timeout-seconds", type=float, default=5.0)
    args = parser.parse_args(argv)
    return Inputs(
        manifest=args.manifest,
        contract=args.contract,
        profile=args.profile,
        agents_dir=args.agents_dir,
        operator_credential=args.operator_credential_file,
        expected_git_sha=args.expected_git_sha,
        expected_manifest_sha256=args.expected_manifest_sha256,
        event_store=args.event_store,
        projection_store=args.projection_store,
        timeout_seconds=args.timeout_seconds,
    )


def main(argv: list[str] | None = None) -> int:
    try:
        result = evaluate(parse_args(sys.argv[1:] if argv is None else argv))
    except PreflightError as exc:
        result = {
            "schema_version": SCHEMA_VERSION,
            "claim": "runtime_preflight_fail",
            "runtime_preflight_pass": False,
            "m0_acceptance_pass": False,
            "checks": [],
            "fatal_reason": exc.code,
        }
        result["result_digest"] = evidence_digest(result)
    sys.stdout.buffer.write(canonical_json(result) + b"\n")
    return 0 if result["runtime_preflight_pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
