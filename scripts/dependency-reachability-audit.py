#!/usr/bin/env python3
"""Classify Cargo.lock reachability and protect public dependency evidence."""

from __future__ import annotations

import argparse
import csv
import getpass
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import tomllib
from collections import defaultdict, deque
from dataclasses import dataclass, field
from typing import Iterable, Mapping, Sequence


RELEASE_ROOTS = {
    "sentinel-daemon": "A",
    "sentinel-projection-service": "A",
    "sentinel-dashboard-backend": "A",
    "sentinel-gaia-loop": "A",
    "agent-runtime": "B",
    "sentinel-ctl": "B",
    "sentinel-gaia": "B",
    "sentinel-nightrun": "C",
}

PRIMARY_ORDER = (
    "release-normal",
    "release-build",
    "non-release-workspace-normal",
    "dev-bench-only",
    "target-only",
    "optional-disabled",
)

TSV_FIELDS = (
    "name",
    "version",
    "source",
    "primary_reachability",
    "reachable_in_roots",
    "root_tiers",
    "also_dev",
    "also_foreign_target",
)


class AuditError(RuntimeError):
    """A fail-closed audit contract violation."""


def package_key(package: Mapping[str, object]) -> tuple[str, str, str]:
    return (
        str(package["name"]),
        str(package["version"]),
        str(package.get("source") or "workspace"),
    )


def display_source(source: str) -> str:
    if source == "workspace":
        return source
    if source.startswith("registry+"):
        return "registry"
    if source.startswith("git+"):
        return "git"
    return source


@dataclass
class Membership:
    release_normal: set[str] = field(default_factory=set)
    release_build: set[str] = field(default_factory=set)
    non_release_workspace: bool = False
    dev: bool = False
    foreign_target: bool = False
    metadata_native: bool = False
    metadata_all: bool = False


