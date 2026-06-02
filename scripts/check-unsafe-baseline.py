#!/usr/bin/env python3
"""Check first-party Rust unsafe usage against the documented baseline."""

from __future__ import annotations

import json
import re
import sys
from collections import Counter
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BASELINE = ROOT / "docs" / "security" / "unsafe-baseline.json"
UNSAFE_RE = re.compile(r"\bunsafe\s*(?:\{|fn\b|impl\b|trait\b)")
SAFETY_RE = re.compile(r"\bSAFETY:")


def is_excluded(path: Path, excludes: list[str]) -> bool:
    rel = path.relative_to(ROOT).as_posix()
    return any(rel.startswith(prefix.rstrip("/") + "/") for prefix in excludes)


def rust_sources(excludes: list[str]) -> list[Path]:
    sources: list[Path] = []
    for root_name in ("crates", "services"):
        root = ROOT / root_name
        if not root.exists():
            continue
        for path in root.rglob("*.rs"):
            if not is_excluded(path, excludes):
                sources.append(path)
    return sorted(sources)


def has_nearby_safety(lines: list[str], index: int) -> bool:
    window_start = max(0, index - 4)
    return any(SAFETY_RE.search(line) for line in lines[window_start : index + 1])


def main() -> int:
    baseline = json.loads(BASELINE.read_text())
    excludes = baseline.get("exclude_prefixes", [])
    max_unsafe = int(baseline["max_unsafe_constructs"])

    matches: list[tuple[str, int, str]] = []
    missing_safety: list[tuple[str, int, str]] = []

    for path in rust_sources(excludes):
        rel = path.relative_to(ROOT).as_posix()
        lines = path.read_text(encoding="utf-8").splitlines()
        for index, line in enumerate(lines):
            if UNSAFE_RE.search(line):
                entry = (rel, index + 1, line.strip())
                matches.append(entry)
                if not has_nearby_safety(lines, index):
                    missing_safety.append(entry)

    print(f"unsafe constructs: {len(matches)} / baseline {max_unsafe}")
    for rel, count in sorted(Counter(path for path, _, _ in matches).items()):
        print(f"  {rel}: {count}")

    if missing_safety:
        print("missing SAFETY comments:", file=sys.stderr)
        for rel, line_no, text in missing_safety:
            print(f"  {rel}:{line_no}: {text}", file=sys.stderr)
        return 1

    if len(matches) > max_unsafe:
        print(
            f"unsafe construct count {len(matches)} exceeds baseline {max_unsafe}",
            file=sys.stderr,
        )
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
