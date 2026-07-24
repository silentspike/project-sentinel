#!/usr/bin/env python3
"""Validate Cargo source interventions against docs/dependency-patches.md.

The registry covers repository-controlled Cargo source overrides:

* [patch.<source>] entries in Cargo.toml
* [replace] entries in Cargo.toml
* [source.<name>].replace-with entries in repository Cargo config files

Ordinary git dependencies are inventoried but are not patches or forks. A temporary
fork must use one of the override mechanisms above and have an active registry row.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import date
from pathlib import Path
import sys
import tomllib
from typing import Any


REGISTRY_START = "<!-- patch-registry:toml:start -->"
REGISTRY_END = "<!-- patch-registry:toml:end -->"
IGNORED_DIRS = {".git", "node_modules", "target", "vendor"}
CARGO_CONFIG_DIR = "." + "cargo"
SOURCE_FIELDS = ("branch", "directory", "git", "local-registry", "package", "path", "registry", "rev", "tag", "version")
REQUIRED_FIELDS = (
    "id",
    "ecosystem",
    "package",
    "version",
    "kind",
    "manifest",
    "override_key",
    "source",
    "reason",
    "evidence",
    "upstream_basis",
    "diff_lines",
    "upstream_pr",
    "owner",
    "status",
    "expires_on",
    "revisit_condition",
    "advisory_ids",
    "rollback",
)
VALID_KINDS = {"PATCH_UPSTREAM", "FORK_TEMPORARY"}
VALID_STATUSES = {"ACTIVE"}


@dataclass(frozen=True)
class Override:
    override_key: str
    mechanism: str
    manifest: str
    package: str
    source: str


@dataclass(frozen=True)
class CheckResult:
    errors: tuple[str, ...]
    overrides: int
    registry_entries: int
    git_dependencies: int

    @property
    def ok(self) -> bool:
        return not self.errors


def _is_ignored(path: Path, root: Path) -> bool:
    return any(part in IGNORED_DIRS for part in path.relative_to(root).parts)


def _relative(path: Path, root: Path) -> str:
    return path.relative_to(root).as_posix()


def _load_toml(path: Path) -> dict[str, Any]:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def _normalize_spec(spec: Any) -> str:
    if isinstance(spec, str):
        return f"version={spec}"
    if not isinstance(spec, dict):
        return f"value={spec!r}"
    values = []
    for field in SOURCE_FIELDS:
        if field in spec:
            values.append(f"{field}={spec[field]}")
    return ";".join(values) if values else "inline-table"


def _manifest_paths(root: Path) -> list[Path]:
    return sorted(
        path
        for path in root.rglob("Cargo.toml")
        if not _is_ignored(path, root)
    )


def _cargo_config_paths(root: Path) -> list[Path]:
    paths: list[Path] = []
    for name in ("config", "config.toml"):
        paths.extend(root.rglob(f"{CARGO_CONFIG_DIR}/{name}"))
    return sorted(path for path in paths if not _is_ignored(path, root))


def _manifest_overrides(root: Path, path: Path) -> list[Override]:
    data = _load_toml(path)
    relative = _relative(path, root)
    overrides: list[Override] = []

    patch = data.get("patch", {})
    if isinstance(patch, dict):
        for source_name, packages in sorted(patch.items()):
            if not isinstance(packages, dict):
                continue
            for alias, spec in sorted(packages.items()):
                package = spec.get("package", alias) if isinstance(spec, dict) else alias
                overrides.append(
                    Override(
                        override_key=f"patch:{relative}:{source_name}:{alias}",
                        mechanism="patch",
                        manifest=relative,
                        package=str(package),
                        source=_normalize_spec(spec),
                    )
                )

    replace = data.get("replace", {})
    if isinstance(replace, dict):
        for package_version, spec in sorted(replace.items()):
            package = package_version.rsplit(":", 1)[0]
            overrides.append(
                Override(
                    override_key=f"replace:{relative}:{package_version}",
                    mechanism="replace",
                    manifest=relative,
                    package=package,
                    source=_normalize_spec(spec),
                )
            )
    return overrides


def _config_overrides(root: Path, path: Path) -> list[Override]:
    data = _load_toml(path)
    relative = _relative(path, root)
    sources = data.get("source", {})
    if not isinstance(sources, dict):
        return []

    overrides: list[Override] = []
    for source_name, source_config in sorted(sources.items()):
        if not isinstance(source_config, dict) or "replace-with" not in source_config:
            continue
        replacement = str(source_config["replace-with"])
        replacement_config = sources.get(replacement, {})
        replacement_spec = (
            _normalize_spec(replacement_config)
            if isinstance(replacement_config, dict)
            else f"value={replacement_config!r}"
        )
        overrides.append(
            Override(
                override_key=f"source:{relative}:{source_name}",
                mechanism="source-replacement",
                manifest=relative,
                package="*",
                source=f"replace-with={replacement};{replacement_spec}",
            )
        )
    return overrides


def discover_overrides(root: Path) -> tuple[Override, ...]:
    overrides: list[Override] = []
    for path in _manifest_paths(root):
        overrides.extend(_manifest_overrides(root, path))
    for path in _cargo_config_paths(root):
        overrides.extend(_config_overrides(root, path))
    return tuple(sorted(overrides, key=lambda item: item.override_key))


def _dependency_tables(data: dict[str, Any]) -> list[dict[str, Any]]:
    tables: list[dict[str, Any]] = []
    for name in ("dependencies", "dev-dependencies", "build-dependencies"):
        value = data.get(name)
        if isinstance(value, dict):
            tables.append(value)

    workspace = data.get("workspace")
    if isinstance(workspace, dict):
        value = workspace.get("dependencies")
        if isinstance(value, dict):
            tables.append(value)

    targets = data.get("target")
    if isinstance(targets, dict):
        for target in targets.values():
            if not isinstance(target, dict):
                continue
            for name in ("dependencies", "dev-dependencies", "build-dependencies"):
                value = target.get(name)
                if isinstance(value, dict):
                    tables.append(value)
    return tables


def count_git_dependencies(root: Path) -> int:
    count = 0
    for path in _manifest_paths(root):
        data = _load_toml(path)
        for table in _dependency_tables(data):
            count += sum(
                1
                for spec in table.values()
                if isinstance(spec, dict) and isinstance(spec.get("git"), str)
            )
    return count


def load_registry(path: Path) -> dict[str, Any]:
    text = path.read_text(encoding="utf-8")
    if text.count(REGISTRY_START) != 1 or text.count(REGISTRY_END) != 1:
        raise ValueError("registry must contain exactly one TOML marker pair")
    payload = text.split(REGISTRY_START, 1)[1].split(REGISTRY_END, 1)[0].strip()
    if payload.startswith("```toml"):
        payload = payload[len("```toml") :].strip()
    if payload.endswith("```"):
        payload = payload[:-3].strip()
    return tomllib.loads(payload)


def _non_empty(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def _validate_entry_shape(entry: Any, index: int, today: date) -> list[str]:
    errors: list[str] = []
    label = f"entry[{index}]"
    if not isinstance(entry, dict):
        return [f"ERROR[INVALID_ENTRY] {label} must be a TOML table"]

    for field in REQUIRED_FIELDS:
        if field not in entry:
            errors.append(f"ERROR[MISSING_FIELD] {label} missing `{field}`")

    if errors:
        return errors

    for field in REQUIRED_FIELDS:
        if field in {"diff_lines", "advisory_ids"}:
            continue
        if not _non_empty(entry[field]):
            errors.append(f"ERROR[EMPTY_FIELD] {label} has empty `{field}`")

    if entry["ecosystem"] != "cargo":
        errors.append(f"ERROR[INVALID_ECOSYSTEM] {label} ecosystem must be `cargo`")
    if entry["kind"] not in VALID_KINDS:
        errors.append(
            f"ERROR[INVALID_KIND] {label} kind `{entry['kind']}` is not supported"
        )
    if entry["status"] not in VALID_STATUSES:
        errors.append(
            f"ERROR[INVALID_STATUS] {label} status `{entry['status']}` must be `ACTIVE`"
        )
    if not isinstance(entry["diff_lines"], int) or entry["diff_lines"] < 0:
        errors.append(f"ERROR[INVALID_DIFF_SIZE] {label} `diff_lines` must be >= 0")
    if not isinstance(entry["advisory_ids"], list) or not all(
        isinstance(item, str) and item for item in entry["advisory_ids"]
    ):
        errors.append(
            f"ERROR[INVALID_ADVISORIES] {label} `advisory_ids` must be a string array"
        )

    try:
        expires_on = date.fromisoformat(str(entry["expires_on"]))
    except ValueError:
        errors.append(
            f"ERROR[INVALID_EXPIRY] {label} `expires_on` must be an ISO date"
        )
    else:
        if expires_on <= today:
            code = (
                "EXPIRED_TEMPORARY_FORK"
                if entry["kind"] == "FORK_TEMPORARY"
                else "EXPIRED_PATCH"
            )
            errors.append(
                f"ERROR[{code}] {label} expired on {expires_on.isoformat()}"
            )
    return errors


def check_repository(root: Path, registry_path: Path, today: date) -> CheckResult:
    errors: list[str] = []
    try:
        registry = load_registry(registry_path)
    except (OSError, ValueError, tomllib.TOMLDecodeError) as exc:
        return CheckResult(
            errors=(f"ERROR[INVALID_REGISTRY] {exc}",),
            overrides=0,
            registry_entries=0,
            git_dependencies=0,
        )

    if registry.get("schema_version") != 1:
        errors.append("ERROR[SCHEMA_VERSION] `schema_version` must equal 1")
    entries = registry.get("entries")
    if not isinstance(entries, list):
        errors.append("ERROR[INVALID_REGISTRY] `entries` must be an array")
        entries = []

    for index, entry in enumerate(entries):
        errors.extend(_validate_entry_shape(entry, index, today))

    ids: set[str] = set()
    keys: set[str] = set()
    for index, entry in enumerate(entries):
        if not isinstance(entry, dict):
            continue
        entry_id = entry.get("id")
        override_key = entry.get("override_key")
        if isinstance(entry_id, str):
            if entry_id in ids:
                errors.append(f"ERROR[DUPLICATE_ID] duplicate registry id `{entry_id}`")
            ids.add(entry_id)
        if isinstance(override_key, str):
            if override_key in keys:
                errors.append(
                    f"ERROR[DUPLICATE_OVERRIDE_KEY] duplicate `{override_key}`"
                )
            keys.add(override_key)

    try:
        overrides = discover_overrides(root)
        git_dependencies = count_git_dependencies(root)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        errors.append(f"ERROR[INVALID_CARGO_TOML] {exc}")
        overrides = ()
        git_dependencies = 0

    entry_by_key = {
        entry["override_key"]: entry
        for entry in entries
        if isinstance(entry, dict) and isinstance(entry.get("override_key"), str)
    }
    override_by_key = {override.override_key: override for override in overrides}

    for override in overrides:
        entry = entry_by_key.get(override.override_key)
        if entry is None:
            errors.append(
                "ERROR[UNREGISTERED_OVERRIDE] "
                f"`{override.override_key}` ({override.source})"
            )
            continue
        comparisons = {
            "manifest": override.manifest,
            "package": override.package,
            "source": override.source,
        }
        for field, expected in comparisons.items():
            if entry.get(field) != expected:
                errors.append(
                    f"ERROR[OVERRIDE_MISMATCH] `{override.override_key}` field "
                    f"`{field}` expected `{expected}`"
                )

    for override_key in sorted(entry_by_key):
        if override_key not in override_by_key:
            errors.append(
                f"ERROR[STALE_REGISTRY_ROW] `{override_key}` has no active Cargo override"
            )

    return CheckResult(
        errors=tuple(errors),
        overrides=len(overrides),
        registry_entries=len(entries),
        git_dependencies=git_dependencies,
    )


def _parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Validate Cargo patches and temporary forks against the registry."
    )
    default_root = Path(__file__).resolve().parents[1]
    parser.add_argument("--repo-root", type=Path, default=default_root)
    parser.add_argument(
        "--registry",
        type=Path,
        default=Path("docs/dependency-patches.md"),
        help="registry path, relative to --repo-root by default",
    )
    parser.add_argument(
        "--today",
        type=date.fromisoformat,
        default=date.today(),
        help="ISO date override for deterministic tests",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv if argv is not None else sys.argv[1:])
    root = args.repo_root.resolve()
    registry_path = args.registry
    if not registry_path.is_absolute():
        registry_path = root / registry_path
    result = check_repository(root, registry_path, args.today)

    for error in result.errors:
        print(f"::error::{error}")
    status = "PASS" if result.ok else "FAIL"
    print(
        f"patch-registry={status} overrides={result.overrides} "
        f"registry_entries={result.registry_entries} "
        f"direct_git_dependencies={result.git_dependencies}"
    )
    return 0 if result.ok else 1


if __name__ == "__main__":
    sys.exit(main())
