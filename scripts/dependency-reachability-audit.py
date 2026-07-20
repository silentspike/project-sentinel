#!/usr/bin/env python3
"""Classify Cargo.lock reachability and protect public dependency evidence."""

from __future__ import annotations

import argparse
import csv
import getpass
import hashlib
import io
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
    workspace_all: bool = False


@dataclass
class TreeGraph:
    packages: set[tuple[str, str, str]] = field(default_factory=set)
    normal_context: set[tuple[str, str, str]] = field(default_factory=set)
    build_context: set[tuple[str, str, str]] = field(default_factory=set)
    edges: set[
        tuple[tuple[str, str, str], tuple[str, str, str]]
    ] = field(default_factory=set)
    roots: set[tuple[str, str, str]] = field(default_factory=set)


TREE_LINE = re.compile(
    r"^(?P<depth>[0-9]+)(?P<name>[A-Za-z0-9_-]+) v(?P<version>[^\s]+)(?P<tail>.*)$"
)

COMPACT_TREE_FIELDS = (
    "record",
    "parent_name",
    "parent_version",
    "parent_source",
    "name",
    "version",
    "source",
    "normal_context",
    "build_context",
    "is_root",
)

WORKSPACE_SET_FIELDS = (
    "name",
    "version",
    "source",
    "native_normal_build",
    "native_all_edges",
    "all_targets_all_edges",
    "native_dev_context",
    "foreign_target_context",
)

EDGE_FIELDS = (
    "parent_name",
    "parent_version",
    "child_name",
    "child_version",
    "edge_contexts",
    "cargo_all_targets_active",
)

REVERSE_CLOSURE_FIELDS = (
    "duplicate_name",
    "duplicate_version",
    "child_name",
    "child_version",
    "parent_name",
    "parent_version",
    "edge_contexts",
    "parent_workspace_root",
    "parent_release_tier",
    "cargo_all_targets_active",
)


def parse_tree(
    path: Path,
    keys_by_name_version: Mapping[tuple[str, str], tuple[str, str, str]],
) -> TreeGraph:
    graph = TreeGraph()
    stack: dict[int, tuple[tuple[str, str, str], bool]] = {}
    matched = 0
    with path.open(encoding="utf-8", errors="replace") as handle:
        for physical_line in handle:
            for line in physical_line.replace("\r", "\n").splitlines():
                match = TREE_LINE.fullmatch(line.strip())
                if match is None:
                    continue
                matched += 1
                depth = int(match.group("depth"))
                pair = (match.group("name"), match.group("version"))
                key = keys_by_name_version.get(pair)
                if key is None:
                    raise AuditError(
                        f"tree package {pair[0]}@{pair[1]} is absent or ambiguous in Cargo.lock"
                    )
                parent_entry = stack.get(depth - 1) if depth > 0 else None
                if depth > 0 and parent_entry is None:
                    raise AuditError(f"tree depth jumps to {depth} at {pair[0]}@{pair[1]}")
                proc_macro = "(proc-macro)" in match.group("tail")
                build_context = proc_macro or bool(parent_entry and parent_entry[1])
                graph.packages.add(key)
                if build_context:
                    graph.build_context.add(key)
                else:
                    graph.normal_context.add(key)
                if depth == 0:
                    graph.roots.add(key)
                else:
                    graph.edges.add((parent_entry[0], key))
                stack[depth] = (key, build_context)
                for stale_depth in [value for value in stack if value > depth]:
                    del stack[stale_depth]
    if matched == 0 or not graph.roots:
        raise AuditError(f"tree output has no package rows: {path}")
    return graph


def compact_tree_rows(graph: TreeGraph) -> list[dict[str, str]]:
    rows = [
        {
            "record": "package",
            "name": key[0],
            "version": key[1],
            "source": display_source(key[2]),
            "normal_context": str(key in graph.normal_context).lower(),
            "build_context": str(key in graph.build_context).lower(),
            "is_root": str(key in graph.roots).lower(),
            "parent_name": "",
            "parent_version": "",
            "parent_source": "",
        }
        for key in sorted(graph.packages)
    ]
    rows.extend(
        {
            "record": "edge",
            "parent_name": parent[0],
            "parent_version": parent[1],
            "parent_source": display_source(parent[2]),
            "name": child[0],
            "version": child[1],
            "source": display_source(child[2]),
            "normal_context": "false",
            "build_context": "false",
            "is_root": "false",
        }
        for parent, child in sorted(graph.edges)
    )
    return rows


