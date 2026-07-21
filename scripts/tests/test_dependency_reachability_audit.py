import contextlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock


SCRIPT = Path(__file__).parents[1] / "dependency-reachability-audit.py"
SPEC = importlib.util.spec_from_file_location("dependency_audit", SCRIPT)
audit_module = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = audit_module
SPEC.loader.exec_module(audit_module)


ROOTS = tuple(audit_module.RELEASE_ROOTS)
REMOTE_PROJECT = "/" + "tmp" + "/" + "builds" + "/123"


def package(name, version="1.0.0", *, proc_macro=False):
    target_kind = ["proc-macro"] if proc_macro else ["lib"]
    return {
        "id": f"registry+test#{name}@{version}",
        "name": name,
        "version": version,
        "source": "registry+test",
        "targets": [{"kind": target_kind}],
        "manifest_path": f"{REMOTE_PROJECT}/{name}/Cargo.toml",
        "dependencies": [],
    }


def workspace_package(name):
    value = package(name)
    value["id"] = f"path+file://{REMOTE_PROJECT}/{name}#0.1.0"
    value["version"] = "0.1.0"
    value["source"] = None
    value["manifest_path"] = f"{REMOTE_PROJECT}/services/{name}/Cargo.toml"
    return value


def dependency(package_value, kind="normal", target=None, name=None):
    return {
        "name": name or package_value["name"].replace("-", "_"),
        "pkg": package_value["id"],
        "dep_kinds": [{"kind": kind, "target": target}],
    }


def node(package_value, deps=(), features=()):
    return {"id": package_value["id"], "deps": list(deps), "features": list(features)}


def fixture():
    roots = [workspace_package(name) for name in ROOTS]
    other_root = workspace_package("internal-tool")
    normal = package("normal-dep")
    build = package("build-dep")
    build_child = package("build-child")
    proc_macro = package("derive-dep", proc_macro=True)
    dev = package("dev-dep")
    non_release = package("internal-only")
    foreign = package("windows-only")
    optional = package("disabled-optional")
    split_one = package("split-dep", "1.0.0")
    split_two = package("split-dep", "2.0.0")

    roots[0]["dependencies"] = [
        {
            "name": normal["name"],
            "rename": None,
            "kind": "normal",
            "uses_default_features": True,
            "features": ["tls"],
        },
        {
            "name": optional["name"],
            "rename": None,
            "kind": "normal",
            "optional": True,
            "uses_default_features": True,
            "features": [],
        },
    ]

    native_packages = roots + [
        other_root,
        normal,
        build,
        build_child,
        proc_macro,
        dev,
        non_release,
        optional,
        split_one,
        split_two,
    ]
    all_packages = native_packages + [foreign]
    root_zero_deps = [
        dependency(normal),
        dependency(build, "build"),
        dependency(proc_macro),
        dependency(dev, "dev"),
        dependency(optional),
        dependency(split_one),
    ]
    native_nodes = [node(roots[0], root_zero_deps)]
    native_nodes.append(node(roots[1], [dependency(split_two)]))
    native_nodes.extend(node(root) for root in roots[2:])
    native_nodes.extend(
        [
            node(other_root, [dependency(non_release)]),
            node(normal, features=["default", "tls"]),
            node(build, [dependency(build_child)]),
            node(build_child),
            node(proc_macro),
            node(dev),
            node(non_release),
            node(optional),
            node(split_one),
            node(split_two),
        ]
    )
    all_nodes = [dict(item) for item in native_nodes]
    all_nodes[0] = node(roots[0], root_zero_deps + [dependency(foreign, target="cfg(windows)")])
    all_nodes.append(node(foreign))

    workspace_members = [item["id"] for item in roots + [other_root]]
    metadata_native = {
        "packages": native_packages,
        "workspace_members": workspace_members,
        "resolve": {"nodes": native_nodes},
    }
    metadata_all = {
        "packages": all_packages,
        "workspace_members": workspace_members,
        "resolve": {"nodes": all_nodes},
    }
    lock_packages = []
    for item in all_packages:
        lock_packages.append(
            {
                "name": item["name"],
                "version": item["version"],
                "source": item["source"],
            }
        )
    return {"package": lock_packages}, metadata_all, metadata_native


