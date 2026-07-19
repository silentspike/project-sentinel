import contextlib
import importlib.util
import io
from pathlib import Path
import sys
import tempfile
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

    roots[0]["dependencies"] = [
        {
            "name": normal["name"],
            "rename": None,
            "kind": "normal",
            "uses_default_features": True,
            "features": ["tls"],
        }
    ]

    native_packages = roots + [other_root, normal, build, build_child, proc_macro, dev, non_release]
    all_packages = native_packages + [foreign]
    root_zero_deps = [
        dependency(normal),
        dependency(build, "build"),
        dependency(proc_macro),
        dependency(dev, "dev"),
    ]
    native_nodes = [node(roots[0], root_zero_deps)]
    native_nodes.extend(node(root) for root in roots[1:])
    native_nodes.extend(
        [
            node(other_root, [dependency(non_release)]),
            node(normal, features=["default", "tls"]),
            node(build, [dependency(build_child)]),
            node(build_child),
            node(proc_macro),
            node(dev),
            node(non_release),
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
    for item in all_packages + [optional]:
        lock_packages.append(
            {
                "name": item["name"],
                "version": item["version"],
                "source": item["source"],
            }
        )
    return {"package": lock_packages}, metadata_all, metadata_native


class ReachabilityTests(unittest.TestCase):
    def test_primary_categories_and_proc_macro_context(self):
        lock, metadata_all, metadata_native = fixture()
        result = audit_module.ReachabilityAudit(lock, metadata_all, metadata_native)
        rows = {row["name"]: row for row in result.classify()}

        self.assertEqual(rows["normal-dep"]["primary_reachability"], "release-normal")
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
        lock, metadata_all, metadata_native = fixture()
        result = audit_module.ReachabilityAudit(lock, metadata_all, metadata_native)
        result.classify()
        rows = result.direct_feature_rows()

        row = next(item for item in rows if item["dependency"] == "normal-dep")
        self.assertEqual(row["requested_features"], "tls")
        self.assertEqual(row["metadata_union_features"], "default,tls")
        self.assertEqual(row["release_features"], "")
        self.assertEqual(row["default_features"], "true")
        self.assertEqual(row["manifest"], "services/sentinel-daemon/Cargo.toml")

    def test_unreachable_metadata_package_fails_closed(self):
        lock, metadata_all, metadata_native = fixture()
        orphan = package("orphan")
        metadata_all["packages"].append(orphan)
        metadata_all["resolve"]["nodes"].append(node(orphan))
        metadata_native["packages"].append(orphan)
        metadata_native["resolve"]["nodes"].append(node(orphan))
        lock["package"].append(
            {"name": orphan["name"], "version": orphan["version"], "source": orphan["source"]}
        )

        result = audit_module.ReachabilityAudit(lock, metadata_all, metadata_native)
        with self.assertRaisesRegex(audit_module.AuditError, "unclassified"):
            result.classify()


class SanitizationTests(unittest.TestCase):
    def test_normalizer_replaces_private_locations(self):
        authority = "root" + "@" + ".".join(["10", "0", "0", "155"])
        workspace = "/" + "work" + "/" + "company" + "/ps-631-dep-audit"
        cargo_home = "/" + "root" + "/" + ".cargo"
        remote_targets = (
            "/" + "tmp" + "/" + "issue-631-run",
            "/" + "tmp" + "/" + "issue631-tree/1234567890",
            "/" + "tmp" + "/" + "cargo-remote/project",
        )
        raw = (
            f"{authority} {workspace} {REMOTE_PROJECT} {cargo_home} "
            + " ".join(remote_targets)
            + "\n"
        )
        normalized = audit_module.normalize_text(raw)

        self.assertIn("<USER>@<HOST>", normalized)
        self.assertIn("<WORKSPACE>", normalized)
        self.assertIn("<REMOTE_PROJECT>", normalized)
        self.assertIn("<CARGO_HOME>", normalized)
        self.assertEqual(normalized.count("<REMOTE_TARGET>"), len(remote_targets))
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
            "authority": "builder" + "@" + "example" + ".internal",
            "absolute": "/" + "opt" + "/private/bin",
        }
        for name, value in fixtures.items():
            with self.subTest(name=name):
                self.assertTrue(audit_module.suspicious_tokens(value))

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