def compact_tree_text(graph: TreeGraph) -> str:
    return tsv_text(compact_tree_rows(graph), COMPACT_TREE_FIELDS)


def load_compact_tree(
    path: Path,
    keys_by_name_version: Mapping[tuple[str, str], tuple[str, str, str]],
) -> TreeGraph:
    with path.open(encoding="utf-8", newline="") as handle:
        reader = csv.DictReader(handle, delimiter="\t")
        if tuple(reader.fieldnames or ()) != COMPACT_TREE_FIELDS:
            raise AuditError(f"invalid compact tree header: {path}")
        rows = list(reader)

    graph = TreeGraph()
    package_rows = set()
    edge_rows = set()

    def resolve(row: Mapping[str, str], prefix: str = "") -> tuple[str, str, str]:
        name = row[f"{prefix}name"]
        version = row[f"{prefix}version"]
        source = row[f"{prefix}source"]
        key = keys_by_name_version.get((name, version))
        if key is None or display_source(key[2]) != source:
            raise AuditError(
                f"compact tree package {name}@{version} ({source}) is absent from Cargo.lock"
            )
        return key

    for row in rows:
        record = row["record"]
        if record == "package":
            key = resolve(row)
            if key in package_rows:
                raise AuditError(f"duplicate compact tree package row: {key!r}")
            package_rows.add(key)
            for field_name in ("normal_context", "build_context", "is_root"):
                if row[field_name] not in {"true", "false"}:
                    raise AuditError(
                        f"invalid compact tree boolean {field_name}={row[field_name]!r}"
                    )
            if any(
                row[field_name]
                for field_name in ("parent_name", "parent_version", "parent_source")
            ):
                raise AuditError(f"compact package row has parent fields: {key!r}")
            if row["normal_context"] == "false" and row["build_context"] == "false":
                raise AuditError(f"compact tree package lacks a dependency context: {key!r}")
            graph.packages.add(key)
            if row["normal_context"] == "true":
                graph.normal_context.add(key)
            if row["build_context"] == "true":
                graph.build_context.add(key)
            if row["is_root"] == "true":
                graph.roots.add(key)
        elif record == "edge":
            child = resolve(row)
            parent = resolve(row, "parent_")
            edge = (parent, child)
            if edge in edge_rows:
                raise AuditError(f"duplicate compact tree edge row: {edge!r}")
            edge_rows.add(edge)
            if any(
                row[field_name] != "false"
                for field_name in ("normal_context", "build_context", "is_root")
            ):
                raise AuditError(f"compact edge row has package flags: {edge!r}")
            graph.edges.add(edge)
        else:
            raise AuditError(f"invalid compact tree record {record!r}: {path}")

    if not graph.packages or not graph.roots:
        raise AuditError(f"compact tree has no packages or roots: {path}")
    edge_packages = {key for edge in graph.edges for key in edge}
    if not edge_packages <= graph.packages:
        raise AuditError(
            f"compact tree edges reference packages without package rows: {path}"
        )
    forward = defaultdict(set)
    for parent, child in graph.edges:
        forward[parent].add(child)
    reached = set(graph.roots)
    queue = deque(graph.roots)
    while queue:
        for child in forward[queue.popleft()]:
            if child not in reached:
                reached.add(child)
                queue.append(child)
    if reached != graph.packages:
        raise AuditError(
            f"compact tree contains packages unreachable from its roots: {path}"
        )
    return graph


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
        self.workspace_ids = set(str(value) for value in metadata_all["workspace_members"])
        self.workspace_keys = {self.key_for_id[value] for value in self.workspace_ids}
        self.root_ids = self._resolve_roots()
        self.root_keys = {
            name: self.key_for_id[package_id] for name, package_id in self.root_ids.items()
        }
        pairs: dict[tuple[str, str], list[tuple[str, str, str]]] = defaultdict(list)
        for key in self.lock_packages:
            pairs[(key[0], key[1])].append(key)
        ambiguous = {pair: keys for pair, keys in pairs.items() if len(keys) != 1}
        if ambiguous:
            raise AuditError(f"ambiguous Cargo.lock name/version identities: {ambiguous!r}")
        self.keys_by_name_version = {pair: keys[0] for pair, keys in pairs.items()}
        self.root_normal_graphs: dict[str, TreeGraph] = {}
        self.root_combined_graphs: dict[str, TreeGraph] = {}
        self.workspace_sets: dict[str, set[tuple[str, str, str]]] = {}
        self.all_target_edges: dict[
            tuple[tuple[str, str, str], tuple[str, str, str]], set[str]
        ] = {}
        self.active_all_target_edges: set[
            tuple[tuple[str, str, str], tuple[str, str, str]]
        ] = set()
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
    def _edge_specs(dep: Mapping[str, object]) -> set[tuple[str, str]]:
        specs = set()
        for item in dep.get("dep_kinds", []):
            specs.add((str(item.get("kind") or "normal"), str(item.get("target") or "")))
        return specs or {("normal", "")}

    def load_release_trees(self, trees_dir: Path, *, compact: bool) -> None:
        loader = load_compact_tree if compact else parse_tree
        suffix = ".graph.tsv" if compact else ".txt"
        for root_name, root_key in self.root_keys.items():
            normal = loader(
                trees_dir / f"{root_name}.normal{suffix}", self.keys_by_name_version
            )
            combined = loader(
                trees_dir / f"{root_name}.normal-build{suffix}",
                self.keys_by_name_version,
            )
            if normal.roots != {root_key} or combined.roots != {root_key}:
                raise AuditError(f"{root_name} trees do not resolve exactly to their root")
            if not normal.packages <= combined.packages:
                raise AuditError(f"{root_name} normal tree is not a subset of normal+build")
            self.root_normal_graphs[root_name] = normal
            self.root_combined_graphs[root_name] = combined

    def set_workspace_sets(
        self,
        native_normal_build: set[tuple[str, str, str]],
        native_all_edges: set[tuple[str, str, str]],
        all_targets_all_edges: set[tuple[str, str, str]],
        native_dev_context: set[tuple[str, str, str]] | None = None,
        foreign_target_context: set[tuple[str, str, str]] | None = None,
    ) -> None:
        if not native_normal_build <= native_all_edges:
            raise AuditError("native normal+build set is not a subset of native all-edge set")
        if not native_all_edges <= all_targets_all_edges:
            raise AuditError("native all-edge set is not a subset of all-target set")
        if not all_targets_all_edges <= set(self.lock_packages):
            raise AuditError("workspace trees contain packages absent from Cargo.lock")
        native_dev_context = (
            native_dev_context
            if native_dev_context is not None
            else native_all_edges - native_normal_build
        )
        foreign_target_context = (
            foreign_target_context
            if foreign_target_context is not None
            else all_targets_all_edges - native_all_edges
        )
        if not native_dev_context <= native_all_edges:
            raise AuditError("dev context contains packages absent from native all-edge tree")
        if not foreign_target_context <= all_targets_all_edges:
            raise AuditError("foreign context contains packages absent from all-target tree")
        missing_workspace = self.workspace_keys - native_normal_build
        if missing_workspace:
            raise AuditError(f"native workspace tree misses members: {missing_workspace!r}")
        self.workspace_sets = {
            "native_normal_build": native_normal_build,
            "native_all_edges": native_all_edges,
            "all_targets_all_edges": all_targets_all_edges,
            "native_dev_context": native_dev_context,
            "foreign_target_context": foreign_target_context,
        }

    def load_workspace_tree_sets(
        self,
        native_build: Path,
        native_all: Path,
        native_dev: Path,
        all_targets: Path,
        *,
        compact: bool = False,
    ) -> TreeGraph:
        loader = load_compact_tree if compact else parse_tree
        native_build_graph = loader(native_build, self.keys_by_name_version)
        native_all_graph = loader(native_all, self.keys_by_name_version)
        native_dev_graph = loader(native_dev, self.keys_by_name_version)
        all_targets_graph = loader(all_targets, self.keys_by_name_version)
        return self.set_workspace_graphs(
            native_build_graph,
            native_all_graph,
            native_dev_graph,
            all_targets_graph,
        )

    def set_workspace_graphs(
        self,
        native_build_graph: TreeGraph,
        native_all_graph: TreeGraph,
        native_dev_graph: TreeGraph,
        all_targets_graph: TreeGraph,
    ) -> TreeGraph:
        if native_dev_graph.roots != self.workspace_keys:
            raise AuditError("native dev tree roots differ from workspace members")
        native_forward = defaultdict(set)
        for parent, child in native_all_graph.edges:
            native_forward[parent].add(child)
        dev_context = set(native_dev_graph.packages - native_dev_graph.roots)
        queue = deque(dev_context)
        while queue:
            parent = queue.popleft()
            for child in native_forward[parent]:
                if child not in dev_context:
                    dev_context.add(child)
                    queue.append(child)

        native_specs = set()
        for parent_id, node in self.native_nodes.items():
            for dep in node.get("deps", []):
                for kind, target in self._edge_specs(dep):
                    native_specs.add(
                        (
                            self.key_for_id[parent_id],
                            self.key_for_id[str(dep["pkg"])],
                            kind,
                            target,
                        )
                    )
        foreign_edges = set()
        for parent, child in all_targets_graph.edges:
            all_specs = {
                (parent, child, kind, target)
                for kind, target in self._edge_specs_for_keys(parent, child)
            }
            if all_specs - native_specs:
                foreign_edges.add((parent, child))
        all_forward = defaultdict(set)
        for parent, child in all_targets_graph.edges:
            all_forward[parent].add(child)
        foreign_context = set()
        queue = deque((root, False) for root in self.workspace_keys)
        visited = set()
        while queue:
            parent, inherited_foreign = queue.popleft()
            if (parent, inherited_foreign) in visited:
                continue
            visited.add((parent, inherited_foreign))
            if inherited_foreign:
                foreign_context.add(parent)
            for child in all_forward[parent]:
                queue.append(
                    (child, inherited_foreign or (parent, child) in foreign_edges)
                )
        self.set_workspace_sets(
            native_build_graph.packages,
            native_all_graph.packages,
            all_targets_graph.packages,
            dev_context,
            foreign_context,
        )
        for label, graph in (
            ("native normal+build", native_build_graph),
            ("native all-edge", native_all_graph),
            ("all-target", all_targets_graph),
        ):
            if graph.roots != self.workspace_keys:
                raise AuditError(f"{label} tree roots differ from workspace members")
        return all_targets_graph

    def write_compact_sources(
        self,
        trees_dir: Path,
        workspace_graphs: Sequence[tuple[str, TreeGraph]],
    ) -> None:
        trees_dir.mkdir(parents=True, exist_ok=True)
        for root_name in RELEASE_ROOTS:
            for label, graph in (
                ("normal", self.root_normal_graphs[root_name]),
                ("normal-build", self.root_combined_graphs[root_name]),
            ):
                path = trees_dir / f"{root_name}.{label}.graph.tsv"
                path.write_text(compact_tree_text(graph), encoding="utf-8")
        for label, graph in workspace_graphs:
            path = trees_dir / f"workspace-{label}.graph.tsv"
            path.write_text(compact_tree_text(graph), encoding="utf-8")

    def _edge_specs_for_keys(self, parent_key, child_key) -> set[tuple[str, str]]:
        parent_ids = [value for value, key in self.key_for_id.items() if key == parent_key]
        child_ids = {value for value, key in self.key_for_id.items() if key == child_key}
        specs = set()
        for parent_id in parent_ids:
            for dep in self.all_nodes[parent_id].get("deps", []):
                if str(dep["pkg"]) in child_ids:
                    specs.update(self._edge_specs(dep))
        return specs

    def classify(self) -> list[dict[str, str]]:
        if set(self.root_normal_graphs) != set(RELEASE_ROOTS):
            raise AuditError("release trees are not loaded for every root")
        if set(self.workspace_sets) != {
            "native_normal_build",
            "native_all_edges",
            "all_targets_all_edges",
            "native_dev_context",
            "foreign_target_context",
        }:
            raise AuditError("workspace reachability sets are not loaded")

        for root_name in RELEASE_ROOTS:
            normal = self.root_normal_graphs[root_name]
            combined = self.root_combined_graphs[root_name]
            for key in normal.normal_context:
                self.memberships[key].release_normal.add(root_name)
            for key in normal.build_context | (combined.packages - normal.packages):
                self.memberships[key].release_build.add(root_name)

        release_union = set().union(
            *(graph.packages for graph in self.root_combined_graphs.values())
        )
        for key in self.workspace_sets["native_normal_build"] - release_union:
            self.memberships[key].non_release_workspace = True
        for key in self.workspace_sets["native_dev_context"]:
            self.memberships[key].dev = True
        for key in self.workspace_sets["foreign_target_context"]:
            self.memberships[key].foreign_target = True
        for key in self.workspace_sets["all_targets_all_edges"]:
            self.memberships[key].workspace_all = True

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
            elif not membership.workspace_all:
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
            direct_tree_keys = {
                child
                for parent, child in self.root_normal_graphs[root_name].edges
                if parent == self.root_keys[root_name]
            }
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
                if self.key_for_id[dep_id] not in direct_tree_keys:
                    continue
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

    def set_all_target_edges(self, graph: TreeGraph) -> None:
        metadata_edges: dict[
            tuple[tuple[str, str, str], tuple[str, str, str]], set[str]
        ] = defaultdict(set)
        for parent_id, node in self.all_nodes.items():
            parent_key = self.key_for_id[parent_id]
            for dep in node.get("deps", []):
                child_key = self.key_for_id[str(dep["pkg"])]
                for kind, target in self._edge_specs(dep):
                    metadata_edges[(parent_key, child_key)].add(
                        f"{kind}:{target or 'all'}"
                    )
        missing = graph.edges - set(metadata_edges)
        if missing:
            raise AuditError(f"active Cargo edges lack metadata annotation: {missing!r}")
        self.all_target_edges = dict(metadata_edges)
        self.active_all_target_edges = set(graph.edges)

    def workspace_set_rows(self) -> list[dict[str, str]]:
        return [
            {
                "name": key[0],
                "version": key[1],
                "source": display_source(key[2]),
                "native_normal_build": str(
                    key in self.workspace_sets["native_normal_build"]
                ).lower(),
                "native_all_edges": str(
                    key in self.workspace_sets["native_all_edges"]
                ).lower(),
                "all_targets_all_edges": str(
                    key in self.workspace_sets["all_targets_all_edges"]
                ).lower(),
                "native_dev_context": str(
                    key in self.workspace_sets["native_dev_context"]
                ).lower(),
                "foreign_target_context": str(
                    key in self.workspace_sets["foreign_target_context"]
                ).lower(),
            }
            for key in sorted(self.lock_packages)
        ]

    def edge_rows(self) -> list[dict[str, str]]:
        return [
            {
                "parent_name": parent[0],
                "parent_version": parent[1],
                "child_name": child[0],
                "child_version": child[1],
                "edge_contexts": ";".join(sorted(contexts)),
                "cargo_all_targets_active": str(
                    (parent, child) in self.active_all_target_edges
                ).lower(),
            }
            for (parent, child), contexts in sorted(self.all_target_edges.items())
        ]

    def duplicate_rows_and_closure(
        self,
    ) -> tuple[list[dict[str, str]], list[dict[str, str]]]:
        by_name: dict[str, list[dict[str, str]]] = defaultdict(list)
        for row in self.rows:
            by_name[row["name"]].append(row)
        if not self.all_target_edges:
            raise AuditError("all-target edge graph is not loaded")
        reverse_all: dict[tuple[str, str, str], set[tuple[str, str, str]]] = defaultdict(set)
        reverse_active: dict[
            tuple[str, str, str], set[tuple[str, str, str]]
        ] = defaultdict(set)
        for parent_key, child_key in self.all_target_edges:
            reverse_all[child_key].add(parent_key)
            if (parent_key, child_key) in self.active_all_target_edges:
                reverse_active[child_key].add(parent_key)
        result = []
        closure_rows = []
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
                if len(key_matches) != 1:
                    raise AuditError(f"duplicate row identity is ambiguous: {name}@{row['version']}")
                duplicate_key = key_matches[0]
                active_duplicate = duplicate_key in self.workspace_sets["all_targets_all_edges"]
                reverse = reverse_active if active_duplicate else reverse_all
                closure_basis = "active-all-target-tree" if active_duplicate else "disabled-metadata"
                immediate = reverse[duplicate_key]
                queue = deque([duplicate_key])
                visited = {duplicate_key}
                closure_edges = set()
                reached_roots = set()
                while queue:
                    child_key = queue.popleft()
                    if child_key in self.workspace_keys:
                        reached_roots.add(child_key)
                    for parent_key in reverse[child_key]:
                        closure_edges.add((parent_key, child_key))
                        if parent_key not in visited:
                            visited.add(parent_key)
                            queue.append(parent_key)
                if not reached_roots or not closure_edges:
                    raise AuditError(
                        f"duplicate version lacks complete workspace-root closure: {name}@{row['version']}"
                    )
                root_names = sorted(key[0] for key in reached_roots)
                release_root_names = sorted(set(root_names) & set(RELEASE_ROOTS))
                for parent_key, child_key in sorted(closure_edges):
                    closure_rows.append(
                        {
                            "duplicate_name": name,
                            "duplicate_version": row["version"],
                            "child_name": child_key[0],
                            "child_version": child_key[1],
                            "parent_name": parent_key[0],
                            "parent_version": parent_key[1],
                            "edge_contexts": ";".join(
                                sorted(self.all_target_edges[(parent_key, child_key)])
                            ),
                            "cargo_all_targets_active": str(
                                (parent_key, child_key) in self.active_all_target_edges
                            ).lower(),
                            "parent_workspace_root": str(
                                parent_key in self.workspace_keys
                            ).lower(),
                            "parent_release_tier": RELEASE_ROOTS.get(parent_key[0], ""),
                        }
                    )
                result.append(
                    {
                        "name": name,
                        "version": row["version"],
                        "primary_reachability": row["primary_reachability"],
                        "reachable_in_roots": row["reachable_in_roots"],
                        "immediate_forcers": ",".join(
                            sorted(f"{key[0]}@{key[1]}" for key in immediate)
                        ),
                        "workspace_roots": ",".join(root_names),
                        "release_roots": ",".join(release_root_names),
                        "closure_edges": str(len(closure_edges)),
                        "closure_basis": closure_basis,
                        "decision": group_decision,
                        "constraint_assessment": constraint_assessment,
                        "revisit_condition": revisit_condition,
                    }
                )
        closure_rows.sort(
            key=lambda item: (
                item["duplicate_name"],
                item["duplicate_version"],
                item["child_name"],
                item["child_version"],
                item["parent_name"],
                item["parent_version"],
            )
        )
        return result, closure_rows


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


