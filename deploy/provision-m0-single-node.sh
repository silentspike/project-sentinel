#!/usr/bin/env bash
set -euo pipefail

# The Python core provides descriptor-relative path handling and bounded rollback
# without adding another installed provisioning artifact.
exec python3 - "$@" <<'PY'
from __future__ import annotations

import argparse
import fcntl
import hashlib
import hmac
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
from typing import Any


SCHEMA_VERSION = 1
MAX_MANIFEST_BYTES = 4 * 1024 * 1024
MAX_ARTIFACT_BYTES = 1024 * 1024 * 1024
MAX_COMMAND_BYTES = 64 * 1024
DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
CREATED_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
PRODUCTION_STAGE_PREFIX = Path("/work/tmp/project-sentinel")
STOPPED_UNITS = {
    "nats-server.service",
    "sentinel-daemon.service",
    "sentinel-dashboard-backend.service",
    "sentinel-gaia-loop.service",
    "sentinel-gateway.service",
    "sentinel-health-monitor.service",
    "sentinel-health-monitor.timer",
    "sentinel-judge.service",
    "sentinel-nats-bridge.service",
    "sentinel-nightrun.service",
    "sentinel-nightrun.timer",
    "sentinel-projection.service",
    "sentinel.target",
}
AGENT_FILES = {
    "AGENT-01-THOMAS-CEO.toml", "AGENT-02-LISA-DESIGN.toml",
    "AGENT-03-MAX-DESIGN.toml", "AGENT-04-SOPHIE-DESIGN.toml",
    "AGENT-05-ANDREAS-DEV.toml", "AGENT-06-JULIA-DEV.toml",
    "AGENT-07-KAI-DEV.toml", "AGENT-08-LENA-DEV.toml",
    "AGENT-09-SARAH-PM.toml", "AGENT-10-DANIEL-PM.toml",
    "AGENT-11-MARCO-SALES.toml", "AGENT-12-NINA-MARKETING.toml",
    "AGENT-13-PETRA-ADMIN.toml", "AGENT-14-FLORIAN-IT.toml",
    "AGENT-15-HANNAH-WERKSTUD.toml", "AGENT-16-MICHAEL-CEO.toml",
    "AGENT-17-CARLA-DESIGN.toml", "AGENT-18-ROBIN-DESIGN.toml",
    "AGENT-19-TIM-DESIGN.toml", "AGENT-20-MARTIN-DEV.toml",
    "AGENT-21-FATIMA-DEV.toml", "AGENT-22-JONAS-DEV.toml",
    "AGENT-23-ANNA-DEVOPS.toml", "AGENT-24-ELENA-PM.toml",
    "AGENT-25-LUKAS-PM.toml", "AGENT-26-OLIVER-SALES.toml",
    "AGENT-27-MARA-MARKETING.toml", "AGENT-28-GABI-ADMIN.toml",
    "AGENT-29-TOBIAS-IT.toml", "AGENT-30-YARA-WERKSTUD.toml",
    "AGENT-31-SANDRA-CEO.toml", "AGENT-32-JENS-DESIGN.toml",
    "AGENT-33-PRIYA-DESIGN.toml", "AGENT-34-LEA-DESIGN.toml",
    "AGENT-35-KEVIN-DEV.toml", "AGENT-36-NILS-DEV.toml",
    "AGENT-37-SELINA-DEV.toml", "AGENT-38-PAUL-DEV.toml",
    "AGENT-39-VICTORIA-PM.toml", "AGENT-40-DAVID-PM.toml",
    "AGENT-41-FRANK-SALES.toml", "AGENT-42-JASMIN-MARKETING.toml",
    "AGENT-43-MONIKA-ADMIN.toml", "AGENT-44-MARCUS-IT.toml",
    "AGENT-45-EMILIA-WERKSTUD.toml", "AGENT-46-RALF-BETRIEBSRAT.toml",
    "AGENT-47-AYLIN-BETRIEBSRAT.toml", "AGENT-48-STEFAN-BETRIEBSRAT.toml",
    "AGENT-49-CARLA-BETRIEBSPSYCH.toml",
    "AGENT-50-KATHARINA-BETRIEBSPSYCH.toml",
    "AGENT-51-HENDRIK-BETRIEBSPSYCH.toml",
    "AGENT-52-WERNER-BETRIEBSARZT.toml",
    "AGENT-53-WIESNER-BETRIEBSARZT.toml",
    "AGENT-54-BRANDT-BETRIEBSARZT.toml", "AGENT-55-LAURA-QA.toml",
    "AGENT-56-TOBIAS-DELIVERY.toml", "AGENT-57-CHEN-QA.toml",
    "AGENT-58-MARIA-DELIVERY.toml", "AGENT-59-AMIR-QA.toml",
    "AGENT-60-KATRIN-DELIVERY.toml",
}


