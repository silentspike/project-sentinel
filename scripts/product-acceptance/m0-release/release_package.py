#!/usr/bin/env python3
"""Build and verify the deterministic single-node M0 release package."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import fcntl
import hashlib
import hmac
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
from typing import Any, Callable, Mapping


SCHEMA_VERSION = 1
MANIFEST_VERSION = "1.0"
MAX_ARTIFACT_BYTES = 1024 * 1024 * 1024
MAX_MANIFEST_BYTES = 4 * 1024 * 1024
SHA1_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
PACKAGE_PREFIX = "m0-release-"
MANIFEST_NAME = "release-manifest.json"
FILE_MODES = {"binary": 0o500, "script": 0o500, "config": 0o400, "systemd": 0o400}
SOURCE_TYPES = frozenset(FILE_MODES)
FORBIDDEN_SOURCE_PARTS = frozenset(
    {"credential", "credentials", "data", "database", "databases", "secret", "secrets", "state"}
)
FORBIDDEN_SOURCE_SUFFIXES = (".db", ".env", ".key", ".pem", ".redb", ".sqlite", ".sqlite3")


class PackageError(Exception):
    """A stable, public-safe release package failure."""


def fail(code: str) -> None:
    raise PackageError(code)


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n").encode(
        "ascii"
    )


def strict_json(raw: bytes) -> Any:
    def object_pairs(values: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in values:
            if key in result:
                fail("json_duplicate_key")
            result[key] = value
        return result

    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=object_pairs)
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise PackageError("json_invalid") from exc


def safe_parts(value: str, *, absolute: bool) -> tuple[str, ...]:
    path = PurePosixPath(value)
    if path.is_absolute() != absolute or not path.parts:
        fail("path_invalid")
    parts = path.parts[1:] if absolute else path.parts
    if not parts or any(part in {"", ".", ".."} for part in parts):
        fail("path_invalid")
    return tuple(parts)


def load_inventory() -> dict[str, tuple[str, str]]:
    authority_path = Path(__file__).resolve().parent.parent / "run_m0_preflight.py"
    spec = importlib.util.spec_from_file_location("m0_release_inventory_authority", authority_path)
    if spec is None or spec.loader is None:
        fail("inventory_unavailable")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    try:
        spec.loader.exec_module(module)
    except Exception as exc:
        raise PackageError("inventory_unavailable") from exc
    value = getattr(module, "CANONICAL_RELEASE_ARTIFACTS", None)
    if not isinstance(value, dict):
        fail("inventory_invalid")
    return validate_inventory(value)


def validate_inventory(value: Mapping[Any, Any]) -> dict[str, tuple[str, str]]:
    result: dict[str, tuple[str, str]] = {}
    sources: set[str] = set()
    for destination, authority in value.items():
        if not isinstance(destination, str) or not isinstance(authority, tuple) or len(authority) != 2:
            fail("inventory_invalid")
        source, kind = authority
        if not isinstance(source, str) or not isinstance(kind, str) or kind not in SOURCE_TYPES:
            fail("inventory_invalid")
        safe_parts(destination, absolute=True)
        source_parts = safe_parts(source, absolute=False)
        lowered = tuple(part.lower() for part in source_parts)
        suffix_forbidden = lowered[-1].endswith(FORBIDDEN_SOURCE_SUFFIXES) and source != "deploy/runtime-base.env"
        if (
            any(part in FORBIDDEN_SOURCE_PARTS for part in lowered)
            or lowered[-1].startswith(".")
            or suffix_forbidden
        ):
            fail("inventory_forbidden_source")
        if destination in result or source in sources:
            fail("inventory_duplicate")
        result[destination] = (source, kind)
        sources.add(source)
    if not result or "external/nats-server" not in sources:
        fail("inventory_invalid")
    return result


def validate_dir(
    info: os.stat_result, *, exact_mode: int | None = None, exact_owner: bool = False
) -> None:
    allowed_owners = {os.geteuid()} if exact_owner else {0, os.geteuid()}
    if not stat.S_ISDIR(info.st_mode) or info.st_uid not in allowed_owners:
        fail("directory_authority_invalid")
    mode = stat.S_IMODE(info.st_mode)
    if info.st_mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX):
        fail("directory_mode_invalid")
    if exact_mode is not None:
        if mode != exact_mode:
            fail("directory_mode_invalid")
    elif mode & 0o022:
        fail("directory_mode_invalid")


def open_absolute_dir(path: Path, *, exact_mode: int | None = None) -> int:
    if not path.is_absolute():
        fail("path_invalid")
    parts = safe_parts(str(path), absolute=True)
    fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for part in parts:
            next_fd = os.open(
                part,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                dir_fd=fd,
            )
            os.close(fd)
            fd = next_fd
            validate_dir(os.fstat(fd))
        if exact_mode is not None:
            validate_dir(os.fstat(fd), exact_mode=exact_mode, exact_owner=True)
        return fd
    except Exception:
        os.close(fd)
        raise


def open_relative_file(root_fd: int, relative: str) -> tuple[int, os.stat_result]:
    parts = safe_parts(relative, absolute=False)
    current = os.dup(root_fd)
    try:
        for part in parts[:-1]:
            next_fd = os.open(
                part,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                dir_fd=current,
            )
            os.close(current)
            current = next_fd
            validate_dir(os.fstat(current))
        leaf = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=current)
        return leaf, os.fstat(leaf)
    finally:
        os.close(current)


def open_absolute_file(path: Path) -> tuple[int, os.stat_result]:
    if not path.is_absolute():
        fail("path_invalid")
    parent_fd = open_absolute_dir(path.parent)
    try:
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent_fd)
        return fd, os.fstat(fd)
    finally:
        os.close(parent_fd)


def source_mode_valid(info: os.stat_result, kind: str) -> bool:
    mode = stat.S_IMODE(info.st_mode)
    if info.st_mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX) or mode & 0o022:
        return False
    if kind in {"binary", "script"}:
        return bool(mode & 0o100)
    return not bool(mode & 0o111)


def digest_fd(fd: int, maximum: int = MAX_ARTIFACT_BYTES) -> tuple[str, int]:
    os.lseek(fd, 0, os.SEEK_SET)
    digest = hashlib.sha256()
    size = 0
    while True:
        chunk = os.read(fd, min(1024 * 1024, maximum + 1 - size))
        if not chunk:
            break
        size += len(chunk)
        if size > maximum:
            fail("artifact_oversized")
        digest.update(chunk)
    os.lseek(fd, 0, os.SEEK_SET)
    return digest.hexdigest(), size


@dataclass
class PinnedSource:
    source: str
    destination: str
    kind: str
    fd: int
    identity: tuple[int, int, int, int, int]
    sha256: str

    def close(self) -> None:
        os.close(self.fd)


def pin_source(fd: int, info: os.stat_result, source: str, destination: str, kind: str) -> PinnedSource:
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_nlink != 1
        or info.st_uid != os.geteuid()
        or not source_mode_valid(info, kind)
    ):
        os.close(fd)
        fail("source_authority_invalid")
    try:
        digest, size = digest_fd(fd)
    except Exception:
        os.close(fd)
        raise
    if size != info.st_size:
        os.close(fd)
        fail("source_changed")
    identity = (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, stat.S_IMODE(info.st_mode))
    return PinnedSource(source, destination, kind, fd, identity, digest)


def current_source_identity(root_fd: int, item: PinnedSource) -> tuple[int, int, int, int, int]:
    fd, info = open_relative_file(root_fd, item.source)
    os.close(fd)
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, stat.S_IMODE(info.st_mode))


def current_absolute_identity(path: Path) -> tuple[int, int, int, int, int]:
    fd, info = open_absolute_file(path)
    os.close(fd)
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, stat.S_IMODE(info.st_mode))


def git_metadata(source_root: Path, expected_git_sha: str) -> str:
    if not SHA1_RE.fullmatch(expected_git_sha):
        fail("git_sha_invalid")
    environment = {
        "PATH": os.environ.get("PATH", ""),
        "HOME": "/nonexistent",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "LC_ALL": "C",
    }
    try:
        head = subprocess.run(
            ["git", "-C", str(source_root), "rev-parse", "--verify", "HEAD^{commit}"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=10,
            check=False,
            env=environment,
        )
        dirty = subprocess.run(
            ["git", "-C", str(source_root), "status", "--porcelain", "--untracked-files=no"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=10,
            check=False,
            env=environment,
        )
        timestamp = subprocess.run(
            ["git", "-C", str(source_root), "show", "-s", "--format=%ct", expected_git_sha],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=10,
            check=False,
            env=environment,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise PackageError("git_authority_unavailable") from exc
    try:
        head_text = head.stdout.decode("ascii").strip()
        timestamp_text = timestamp.stdout.decode("ascii").strip()
    except UnicodeError as exc:
        raise PackageError("git_authority_invalid") from exc
    if head.returncode != 0 or not hmac.compare_digest(head_text, expected_git_sha):
        fail("git_sha_mismatch")
    if dirty.returncode != 0 or dirty.stdout:
        fail("git_source_dirty")
    if timestamp.returncode != 0 or not timestamp_text.isdigit():
        fail("git_timestamp_invalid")
    return datetime.fromtimestamp(int(timestamp_text), timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def write_pinned(item: PinnedSource, destination: Path) -> None:
    destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    digest = hashlib.sha256()
    size = 0
    try:
        os.lseek(item.fd, 0, os.SEEK_SET)
        while True:
            chunk = os.read(item.fd, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
            size += len(chunk)
            view = memoryview(chunk)
            while view:
                written = os.write(fd, view)
                view = view[written:]
        if size != item.identity[2] or not hmac.compare_digest(digest.hexdigest(), item.sha256):
            fail("source_changed")
        os.fchmod(fd, FILE_MODES[item.kind])
        os.fsync(fd)
    finally:
        os.close(fd)


def fsync_tree_and_freeze(root: Path) -> None:
    directories = sorted((path for path in root.rglob("*") if path.is_dir()), key=lambda path: len(path.parts), reverse=True)
    for directory in directories:
        directory.chmod(0o500)
        fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def remove_owned_tree(path: Path) -> None:
    try:
        info = os.lstat(path)
    except FileNotFoundError:
        return
    if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode) or info.st_uid != os.geteuid():
        fail("stale_stage_unsafe")
    path.chmod(0o700)
    for child in path.rglob("*"):
        if child.is_dir() and not child.is_symlink():
            child.chmod(0o700)
        elif child.is_symlink():
            child.unlink()
        else:
            child.chmod(0o600)
    shutil.rmtree(path)


def package_name(expected_git_sha: str) -> str:
    if not SHA1_RE.fullmatch(expected_git_sha):
        fail("git_sha_invalid")
    return f"{PACKAGE_PREFIX}{expected_git_sha}"


def manifest_for(created_at: str, git_sha: str, pinned: list[PinnedSource]) -> dict[str, Any]:
    return {
        "version": MANIFEST_VERSION,
        "created_at": created_at,
        "git_sha": git_sha,
        "artifacts": [
            {"path": item.destination, "source": item.source, "sha256": item.sha256, "type": item.kind}
            for item in sorted(pinned, key=lambda row: row.destination)
        ],
    }


def verify_package(package: Path, expected_git_sha: str) -> dict[str, Any]:
    inventory = load_inventory()
    try:
        root_fd = open_absolute_dir(package, exact_mode=0o500)
    except OSError as exc:
        raise PackageError("package_root_unsafe") from exc
    try:
        try:
            manifest_fd, manifest_info = open_relative_file(root_fd, MANIFEST_NAME)
        except OSError as exc:
            raise PackageError("manifest_missing_or_unsafe") from exc
        try:
            if (
                not stat.S_ISREG(manifest_info.st_mode)
                or manifest_info.st_nlink != 1
                or manifest_info.st_uid != os.geteuid()
                or stat.S_IMODE(manifest_info.st_mode) != 0o400
            ):
                fail("manifest_authority_invalid")
            manifest_sha, size = digest_fd(manifest_fd, MAX_MANIFEST_BYTES)
            os.lseek(manifest_fd, 0, os.SEEK_SET)
            raw = os.read(manifest_fd, size + 1)
        finally:
            os.close(manifest_fd)
        manifest = strict_json(raw)
        if canonical_json(manifest) != raw:
            fail("manifest_not_canonical")
        if not isinstance(manifest, dict) or set(manifest) != {"version", "created_at", "git_sha", "artifacts"}:
            fail("manifest_shape")
        if manifest["version"] != MANIFEST_VERSION or manifest["git_sha"] != expected_git_sha:
            fail("manifest_authority_mismatch")
        if not isinstance(manifest["created_at"], str) or not re.fullmatch(
            r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", manifest["created_at"]
        ):
            fail("manifest_time_invalid")
        rows = manifest["artifacts"]
        if not isinstance(rows, list) or len(rows) != len(inventory):
            fail("manifest_artifact_count")
        ordered_rows = sorted(
            rows, key=lambda row: row.get("path", "") if isinstance(row, dict) else ""
        )
        if rows != ordered_rows:
            fail("manifest_artifact_order")
        expected_files = {MANIFEST_NAME}
        seen_destinations: set[str] = set()
        seen_sources: set[str] = set()
        for row in rows:
            if not isinstance(row, dict) or set(row) != {"path", "source", "sha256", "type"}:
                fail("manifest_artifact_shape")
            destination, source, digest, kind = (row.get(key) for key in ("path", "source", "sha256", "type"))
            if not all(isinstance(value, str) for value in (destination, source, digest, kind)):
                fail("manifest_artifact_invalid")
            if inventory.get(destination) != (source, kind) or not SHA256_RE.fullmatch(digest):
                fail("manifest_artifact_authority_mismatch")
            if destination in seen_destinations or source in seen_sources:
                fail("manifest_artifact_duplicate")
            try:
                artifact_fd, artifact_info = open_relative_file(root_fd, source)
            except OSError as exc:
                raise PackageError("package_artifact_missing_or_unsafe") from exc
            try:
                if (
                    not stat.S_ISREG(artifact_info.st_mode)
                    or artifact_info.st_nlink != 1
                    or artifact_info.st_uid != os.geteuid()
                    or stat.S_IMODE(artifact_info.st_mode) != FILE_MODES[kind]
                ):
                    fail("package_artifact_authority_invalid")
                actual, _ = digest_fd(artifact_fd)
            finally:
                os.close(artifact_fd)
            if not hmac.compare_digest(actual, digest):
                fail("package_artifact_digest_mismatch")
            expected_files.add(source)
            seen_destinations.add(destination)
            seen_sources.add(source)
        if seen_destinations != set(inventory) or seen_sources != {value[0] for value in inventory.values()}:
            fail("manifest_required_artifact_missing")
    finally:
        os.close(root_fd)

    actual_files: set[str] = set()
    for path in package.rglob("*"):
        relative = path.relative_to(package).as_posix()
        info = os.lstat(path)
        if stat.S_ISLNK(info.st_mode):
            fail("package_symlink")
        if stat.S_ISREG(info.st_mode):
            actual_files.add(relative)
        elif stat.S_ISDIR(info.st_mode):
            if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) != 0o500:
                fail("package_directory_authority_invalid")
        else:
            fail("package_special_file")
    if actual_files != expected_files:
        fail("package_file_set_mismatch")
    return {
        "schema_version": SCHEMA_VERSION,
        "status": "VERIFIED",
        "git_sha": expected_git_sha,
        "manifest_sha256": manifest_sha,
        "artifact_count": len(inventory),
        "package_name": package.name,
    }


def build_package(
    source_root: Path,
    nats_server: Path,
    output_root: Path,
    stage_root: Path,
    expected_git_sha: str,
    *,
    failure_hook: Callable[[int], None] | None = None,
) -> dict[str, Any]:
    if not all(path.is_absolute() for path in (source_root, nats_server, output_root, stage_root)):
        fail("path_invalid")
    inventory = load_inventory()
    created_at = git_metadata(source_root, expected_git_sha)
    source_root_fd: int | None = None
    output_root_fd: int | None = None
    stage_root_fd: int | None = None
    lock_fd: int | None = None
    pinned: list[PinnedSource] = []
    operation = stage_root / f".build-{expected_git_sha}"
    final = output_root / package_name(expected_git_sha)
    renamed = False
    try:
        source_root_fd = open_absolute_dir(source_root)
        output_root_fd = open_absolute_dir(output_root, exact_mode=0o700)
        stage_root_fd = open_absolute_dir(stage_root, exact_mode=0o700)
        if os.fstat(output_root_fd).st_dev != os.fstat(stage_root_fd).st_dev:
            fail("roots_cross_device")
        lock_fd = os.open(
            ".m0-release.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600, dir_fd=stage_root_fd
        )
        lock_info = os.fstat(lock_fd)
        if (
            not stat.S_ISREG(lock_info.st_mode)
            or lock_info.st_nlink != 1
            or lock_info.st_uid != os.geteuid()
            or stat.S_IMODE(lock_info.st_mode) != 0o600
        ):
            fail("stage_lock_invalid")
        try:
            fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise PackageError("package_build_active") from exc

        remove_owned_tree(operation)
        if final.exists() or final.is_symlink():
            try:
                result = verify_package(final, expected_git_sha)
            except (OSError, PackageError) as exc:
                raise PackageError("stale_output_conflict") from exc
            result["status"] = "REUSED"
            return result
        operation.mkdir(mode=0o700)

        nats_lexical = Path(os.path.abspath(nats_server))
        try:
            nats_lexical.relative_to(source_root)
        except ValueError:
            pass
        else:
            fail("nats_not_separate")

        for destination, (source, kind) in sorted(inventory.items()):
            try:
                if source == "external/nats-server":
                    fd, info = open_absolute_file(nats_server)
                else:
                    fd, info = open_relative_file(source_root_fd, source)
            except OSError as exc:
                raise PackageError("source_missing_or_unsafe") from exc
            pinned.append(pin_source(fd, info, source, destination, kind))

        manifest = manifest_for(created_at, expected_git_sha, pinned)
        manifest_raw = canonical_json(manifest)
        for index, item in enumerate(pinned, start=1):
            write_pinned(item, operation / item.source)
            if failure_hook is not None:
                failure_hook(index)

        manifest_path = operation / MANIFEST_NAME
        manifest_fd = os.open(
            manifest_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o400
        )
        try:
            os.write(manifest_fd, manifest_raw)
            os.fsync(manifest_fd)
        finally:
            os.close(manifest_fd)

        for item in pinned:
            descriptor_info = os.fstat(item.fd)
            descriptor_identity = (
                descriptor_info.st_dev,
                descriptor_info.st_ino,
                descriptor_info.st_size,
                descriptor_info.st_mtime_ns,
                stat.S_IMODE(descriptor_info.st_mode),
            )
            descriptor_digest, _ = digest_fd(item.fd)
            if descriptor_identity != item.identity or not hmac.compare_digest(descriptor_digest, item.sha256):
                fail("source_changed")
            try:
                if item.source == "external/nats-server":
                    current = current_absolute_identity(nats_server)
                else:
                    current = current_source_identity(source_root_fd, item)
            except OSError as exc:
                raise PackageError("source_changed") from exc
            if current != item.identity:
                fail("source_changed")

        fsync_tree_and_freeze(operation)
        os.rename(operation.name, final.name, src_dir_fd=stage_root_fd, dst_dir_fd=output_root_fd)
        renamed = True
        final.chmod(0o500)
        final_fd = os.open(final, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            os.fsync(final_fd)
        finally:
            os.close(final_fd)
        os.fsync(stage_root_fd)
        os.fsync(output_root_fd)
        result = verify_package(final, expected_git_sha)
        result["status"] = "COMPLETE"
        return result
    except PackageError:
        if renamed:
            remove_owned_tree(final)
            if output_root_fd is not None:
                os.fsync(output_root_fd)
        elif stage_root_fd is not None:
            remove_owned_tree(operation)
        raise
    except Exception as exc:
        if not renamed and stage_root_fd is not None:
            try:
                remove_owned_tree(operation)
            except Exception:
                pass
        raise PackageError("internal_failure") from exc
    finally:
        for item in pinned:
            item.close()
        if lock_fd is not None:
            try:
                fcntl.flock(lock_fd, fcntl.LOCK_UN)
            except OSError:
                pass
            os.close(lock_fd)
        for fd in (source_root_fd, output_root_fd, stage_root_fd):
            if fd is not None:
                os.close(fd)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    build = subparsers.add_parser("build")
    build.add_argument("--source-root", type=Path, required=True)
    build.add_argument("--nats-server", type=Path, required=True)
    build.add_argument("--output-root", type=Path, required=True)
    build.add_argument("--stage-root", type=Path, required=True)
    build.add_argument("--expected-git-sha", required=True)
    verify = subparsers.add_parser("verify")
    verify.add_argument("--package", type=Path, required=True)
    verify.add_argument("--expected-git-sha", required=True)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        if args.command == "build":
            result = build_package(
                args.source_root,
                args.nats_server,
                args.output_root,
                args.stage_root,
                args.expected_git_sha,
            )
        else:
            result = verify_package(args.package, args.expected_git_sha)
    except Exception as exc:
        reason = str(exc) if isinstance(exc, PackageError) else "internal_failure"
        result = {"schema_version": SCHEMA_VERSION, "status": "FAIL", "reason": reason}
        sys.stderr.buffer.write(canonical_json(result))
        return 1
    sys.stdout.buffer.write(canonical_json(result))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