def tsv_text(rows: Sequence[Mapping[str, str]], fields: Sequence[str]) -> str:
    handle = io.StringIO(newline="")
    writer = csv.DictWriter(handle, fieldnames=fields, delimiter="\t", lineterminator="\n")
    writer.writeheader()
    writer.writerows(rows)
    return handle.getvalue()


def read_tsv(path: Path) -> list[dict[str, str]]:
    with path.open(encoding="utf-8", newline="") as handle:
        return list(csv.DictReader(handle, delimiter="\t"))


def summary_text(rows: Sequence[Mapping[str, str]], root_count: int) -> str:
    counts = {category: 0 for category in PRIMARY_ORDER}
    for row in rows:
        counts[row["primary_reachability"]] += 1
    lines = [
        f"lockfile_packages={len(rows)}",
        f"classified_primary={sum(counts.values())}",
        "unclassified=0",
        "duplicate_primary_assignments=0",
        f"release_roots_resolved={root_count}/{len(RELEASE_ROOTS)}",
        f"also_dev={sum(row['also_dev'] == 'true' for row in rows)}",
        f"also_foreign_target={sum(row['also_foreign_target'] == 'true' for row in rows)}",
    ]
    lines.extend(f"{category}={counts[category]}" for category in PRIMARY_ORDER)
    return "\n".join(lines) + "\n"