def artifact_authority() -> dict[str, tuple[str, str]]:
    rows = {
        "/opt/sentinel/bin/sentinel-daemon": ("target/release/sentinel-daemon", "binary"),
        "/usr/bin/agent-runtime": ("target/release/agent-runtime", "binary"),
        "/opt/sentinel/bin/landlock-wrapper": ("target/release/landlock-wrapper", "binary"),
        "/opt/sentinel/bin/sentinel-nightrun": ("target/release/sentinel-nightrun", "binary"),
        "/opt/sentinel/bin/sentinel-projection": ("target/release/sentinel-projection", "binary"),
        "/opt/sentinel/bin/sentinel-dashboard-backend": ("target/release/sentinel-dashboard-backend", "binary"),
        "/opt/sentinel/bin/sentinel-gaia-loop": ("target/release/sentinel-gaia-loop", "binary"),
        "/opt/sentinel/bin/sentinel-ctl": ("target/release/sentinel-ctl", "binary"),
        "/opt/sentinel/bin/sentinel-gaia": ("target/release/sentinel-gaia", "binary"),
        "/opt/sentinel/bin/cortex-gateway": ("cmd/cortex-gateway/cortex-gateway", "binary"),
        "/opt/sentinel/bin/sentinel-judge": ("services/sentinel-judge/sentinel-judge", "binary"),
        "/opt/sentinel/bin/sentinel-nats-bridge": ("services/sentinel-nats-bridge/sentinel-nats-bridge", "binary"),
        "/usr/local/bin/nats-server": ("external/nats-server", "binary"),
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
        "/etc/systemd/system/sentinel-daemon.service": ("deploy/systemd/sentinel-daemon.service", "systemd"),
        "/etc/systemd/system/sentinel-gateway.service": ("deploy/systemd/sentinel-gateway.service", "systemd"),
        "/etc/systemd/system/sentinel-judge.service": ("deploy/systemd/sentinel-judge.service", "systemd"),
        "/etc/systemd/system/sentinel-nats-bridge.service": ("deploy/systemd/sentinel-nats-bridge.service", "systemd"),
        "/etc/systemd/system/sentinel-nightrun.service": ("deploy/systemd/sentinel-nightrun.service", "systemd"),
        "/etc/systemd/system/sentinel-nightrun.timer": ("deploy/systemd/sentinel-nightrun.timer", "systemd"),
        "/etc/systemd/system/sentinel-projection.service": ("deploy/systemd/sentinel-projection.service", "systemd"),
        "/etc/systemd/system/sentinel-dashboard-backend.service": ("deploy/systemd/sentinel-dashboard-backend.service", "systemd"),
        "/etc/systemd/system/sentinel-gaia-loop.service": ("deploy/systemd/sentinel-gaia-loop.service", "systemd"),
        "/etc/systemd/system/nats-server.service": ("deploy/systemd/nats-server.service", "systemd"),
        "/etc/systemd/system/sentinel.target": ("deploy/systemd/sentinel.target", "systemd"),
        "/opt/sentinel/scripts/init-cgroups.sh": ("deploy/scripts/init-cgroups.sh", "script"),
        "/opt/sentinel/scripts/init-dirs.sh": ("deploy/scripts/init-dirs.sh", "script"),
        "/opt/sentinel/scripts/init-runtime-base-dirs.sh": ("deploy/scripts/init-runtime-base-dirs.sh", "script"),
        "/opt/sentinel/scripts/init-dashboard-auth.sh": ("deploy/scripts/init-dashboard-auth.sh", "script"),
        "/opt/sentinel/scripts/install-native-claude.sh": ("deploy/scripts/install-native-claude.sh", "script"),
        "/opt/sentinel/scripts/init-hugepages.sh": ("deploy/scripts/init-hugepages.sh", "script"),
        "/opt/sentinel/scripts/init-sysctl.sh": ("deploy/scripts/init-sysctl.sh", "script"),
        "/opt/sentinel/scripts/init-tmpfs.sh": ("deploy/scripts/init-tmpfs.sh", "script"),
        "/opt/sentinel/scripts/sentinel-health-monitor.sh": ("deploy/scripts/sentinel-health-monitor.sh", "script"),
        "/opt/sentinel/scripts/m0-readiness.py": ("scripts/product-acceptance/m0-readiness/readiness.py", "script"),
        "/opt/sentinel/share/runtime-base.env": ("deploy/runtime-base.env", "config"),
        "/etc/apt/preferences.d/sentinel-runtime": ("deploy/apt/sentinel-runtime.pref", "config"),
        "/etc/sysctl.d/99-sentinel-bwrap.conf": ("deploy/vm-config/99-sentinel-bwrap.conf", "config"),
        "/etc/systemd/system/sentinel-health-monitor.service": ("deploy/systemd/sentinel-health-monitor.service", "systemd"),
        "/etc/systemd/system/sentinel-health-monitor.timer": ("deploy/systemd/sentinel-health-monitor.timer", "systemd"),
        "/opt/sentinel/config/work-profiles/web-project-v1.toml": ("config/work-profiles/web-project-v1.toml", "config"),
        "/opt/sentinel/config/workbench-profiles/web-authoring-v1.toml": ("config/workbench-profiles/web-authoring-v1.toml", "config"),
        "/opt/sentinel/config/product-acceptance/m0-contract.toml": ("scripts/product-acceptance/m0-contract.toml", "config"),
    }
    rows.update({
        f"/opt/sentinel/config/agents/{name}": (f"config/agents/{name}", "config")
        for name in AGENT_FILES
    })
    return rows


