#!/usr/bin/env python3
"""Check the Nano-Container runtime contract decision text."""

from __future__ import annotations

import sys
from pathlib import Path


def usage() -> int:
    print(
        "usage: check-adr-runtime-contract.py docs/togaf-deviations-v22.md",
        file=sys.stderr,
    )
    return 2


def check_contains(label: str, text: str, needles: list[str]) -> list[str]:
    lower = text.lower()
    missing = [needle for needle in needles if needle.lower() not in lower]
    if missing:
        print(f"FAIL {label}: missing {', '.join(missing)}")
    else:
        print(f"PASS {label}")
    return missing


def main() -> int:
    if len(sys.argv) != 2:
        return usage()

    deviations = Path(sys.argv[1])
    if not deviations.exists():
        print(f"missing file: {deviations}", file=sys.stderr)
        return 2

    gap = deviations.with_name("togaf-gap-v22.md")
    if not gap.exists():
        print(f"missing sibling file: {gap}", file=sys.stderr)
        return 2

    deviations_text = deviations.read_text(encoding="utf-8")
    gap_text = gap.read_text(encoding="utf-8")

    failures: list[str] = []
    failures += check_contains(
        "DEV-006 superseded",
        deviations_text,
        ["DEV-006", "Superseded by DEV-007 (#407)"],
    )
    failures += check_contains(
        "DEV-007 active decision",
        deviations_text,
        [
            "DEV-007",
            "runtime-agnostic",
            "CRI-style",
            "no global default runtime",
            "explicit runtime key",
        ],
    )
    failures += check_contains(
        "options considered",
        deviations_text,
        ["Option 1", "Option 2", "Option 3", "DEV-007 chooses Option 3"],
    )
    failures += check_contains(
        "contract operations",
        deviations_text,
        ["spawn", "exec", "snapshot", "restore", "migrate", "health", "isolate"],
    )
    failures += check_contains(
        "runtime families",
        deviations_text,
        ["ecs-native", "wasm-wasmtime", "bwrap-landlock", "microvm"],
    )
    failures += check_contains(
        "epic and cross-architecture gates",
        deviations_text,
        ["#397", "#394/#406", "cross-architecture"],
    )
    failures += check_contains(
        "gap document Cluster 12",
        gap_text,
        [
            "Runtime contract decision (#407)",
            "DEV-007",
            "supersedes DEV-006",
            "no global default runtime",
            "#408",
            "#409",
            "#410",
            "#411",
        ],
    )

    if failures:
        return 1

    print("runtime contract ADR check passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