def verify_exact(path: Path, expected: str) -> None:
    actual = path.read_text(encoding="utf-8")
    if actual != expected:
        raise AuditError(f"committed evidence differs from recomputed result: {path}")


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


def generated_outputs(
    audit: ReachabilityAudit, trees_dir: Path
) -> tuple[dict[str, str], int, int, int]:
    rows = audit.classify()
    feature_rows = add_release_tree_features(audit.direct_feature_rows(), trees_dir)
    feature_fields = (
        "root",
        "tier",
        "dependency",
        "version",
        "requested_features",
        "default_features",
        "release_features",
        "metadata_union_features",
        "manifest",
    )
    duplicate_rows, closure_rows = audit.duplicate_rows_and_closure()
    duplicate_fields = (
        "name",
        "version",
        "primary_reachability",
        "reachable_in_roots",
        "immediate_forcers",
        "workspace_roots",
        "release_roots",
        "closure_edges",
        "closure_basis",
        "decision",
        "constraint_assessment",
        "revisit_condition",
    )
    generated = {
        "reachability.tsv": tsv_text(rows, TSV_FIELDS),
        "reachability-summary.txt": summary_text(rows, len(audit.root_ids)),
        "direct-release-features.tsv": tsv_text(feature_rows, feature_fields),
        "duplicate-versions.tsv": tsv_text(duplicate_rows, duplicate_fields),
        "workspace-reachability-sets.tsv": tsv_text(
            audit.workspace_set_rows(), WORKSPACE_SET_FIELDS
        ),
        "workspace-all-target-edges.tsv": tsv_text(audit.edge_rows(), EDGE_FIELDS),
        "duplicates/reverse-closure.tsv": tsv_text(
            closure_rows, REVERSE_CLOSURE_FIELDS
        ),
    }
    return generated, len(rows), len(duplicate_rows), len(closure_rows)