AUTHORITY = artifact_authority()
TARGET_MODES = {"binary": 0o755, "script": 0o755, "config": 0o644, "systemd": 0o644}


class ProvisionError(Exception):
    pass


def fail(reason: str) -> None:
    raise ProvisionError(reason)


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
        raise ProvisionError("json_invalid") from exc


def canonical(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n").encode("ascii")


def safe_parts(path: str, *, absolute: bool) -> tuple[str, ...]:
    value = PurePosixPath(path)
    if value.is_absolute() != absolute or not value.parts:
        fail("path_invalid")
    parts = value.parts[1:] if absolute else value.parts
    if any(part in {"", ".", ".."} for part in parts):
        fail("path_invalid")
    return tuple(parts)


def safe_abs(path: Path) -> Path:
    if not path.is_absolute() or ".." in path.parts:
        fail("path_invalid")
    return path


def validate_dir_stat(info: os.stat_result, *, owners: set[int]) -> None:
    if not stat.S_ISDIR(info.st_mode) or info.st_uid not in owners:
        fail("directory_authority_invalid")
    if info.st_mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX | 0o022):
        fail("directory_mode_invalid")


def open_absolute(path: Path, *, owners: set[int]) -> tuple[int, os.stat_result]:
    parts = safe_parts(str(path), absolute=True)
    fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for part in parts[:-1]:
            next_fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
            os.close(fd)
            fd = next_fd
            validate_dir_stat(os.fstat(fd), owners=owners)
        leaf = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
        info = os.fstat(leaf)
        return leaf, info
    finally:
        os.close(fd)


def read_bounded(fd: int, maximum: int) -> bytes:
    chunks: list[bytes] = []
    size = 0
    while True:
        chunk = os.read(fd, min(1024 * 1024, maximum + 1 - size))
        if not chunk:
            break
        size += len(chunk)
        if size > maximum:
            fail("file_oversized")
        chunks.append(chunk)
    return b"".join(chunks)


