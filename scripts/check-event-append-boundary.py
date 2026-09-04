#!/usr/bin/env python3
"""Fail closed when production code bypasses the canonical event append boundary."""

from __future__ import annotations

import argparse
import re
from pathlib import Path


RAW_RUST_METHODS = (
    "append_event",
    "append_with_outbox",
    "append_with_outbox_batch",
)

RUST_SCHEMA_AUTHORITY_PATHS = {
    Path("services/sentinel-daemon/src/main.rs"),
    Path("services/sentinel-daemon/src/orchestrator.rs"),
    # These binaries create isolated output/test stores, never the live store.
    Path("services/sentinel-daemon/src/bin/replay-spike.rs"),
    Path("services/sentinel-daemon/src/bin/sentinel-db-maint.rs"),
}

RUST_SCHEMA_OPEN = re.compile(r"(?:sentinel_limbo::)?EventStore::open\s*\(")
RUST_TEST_MODULE = re.compile(r"(?m)^\s*#\[cfg\(test\)\]\s*\n\s*mod\s+tests\s*\{")


def _source_files(root: Path, suffix: str) -> list[Path]:
    ignored = {".git", "target", "node_modules", "vendor"}
    return sorted(
        path
        for path in root.rglob(f"*{suffix}")
        if not any(part in ignored for part in path.parts)
    )


def _production_rust(relative: Path, text: str) -> str:
    if "tests" in relative.parts or "benches" in relative.parts:
        return ""
    match = RUST_TEST_MODULE.search(text)
    return text[: match.start()] if match else text


def check(root: Path) -> list[str]:
    errors: list[str] = []
    store_path = root / "crates/sentinel-limbo/src/event_store.rs"
    if not store_path.is_file():
        return [f"missing canonical Rust event store: {store_path}"]

    store = store_path.read_text(encoding="utf-8")
    for method in RAW_RUST_METHODS:
        if re.search(rf"\bpub\s+fn\s+{method}\b", store):
            errors.append(f"raw Rust writer {method} is public")
        if not re.search(rf"\bpub\(crate\)\s+fn\s+{method}\b", store):
            errors.append(f"raw Rust writer {method} is not crate-private")

    limbo_internal = root / "crates/sentinel-limbo/src"
    for path in _source_files(root, ".rs"):
        relative = path.relative_to(root)
        production = _production_rust(relative, path.read_text(encoding="utf-8"))
        if relative not in RUST_SCHEMA_AUTHORITY_PATHS:
            for match in RUST_SCHEMA_OPEN.finditer(production):
                line = production.count("\n", 0, match.start()) + 1
                errors.append(
                    f"{relative}:{line}: production Rust process may not own event DDL"
                )
        if path.is_relative_to(limbo_internal):
            continue
        text = path.read_text(encoding="utf-8")
        for method in RAW_RUST_METHODS:
            for match in re.finditer(rf"\.{method}\s*\(", text):
                prefix = text[max(0, match.start() - 240) : match.start()]
                if ".legacy_append_gateway(" not in prefix:
                    line = text.count("\n", 0, match.start()) + 1
                    errors.append(f"{path.relative_to(root)}:{line}: unclassified Rust {method}")

    go_store = root / "pkg/sentinel-go/eventstore/store.go"
    if not go_store.is_file():
        errors.append("missing canonical Go event store")
    elif "func (s *Store) AppendWithOutbox(" in go_store.read_text(encoding="utf-8"):
        errors.append("raw Go AppendWithOutbox writer is exported")

    for path in _source_files(root, ".go"):
        if path.name.endswith("_test.go") or path == go_store:
            continue
        text = path.read_text(encoding="utf-8")
        for match in re.finditer(r"\.AppendWithOutbox\s*\(", text):
            prefix = text[max(0, match.start() - 240) : match.start()]
            if ".LegacyAppendGateway(" not in prefix:
                line = text.count("\n", 0, match.start()) + 1
                errors.append(f"{path.relative_to(root)}:{line}: unclassified Go AppendWithOutbox")
        if "eventstore.Open(" in text:
            line = text.count("\n", 0, text.index("eventstore.Open(")) + 1
            errors.append(
                f"{path.relative_to(root)}:{line}: production Go process may not own event DDL"
            )

    allowed_sql = {
        Path("crates/sentinel-limbo/src/event_store.rs"),
        Path("crates/sentinel-limbo/src/event_gateway.rs"),
        Path("pkg/sentinel-go/eventstore/store.go"),
    }
    insert_pattern = re.compile(r"INSERT(?:\s+OR\s+IGNORE)?\s+INTO\s+events\b", re.IGNORECASE)
    for suffix in (".rs", ".go"):
        for path in _source_files(root, suffix):
            relative = path.relative_to(root)
            if (
                relative in allowed_sql
                or path.name.endswith("_test.go")
                or "tests" in relative.parts
                or "benches" in relative.parts
            ):
                continue
            text = path.read_text(encoding="utf-8")
            for match in insert_pattern.finditer(text):
                if "SELECT" in text[match.end() : match.end() + 400]:
                    continue
                if "#[cfg(test)]" in text[: match.start()]:
                    continue
                errors.append(f"{relative}: raw events-table insert outside the sealed boundary")

    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    errors = check(args.root.resolve())
    if errors:
        for error in errors:
            print(f"ERROR: {error}")
        return 1
    print("event append boundary: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
