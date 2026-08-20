#!/usr/bin/env python3
"""Deterministic, network-free integrity gate for one sealed work item."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import sys


MAX_FILES = 64
MAX_FILE_BYTES = 8 * 1024 * 1024


def fail(code: str, detail: str) -> None:
    print(json.dumps({
        "schema_version": 1,
        "outcome": "fail",
        "code": code,
        "detail_sha256": hashlib.sha256(detail.encode()).hexdigest(),
    }, sort_keys=True, separators=(",", ":")))
    raise SystemExit(1)


def main() -> None:
    if not 2 <= len(sys.argv) <= MAX_FILES + 1:
        fail("arguments_denied", "runner requires one to sixty-four declared inputs")
    paths = [Path(value) for value in sys.argv[1:]]
    resolved = [path.resolve(strict=True) for path in paths]
    if len(set(resolved)) != len(resolved):
        fail("input_inventory_invalid", "duplicate input")
    inventory: list[str] = []
    total_bytes = 0
    for supplied, path in zip(paths, resolved, strict=True):
        stat = path.stat()
        if supplied.is_symlink() or not os.path.isfile(path):
            fail("input_file_invalid", str(supplied))
        if stat.st_mode & 0o222 or stat.st_size <= 0 or stat.st_size > MAX_FILE_BYTES:
            fail("input_file_contract", str(supplied))
        with path.open("rb") as handle:
            digest = hashlib.file_digest(handle, "sha256").hexdigest()
        total_bytes += stat.st_size
        inventory.append(f"{supplied.name}:{stat.st_size}:{digest}")
    encoded = "\n".join(sorted(inventory)).encode()
    print(json.dumps({
        "schema_version": 1,
        "outcome": "pass",
        "suite_id": "web-work-item-qa-v1",
        "files": len(paths),
        "bytes": total_bytes,
        "inventory_sha256": hashlib.sha256(encoded).hexdigest(),
    }, sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