def source_snapshot(root: Path, source: str, kind: str) -> dict[str, Any]:
    parts = safe_parts(source, absolute=False)
    root_fd, root_info = open_absolute(root, owners={0, os.geteuid()})
    os.close(root_fd)
    if not stat.S_ISDIR(root_info.st_mode):
        fail("source_root_invalid")
    fd = os.open(str(root), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        validate_dir_stat(os.fstat(fd), owners={0, os.geteuid()})
        for part in parts[:-1]:
            next_fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
            os.close(fd)
            fd = next_fd
            validate_dir_stat(os.fstat(fd), owners={0, os.geteuid()})
        leaf = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
        try:
            info = os.fstat(leaf)
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_uid != os.geteuid():
                fail("source_file_authority_invalid")
            mode = stat.S_IMODE(info.st_mode)
            if mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX | 0o022):
                fail("source_file_mode_invalid")
            if kind in {"binary", "script"} and not mode & 0o100:
                fail("source_file_mode_invalid")
            if kind in {"config", "systemd"} and mode & 0o111:
                fail("source_file_mode_invalid")
            digest = hashlib.sha256()
            size = 0
            while True:
                chunk = os.read(leaf, 1024 * 1024)
                if not chunk:
                    break
                size += len(chunk)
                if size > MAX_ARTIFACT_BYTES:
                    fail("artifact_oversized")
                digest.update(chunk)
            return {"dev": info.st_dev, "ino": info.st_ino, "size": size,
                    "mtime_ns": info.st_mtime_ns, "mode": mode, "sha256": digest.hexdigest()}
        finally:
            os.close(leaf)
    finally:
        os.close(fd)


def read_manifest(path: Path) -> tuple[bytes, dict[str, Any]]:
    fd, info = open_absolute(path, owners={0, os.geteuid()})
    try:
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_uid != os.geteuid():
            fail("manifest_authority_invalid")
        if stat.S_IMODE(info.st_mode) & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX | 0o022 | 0o111):
            fail("manifest_mode_invalid")
        raw = read_bounded(fd, MAX_MANIFEST_BYTES)
    finally:
        os.close(fd)
    value = strict_json(raw)
    if not isinstance(value, dict):
        fail("manifest_not_object")
    return raw, value


def validate_manifest(args: argparse.Namespace) -> tuple[list[dict[str, Any]], str]:
    raw, manifest = read_manifest(args.manifest)
    actual_manifest_sha = hashlib.sha256(raw).hexdigest()
    if not DIGEST_RE.fullmatch(args.expected_manifest_sha256) or not hmac.compare_digest(actual_manifest_sha, args.expected_manifest_sha256):
        fail("manifest_authority_digest_mismatch")
    if set(manifest) != {"version", "created_at", "git_sha", "artifacts"}:
        fail("manifest_shape")
    if manifest["version"] != "1.0" or not isinstance(manifest["created_at"], str) or not CREATED_RE.fullmatch(manifest["created_at"]):
        fail("manifest_version_or_time")
    if not isinstance(manifest["git_sha"], str) or not SHA_RE.fullmatch(manifest["git_sha"]) or manifest["git_sha"] != args.expected_git_sha:
        fail("manifest_git_sha_mismatch")
    items = manifest["artifacts"]
    if not isinstance(items, list) or len(items) != len(AUTHORITY):
        fail("manifest_artifact_count")
    seen_paths: set[str] = set()
    seen_sources: set[str] = set()
    verified: list[dict[str, Any]] = []
    for item in items:
        if not isinstance(item, dict) or set(item) != {"path", "source", "sha256", "type"}:
            fail("manifest_artifact_shape")
        dest, source, digest, kind = (item.get(key) for key in ("path", "source", "sha256", "type"))
        if not all(isinstance(value, str) for value in (dest, source, digest, kind)):
            fail("manifest_artifact_invalid")
        safe_parts(dest, absolute=True)
        safe_parts(source, absolute=False)
        if not DIGEST_RE.fullmatch(digest):
            fail("manifest_artifact_invalid")
        if dest in seen_paths or source in seen_sources:
            fail("manifest_artifact_duplicate")
        if AUTHORITY.get(dest) != (source, kind):
            fail("manifest_artifact_authority_mismatch")
        try:
            snap = source_snapshot(args.source_root, source, kind)
        except OSError as exc:
            raise ProvisionError("source_path_unsafe") from exc
        if not hmac.compare_digest(snap["sha256"], digest):
            fail("source_hash_mismatch")
        seen_paths.add(dest)
        seen_sources.add(source)
        verified.append({**item, "snapshot": snap})
    if seen_paths != set(AUTHORITY):
        fail("manifest_required_artifact_missing")
    return sorted(verified, key=lambda row: row["path"]), actual_manifest_sha