def prepared_audit():
    lock, metadata_all, metadata_native = fixture()
    result = audit_module.ReachabilityAudit(lock, metadata_all, metadata_native)
    key = result.keys_by_name_version
    roots = result.root_keys

    normal = key[("normal-dep", "1.0.0")]
    build = key[("build-dep", "1.0.0")]
    build_child = key[("build-child", "1.0.0")]
    proc_macro = key[("derive-dep", "1.0.0")]
    dev = key[("dev-dep", "1.0.0")]
    non_release = key[("internal-only", "1.0.0")]
    foreign = key[("windows-only", "1.0.0")]
    optional = key[("disabled-optional", "1.0.0")]
    split_one = key[("split-dep", "1.0.0")]
    split_two = key[("split-dep", "2.0.0")]
    other_root = key[("internal-tool", "0.1.0")]

    for root_name, root_key in roots.items():
        normal_graph = audit_module.TreeGraph(
            packages={root_key}, normal_context={root_key}, roots={root_key}
        )
        combined_graph = audit_module.TreeGraph(
            packages={root_key}, normal_context={root_key}, roots={root_key}
        )
        if root_name == ROOTS[0]:
            normal_graph.packages |= {normal, proc_macro, split_one}
            normal_graph.normal_context |= {normal, split_one}
            normal_graph.build_context.add(proc_macro)
            normal_graph.edges |= {
                (root_key, normal),
                (root_key, proc_macro),
                (root_key, split_one),
            }
            combined_graph.packages |= {
                normal,
                proc_macro,
                split_one,
                build,
                build_child,
            }
            combined_graph.normal_context |= {normal, split_one, build, build_child}
            combined_graph.build_context.add(proc_macro)
            combined_graph.edges |= normal_graph.edges | {
                (root_key, build),
                (build, build_child),
            }
        elif root_name == ROOTS[1]:
            normal_graph.packages.add(split_two)
            normal_graph.normal_context.add(split_two)
            normal_graph.edges.add((root_key, split_two))
            combined_graph.packages.add(split_two)
            combined_graph.normal_context.add(split_two)
            combined_graph.edges.add((root_key, split_two))
        result.root_normal_graphs[root_name] = normal_graph
        result.root_combined_graphs[root_name] = combined_graph

    native_build = (
        set(roots.values())
        | {other_root, normal, build, build_child, proc_macro, non_release, split_one, split_two}
    )
    native_all = native_build | {dev}
    all_targets = native_all | {foreign}
    result.set_workspace_sets(
        native_build,
        native_all,
        all_targets,
        {dev, normal},
        {foreign, normal},
    )
    active_edges = {
        (roots[ROOTS[0]], normal),
        (roots[ROOTS[0]], build),
        (roots[ROOTS[0]], proc_macro),
        (roots[ROOTS[0]], dev),
        (roots[ROOTS[0]], foreign),
        (roots[ROOTS[0]], split_one),
        (roots[ROOTS[1]], split_two),
        (build, build_child),
        (other_root, non_release),
    }
    result.set_all_target_edges(
        audit_module.TreeGraph(
            packages=all_targets,
            edges=active_edges,
            roots=set(roots.values()) | {other_root},
        )
    )
    return result, optional