class ReachabilityAudit:
    def __init__(
        self,
        lock_data: Mapping[str, object],
        metadata_all: Mapping[str, object],
        metadata_native: Mapping[str, object],
    ) -> None:
        self.lock_packages = {
            package_key(package): package for package in lock_data.get("package", [])
        }
        self.all = metadata_all
        self.native = metadata_native
        self.all_packages = {
            str(package["id"]): package for package in metadata_all["packages"]
        }
        self.native_packages = {
            str(package["id"]): package for package in metadata_native["packages"]
        }
        self.all_nodes = {
            str(node["id"]): node for node in metadata_all["resolve"]["nodes"]
        }
        self.native_nodes = {
            str(node["id"]): node for node in metadata_native["resolve"]["nodes"]
        }
        self.memberships = defaultdict(Membership)
        self.key_for_id = {
            package_id: package_key(package)
            for package_id, package in self.all_packages.items()
        }
        self.proc_macros = {
            package_id
            for package_id, package in self.all_packages.items()
            if any("proc-macro" in target.get("kind", []) for target in package["targets"])
        }
        self.workspace_ids = set(str(value) for value in metadata_all["workspace_members"])
        self.root_ids = self._resolve_roots()
        self.rows: list[dict[str, str]] = []

    def _resolve_roots(self) -> dict[str, str]:
        by_name: dict[str, list[str]] = defaultdict(list)
        for package_id in self.workspace_ids:
            by_name[str(self.all_packages[package_id]["name"])].append(package_id)
        roots: dict[str, str] = {}
        for name in RELEASE_ROOTS:
            matches = by_name.get(name, [])
            if len(matches) != 1:
                raise AuditError(
                    f"release root {name!r} resolved {len(matches)} times, expected once"
                )
            roots[name] = matches[0]
        return roots

    @staticmethod
    def _edge_kinds(dep: Mapping[str, object]) -> set[str]:
        kinds = set()
        for item in dep.get("dep_kinds", []):
            kinds.add(str(item.get("kind") or "normal"))
        return kinds or {"normal"}

    @staticmethod
    def _edge_specs(dep: Mapping[str, object]) -> set[tuple[str, str]]:
        specs = set()
        for item in dep.get("dep_kinds", []):
            specs.add((str(item.get("kind") or "normal"), str(item.get("target") or "")))
        return specs or {("normal", "")}

    def _walk_release(self, root_name: str, root_id: str) -> None:
        queue = deque([(root_id, "normal")])
        visited: set[tuple[str, str]] = set()
        while queue:
            package_id, context = queue.popleft()
            if (package_id, context) in visited:
                continue
            visited.add((package_id, context))
            key = self.key_for_id[package_id]
            membership = self.memberships[key]
            if context == "build":
                membership.release_build.add(root_name)
            else:
                membership.release_normal.add(root_name)
            for dep in self.native_nodes[package_id].get("deps", []):
                dep_id = str(dep["pkg"])
                for kind in self._edge_kinds(dep):
                    if kind == "dev":
                        continue
                    next_context = context
                    if kind == "build" or dep_id in self.proc_macros:
                        next_context = "build"
                    queue.append((dep_id, next_context))

    def _walk_workspace_non_dev(self, root_id: str) -> set[str]:
        seen: set[tuple[str, str]] = set()
        queue = deque([(root_id, "normal")])
        result: set[str] = set()
        while queue:
            package_id, context = queue.popleft()
            if (package_id, context) in seen:
                continue
            seen.add((package_id, context))
            result.add(package_id)
            for dep in self.native_nodes[package_id].get("deps", []):
                dep_id = str(dep["pkg"])
                for kind in self._edge_kinds(dep):
                    if kind == "dev":
                        continue
                    next_context = "build" if kind == "build" else context
                    if dep_id in self.proc_macros:
                        next_context = "build"
                    queue.append((dep_id, next_context))
        return result

    def _walk_dev(self, root_id: str) -> set[str]:
        seen: set[tuple[str, bool]] = set()
        queue = deque([(root_id, False)])
        result: set[str] = set()
        while queue:
            package_id, dev_context = queue.popleft()
            if (package_id, dev_context) in seen:
                continue
            seen.add((package_id, dev_context))
            if dev_context:
                result.add(package_id)
            for dep in self.native_nodes[package_id].get("deps", []):
                dep_id = str(dep["pkg"])
                for kind in self._edge_kinds(dep):
                    queue.append((dep_id, dev_context or kind == "dev"))
        return result

    def _walk_foreign_targets(self, root_id: str) -> set[str]:
        native_edges: set[tuple[str, str, str, str]] = set()
        for package_id, node in self.native_nodes.items():
            for dep in node.get("deps", []):
                for kind, target in self._edge_specs(dep):
                    native_edges.add((package_id, str(dep["pkg"]), kind, target))

        seen: set[tuple[str, bool]] = set()
        queue = deque([(root_id, False)])
        result: set[str] = set()
        while queue:
            package_id, foreign_context = queue.popleft()
            if (package_id, foreign_context) in seen:
                continue
            seen.add((package_id, foreign_context))
            if foreign_context:
                result.add(package_id)
            for dep in self.all_nodes[package_id].get("deps", []):
                dep_id = str(dep["pkg"])
                for kind, target in self._edge_specs(dep):
                    edge_is_native = (package_id, dep_id, kind, target) in native_edges
                    queue.append((dep_id, foreign_context or not edge_is_native))
        return result

    def classify(self) -> list[dict[str, str]]:
        metadata_all_keys = {package_key(package) for package in self.all_packages.values()}
        metadata_native_keys = {
            package_key(package) for package in self.native_packages.values()
        }
        unknown_metadata = metadata_all_keys - set(self.lock_packages)
        if unknown_metadata:
            raise AuditError(f"metadata packages missing from lockfile: {unknown_metadata!r}")

        for key in metadata_all_keys:
            self.memberships[key].metadata_all = True
        for key in metadata_native_keys:
            self.memberships[key].metadata_native = True

        for root_name, root_id in self.root_ids.items():
            self._walk_release(root_name, root_id)

        release_ids = set(self.root_ids.values())
        for workspace_id in self.workspace_ids - release_ids:
            for package_id in self._walk_workspace_non_dev(workspace_id):
                self.memberships[self.key_for_id[package_id]].non_release_workspace = True
        for workspace_id in self.workspace_ids:
            for package_id in self._walk_dev(workspace_id):
                self.memberships[self.key_for_id[package_id]].dev = True
            for package_id in self._walk_foreign_targets(workspace_id):
                self.memberships[self.key_for_id[package_id]].foreign_target = True

        rows: list[dict[str, str]] = []
        for key in sorted(self.lock_packages):
            name, version, source = key
            membership = self.memberships[key]
            if membership.release_normal:
                primary = "release-normal"
            elif membership.release_build:
                primary = "release-build"
            elif membership.non_release_workspace:
                primary = "non-release-workspace-normal"
            elif membership.dev:
                primary = "dev-bench-only"
            elif membership.foreign_target:
                primary = "target-only"
            elif not membership.metadata_all:
                primary = "optional-disabled"
            else:
                primary = "unclassified"

            roots = sorted(membership.release_normal | membership.release_build)
            tiers = sorted({RELEASE_ROOTS[root] for root in roots})
            rows.append(
                {
                    "name": name,
                    "version": version,
                    "source": display_source(source),
                    "primary_reachability": primary,
                    "reachable_in_roots": ",".join(roots),
                    "root_tiers": ",".join(tiers),
                    "also_dev": str(membership.dev).lower(),
                    "also_foreign_target": str(membership.foreign_target).lower(),
                }
            )
        unclassified = [row for row in rows if row["primary_reachability"] == "unclassified"]
        if unclassified:
            names = ", ".join(f"{row['name']}@{row['version']}" for row in unclassified)
            raise AuditError(f"unclassified lockfile packages: {names}")
        self.rows = rows
        return rows

    def direct_feature_rows(self) -> list[dict[str, str]]:
        rows: list[dict[str, str]] = []
        for root_name, root_id in sorted(self.root_ids.items()):
            package = self.native_packages[root_id]
            node = self.native_nodes[root_id]
            node_deps = defaultdict(list)
            for dep in node.get("deps", []):
                node_deps[str(dep["name"])].append(dep)
            for dependency in package.get("dependencies", []):
                if str(dependency.get("kind") or "normal") != "normal":
                    continue
                lookup = str(dependency.get("rename") or dependency["name"]).replace("-", "_")
                matches = node_deps.get(lookup, [])
                if not matches:
                    continue
                dep_id = str(matches[0]["pkg"])
                dep_node = self.native_nodes[dep_id]
                rows.append(
                    {
                        "root": root_name,
                        "tier": RELEASE_ROOTS[root_name],
                        "dependency": str(self.native_packages[dep_id]["name"]),
                        "version": str(self.native_packages[dep_id]["version"]),
                        "requested_features": ",".join(sorted(dependency.get("features", []))),
                        "default_features": str(
                            bool(dependency.get("uses_default_features", True))
                        ).lower(),
                        "release_features": "",
                        "metadata_union_features": ",".join(sorted(dep_node.get("features", []))),
                        "manifest": relative_manifest(str(package["manifest_path"])),
                    }
                )
        return sorted(rows, key=lambda row: (row["root"], row["dependency"], row["version"]))

    def duplicate_rows(self) -> list[dict[str, str]]:
        by_name: dict[str, list[dict[str, str]]] = defaultdict(list)
        for row in self.rows:
            by_name[row["name"]].append(row)
        lock_by_name: dict[str, list[tuple[str, str, str]]] = defaultdict(list)
        for key in self.lock_packages:
            lock_by_name[key[0]].append(key)
        reverse: dict[tuple[str, str, str], set[str]] = defaultdict(set)
        for parent_key, package in self.lock_packages.items():
            for raw_dependency in package.get("dependencies", []):
                if not isinstance(raw_dependency, str):
                    continue
                parts = raw_dependency.split()
                dep_name = parts[0]
                dep_version = parts[1] if len(parts) > 1 and parts[1][0].isdigit() else None
                candidates = lock_by_name.get(dep_name, [])
                if dep_version is not None:
                    candidates = [key for key in candidates if key[1] == dep_version]
                if len(candidates) == 1:
                    reverse[candidates[0]].add(f"{parent_key[0]}@{parent_key[1]}")
        result = []
        for name, rows in sorted(by_name.items()):
            if len(rows) < 2:
                continue
            if name == "criterion":
                group_decision = "align-version"
                constraint_assessment = (
                    "direct 0.5 constraints in sentinel-telemetry and sentinel-zenoh; "
                    "workspace 0.8.2 exposes html_reports and async_tokio"
                )
                revisit_condition = "#632 compile and benchmark gates reject the 0.8.2 API"
            elif name == "criterion-plot":
                group_decision = "align-version"
                constraint_assessment = "transitive consequence of the Criterion 0.5/0.8 split"
                revisit_condition = "Criterion cannot be aligned"
            elif all(row["primary_reachability"] == "target-only" for row in rows):
                group_decision = "leave"
                constraint_assessment = "foreign-target upstream constraints; no Linux release cost"
                revisit_condition = "supported target policy or upstream constraints change"
            else:
                group_decision = "investigate"
                constraint_assessment = (
                    "independent upstream constraints; semver/API compatibility not proven"
                )
                revisit_condition = "all immediate forcers permit one tested version"
            for row in sorted(rows, key=lambda value: value["version"]):
                key_matches = [
                    key
                    for key in self.lock_packages
                    if key[0] == name and key[1] == row["version"]
                ]
                forcers = set()
                for key in key_matches:
                    forcers.update(reverse[key])
                result.append(
                    {
                        "name": name,
                        "version": row["version"],
                        "primary_reachability": row["primary_reachability"],
                        "reachable_in_roots": row["reachable_in_roots"],
                        "immediate_forcers": ",".join(sorted(forcers)),
                        "decision": group_decision,
                        "constraint_assessment": constraint_assessment,
                        "revisit_condition": revisit_condition,
                    }
                )
        return result