def validate_fake_contract(args: argparse.Namespace) -> tuple[int, int]:
    production = args.target_root == Path("/")
    if production:
        if os.geteuid() != 0 or args.service_state_file is not None or args.fail_after is not None:
            fail("production_authority_invalid")
        if args.install_uid != 0 or args.install_gid != 0:
            fail("install_owner_invalid")
        try:
            args.stage_root.relative_to(PRODUCTION_STAGE_PREFIX)
        except ValueError:
            fail("stage_root_invalid")
    else:
        if args.install_uid != os.geteuid() or args.install_gid != os.getegid():
            fail("install_owner_invalid")
        if args.service_state_file is None:
            fail("fake_service_state_required")
    return args.install_uid, args.install_gid


def validate_existing_chain(path: Path, uid: int, gid: int) -> None:
    current = Path("/")
    for part in path.parts[1:]:
        current /= part
        try:
            info = os.lstat(current)
        except FileNotFoundError:
            return
        if stat.S_ISLNK(info.st_mode):
            fail("target_parent_symlink")
        if not stat.S_ISDIR(info.st_mode):
            fail("target_parent_invalid")
        if info.st_uid not in {0, uid} or info.st_gid not in {0, gid} or info.st_mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX | 0o022):
            fail("target_parent_authority_invalid")


def target_path(root: Path, destination: str) -> Path:
    return root.joinpath(*safe_parts(destination, absolute=True))


def validate_targets(args: argparse.Namespace, rows: list[dict[str, Any]], uid: int, gid: int) -> None:
    validate_existing_chain(args.target_root, uid, gid)
    for row in rows:
        target = target_path(args.target_root, row["path"])
        validate_existing_chain(target.parent, uid, gid)
        try:
            info = os.lstat(target)
        except FileNotFoundError:
            continue
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            fail("target_file_authority_invalid")
        if info.st_uid != uid or info.st_gid != gid or stat.S_IMODE(info.st_mode) != TARGET_MODES[row["type"]]:
            fail("target_file_owner_or_mode_invalid")


def command(argv: list[str]) -> bytes:
    try:
        result = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, timeout=10, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ProvisionError("service_state_command_failed") from exc
    if len(result.stdout) > MAX_COMMAND_BYTES:
        fail("service_state_output_oversized")
    if result.returncode not in {0, 3, 4}:
        fail("service_state_command_failed")
    return result.stdout


def validate_services(args: argparse.Namespace) -> None:
    if args.service_state_file is not None:
        fd, info = open_absolute(args.service_state_file, owners={0, os.geteuid()})
        try:
            if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                fail("service_state_invalid")
            value = strict_json(read_bounded(fd, MAX_COMMAND_BYTES))
        finally:
            os.close(fd)
        if not isinstance(value, dict) or set(value) != STOPPED_UNITS:
            fail("service_state_incomplete")
        if any(state not in {"inactive", "unknown"} for state in value.values()):
            fail("service_running_or_failed")
        return
    for unit in sorted(STOPPED_UNITS):
        state = command(["systemctl", "is-active", unit]).decode("ascii", "strict").strip()
        if state not in {"inactive", "unknown"}:
            fail("service_running_or_failed")


