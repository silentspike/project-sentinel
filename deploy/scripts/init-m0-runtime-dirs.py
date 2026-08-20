#!/usr/bin/env python3
"""Create the private persistent directories required before M0 services start."""

from __future__ import annotations

import os
from pathlib import Path, PurePosixPath
import pwd
import grp
import stat
import sys


GAIA_CONSOLE = PurePosixPath("/opt/sentinel/data/gaia-console")
GAIA_SESSIONS = GAIA_CONSOLE / "sessions"
DASHBOARD_CERTS = PurePosixPath("/opt/sentinel/data/dashboard-cert")
WORKBENCH_STORE = PurePosixPath("/opt/sentinel/data/company-workbench")
TEST_ROOT_ENV = "SENTINEL_M0_RUNTIME_DIRS_TEST_ROOT"


class InitError(RuntimeError):
    pass


def fail(reason: str) -> None:
    raise InitError(reason)


def open_directory(parent_fd: int, name: str) -> int:
    try:
        return os.open(
            name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
            dir_fd=parent_fd,
        )
    except OSError as exc:
        raise InitError("runtime_directory_unsafe") from exc


def validate_directory(
    fd: int,
    *,
    allowed_uids: set[int],
    allowed_gids: set[int],
    exact_mode: int | None = None,
) -> None:
    info = os.fstat(fd)
    if not stat.S_ISDIR(info.st_mode):
        fail("runtime_directory_unsafe")
    if info.st_uid not in allowed_uids or info.st_gid not in allowed_gids:
        fail("runtime_directory_owner_invalid")
    mode = stat.S_IMODE(info.st_mode)
    if mode & 0o022:
        fail("runtime_directory_mode_invalid")
    if exact_mode is not None and mode != exact_mode:
        fail("runtime_directory_mode_invalid")


def ensure_private_directory(parent_fd: int, name: str, uid: int, gid: int) -> int:
    created = False
    try:
        os.mkdir(name, 0o700, dir_fd=parent_fd)
        created = True
    except FileExistsError:
        pass
    fd = open_directory(parent_fd, name)
    try:
        if created:
            os.fchown(fd, uid, gid)
            os.fchmod(fd, 0o700)
            os.fsync(fd)
            os.fsync(parent_fd)
        validate_directory(
            fd,
            allowed_uids={uid},
            allowed_gids={gid},
            exact_mode=0o700,
        )
        return fd
    except BaseException:
        os.close(fd)
        raise


def normalize_data_directory(fd: int, uid: int, gid: int) -> None:
    info = os.fstat(fd)
    mode = stat.S_IMODE(info.st_mode)
    if not stat.S_ISDIR(info.st_mode):
        fail("runtime_directory_unsafe")
    if info.st_uid != uid or info.st_gid != gid:
        fail("runtime_directory_owner_invalid")
    if info.st_mode & (stat.S_ISUID | stat.S_ISGID | stat.S_ISVTX) or mode & 0o002:
        fail("runtime_directory_mode_invalid")
    if mode != 0o750:
        os.fchmod(fd, 0o750)
        os.fsync(fd)
    validate_directory(
        fd,
        allowed_uids={uid},
        allowed_gids={gid},
        exact_mode=0o750,
    )


def test_root() -> Path | None:
    raw = os.environ.get(TEST_ROOT_ENV)
    if raw is None:
        return None
    pure = PurePosixPath(raw)
    root = Path(raw)
    if (
        not root.is_absolute()
        or pure.parts in {(), ("/",)}
        or any(component in {".", ".."} for component in pure.parts)
        or str(pure) != raw
    ):
        fail("test_root_invalid")
    return root


def initialize() -> None:
    root = test_root()
    if root is None:
        if os.geteuid() != 0:
            fail("root_required")
        root_path = Path("/")
        target_uid = pwd.getpwnam("ubuntu").pw_uid
        target_gid = grp.getgrnam("ubuntu").gr_gid
        authority_uid = 0
        authority_gid = 0
        parent_uids = {0, target_uid}
        parent_gids = {0, target_gid}
    else:
        root_path = root
        target_uid = os.geteuid()
        target_gid = os.getegid()
        authority_uid = target_uid
        authority_gid = target_gid
        parent_uids = {target_uid}
        parent_gids = {target_gid}

    try:
        root_fd = os.open(
            root_path,
            os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
        )
    except OSError as exc:
        raise InitError("runtime_root_unsafe") from exc
    try:
        validate_directory(
            root_fd,
            allowed_uids=parent_uids,
            allowed_gids=parent_gids,
            exact_mode=0o700 if root is not None else None,
        )
        current_fd = os.dup(root_fd)
        try:
            for component in GAIA_CONSOLE.parts[1:-1]:
                next_fd = open_directory(current_fd, component)
                os.close(current_fd)
                current_fd = next_fd
                if component == "data":
                    normalize_data_directory(current_fd, target_uid, target_gid)
                else:
                    validate_directory(
                        current_fd,
                        allowed_uids=parent_uids,
                        allowed_gids=parent_gids,
                    )
            gaia_fd = ensure_private_directory(
                current_fd, GAIA_CONSOLE.name, target_uid, target_gid
            )
            try:
                sessions_fd = ensure_private_directory(
                    gaia_fd, GAIA_SESSIONS.name, target_uid, target_gid
                )
                os.close(sessions_fd)
            finally:
                os.close(gaia_fd)
            dashboard_certs_fd = ensure_private_directory(
                current_fd, DASHBOARD_CERTS.name, target_uid, target_gid
            )
            os.close(dashboard_certs_fd)
            workbench_fd = ensure_private_directory(
                current_fd, WORKBENCH_STORE.name, authority_uid, authority_gid
            )
            os.close(workbench_fd)
        finally:
            os.close(current_fd)
    finally:
        os.close(root_fd)


def main() -> int:
    try:
        initialize()
    except (InitError, KeyError, OSError) as exc:
        if isinstance(exc, InitError):
            reason = str(exc)
        elif isinstance(exc, KeyError):
            reason = "runtime_identity_missing"
        else:
            reason = "runtime_directory_operation_failed"
        print(f"ERROR: {reason}", file=sys.stderr)
        return 1
    print("m0_runtime_dirs=verified directories=4 mode=0700")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