def write_check_fixture(base: Path) -> SimpleNamespace:
    lock, metadata_all, metadata_native = fixture()
    lock_lines = []
    for package_value in lock["package"]:
        lock_lines.extend(
            [
                "[[package]]",
                f'name = "{package_value["name"]}"',
                f'version = "{package_value["version"]}"',
            ]
        )
        if package_value["source"] is not None:
            lock_lines.append(f'source = "{package_value["source"]}"')
        lock_lines.append("")
    lock_path = base / "Cargo.lock"
    lock_path.write_text("\n".join(lock_lines), encoding="utf-8")
    metadata_all_path = base / "metadata-all.json"
    metadata_native_path = base / "metadata-native.json"
    metadata_all_path.write_text(json.dumps(metadata_all), encoding="utf-8")
    metadata_native_path.write_text(json.dumps(metadata_native), encoding="utf-8")

    source, _ = prepared_audit()
    trees_dir = base / "trees"
    trees_dir.mkdir()
    for root in ROOTS:
        (trees_dir / f"{root}.normal.graph.tsv").write_text(
            audit_module.compact_tree_text(source.root_normal_graphs[root]),
            encoding="utf-8",
        )
        (trees_dir / f"{root}.normal-build.graph.tsv").write_text(
            audit_module.compact_tree_text(source.root_combined_graphs[root]),
            encoding="utf-8",
        )
        (trees_dir / f"{root}.features.txt").write_text("", encoding="utf-8")

    roots = set(source.workspace_keys)
    native_build = source.workspace_sets["native_normal_build"]
    native_all = source.workspace_sets["native_all_edges"]
    all_targets = source.workspace_sets["all_targets_all_edges"]
    dev_packages = roots | source.workspace_sets["native_dev_context"]

    def graph(packages, edges):
        return audit_module.TreeGraph(
            packages=set(packages),
            normal_context=set(packages),
            edges=set(edges),
            roots=roots,
        )

    workspace_graphs = {
        "native-normal-build": graph(
            native_build,
            {
                edge
                for edge in source.active_all_target_edges
                if edge[0] in native_build and edge[1] in native_build
            },
        ),
        "native-all": graph(
            native_all,
            {
                edge
                for edge in source.active_all_target_edges
                if edge[0] in native_all and edge[1] in native_all
            },
        ),
        "native-dev": graph(
            dev_packages,
            {
                edge
                for edge in source.active_all_target_edges
                if edge[0] in dev_packages and edge[1] in dev_packages
            },
        ),
        "all-targets": graph(all_targets, source.active_all_target_edges),
    }
    for label, graph_value in workspace_graphs.items():
        (trees_dir / f"workspace-{label}.graph.tsv").write_text(
            audit_module.compact_tree_text(graph_value), encoding="utf-8"
        )

    verifier = audit_module.ReachabilityAudit(lock, metadata_all, metadata_native)
    verifier.load_release_trees(trees_dir, compact=True)
    all_targets_graph = verifier.load_workspace_tree_sets(
        trees_dir / "workspace-native-normal-build.graph.tsv",
        trees_dir / "workspace-native-all.graph.tsv",
        trees_dir / "workspace-native-dev.graph.tsv",
        trees_dir / "workspace-all-targets.graph.tsv",
        compact=True,
    )
    verifier.set_all_target_edges(all_targets_graph)
    generated, _, _, _ = audit_module.generated_outputs(verifier, trees_dir)
    output_dir = base / "output"
    for relative_path, value in generated.items():
        path = output_dir / relative_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(value, encoding="utf-8")

    return SimpleNamespace(
        lock=lock_path,
        metadata_all=metadata_all_path,
        metadata_native=metadata_native_path,
        output_dir=output_dir,
        trees_dir=trees_dir,
        raw_trees_dir=None,
        workspace_native_build_tree=None,
        workspace_native_all_tree=None,
        workspace_native_dev_tree=None,
        workspace_all_targets_tree=None,
        check=True,
        compact_sources_only=False,
    )