def ensure_dir(path: Path, uid: int, gid: int, created: list[Path]) -> None:
    current = Path("/")
    for part in path.parts[1:]:
        current /= part
        try:
            info = os.lstat(current)
        except FileNotFoundError:
            os.mkdir(current, 0o755)
            os.chown(current, uid, gid)
            created.append(current)
            info = os.lstat(current)
        if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode):
            fail("target_parent_invalid")
        if info.st_uid not in {0, uid} or info.st_gid not in {0, gid} or info.st_mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX | 0o022):
            fail("target_parent_authority_invalid")


def copy_and_hash(source: Path, destination: Path, mode: int, uid: int, gid: int,
                  expected_snapshot: dict[str, Any] | None = None) -> tuple[str, int]:
    source_fd, source_info = open_absolute(source, owners={0, os.geteuid()})
    if not stat.S_ISREG(source_info.st_mode) or source_info.st_nlink != 1:
        os.close(source_fd)
        fail("copy_source_invalid")
    if expected_snapshot is not None:
        actual_snapshot = {
            "dev": source_info.st_dev,
            "ino": source_info.st_ino,
            "size": source_info.st_size,
            "mtime_ns": source_info.st_mtime_ns,
            "mode": stat.S_IMODE(source_info.st_mode),
        }
        expected_identity = {key: expected_snapshot[key] for key in actual_snapshot}
        if source_info.st_uid != os.geteuid() or actual_snapshot != expected_identity:
            os.close(source_fd)
            fail("source_changed_after_validation")
    dest_fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    digest = hashlib.sha256()
    size = 0
    try:
        while True:
            chunk = os.read(source_fd, 1024 * 1024)
            if not chunk:
                break
            size += len(chunk)
            if size > MAX_ARTIFACT_BYTES:
                fail("artifact_oversized")
            digest.update(chunk)
            view = memoryview(chunk)
            while view:
                written = os.write(dest_fd, view)
                view = view[written:]
        os.fchmod(dest_fd, mode)
        os.fchown(dest_fd, uid, gid)
        os.fsync(dest_fd)
    finally:
        os.close(source_fd)
        os.close(dest_fd)
    return digest.hexdigest(), size


def hash_file(path: Path, maximum: int = MAX_ARTIFACT_BYTES) -> tuple[str, int]:
    fd, info = open_absolute(path, owners={0, os.geteuid()})
    try:
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            fail("hash_source_invalid")
        digest = hashlib.sha256()
        size = 0
        while True:
            chunk = os.read(fd, 1024 * 1024)
            if not chunk:
                break
            size += len(chunk)
            if size > maximum:
                fail("artifact_oversized")
            digest.update(chunk)
        return digest.hexdigest(), size
    finally:
        os.close(fd)


def write_receipt(stage: Path, value: dict[str, Any]) -> str:
    data = canonical(value)
    target = stage / "provision-receipt.json"
    temp = stage / ".provision-receipt.tmp"
    fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        os.write(fd, data)
        os.fsync(fd)
    finally:
        os.close(fd)
    os.replace(temp, target)
    dir_fd = os.open(stage, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        os.fsync(dir_fd)
    finally:
        os.close(dir_fd)
    return hashlib.sha256(data).hexdigest()


def ensure_stage_root(path: Path) -> None:
    parts = safe_parts(str(path), absolute=True)
    fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for part in parts:
            try:
                next_fd = os.open(
                    part,
                    os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                    dir_fd=fd,
                )
            except FileNotFoundError:
                os.mkdir(part, 0o700, dir_fd=fd)
                next_fd = os.open(
                    part,
                    os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                    dir_fd=fd,
                )
            os.close(fd)
            fd = next_fd
            validate_dir_stat(os.fstat(fd), owners={0, os.geteuid()})
    finally:
        os.close(fd)


def prepare_stage(args: argparse.Namespace) -> tuple[int, Path]:
    safe_abs(args.stage_root)
    try:
        ensure_stage_root(args.stage_root)
    except ProvisionError:
        raise
    except OSError as exc:
        raise ProvisionError("stage_root_unsafe") from exc
    try:
        info = os.lstat(args.stage_root)
    except OSError as exc:
        raise ProvisionError("stage_root_unsafe") from exc
    if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode) or info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) != 0o700:
        fail("stage_root_authority_invalid")
    lock_path = args.stage_root / ".provision.lock"
    try:
        lock_fd = os.open(
            lock_path,
            os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC,
            0o600,
        )
    except OSError as exc:
        raise ProvisionError("stage_lock_unsafe") from exc
    locked = False
    try:
        lock_info = os.fstat(lock_fd)
        if (
            not stat.S_ISREG(lock_info.st_mode)
            or lock_info.st_nlink != 1
            or stat.S_IMODE(lock_info.st_mode) != 0o600
            or lock_info.st_uid != os.geteuid()
        ):
            fail("stage_lock_authority_invalid")
        try:
            fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            locked = True
        except BlockingIOError as exc:
            raise ProvisionError("provision_already_running") from exc
        except OSError as exc:
            raise ProvisionError("stage_lock_failed") from exc
        operation = args.stage_root / "operation"
        try:
            if operation.is_symlink():
                fail("stage_operation_unsafe")
            if operation.exists():
                fail("stage_operation_stale")
        except ProvisionError:
            raise
        except OSError as exc:
            raise ProvisionError("stage_operation_unsafe") from exc
        try:
            operation.mkdir(mode=0o700)
        except OSError as exc:
            raise ProvisionError("stage_operation_unsafe") from exc
        return lock_fd, operation
    except Exception:
        if locked:
            try:
                fcntl.flock(lock_fd, fcntl.LOCK_UN)
            except OSError:
                pass
        os.close(lock_fd)
        raise


