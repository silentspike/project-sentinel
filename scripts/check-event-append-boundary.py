#!/usr/bin/env python3
"""Fail closed when production code bypasses the canonical event append boundary."""

from __future__ import annotations

import argparse
from collections import Counter
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

# Every production V1 compatibility writer is frozen here until its owning
# domain migrates to EventAppendGateway V2.  The count is intentional: adding a
# second call in an otherwise approved file must still receive explicit review.
RUST_LEGACY_CALLSITE_INVENTORY = {
    (Path("crates/sentinel-ecs/src/systems.rs"), "EcsTickBatch"): 1,
    (Path("crates/sentinel-runtime/src/lib.rs"), "RuntimeAgent"): 1,
    (Path("services/sentinel-daemon/src/operator_api.rs"), "DaemonOperatorApi"): 1,
    (Path("services/sentinel-daemon/src/orchestrator.rs"), "DaemonOrchestrator"): 9,
    (
        Path("services/sentinel-daemon/src/platform_controlplane/mod.rs"),
        "PlatformControlPlane",
    ): 2,
    (Path("services/sentinel-daemon/src/resource_manager.rs"), "ResourceManager"): 1,
    (Path("services/sentinel-daemon/src/workbench.rs"), "DaemonWorkbench"): 1,
    (Path("services/sentinel-daemon/src/workflow_api.rs"), "DaemonWorkflow"): 1,
    (
        Path("services/sentinel-daemon/src/workflow_api/delivery_runtime.rs"),
        "DaemonWorkflow",
    ): 2,
    (Path("services/sentinel-nightrun/src/runner.rs"), "NightRun"): 4,
}

GO_LEGACY_CALLSITE_INVENTORY = {
    (
        Path("cmd/cortex-gateway/internal/proxy/pipeline.go"),
        "LegacyProducerCortexAudit",
    ): 1,
}

RUST_CLASSIFIED_WRITE = re.compile(
    r"legacy_append_gateway\s*\(\s*"
    r"(?:sentinel_limbo::)?LegacyEventProducer::(?P<producer>[A-Za-z0-9_]+)\s*,?\s*\)"
    r"\s*\.\s*(?P<method>append_event|append_with_outbox|append_with_outbox_batch)\s*\(",
    re.DOTALL,
)
RUST_RAW_WRITE = re.compile(
    r"\.\s*(?P<method>append_event|append_with_outbox|append_with_outbox_batch)\s*\("
)
GO_CLASSIFIED_WRITE = re.compile(
    r"LegacyAppendGateway\s*\(\s*"
    r"(?:eventstore\.)?(?P<producer>LegacyProducer[A-Za-z0-9_]+)\s*\)"
    r"\s*\.\s*(?P<method>AppendWithOutbox)\s*\(",
    re.DOTALL,
)
GO_RAW_WRITE = re.compile(r"\.\s*(?P<method>AppendWithOutbox)\s*\(")


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


def _check_inventory(
    language: str,
    actual: Counter[tuple[Path, str]],
    expected: dict[tuple[Path, str], int],
) -> list[str]:
    errors: list[str] = []
    for key in sorted(set(actual) | set(expected), key=lambda item: (str(item[0]), item[1])):
        actual_count = actual.get(key, 0)
        expected_count = expected.get(key, 0)
        if actual_count != expected_count:
            path, producer = key
            errors.append(
                f"{language} legacy writer inventory mismatch for {path} "
                f"producer={producer}: expected {expected_count}, found {actual_count}"
            )
    return errors


def check(
    root: Path,
    *,
    rust_inventory: dict[tuple[Path, str], int] | None = None,
    go_inventory: dict[tuple[Path, str], int] | None = None,
) -> list[str]:
    errors: list[str] = []
    expected_rust = (
        RUST_LEGACY_CALLSITE_INVENTORY if rust_inventory is None else rust_inventory
    )
    expected_go = GO_LEGACY_CALLSITE_INVENTORY if go_inventory is None else go_inventory
    actual_rust: Counter[tuple[Path, str]] = Counter()
    actual_go: Counter[tuple[Path, str]] = Counter()
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
        classified_writes: set[tuple[int, str]] = set()
        for match in RUST_CLASSIFIED_WRITE.finditer(production):
            producer = match.group("producer")
            actual_rust[(relative, producer)] += 1
            classified_writes.add((match.start("method"), match.group("method")))
        if relative not in RUST_SCHEMA_AUTHORITY_PATHS:
            for match in RUST_SCHEMA_OPEN.finditer(production):
                line = production.count("\n", 0, match.start()) + 1
                errors.append(
                    f"{relative}:{line}: production Rust process may not own event DDL"
                )
        if path.is_relative_to(limbo_internal):
            continue
        for match in RUST_RAW_WRITE.finditer(production):
            method = match.group("method")
            if (match.start("method"), method) not in classified_writes:
                line = production.count("\n", 0, match.start()) + 1
                errors.append(f"{relative}:{line}: unclassified Rust {method}")

    errors.extend(_check_inventory("Rust", actual_rust, expected_rust))

    go_store = root / "pkg/sentinel-go/eventstore/store.go"
    if not go_store.is_file():
        errors.append("missing canonical Go event store")
    elif "func (s *Store) AppendWithOutbox(" in go_store.read_text(encoding="utf-8"):
        errors.append("raw Go AppendWithOutbox writer is exported")

    for path in _source_files(root, ".go"):
        if path.name.endswith("_test.go") or path == go_store:
            continue
        relative = path.relative_to(root)
        text = path.read_text(encoding="utf-8")
        classified_writes: set[tuple[int, str]] = set()
        for match in GO_CLASSIFIED_WRITE.finditer(text):
            producer = match.group("producer")
            actual_go[(relative, producer)] += 1
            classified_writes.add((match.start("method"), match.group("method")))
        for match in GO_RAW_WRITE.finditer(text):
            method = match.group("method")
            if (match.start("method"), method) not in classified_writes:
                line = text.count("\n", 0, match.start()) + 1
                errors.append(f"{path.relative_to(root)}:{line}: unclassified Go AppendWithOutbox")
        if "eventstore.Open(" in text:
            line = text.count("\n", 0, text.index("eventstore.Open(")) + 1
            errors.append(
                f"{path.relative_to(root)}:{line}: production Go process may not own event DDL"
            )

    errors.extend(_check_inventory("Go", actual_go, expected_go))

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