class ReachabilityTests(unittest.TestCase):
    def test_primary_categories_and_proc_macro_context(self):
        result, _ = prepared_audit()
        rows = {row["name"]: row for row in result.classify()}

        self.assertEqual(rows["normal-dep"]["primary_reachability"], "release-normal")
        self.assertEqual(rows["normal-dep"]["also_dev"], "true")
        self.assertEqual(rows["normal-dep"]["also_foreign_target"], "true")
        self.assertEqual(rows["build-dep"]["primary_reachability"], "release-build")
        self.assertEqual(rows["build-child"]["primary_reachability"], "release-build")
        self.assertEqual(rows["derive-dep"]["primary_reachability"], "release-build")
        self.assertEqual(rows["dev-dep"]["primary_reachability"], "dev-bench-only")
        self.assertEqual(
            rows["internal-only"]["primary_reachability"],
            "non-release-workspace-normal",
        )
        self.assertEqual(rows["windows-only"]["primary_reachability"], "target-only")
        self.assertEqual(
            rows["disabled-optional"]["primary_reachability"], "optional-disabled"
        )

    def test_direct_feature_origin(self):
        result, _ = prepared_audit()
        result.classify()
        rows = result.direct_feature_rows()

        row = next(item for item in rows if item["dependency"] == "normal-dep")
        self.assertEqual(row["requested_features"], "tls")
        self.assertEqual(row["metadata_union_features"], "default,tls")
        self.assertEqual(row["release_features"], "")
        self.assertEqual(row["default_features"], "true")
        self.assertEqual(row["manifest"], "services/sentinel-daemon/Cargo.toml")
        self.assertNotIn("disabled-optional", {item["dependency"] for item in rows})

    def test_optional_metadata_edge_absent_from_cargo_tree_is_disabled(self):
        result, optional = prepared_audit()
        rows = {(row["name"], row["version"]): row for row in result.classify()}
        self.assertEqual(
            rows[(optional[0], optional[1])]["primary_reachability"],
            "optional-disabled",
        )

    def test_duplicate_versions_have_complete_root_closures(self):
        result, _ = prepared_audit()
        result.classify()
        rows, closure = result.duplicate_rows_and_closure()
        split_rows = [row for row in rows if row["name"] == "split-dep"]
        self.assertEqual(len(split_rows), 2)
        self.assertTrue(all(row["workspace_roots"] for row in split_rows))
        self.assertTrue(all(int(row["closure_edges"]) > 0 for row in split_rows))
        self.assertEqual(
            {row["closure_basis"] for row in split_rows}, {"active-all-target-tree"}
        )
        self.assertTrue(any(row["duplicate_name"] == "split-dep" for row in closure))

    def test_target_constraint_is_preserved_on_active_edge(self):
        result, _ = prepared_audit()
        foreign = result.keys_by_name_version[("windows-only", "1.0.0")]
        root = result.root_keys[ROOTS[0]]
        self.assertEqual(
            result.all_target_edges[(root, foreign)], {"normal:cfg(windows)"}
        )

    def test_tree_parser_marks_proc_macro_subtree_as_build_context(self):
        keys = {
            ("root", "1.0.0"): ("root", "1.0.0", "workspace"),
            ("derive", "1.0.0"): ("derive", "1.0.0", "registry"),
            ("quote", "1.0.0"): ("quote", "1.0.0", "registry"),
        }
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "tree.txt"
            path.write_text(
                "0root v1.0.0\n1derive v1.0.0 (proc-macro)\n2quote v1.0.0\n",
                encoding="utf-8",
            )
            graph = audit_module.parse_tree(path, keys)
        self.assertEqual(
            graph.build_context,
            {keys[("derive", "1.0.0")], keys[("quote", "1.0.0")]},
        )

    def test_exact_evidence_check_rejects_root_overclaim(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "reachability.tsv"
            path.write_text("claimed-root\n", encoding="utf-8")
            with self.assertRaisesRegex(audit_module.AuditError, "differs"):
                audit_module.verify_exact(path, "tree-root\n")

    def test_release_tree_loader_rejects_wrong_claimed_root(self):
        lock, metadata_all, metadata_native = fixture()
        result = audit_module.ReachabilityAudit(lock, metadata_all, metadata_native)
        with tempfile.TemporaryDirectory() as temp:
            trees = Path(temp)
            for root in ROOTS:
                graph = audit_module.TreeGraph(
                    packages={result.root_keys[root]},
                    normal_context={result.root_keys[root]},
                    roots={result.root_keys[root]},
                )
                for label in ("normal", "normal-build"):
                    (trees / f"{root}.{label}.graph.tsv").write_text(
                        audit_module.compact_tree_text(graph), encoding="utf-8"
                    )
            wrong_graph = audit_module.TreeGraph(
                packages={result.root_keys[ROOTS[1]]},
                normal_context={result.root_keys[ROOTS[1]]},
                roots={result.root_keys[ROOTS[1]]},
            )
            (trees / f"{ROOTS[0]}.normal-build.graph.tsv").write_text(
                audit_module.compact_tree_text(wrong_graph), encoding="utf-8"
            )
            with self.assertRaisesRegex(audit_module.AuditError, "exactly to their root"):
                result.load_release_trees(trees, compact=True)

    def test_compact_tree_roundtrip_preserves_unique_graph(self):
        source, _ = prepared_audit()
        graph = source.root_combined_graphs[ROOTS[0]]
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "tree.graph.tsv"
            path.write_text(audit_module.compact_tree_text(graph), encoding="utf-8")
            loaded = audit_module.load_compact_tree(path, source.keys_by_name_version)
        self.assertEqual(loaded, graph)

    def test_compact_tree_rejects_package_unreachable_from_roots(self):
        source, _ = prepared_audit()
        root = source.root_keys[ROOTS[0]]
        orphan = source.keys_by_name_version[("normal-dep", "1.0.0")]
        graph = audit_module.TreeGraph(
            packages={root, orphan},
            normal_context={root, orphan},
            roots={root},
        )
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "tree.graph.tsv"
            path.write_text(audit_module.compact_tree_text(graph), encoding="utf-8")
            with self.assertRaisesRegex(audit_module.AuditError, "unreachable"):
                audit_module.load_compact_tree(path, source.keys_by_name_version)

    def test_check_rejects_manipulated_workspace_membership_output(self):
        with tempfile.TemporaryDirectory() as temp:
            args = write_check_fixture(Path(temp))
            path = args.output_dir / "workspace-reachability-sets.tsv"
            rows = audit_module.read_tsv(path)
            row = next(item for item in rows if item["name"] == "dev-dep")
            row["native_dev_context"] = "false"
            audit_module.write_tsv(path, rows, audit_module.WORKSPACE_SET_FIELDS)
            with self.assertRaisesRegex(audit_module.AuditError, "workspace-reachability"):
                audit_module.run_audit(args)

    def test_check_rejects_manipulated_active_edge_output(self):
        with tempfile.TemporaryDirectory() as temp:
            args = write_check_fixture(Path(temp))
            path = args.output_dir / "workspace-all-target-edges.tsv"
            rows = audit_module.read_tsv(path)
            row = next(
                item for item in rows if item["cargo_all_targets_active"] == "true"
            )
            row["cargo_all_targets_active"] = "false"
            audit_module.write_tsv(path, rows, audit_module.EDGE_FIELDS)
            with self.assertRaisesRegex(audit_module.AuditError, "workspace-all-target-edges"):
                audit_module.run_audit(args)

    def test_duplicate_closure_fails_when_active_chain_is_missing(self):
        result, _ = prepared_audit()
        result.classify()
        split_one = result.keys_by_name_version[("split-dep", "1.0.0")]
        result.active_all_target_edges = {
            edge for edge in result.active_all_target_edges if edge[1] != split_one
        }
        with self.assertRaisesRegex(audit_module.AuditError, "complete workspace-root closure"):
            result.duplicate_rows_and_closure()


class SanitizationTests(unittest.TestCase):
    def test_normalizer_replaces_private_locations(self):
        authority = "root" + "@" + ".".join(["10", "0", "0", "155"])
        workspace = "/" + "work" + "/" + "company" + "/ps-631-dep-audit"
        cargo_home = "/" + "root" + "/" + ".cargo"
        remote_host_key = "remote_" + "host"
        remote_user_key = "remote_" + "user"
        remote_targets = (
            "/" + "tmp" + "/" + "issue-631-run",
            "/" + "tmp" + "/" + "issue631-tree/1234567890",
            "/" + "tmp" + "/" + "cargo-remote/project",
            "/" + "tmp" + "/" + "previously-unknown/session-output",
        )
        raw = (
            f"{authority} {workspace} {REMOTE_PROJECT} {cargo_home} "
            + " ".join(remote_targets)
            + f"\n{remote_host_key}=build-node {remote_user_key}=build-user\n"
        )
        normalized = audit_module.normalize_text(raw)

        self.assertIn("<USER>@<HOST>", normalized)
        self.assertIn("<WORKSPACE>", normalized)
        self.assertIn("<REMOTE_PROJECT>", normalized)
        self.assertIn("<CARGO_HOME>", normalized)
        self.assertEqual(normalized.count("<REMOTE_TARGET>"), len(remote_targets))
        self.assertIn(f"{remote_host_key}=<HOST>", normalized)
        self.assertIn(f"{remote_user_key}=<USER>", normalized)
        self.assertEqual(audit_module.suspicious_tokens(normalized), [])

    def test_tree_normalizer_ignores_wrapper_timestamps(self):
        raw = (
            "2026-07-19 20:00:00 INFO wrapper\n"
            f"0demo v1.0.0 ({REMOTE_PROJECT}/demo)\n"
            "1serde v1.0.0\n"
            "2026-07-19 20:00:01 INFO copyback\n"
        )
        self.assertEqual(
            audit_module.normalize_tree(raw),
            "0demo v1.0.0 (<REMOTE_PROJECT>/demo)\n1serde v1.0.0\n",
        )

    def test_bloat_normalizer_extracts_complete_validated_table(self):
        raw = (
            "2026-07-19 20:00:00 INFO wrapper\n"
            " File  .text     Size Crate\n"
            " 5.9%  97.7% 254.7KiB std\n"
            "0.0%   0.8%   2.0KiB agent_runtime\n"
            "6.0% 100.0% 260.7KiB .text section size, the file size is 4.2MiB\n"
            "Note: approximate values\n"
        )
        self.assertEqual(
            audit_module.normalize_bloat(raw),
            "File  .text     Size Crate\n"
            "5.9%  97.7% 254.7KiB std\n"
            "0.0%   0.8%   2.0KiB agent_runtime\n"
            "6.0% 100.0% 260.7KiB .text section size, the file size is 4.2MiB\n",
        )

    def test_bloat_normalizer_fails_closed_on_incomplete_output(self):
        with self.assertRaisesRegex(audit_module.AuditError, "summary rows"):
            audit_module.normalize_bloat(
                "File  .text     Size Crate\n5.9% 97.7% 254.7KiB std\n"
            )

    def test_bloat_normalizer_fails_closed_on_malformed_row(self):
        raw = (
            "File  .text     Size Crate\n"
            "not a bloat row\n"
            "6.0% 100.0% 260.7KiB .text section size, the file size is 4.2MiB\n"
        )
        with self.assertRaisesRegex(audit_module.AuditError, "invalid bloat table row"):
            audit_module.normalize_bloat(raw)

    def test_bloat_summary_separates_release_and_analysis_sizes(self):
        raw = (
            "File  .text     Size Crate\n"
            "3.7%   8.1%   3.1MiB cranelift_codegen\n"
            "12.8% 27.8%  10.5MiB And 286 more crates. Use -n N to show more.\n"
            "46.0% 100.0% 37.8MiB .text section size, the file size is 82.2MiB\n"
        )
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "bloat.txt"
            path.write_text(raw, encoding="utf-8")
            summary = audit_module.parse_bloat_summary(path)
        self.assertEqual(summary["text_size"], "37.8MiB")
        self.assertEqual(summary["analysis_file_size"], "82.2MiB")
        self.assertEqual(summary["top_crate"], "cranelift_codegen")
        self.assertEqual(summary["top_crate_text_percent"], "8.1%")
        self.assertEqual(summary["remainder_crates"], "286")
        self.assertEqual(summary["remainder_text_percent"], "27.8%")

    def test_scan_rejects_every_private_data_class(self):
        fixtures = {
            "ip": "builder=" + ".".join(["10", "0", "0", "155"]),
            "ipv6": "builder=" + ":".join(["2001", "db8", "", "1"]),
            "home": "/" + "home" + "/operator/source",
            "root-home": "/" + "root" + "/source",
            "workspace": "/" + "work" + "/company/project-sentinel",
            "remote": REMOTE_PROJECT,
            "remote-no-hyphen": "/" + "tmp" + "/issue631-tree/project",
            "cargo-remote-temp": "/" + "tmp" + "/cargo-remote/project",
            "cargo-home": "." + "cargo" + "/registry",
            "authority": "ssh " + "builder" + "@" + "example" + ".internal",
            "absolute": "/" + "opt" + "/private/bin",
        }
        for name, value in fixtures.items():
            with self.subTest(name=name):
                self.assertTrue(audit_module.suspicious_tokens(value))

    def test_scan_rejects_remote_host_field(self):
        key = "remote_" + "host"
        self.assertEqual(
            audit_module.suspicious_tokens(f"{key}=build-node\n"),
            ["remote-host-field"],
        )

    def test_scan_rejects_remote_user_field(self):
        key = "remote_" + "user"
        self.assertEqual(
            audit_module.suspicious_tokens(f'{key}: "build-user"\n'),
            ["remote-user-field"],
        )

    def test_scan_rejects_unknown_remote_temp_path(self):
        path = "/" + "tmp" + "/" + "unrecognized-build/session/output.txt"
        self.assertEqual(
            audit_module.suspicious_tokens(f"artifact={path}\n"),
            ["remote-temp-path"],
        )

    def test_scan_accepts_private_field_placeholders(self):
        host_key = "remote_" + "host"
        user_key = "remote_" + "user"
        value = f'{host_key}="<HOST>"\n{user_key}=<USER>\n'
        self.assertEqual(audit_module.suspicious_tokens(value), [])

    def test_scan_does_not_block_public_domains(self):
        value = (
            "repository=https://github.com/example/project\n"
            "contact=maintainer@example.com\n"
        )
        self.assertEqual(audit_module.suspicious_tokens(value), [])

    def test_public_scan_is_fail_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "evidence.txt"
            address = ".".join(["10", "0", "0", "155"])
            path.write_text(f"remote={address}\n", encoding="utf-8")
            with contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(audit_module.check_public_evidence([path]), 1)
            path.write_text("remote=<HOST>\n", encoding="utf-8")
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(audit_module.check_public_evidence([path]), 0)

    def test_package_identity_is_not_an_ssh_authority(self):
        self.assertEqual(audit_module.suspicious_tokens("criterion@0.8.2"), [])

    @mock.patch.object(audit_module.subprocess, "run")
    def test_staged_scan_rejects_added_private_data(self, run):
        address = ".".join(["10", "0", "0", "155"])
        run.return_value = mock.Mock(stdout=f"+++ b/file\n+remote={address}\n")
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(audit_module.check_staged_lines(), 1)

    @mock.patch.object(audit_module.subprocess, "run")
    def test_staged_scan_accepts_placeholders(self, run):
        run.return_value = mock.Mock(stdout="+++ b/file\n+remote=<HOST>\n")
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(audit_module.check_staged_lines(), 0)


if __name__ == "__main__":
    unittest.main()