def run(args: argparse.Namespace) -> dict[str, Any]:
    lock_fd: int | None = None
    operation: Path | None = None
    changed: list[tuple[Path, Path | None, int, int, int]] = []
    created_dirs: list[Path] = []
    target_mutation_started = False
    try:
        uid, gid = validate_fake_contract(args)
        lock_fd, operation = prepare_stage(args)
        if args.inject_pre_mutation_error:
            raise RuntimeError(f"injected private detail at {args.stage_root}")
        rows, manifest_sha = validate_manifest(args)
        validate_services(args)
        validate_targets(args, rows, uid, gid)

        incoming = operation / "incoming"
        rollback = operation / "rollback"
        incoming.mkdir(mode=0o700)
        rollback.mkdir(mode=0o700)
        staged: list[Path] = []
        for index, row in enumerate(rows):
            current = source_snapshot(args.source_root, row["source"], row["type"])
            if current != row["snapshot"]:
                fail("source_changed_after_validation")
            source = args.source_root.joinpath(*safe_parts(row["source"], absolute=False))
            staged_file = incoming / f"{index:03d}"
            digest, size = copy_and_hash(
                source, staged_file, TARGET_MODES[row["type"]], uid, gid,
                expected_snapshot=row["snapshot"],
            )
            if digest != row["sha256"] or size != row["snapshot"]["size"]:
                fail("staged_artifact_mismatch")
            staged.append(staged_file)

        # Recheck every target and service immediately before host mutation.
        validate_services(args)
        validate_targets(args, rows, uid, gid)
        applied = 0
        target_mutation_started = True
        for index, row in enumerate(rows):
            target = target_path(args.target_root, row["path"])
            ensure_dir(target.parent, uid, gid, created_dirs)
            backup: Path | None = None
            try:
                info = os.lstat(target)
                existing = True
            except FileNotFoundError:
                existing = False
                info = None
            if existing:
                assert info is not None
                current_digest, _ = hash_file(target)
                if current_digest == row["sha256"]:
                    continue
                backup = rollback / f"{index:03d}"
                backup_digest, _ = copy_and_hash(target, backup, stat.S_IMODE(info.st_mode), info.st_uid, info.st_gid)
                if backup_digest != current_digest:
                    fail("rollback_capture_mismatch")
                changed.append((target, backup, stat.S_IMODE(info.st_mode), info.st_uid, info.st_gid))
            else:
                changed.append((target, None, 0, 0, 0))
            temp = target.parent / f".{target.name}.m0-new"
            if temp.exists() or temp.is_symlink():
                fail("target_temp_exists")
            digest, _ = copy_and_hash(staged[index], temp, TARGET_MODES[row["type"]], uid, gid)
            if digest != row["sha256"]:
                fail("install_copy_mismatch")
            os.replace(temp, target)
            parent_fd = os.open(target.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
            try:
                os.fsync(parent_fd)
            finally:
                os.close(parent_fd)
            applied += 1
            if args.fail_after is not None and applied >= args.fail_after:
                fail("injected_install_failure")

        validate_targets(args, rows, uid, gid)
        for row in rows:
            target = target_path(args.target_root, row["path"])
            if hash_file(target)[0] != row["sha256"]:
                fail("installed_hash_mismatch")
        receipt = {"schema_version": SCHEMA_VERSION, "status": "COMPLETE",
                   "git_sha": args.expected_git_sha, "manifest_sha256": manifest_sha,
                   "artifact_count": len(rows), "changed_count": applied,
                   "artifact_set_digest": hashlib.sha256(canonical(sorted(row["sha256"] for row in rows))).hexdigest(),
                   "services_started": False}
        receipt["receipt_sha256"] = write_receipt(args.stage_root, receipt)
        shutil.rmtree(operation)
        return receipt
    except Exception as exc:
        reason = exc.args[0] if isinstance(exc, ProvisionError) and exc.args else "internal_failure"
        if not target_mutation_started:
            if operation is not None:
                try:
                    shutil.rmtree(operation)
                except OSError:
                    pass
            raise ProvisionError(reason) from exc
        rollback_ok = True
        for target, backup, mode, old_uid, old_gid in reversed(changed):
            try:
                if backup is None:
                    target.unlink(missing_ok=True)
                else:
                    temp = target.parent / f".{target.name}.m0-rollback"
                    temp.unlink(missing_ok=True)
                    copy_and_hash(backup, temp, mode, old_uid, old_gid)
                    os.replace(temp, target)
                    parent_fd = os.open(target.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
                    try:
                        os.fsync(parent_fd)
                    finally:
                        os.close(parent_fd)
            except Exception:
                rollback_ok = False
        for directory in reversed(created_dirs):
            try:
                directory.rmdir()
            except OSError:
                pass
        receipt = {"schema_version": SCHEMA_VERSION,
                   "status": "ROLLED_BACK" if rollback_ok else "ROLLBACK_FAILED",
                   "reason": reason, "git_sha": args.expected_git_sha,
                   "artifact_count": len(AUTHORITY), "services_started": False}
        try:
            receipt["receipt_sha256"] = write_receipt(args.stage_root, receipt)
        except Exception:
            rollback_ok = False
        raise ProvisionError(reason if rollback_ok else "rollback_failed") from exc
    finally:
        if lock_fd is not None:
            try:
                fcntl.flock(lock_fd, fcntl.LOCK_UN)
            except OSError:
                pass
            os.close(lock_fd)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Install the stopped single-node M0 release set")
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--expected-manifest-sha256", required=True)
    parser.add_argument("--expected-git-sha", required=True)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--target-root", type=Path, default=Path("/"))
    parser.add_argument("--stage-root", type=Path, required=True)
    parser.add_argument("--install-uid", type=int, default=0)
    parser.add_argument("--install-gid", type=int, default=0)
    parser.add_argument("--service-state-file", type=Path)
    parser.add_argument("--fail-after", type=int)
    parser.add_argument("--inject-pre-mutation-error", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    for name in ("manifest", "source_root", "target_root", "stage_root"):
        safe_abs(getattr(args, name))
    if args.service_state_file is not None:
        safe_abs(args.service_state_file)
    if args.fail_after is not None and args.fail_after < 1:
        fail("fail_after_invalid")
    if args.target_root == Path("/") and args.inject_pre_mutation_error:
        fail("production_authority_invalid")
    return args


try:
    result = run(parse_args())
except Exception as exc:
    reason = exc.args[0] if isinstance(exc, ProvisionError) and exc.args else "internal_failure"
    print(canonical({"schema_version": SCHEMA_VERSION, "status": "FAIL", "reason": reason}).decode("ascii"), end="", file=sys.stderr)
    raise SystemExit(1)
print(canonical(result).decode("ascii"), end="")
PY