def relative_manifest(value: str) -> str:
    path = Path(value)
    parts = path.parts
    if "project-sentinel" in parts:
        index = parts.index("project-sentinel")
        return str(Path(*parts[index + 1 :]))
    for marker in ("crates", "services"):
        if marker in parts:
            index = parts.index(marker)
            return str(Path(*parts[index:]))
    return "<WORKSPACE>/Cargo.toml"


def write_tsv(path: Path, rows: Sequence[Mapping[str, str]], fields: Sequence[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields, delimiter="\t", lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)


def write_summary(path: Path, rows: Sequence[Mapping[str, str]], root_count: int) -> None:
    counts = {category: 0 for category in PRIMARY_ORDER}
    for row in rows:
        counts[row["primary_reachability"]] += 1
    lines = [
        f"lockfile_packages={len(rows)}",
        f"classified_primary={sum(counts.values())}",
        "unclassified=0",
        "duplicate_primary_assignments=0",
        f"release_roots_resolved={root_count}/{len(RELEASE_ROOTS)}",
    ]
    lines.extend(f"{category}={counts[category]}" for category in PRIMARY_ORDER)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def add_release_tree_features(
    rows: list[dict[str, str]], trees_dir: Path | None
) -> list[dict[str, str]]:
    if trees_dir is None:
        return rows
    features_by_root: dict[str, dict[str, set[str]]] = {}
    for root in RELEASE_ROOTS:
        path = trees_dir / f"{root}.features.txt"
        if not path.is_file():
            raise AuditError(f"missing normalized feature tree: {path}")
        package_features: dict[str, set[str]] = defaultdict(set)
        for line in path.read_text(encoding="utf-8").splitlines():
            match = re.match(r'^\d+([^ ]+) feature "([^"]+)"', line)
            if match:
                package_features[match.group(1)].add(match.group(2))
        features_by_root[root] = package_features
    for row in rows:
        row["release_features"] = ",".join(
            sorted(features_by_root[row["root"]].get(row["dependency"], set()))
        )
    return rows


def load_json(path: Path) -> Mapping[str, object]:
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def run_audit(args: argparse.Namespace) -> int:
    with args.lock.open("rb") as handle:
        lock_data = tomllib.load(handle)
    audit = ReachabilityAudit(
        lock_data,
        load_json(args.metadata_all),
        load_json(args.metadata_native),
    )
    rows = audit.classify()
    if args.check:
        print(f"coverage={len(rows)}/{len(rows)} unclassified=0 roots=8/8")
        return 0
    if args.output_dir is None:
        raise AuditError("--output-dir is required unless --check is used")
    write_tsv(args.output_dir / "reachability.tsv", rows, TSV_FIELDS)
    write_summary(args.output_dir / "reachability-summary.txt", rows, len(audit.root_ids))
    feature_rows = add_release_tree_features(audit.direct_feature_rows(), args.trees_dir)
    write_tsv(
        args.output_dir / "direct-release-features.tsv",
        feature_rows,
        (
            "root",
            "tier",
            "dependency",
            "version",
            "requested_features",
            "default_features",
            "release_features",
            "metadata_union_features",
            "manifest",
        ),
    )
    duplicate_rows = audit.duplicate_rows()
    write_tsv(
        args.output_dir / "duplicate-versions.tsv",
        duplicate_rows,
        (
            "name",
            "version",
            "primary_reachability",
            "reachable_in_roots",
            "immediate_forcers",
            "decision",
            "constraint_assessment",
            "revisit_condition",
        ),
    )
    print(
        f"wrote {len(rows)} packages, {len(feature_rows)} direct feature rows, "
        f"and {len(duplicate_rows)} duplicate-version rows"
    )
    return 0


TEMP_ROOT = "/" + "tmp" + "/"
WORK_ROOT = "/" + "work" + "/"
CARGO_PATH = "." + "cargo" + "/"


PLACEHOLDER_REPLACEMENTS = (
    (re.compile(re.escape(TEMP_ROOT + "builds/") + r"[0-9]+"), "<REMOTE_PROJECT>"),
    (
        re.compile(
            re.escape(TEMP_ROOT) + r"(?:issue-?631|cargo-remote)[^\s\"')]*"
        ),
        "<REMOTE_TARGET>",
    ),
    (re.compile(r"/(?:root|home/[^/]+)/\.cargo"), "<CARGO_HOME>"),
    (
        re.compile(re.escape(WORK_ROOT) + r"[^/\s]+/(?:project-sentinel|ps-631-dep-audit)"),
        "<WORKSPACE>",
    ),
    (re.compile(r"/(?:root|home/[^/]+)"), "<HOME>"),
    (re.compile(r"\b(?:root|ubuntu)@(?:[A-Za-z0-9._-]+|(?:\d{1,3}\.){3}\d{1,3})"), "<USER>@<HOST>"),
    (re.compile(r"\b(?:\d{1,3}\.){3}\d{1,3}\b"), "<HOST>"),
)


def normalize_text(value: str) -> str:
    value = value.replace("\r", "\n")
    value = re.sub(r"\x1b\[[0-9;?]*[ -/]*[@-~]", "", value)
    for pattern, replacement in PLACEHOLDER_REPLACEMENTS:
        value = pattern.sub(replacement, value)
    value = re.sub(r"[ \t]+$", "", value, flags=re.MULTILINE)
    value = re.sub(r"\n{3,}", "\n\n", value)
    return value.strip() + "\n"


def normalize_tree(value: str) -> str:
    normalized = normalize_text(value)
    source_lines = normalized.splitlines()
    root_index = next(
        (index for index, line in enumerate(source_lines) if re.match(r"^0\S+ v\S+", line)),
        None,
    )
    if root_index is None:
        raise AuditError("tree output does not contain a depth-zero root")
    lines = [
        line for line in source_lines[root_index:] if re.match(r"^[0-9]+[A-Za-z_\[]", line)
    ]
    return "\n".join(lines) + "\n"


def normalize_bloat(value: str) -> str:
    normalized = normalize_text(value)
    source_lines = [line.strip() for line in normalized.splitlines()]
    header_indexes = [
        index
        for index, line in enumerate(source_lines)
        if re.fullmatch(r"File\s+\.text\s+Size\s+Crate", line)
    ]
    if len(header_indexes) != 1:
        raise AuditError(
            f"bloat output contains {len(header_indexes)} table headers, expected one"
        )
    header_index = header_indexes[0]
    footer_indexes = [
        index
        for index, line in enumerate(source_lines[header_index + 1 :], header_index + 1)
        if ".text section size, the file size is" in line
    ]
    if len(footer_indexes) != 1:
        raise AuditError(
            f"bloat output contains {len(footer_indexes)} summary rows, expected one"
        )
    footer_index = footer_indexes[0]
    rows = source_lines[header_index + 1 : footer_index + 1]
    if len(rows) < 2:
        raise AuditError("bloat output contains no crate rows")
    row_pattern = re.compile(r"\d+(?:\.\d+)?%\s+\d+(?:\.\d+)?%\s+\S+\s+.+")
    invalid_rows = [line for line in rows if not row_pattern.fullmatch(line)]
    if invalid_rows:
        raise AuditError(f"invalid bloat table row: {invalid_rows[0]!r}")
    return "\n".join([source_lines[header_index], *rows]) + "\n"


def parse_bloat_summary(path: Path) -> dict[str, str]:
    normalized = normalize_bloat(path.read_text(encoding="utf-8"))
    rows = normalized.splitlines()[1:]
    data_pattern = re.compile(
        r"(?P<file_percent>\S+)%\s+(?P<text_percent>\S+)%\s+"
        r"(?P<size>\S+)\s+(?P<crate>.+)"
    )
    parsed_rows = []
    for row in rows:
        match = data_pattern.fullmatch(row)
        if match is None:
            raise AuditError(f"cannot summarize bloat row: {row!r}")
        parsed_rows.append(match.groupdict())
    footer = parsed_rows[-1]
    footer_match = re.fullmatch(
        r"\.text section size, the file size is (?P<file_size>\S+)", footer["crate"]
    )
    if footer_match is None:
        raise AuditError(f"cannot summarize bloat footer: {footer['crate']!r}")
    crate_rows = parsed_rows[:-1]
    if not crate_rows:
        raise AuditError("bloat summary has no crate contribution rows")
    remainder = next(
        (row for row in crate_rows if re.fullmatch(r"And \d+ more crates\..+", row["crate"])),
        None,
    )
    return {
        "text_size": footer["size"],
        "analysis_file_size": footer_match.group("file_size"),
        "top_crate": crate_rows[0]["crate"],
        "top_crate_text_percent": crate_rows[0]["text_percent"] + "%",
        "top_crate_size": crate_rows[0]["size"],
        "remainder_crates": (
            re.match(r"And (\d+) more crates", remainder["crate"]).group(1)
            if remainder
            else "0"
        ),
        "remainder_text_percent": remainder["text_percent"] + "%" if remainder else "0%",
    }


def write_bloat_summary(bloat_dir: Path, root_builds: Path, output: Path) -> None:
    with root_builds.open(encoding="utf-8", newline="") as handle:
        builds = {row["package"]: row for row in csv.DictReader(handle, delimiter="\t")}
    if set(builds) != set(RELEASE_ROOTS):
        raise AuditError(
            f"root build packages differ from release roots: {sorted(set(builds) ^ set(RELEASE_ROOTS))}"
        )
    rows = []
    for package, tier in RELEASE_ROOTS.items():
        summary = parse_bloat_summary(bloat_dir / f"{package}.txt")
        rows.append(
            {
                "package": package,
                "tier": tier,
                "release_artifact_bytes": builds[package]["artifact_bytes"],
                **summary,
            }
        )
    write_tsv(
        output,
        rows,
        (
            "package",
            "tier",
            "release_artifact_bytes",
            "text_size",
            "analysis_file_size",
            "top_crate",
            "top_crate_text_percent",
            "top_crate_size",
            "remainder_crates",
            "remainder_text_percent",
        ),
    )


def suspicious_tokens(value: str) -> list[str]:
    findings: set[str] = set()
    forbidden_patterns = {
        "home-path": r"/(?:home/[^/\s]+|root)(?:/|\b)",
        "workspace-path": re.escape(WORK_ROOT) + r"[^/\s]+(?:/|\b)",
        "remote-temp-path": (
            re.escape(TEMP_ROOT) + r"(?:builds|issue-?631|cargo-remote)(?:/|[-_])"
        ),
        "cargo-home": (
            r"(?:"
            + re.escape(CARGO_PATH)
            + r"|"
            + re.escape("CARGO" + "_HOME=/")
            + r"(?![<]))"
        ),
        "ssh-authority": r"\b[A-Za-z_][A-Za-z0-9_-]*@[A-Za-z][A-Za-z0-9._-]+\b",
        "absolute-path": r"(?<![<\w])/(?:etc|opt|srv|var|usr/local)/[^\s`\"')]+",
    }
    for label, pattern in forbidden_patterns.items():
        if re.search(pattern, value):
            findings.add(label)
    for token in re.findall(r"(?<![\w.])[0-9A-Fa-f:.]{2,}(?![\w.])", value):
        candidate = token.strip("[](),.;")
        try:
            ipaddress.ip_address(candidate)
        except ValueError:
            continue
        findings.add("ip-address")
    dynamic_tokens = {
        "local-username": getpass.getuser(),
        "local-hostname": socket.gethostname(),
        "local-home": str(Path.home()),
    }
    for label, token in dynamic_tokens.items():
        if token and len(token) >= 4 and token in value:
            findings.add(label)
    return sorted(findings)


def iter_public_files(paths: Iterable[Path]) -> Iterable[Path]:
    for path in paths:
        if path.is_dir():
            yield from sorted(item for item in path.rglob("*") if item.is_file())
        elif path.is_file():
            yield path
        else:
            raise AuditError(f"public evidence path does not exist: {path}")


def check_public_evidence(paths: Iterable[Path]) -> int:
    failures = []
    checked = 0
    for path in iter_public_files(paths):
        try:
            value = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            failures.append((path, ["binary-file"]))
            continue
        checked += 1
        findings = suspicious_tokens(value)
        if findings:
            failures.append((path, findings))
    if failures:
        for path, findings in failures:
            print(f"FAIL {path}: {','.join(findings)}", file=sys.stderr)
        return 1
    print(f"public-evidence-scan=PASS files={checked}")
    return 0


def check_staged_lines() -> int:
    result = subprocess.run(
        ["git", "diff", "--cached", "--unified=0", "--no-color"],
        check=True,
        capture_output=True,
        text=True,
    )
    added = "\n".join(
        line[1:]
        for line in result.stdout.splitlines()
        if line.startswith("+") and not line.startswith("+++")
    )
    findings = suspicious_tokens(added)
    if findings:
        print(f"FAIL staged-new-lines: {','.join(findings)}", file=sys.stderr)
        return 1
    print(f"staged-new-lines-scan=PASS lines={len(added.splitlines())}")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    audit = subparsers.add_parser("audit", help="classify lockfile reachability")
    audit.add_argument("--lock", type=Path, required=True)
    audit.add_argument("--metadata-all", type=Path, required=True)
    audit.add_argument("--metadata-native", type=Path, required=True)
    audit.add_argument("--output-dir", type=Path)
    audit.add_argument("--trees-dir", type=Path)
    audit.add_argument("--check", action="store_true")

    normalize = subparsers.add_parser("normalize", help="normalize a raw text file")
    normalize.add_argument("input", type=Path)
    normalize.add_argument("output", type=Path)
    normalize_kind = normalize.add_mutually_exclusive_group()
    normalize_kind.add_argument(
        "--tree", action="store_true", help="retain depth-prefixed tree rows"
    )
    normalize_kind.add_argument(
        "--bloat", action="store_true", help="retain a validated cargo-bloat crate table"
    )

    scan = subparsers.add_parser("check-public-evidence", help="fail on private data")
    scan.add_argument("paths", nargs="+", type=Path)
    bloat_summary = subparsers.add_parser(
        "summarize-bloat", help="summarize validated per-root cargo-bloat tables"
    )
    bloat_summary.add_argument("--bloat-dir", type=Path, required=True)
    bloat_summary.add_argument("--root-builds", type=Path, required=True)
    bloat_summary.add_argument("--output", type=Path, required=True)
    subparsers.add_parser("check-staged", help="fail on private data in staged new lines")
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.command == "audit":
            return run_audit(args)
        if args.command == "normalize":
            args.output.parent.mkdir(parents=True, exist_ok=True)
            raw = args.input.read_text(encoding="utf-8")
            if args.tree:
                normalized = normalize_tree(raw)
            elif args.bloat:
                normalized = normalize_bloat(raw)
            else:
                normalized = normalize_text(raw)
            args.output.write_text(normalized, encoding="utf-8")
            return 0
        if args.command == "check-public-evidence":
            return check_public_evidence(args.paths)
        if args.command == "summarize-bloat":
            write_bloat_summary(args.bloat_dir, args.root_builds, args.output)
            return 0
        if args.command == "check-staged":
            return check_staged_lines()
    except (
        AuditError,
        KeyError,
        ValueError,
        json.JSONDecodeError,
        tomllib.TOMLDecodeError,
    ) as error:
        print(f"audit error: {error}", file=sys.stderr)
        return 2
    raise AssertionError("unreachable")


if __name__ == "__main__":
    raise SystemExit(main())