def load_json(path: Path) -> Mapping[str, object]:
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def run_audit(args: argparse.Namespace) -> int:
    if args.check and args.compact_sources_only:
        raise AuditError("--check and --compact-sources-only are mutually exclusive")
    with args.lock.open("rb") as handle:
        lock_data = tomllib.load(handle)
    audit = ReachabilityAudit(
        lock_data,
        load_json(args.metadata_all),
        load_json(args.metadata_native),
    )
    if args.output_dir is None and not args.compact_sources_only:
        raise AuditError("--output-dir is required")
    if args.trees_dir is None:
        raise AuditError("--trees-dir is required")
    if args.check:
        audit.load_release_trees(args.trees_dir, compact=True)
        workspace_paths = (
            args.trees_dir / "workspace-native-normal-build.graph.tsv",
            args.trees_dir / "workspace-native-all.graph.tsv",
            args.trees_dir / "workspace-native-dev.graph.tsv",
            args.trees_dir / "workspace-all-targets.graph.tsv",
        )
        all_targets_graph = audit.load_workspace_tree_sets(
            *workspace_paths, compact=True
        )
        audit.set_all_target_edges(all_targets_graph)
    else:
        if args.raw_trees_dir is None:
            raise AuditError("generation requires --raw-trees-dir")
        audit.load_release_trees(args.raw_trees_dir, compact=False)
        raw_paths = (
            args.workspace_native_build_tree,
            args.workspace_native_all_tree,
            args.workspace_native_dev_tree,
            args.workspace_all_targets_tree,
        )
        if any(path is None for path in raw_paths):
            raise AuditError(
                "generation requires all four --workspace-*-tree inputs"
            )
        workspace_graphs = tuple(
            (label, parse_tree(path, audit.keys_by_name_version))
            for label, path in zip(
                ("native-normal-build", "native-all", "native-dev", "all-targets"),
                raw_paths,
                strict=True,
            )
        )
        all_targets_graph = audit.set_workspace_graphs(
            *(graph for _, graph in workspace_graphs)
        )
        audit.set_all_target_edges(all_targets_graph)
        audit.write_compact_sources(args.trees_dir, workspace_graphs)
        if args.compact_sources_only:
            graph_paths = sorted(args.trees_dir.glob("*.graph.tsv"))
            graph_rows = sum(
                max(len(path.read_text(encoding="utf-8").splitlines()) - 1, 0)
                for path in graph_paths
            )
            print(
                f"compact_graph_files={len(graph_paths)} "
                f"compact_graph_rows={graph_rows}"
            )
            return 0

    generated, row_count, duplicate_count, closure_count = generated_outputs(
        audit, args.trees_dir
    )
    if args.check:
        for relative_path, expected in generated.items():
            verify_exact(args.output_dir / relative_path, expected)
        print(
            f"coverage={row_count}/{len(audit.lock_packages)} unclassified=0 "
            f"roots={len(audit.root_ids)}/{len(RELEASE_ROOTS)} "
            f"duplicate_versions={duplicate_count} "
            f"closure_rows={closure_count} evidence_match=PASS"
        )
        return 0
    for relative_path, value in generated.items():
        path = args.output_dir / relative_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value, encoding="utf-8")
    print(
        f"wrote {row_count} packages, {duplicate_count} duplicate-version rows, and "
        f"{closure_count} reverse-closure rows"
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
    audit.add_argument("--raw-trees-dir", type=Path)
    audit.add_argument("--workspace-native-build-tree", type=Path)
    audit.add_argument("--workspace-native-all-tree", type=Path)
    audit.add_argument("--workspace-native-dev-tree", type=Path)
    audit.add_argument("--workspace-all-targets-tree", type=Path)
    audit.add_argument("--check", action="store_true")
    audit.add_argument("--compact-sources-only", action="store_true")

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
