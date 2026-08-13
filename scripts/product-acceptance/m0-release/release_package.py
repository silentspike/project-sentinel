#!/usr/bin/env python3
"""Build and verify the owner- and mode-sensitive single-node M0 release package."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import errno
import fcntl
import hashlib
import hmac
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys
import types
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
GIT_BINARY = Path("/usr/bin/git")
INVENTORY_RELATIVE = "scripts/product-acceptance/run_m0_preflight.py"
TOOL_INVENTORY_PATH = Path(__file__).resolve().parent.parent / "run_m0_preflight.py"
TRANSPORT_NOTICE = (
    "The package is an owner- and mode-sensitive directory tree, not a generic copy-ready archive. "
    "Transport must preserve modes, regular-file identities, and hardlink counts. Ownership must either "
    "remain identical to the later executor, or an already verified whole tree must be remapped in one "
    "closed root-controlled staging step to that executor. Mixed ownership and partial remaps are invalid. "
    "After transport or remap, run verify as exactly that executor before provisioning."
)


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


def read_bounded_fd(fd: int, maximum: int) -> bytes:
    os.lseek(fd, 0, os.SEEK_SET)
    chunks: list[bytes] = []
    size = 0
    while True:
        chunk = os.read(fd, min(1024 * 1024, maximum + 1 - size))
        if not chunk:
            break
        size += len(chunk)
        if size > maximum:
            fail("inventory_authority_oversized")
        chunks.append(chunk)
    os.lseek(fd, 0, os.SEEK_SET)
    return b"".join(chunks)


def inventory_from_bytes(raw: bytes) -> dict[str, tuple[str, str]]:
    digest = hashlib.sha256(raw).hexdigest()
    module_name = f"m0_release_inventory_{digest}"
    module = types.ModuleType(module_name)
    module.__file__ = f"<pinned-source:{INVENTORY_RELATIVE}>"
    sys.modules[module_name] = module
    try:
        code = compile(raw, module.__file__, "exec", dont_inherit=True)
        exec(code, module.__dict__)
    except Exception as exc:
        raise PackageError("inventory_unavailable") from exc
    finally:
        sys.modules.pop(module_name, None)
    value = module.__dict__.get("CANONICAL_RELEASE_ARTIFACTS")
    if not isinstance(value, dict):
        fail("inventory_invalid")
    return validate_inventory(value)


def read_inventory_authority(fd: int, info: os.stat_result) -> bytes:
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_nlink != 1
        or info.st_uid != os.geteuid()
        or stat.S_IMODE(info.st_mode) & 0o022
        or info.st_mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX)
    ):
        fail("inventory_authority_invalid")
    raw = read_bounded_fd(fd, MAX_MANIFEST_BYTES)
    if len(raw) != info.st_size or stable_stat(os.fstat(fd)) != stable_stat(info):
        fail("inventory_authority_changed")
    return raw


def load_tool_inventory() -> dict[str, tuple[str, str]]:
    try:
        fd, info = open_absolute_file(TOOL_INVENTORY_PATH)
    except OSError as exc:
        raise PackageError("inventory_unavailable") from exc
    try:
        return inventory_from_bytes(read_inventory_authority(fd, info))
    finally:
        os.close(fd)


def load_source_inventory(source_root_fd: int) -> dict[str, tuple[str, str]]:
    try:
        source_fd, source_info = open_relative_file(source_root_fd, INVENTORY_RELATIVE)
        tool_fd, tool_info = open_absolute_file(TOOL_INVENTORY_PATH)
    except OSError as exc:
        raise PackageError("inventory_unavailable") from exc
    try:
        source_raw = read_inventory_authority(source_fd, source_info)
        tool_raw = read_inventory_authority(tool_fd, tool_info)
        if not hmac.compare_digest(hashlib.sha256(source_raw).digest(), hashlib.sha256(tool_raw).digest()):
            fail("inventory_authority_mismatch")
        return inventory_from_bytes(source_raw)
    finally:
        os.close(source_fd)
        os.close(tool_fd)


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


def directory_identity(info: os.stat_result) -> tuple[int, int]:
    return (info.st_dev, info.st_ino)


def assert_absolute_directory_identity(
    path: Path, expected: tuple[int, int], error_code: str
) -> None:
    try:
        fd = open_absolute_dir(path)
    except (OSError, PackageError) as exc:
        raise PackageError(error_code) from exc
    try:
        if directory_identity(os.fstat(fd)) != expected:
            fail(error_code)
    finally:
        os.close(fd)


def git_metadata(source_root_fd: int, expected_git_sha: str) -> str:
    if not SHA1_RE.fullmatch(expected_git_sha):
        fail("git_sha_invalid")
    try:
        binary_link = os.lstat(GIT_BINARY)
        binary = os.stat(GIT_BINARY)
    except OSError as exc:
        raise PackageError("git_authority_unavailable") from exc
    if (
        not (stat.S_ISLNK(binary_link.st_mode) or stat.S_ISREG(binary_link.st_mode))
        or binary_link.st_uid != 0
        or not stat.S_ISREG(binary.st_mode)
        or binary.st_uid != 0
        or binary.st_mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX | 0o022)
    ):
        fail("git_binary_authority_invalid")
    environment = {
        "PATH": "/usr/bin:/bin",
        "HOME": "/nonexistent",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_COUNT": "2",
        "GIT_CONFIG_KEY_0": "core.fsmonitor",
        "GIT_CONFIG_VALUE_0": "false",
        "GIT_CONFIG_KEY_1": "core.hooksPath",
        "GIT_CONFIG_VALUE_1": "/dev/null",
        "GIT_NO_REPLACE_OBJECTS": "1",
        "GIT_OPTIONAL_LOCKS": "0",
        "LC_ALL": "C",
    }
    descriptor_path = f"/proc/self/fd/{source_root_fd}"
    common = {
        "stdin": subprocess.DEVNULL,
        "stdout": subprocess.PIPE,
        "stderr": subprocess.DEVNULL,
        "timeout": 10,
        "check": False,
        "env": environment,
        "pass_fds": (source_root_fd,),
    }
    command_prefix = [
        str(GIT_BINARY),
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-C",
        descriptor_path,
    ]
    try:
        head = subprocess.run(
            [*command_prefix, "rev-parse", "--verify", "HEAD^{commit}"],
            **common,
        )
        dirty = subprocess.run(
            [*command_prefix, "status", "--porcelain", "--untracked-files=no"],
            **common,
        )
        timestamp = subprocess.run(
            [*command_prefix, "show", "-s", "--format=%ct", expected_git_sha],
            **common,
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


def ensure_parent_at(root_fd: int, relative: str) -> tuple[int, str]:
    parts = safe_parts(relative, absolute=False)
    current = os.dup(root_fd)
    try:
        for part in parts[:-1]:
            try:
                os.mkdir(part, 0o700, dir_fd=current)
            except FileExistsError:
                pass
            next_fd = os.open(
                part,
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                dir_fd=current,
            )
            os.close(current)
            current = next_fd
            validate_dir(os.fstat(current), exact_mode=0o700, exact_owner=True)
        return current, parts[-1]
    except Exception:
        os.close(current)
        raise


def write_pinned_at(root_fd: int, item: PinnedSource) -> None:
    parent_fd, leaf = ensure_parent_at(root_fd, item.source)
    fd = os.open(
        leaf,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
        0o600,
        dir_fd=parent_fd,
    )
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
        os.close(parent_fd)


def write_bytes_at(root_fd: int, relative: str, raw: bytes, mode: int) -> None:
    parent_fd, leaf = ensure_parent_at(root_fd, relative)
    fd = os.open(
        leaf,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
        0o600,
        dir_fd=parent_fd,
    )
    try:
        view = memoryview(raw)
        while view:
            written = os.write(fd, view)
            view = view[written:]
        os.fchmod(fd, mode)
        os.fsync(fd)
    finally:
        os.close(fd)
        os.close(parent_fd)


def open_entry_at(parent_fd: int, name: str) -> tuple[int, os.stat_result]:
    if not name or name in {".", ".."} or "/" in name or "\x00" in name:
        fail("package_entry_invalid")
    try:
        fd = os.open(
            name,
            os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC,
            dir_fd=parent_fd,
        )
    except OSError as exc:
        if exc.errno == errno.ELOOP:
            raise PackageError("package_symlink") from exc
        raise
    return fd, os.fstat(fd)


def directory_names(directory_fd: int) -> list[str]:
    scan_fd = os.open(
        ".",
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
        dir_fd=directory_fd,
    )
    try:
        return sorted(os.listdir(scan_fd))
    finally:
        os.close(scan_fd)


def fsync_tree_and_freeze_fd(root_fd: int, *, freeze_root: bool) -> None:
    for name in directory_names(root_fd):
        child_fd, info = open_entry_at(root_fd, name)
        try:
            if stat.S_ISDIR(info.st_mode):
                fsync_tree_and_freeze_fd(child_fd, freeze_root=True)
            elif not stat.S_ISREG(info.st_mode):
                fail("package_special_file")
        finally:
            os.close(child_fd)
    if freeze_root:
        os.fchmod(root_fd, 0o500)
    os.fsync(root_fd)


def inode_identity(info: os.stat_result) -> tuple[int, int, int]:
    return (info.st_dev, info.st_ino, stat.S_IFMT(info.st_mode))


def open_path_entry_at(parent_fd: int, name: str) -> tuple[int, os.stat_result]:
    try:
        fd = os.open(name, os.O_PATH | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent_fd)
    except FileNotFoundError:
        raise
    return fd, os.fstat(fd)


def name_matches_identity(parent_fd: int, name: str, expected: tuple[int, int, int]) -> bool:
    try:
        fd, info = open_path_entry_at(parent_fd, name)
    except FileNotFoundError:
        return False
    try:
        return inode_identity(info) == expected
    finally:
        os.close(fd)


def remove_tree_at(
    parent_fd: int,
    name: str,
    *,
    expected_identity: tuple[int, int, int] | None = None,
    missing_ok: bool = True,
) -> None:
    try:
        path_fd, path_info = open_path_entry_at(parent_fd, name)
    except FileNotFoundError:
        if missing_ok:
            return
        fail("cleanup_missing")
    try:
        actual_identity = inode_identity(path_info)
        if expected_identity is not None and actual_identity != expected_identity:
            fail("cleanup_identity_mismatch")
        if not stat.S_ISDIR(path_info.st_mode) or path_info.st_uid != os.geteuid():
            fail("cleanup_authority_invalid")
        directory_fd = os.open(
            name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
            dir_fd=parent_fd,
        )
        try:
            if inode_identity(os.fstat(directory_fd)) != actual_identity:
                fail("cleanup_identity_mismatch")
            os.fchmod(directory_fd, 0o700)
            for child_name in directory_names(directory_fd):
                child_path_fd, child_info = open_path_entry_at(directory_fd, child_name)
                child_identity = inode_identity(child_info)
                os.close(child_path_fd)
                if stat.S_ISDIR(child_info.st_mode):
                    remove_tree_at(
                        directory_fd,
                        child_name,
                        expected_identity=child_identity,
                        missing_ok=False,
                    )
                else:
                    if not name_matches_identity(directory_fd, child_name, child_identity):
                        fail("cleanup_identity_mismatch")
                    os.unlink(child_name, dir_fd=directory_fd)
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
        if not name_matches_identity(parent_fd, name, actual_identity):
            fail("cleanup_identity_mismatch")
        os.rmdir(name, dir_fd=parent_fd)
        os.fsync(parent_fd)
    finally:
        os.close(path_fd)


def remove_owned_tree(path: Path) -> None:
    if not path.is_absolute():
        fail("path_invalid")
    parent_fd = open_absolute_dir(path.parent)
    try:
        remove_tree_at(parent_fd, path.name)
    finally:
        os.close(parent_fd)


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


@dataclass
class TreeFile:
    fd: int
    info: os.stat_result

    def close(self) -> None:
        os.close(self.fd)


def stable_stat(info: os.stat_result) -> tuple[int, ...]:
    return (
        info.st_dev,
        info.st_ino,
        stat.S_IFMT(info.st_mode),
        stat.S_IMODE(info.st_mode),
        info.st_uid,
        info.st_gid,
        info.st_nlink,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
    )


def enumerate_tree(root_fd: int) -> tuple[dict[str, TreeFile], dict[str, tuple[int, ...]]]:
    files: dict[str, TreeFile] = {}
    directories: dict[str, tuple[int, ...]] = {}

    def visit(directory_fd: int, prefix: str) -> None:
        before = stable_stat(os.fstat(directory_fd))
        for name in directory_names(directory_fd):
            try:
                entry_fd, info = open_entry_at(directory_fd, name)
            except OSError as exc:
                raise PackageError("package_entry_unsafe") from exc
            relative = f"{prefix}/{name}" if prefix else name
            if stat.S_ISDIR(info.st_mode):
                try:
                    if info.st_uid != os.geteuid() or stat.S_IMODE(info.st_mode) != 0o500:
                        fail("package_directory_authority_invalid")
                    visit(entry_fd, relative)
                finally:
                    os.close(entry_fd)
            elif stat.S_ISREG(info.st_mode):
                files[relative] = TreeFile(entry_fd, info)
            else:
                os.close(entry_fd)
                fail("package_special_file")
        after = stable_stat(os.fstat(directory_fd))
        if before != after:
            fail("package_tree_changed")
        directories[prefix] = after

    try:
        visit(root_fd, "")
        return files, directories
    except Exception:
        for item in files.values():
            item.close()
        raise


def read_fd(fd: int, size: int) -> bytes:
    os.lseek(fd, 0, os.SEEK_SET)
    chunks: list[bytes] = []
    remaining = size
    while remaining:
        chunk = os.read(fd, remaining)
        if not chunk:
            fail("file_truncated")
        chunks.append(chunk)
        remaining -= len(chunk)
    if os.read(fd, 1):
        fail("file_grew")
    os.lseek(fd, 0, os.SEEK_SET)
    return b"".join(chunks)


def verify_package_fd(root_fd: int, expected_git_sha: str) -> dict[str, Any]:
    inventory = load_tool_inventory()
    validate_dir(os.fstat(root_fd), exact_mode=0o500, exact_owner=True)
    files, directory_snapshot = enumerate_tree(root_fd)
    try:
        manifest_file = files.get(MANIFEST_NAME)
        if manifest_file is None:
            fail("manifest_missing_or_unsafe")
        manifest_info = manifest_file.info
        if (
            manifest_info.st_nlink != 1
            or manifest_info.st_uid != os.geteuid()
            or stat.S_IMODE(manifest_info.st_mode) != 0o400
        ):
            fail("manifest_authority_invalid")
        manifest_sha, size = digest_fd(manifest_file.fd, MAX_MANIFEST_BYTES)
        raw = read_fd(manifest_file.fd, size)
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
            artifact_file = files.get(source)
            if artifact_file is None:
                fail("package_artifact_missing_or_unsafe")
            artifact_info = artifact_file.info
            if (
                artifact_info.st_nlink != 1
                or artifact_info.st_uid != os.geteuid()
                or stat.S_IMODE(artifact_info.st_mode) != FILE_MODES[kind]
            ):
                fail("package_artifact_authority_invalid")
            actual, _ = digest_fd(artifact_file.fd)
            if stable_stat(os.fstat(artifact_file.fd)) != stable_stat(artifact_info):
                fail("package_tree_changed")
            if not hmac.compare_digest(actual, digest):
                fail("package_artifact_digest_mismatch")
            expected_files.add(source)
            seen_destinations.add(destination)
            seen_sources.add(source)
        if seen_destinations != set(inventory) or seen_sources != {value[0] for value in inventory.values()}:
            fail("manifest_required_artifact_missing")
        if set(files) != expected_files:
            fail("package_file_set_mismatch")
        final_files, final_directories = enumerate_tree(root_fd)
        try:
            if final_directories != directory_snapshot or {
                path: stable_stat(item.info) for path, item in final_files.items()
            } != {path: stable_stat(item.info) for path, item in files.items()}:
                fail("package_tree_changed")
        finally:
            for item in final_files.values():
                item.close()
        return {
            "schema_version": SCHEMA_VERSION,
            "status": "VERIFIED",
            "git_sha": expected_git_sha,
            "manifest_sha256": manifest_sha,
            "artifact_count": len(inventory),
            "package_name": package_name(expected_git_sha),
        }
    finally:
        for item in files.values():
            item.close()


def verify_package(
    package: Path,
    expected_git_sha: str,
    *,
    after_root_pin: Callable[[], None] | None = None,
) -> dict[str, Any]:
    try:
        root_fd = open_absolute_dir(package, exact_mode=0o500)
    except (OSError, PackageError) as exc:
        raise PackageError("package_root_unsafe") from exc
    try:
        if after_root_pin is not None:
            after_root_pin()
        return verify_package_fd(root_fd, expected_git_sha)
    finally:
        os.close(root_fd)


def build_package(
    source_root: Path,
    nats_server: Path,
    output_root: Path,
    stage_root: Path,
    expected_git_sha: str,
    *,
    failure_hook: Callable[[int], None] | None = None,
    after_git_hook: Callable[[], None] | None = None,
    after_rename_hook: Callable[[], None] | None = None,
    before_final_verify_hook: Callable[[], None] | None = None,
) -> dict[str, Any]:
    if not all(path.is_absolute() for path in (source_root, nats_server, output_root, stage_root)):
        fail("path_invalid")
    source_root_fd: int | None = None
    output_root_fd: int | None = None
    stage_root_fd: int | None = None
    operation_fd: int | None = None
    final_fd: int | None = None
    lock_fd: int | None = None
    pinned: list[PinnedSource] = []
    operation_name = f".build-{expected_git_sha}"
    final_name = package_name(expected_git_sha)
    operation_identity: tuple[int, int, int] | None = None
    renamed = False
    try:
        source_root_fd = open_absolute_dir(source_root)
        source_root_identity = directory_identity(os.fstat(source_root_fd))
        created_at = git_metadata(source_root_fd, expected_git_sha)
        if after_git_hook is not None:
            after_git_hook()
        assert_absolute_directory_identity(source_root, source_root_identity, "source_root_changed")
        inventory = load_source_inventory(source_root_fd)

        output_root_fd = open_absolute_dir(output_root, exact_mode=0o700)
        stage_root_fd = open_absolute_dir(stage_root, exact_mode=0o700)
        output_root_identity = directory_identity(os.fstat(output_root_fd))
        stage_root_identity = directory_identity(os.fstat(stage_root_fd))
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
        expected_manifest_sha = hashlib.sha256(manifest_raw).hexdigest()

        remove_tree_at(stage_root_fd, operation_name)
        try:
            existing_fd, existing_info = open_entry_at(output_root_fd, final_name)
        except FileNotFoundError:
            existing_fd = None
        except (OSError, PackageError) as exc:
            raise PackageError("stale_output_conflict") from exc
        if existing_fd is not None:
            try:
                if not stat.S_ISDIR(existing_info.st_mode):
                    fail("stale_output_conflict")
                try:
                    result = verify_package_fd(existing_fd, expected_git_sha)
                except PackageError as exc:
                    raise PackageError("stale_output_conflict") from exc
                if not hmac.compare_digest(result["manifest_sha256"], expected_manifest_sha):
                    fail("stale_output_conflict")
                result["status"] = "REUSED"
                return result
            finally:
                os.close(existing_fd)

        os.mkdir(operation_name, 0o700, dir_fd=stage_root_fd)
        operation_fd, operation_info = open_entry_at(stage_root_fd, operation_name)
        validate_dir(operation_info, exact_mode=0o700, exact_owner=True)
        operation_identity = inode_identity(operation_info)

        for index, item in enumerate(pinned, start=1):
            write_pinned_at(operation_fd, item)
            if failure_hook is not None:
                failure_hook(index)

        write_bytes_at(operation_fd, MANIFEST_NAME, manifest_raw, 0o400)

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

        assert_absolute_directory_identity(source_root, source_root_identity, "source_root_changed")
        fsync_tree_and_freeze_fd(operation_fd, freeze_root=False)
        os.rename(operation_name, final_name, src_dir_fd=stage_root_fd, dst_dir_fd=output_root_fd)
        renamed = True
        if after_rename_hook is not None:
            after_rename_hook()
        try:
            final_fd, final_info = open_entry_at(output_root_fd, final_name)
        except (OSError, PackageError) as exc:
            raise PackageError("final_identity_changed") from exc
        if not stat.S_ISDIR(final_info.st_mode) or inode_identity(final_info) != operation_identity:
            fail("final_identity_changed")
        os.fchmod(final_fd, 0o500)
        os.fsync(final_fd)
        if before_final_verify_hook is not None:
            before_final_verify_hook()
        result = verify_package_fd(final_fd, expected_git_sha)
        if not name_matches_identity(output_root_fd, final_name, operation_identity):
            fail("final_identity_changed")
        assert_absolute_directory_identity(output_root, output_root_identity, "output_root_changed")
        assert_absolute_directory_identity(stage_root, stage_root_identity, "stage_root_changed")
        os.fsync(stage_root_fd)
        os.fsync(output_root_fd)
        result["status"] = "COMPLETE"
        return result
    except PackageError as exc:
        cleanup_parent = output_root_fd if renamed else stage_root_fd
        cleanup_name = final_name if renamed else operation_name
        if cleanup_parent is not None and operation_identity is not None:
            try:
                remove_tree_at(
                    cleanup_parent,
                    cleanup_name,
                    expected_identity=operation_identity,
                )
            except PackageError as cleanup_exc:
                if str(cleanup_exc) == "cleanup_identity_mismatch":
                    raise PackageError("final_identity_changed") from exc
                raise
        raise
    except Exception as exc:
        cleanup_parent = output_root_fd if renamed else stage_root_fd
        cleanup_name = final_name if renamed else operation_name
        if cleanup_parent is not None and operation_identity is not None:
            try:
                remove_tree_at(
                    cleanup_parent,
                    cleanup_name,
                    expected_identity=operation_identity,
                )
            except PackageError as cleanup_exc:
                if str(cleanup_exc) == "cleanup_identity_mismatch":
                    raise PackageError("final_identity_changed") from exc
                raise
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
        for fd in (final_fd, operation_fd, source_root_fd, output_root_fd, stage_root_fd):
            if fd is not None:
                os.close(fd)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=__doc__,
        epilog=TRANSPORT_NOTICE,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    build = subparsers.add_parser(
        "build",
        description="Build a local owner- and mode-sensitive package tree.",
        epilog=TRANSPORT_NOTICE,
    )
    build.add_argument("--source-root", type=Path, required=True)
    build.add_argument("--nats-server", type=Path, required=True)
    build.add_argument("--output-root", type=Path, required=True)
    build.add_argument("--stage-root", type=Path, required=True)
    build.add_argument("--expected-git-sha", required=True)
    verify = subparsers.add_parser(
        "verify",
        description="Reverify ownership, modes, identities, inventory, and bytes after transport.",
        epilog=TRANSPORT_NOTICE,
    )
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
