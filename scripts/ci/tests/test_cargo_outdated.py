import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts/ci/cargo_outdated.py"
SPEC = importlib.util.spec_from_file_location("cargo_outdated", SCRIPT)
caller = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(caller)

MANIFEST = '''[workspace]
members = ["member"]
exclude = ["vendor/wasmtime-internal-cache"]
resolver = "2"
[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
[patch.crates-io]
wasmtime-internal-cache = { path = "vendor/wasmtime-internal-cache" }
'''

FAKE_CARGO = '''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys, tempfile, tomllib
root = pathlib.Path.cwd()
assert sys.argv[1:] == ["outdated", "--manifest-path", str(root / "Cargo.toml"),
                       "--root-deps-only", "--depth", "1"]
assert os.environ["GOWORK"] == "off"
assert pathlib.Path(os.environ["TMPDIR"]) == root.parent
assert pathlib.Path(os.environ["CARGO_TARGET_DIR"]) == root.parent / "target"
manifest = tomllib.loads((root / "Cargo.toml").read_text())
vendor = pathlib.Path(manifest["patch"]["crates-io"]["wasmtime-internal-cache"]["path"])
assert vendor == root / "vendor/wasmtime-internal-cache"
assert (vendor / "src/lib.rs").read_text() == "pub fn authentic() {}\\n"
assert (root / ".cargo/config.toml").read_text() == "[net]\\nretry = 2\\n"
assert (root / "go.work").read_text() == "go 1.26.10\\n"
# Reproduce the upstream virtual-root copy with no copied vendor directory.
with tempfile.TemporaryDirectory(dir=os.environ["TMPDIR"]) as second:
    second = pathlib.Path(second)
    shutil.copyfile(root / "Cargo.toml", second / "Cargo.toml")
    copied = tomllib.loads((second / "Cargo.toml").read_text())
    assert not (second / "vendor").exists()
    assert pathlib.Path(copied["patch"]["crates-io"]["wasmtime-internal-cache"]["path"]).samefile(vendor)
pathlib.Path(os.environ["FRESHNESS_RECORD"]).write_text(json.dumps({"root": str(root), "manifest": manifest}))
mode = os.environ.get("FRESHNESS_FAKE_MODE")
if mode == "vendor": (vendor / "src/lib.rs").write_text("mutated")
if mode == "manifest": (root / "Cargo.toml").write_text("[workspace]\\n")
if mode == "lock": (root / "Cargo.lock").write_text("mutated")
if mode == "source": pathlib.Path(os.environ["FRESHNESS_SOURCE"]).joinpath("Cargo.lock").write_text("mutated")
print("Name Project Compat Latest Kind Platform")
print("example 1.0.0 1.1.0 2.0.0 Normal ---")
print("fake diagnostic retained", file=sys.stderr)
raise SystemExit(int(os.environ.get("FRESHNESS_FAKE_EXIT", "0")))
'''


class CargoOutdatedTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="freshness-tests-", dir=os.environ.get("TMPDIR", "/work/tmp"))
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "source"
        self.scratch = self.root / 'scratch with spaces "and quotes"'
        self.source.mkdir()
        self.scratch.mkdir()
        self.record = self.root / "child.json"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "cargo").write_text(FAKE_CARGO)
        (self.bin / "cargo").chmod(0o755)
        self.write("Cargo.toml", MANIFEST)
        self.write("Cargo.lock", "version = 4\n")
        self.write("go.work", "go 1.26.10\n")
        self.write(".cargo/config.toml", "[net]\nretry = 2\n")
        self.write("member/Cargo.toml", '[package]\nname = "member"\nversion = "1.0.0"\n')
        self.write("member/src/lib.rs", "pub fn member() {}\n")
        self.write("vendor/wasmtime-internal-cache/Cargo.toml",
                   '[package]\nname = "wasmtime-internal-cache"\nversion = "49.0.4"\n')
        self.write("vendor/wasmtime-internal-cache/src/lib.rs", "pub fn authentic() {}\n")
        self.git("init", "-q")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.commit()

    def write(self, name, text):
        path = self.source / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.source), *args], stderr=subprocess.PIPE)

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-qm", "fixture")

    def invoke(self, **environment):
        env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                   FRESHNESS_RECORD=str(self.record), FRESHNESS_SOURCE=str(self.source), GOWORK="/wrong/go.work")
        env.update(environment)
        return subprocess.run([sys.executable, "-B", str(SCRIPT), "--source", str(self.source),
                               "--scratch-dir", str(self.scratch)], cwd=self.source, env=env,
                              capture_output=True, text=True, timeout=20)

    def test_actual_entry_uses_exact_snapshot_and_survives_virtual_root_copy(self):
        original = {name: (self.source / name).read_bytes() for name in
                    ("Cargo.toml", "Cargo.lock", ".cargo/config.toml", "go.work")}
        result = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "Name Project Compat Latest Kind Platform\nexample 1.0.0 1.1.0 2.0.0 Normal ---\n")
        self.assertEqual(result.stderr, "fake diagnostic retained\n")
        observed = json.loads(self.record.read_text())
        self.assertFalse(Path(observed["root"]).exists())
        before = caller.tomllib.loads(MANIFEST)
        after = copy.deepcopy(observed["manifest"])
        after["patch"]["crates-io"][caller.PATCH]["path"] = caller.VENDOR
        self.assertEqual(before, after)
        self.assertEqual(original, {name: (self.source / name).read_bytes() for name in original})
        self.assertEqual(self.git("status", "--porcelain"), b"")
        self.assertEqual(list(self.scratch.iterdir()), [])

    def test_original_virtual_root_copy_reproduces_missing_relative_patch(self):
        original = caller.tomllib.loads(MANIFEST)
        relative = Path(original["patch"]["crates-io"][caller.PATCH]["path"])
        self.assertTrue((self.source / relative / "Cargo.toml").is_file())
        copied_root = self.scratch / "upstream-virtual-root"
        copied_root.mkdir()
        (copied_root / "Cargo.toml").write_text(MANIFEST)
        self.assertFalse((copied_root / relative / "Cargo.toml").exists())

    def test_nonzero_child_status_and_output_are_not_hidden(self):
        result = self.invoke(FRESHNESS_FAKE_EXIT="23")
        self.assertEqual(result.returncode, 23)
        self.assertIn("example 1.0.0", result.stdout)
        self.assertEqual(result.stderr, "fake diagnostic retained\n")
        self.assertEqual(list(self.scratch.iterdir()), [])

    def test_dirty_tracked_or_untracked_source_rejected_before_child(self):
        for name in ("Cargo.lock", "untracked"):
            with self.subTest(name=name):
                self.write(name, "dirty")
                result = self.invoke()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("source must be clean", result.stderr)
                self.assertFalse(self.record.exists())
                if name == "untracked": (self.source / name).unlink()
                else: self.git("restore", name)

    def test_unknown_patch_shape_is_not_ignored(self):
        self.write("Cargo.toml", MANIFEST + '\n[patch.other]\nfoo = { version = "1" }\n')
        self.commit()
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unrecognized patch", result.stderr)
        self.assertFalse(self.record.exists())

    def test_missing_or_external_patch_is_not_replaced_by_registry(self):
        for replacement in ("vendor/missing", str(self.root / "external")):
            with self.subTest(replacement=replacement):
                self.write("Cargo.toml", MANIFEST.replace(caller.VENDOR, replacement))
                self.commit()
                result = self.invoke()
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.record.exists())

    def test_untracked_vendor_cannot_substitute_for_committed_patch(self):
        self.git("rm", "-r", "--cached", caller.VENDOR)
        self.write(".gitignore", caller.VENDOR + "/\n")
        self.commit()
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.record.exists())

    def test_patch_package_identity_and_ambiguous_serialization_reject(self):
        self.write(caller.VENDOR + "/Cargo.toml", '[package]\nname = "other"\nversion = "49.0.4"\n')
        self.commit()
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("identity changed", result.stderr)
        self.assertFalse(self.record.exists())
        self.write(caller.VENDOR + "/Cargo.toml", '[package]\nname = "wasmtime-internal-cache"\nversion = "49.0.4"\n')
        self.write("Cargo.toml", MANIFEST + "\n# " + caller.ENTRY + "\n")
        self.commit()
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ambiguous", result.stderr)

    def test_symlink_or_external_ancestor_cannot_supply_patch_bytes(self):
        path = self.source / caller.VENDOR / "src/lib.rs"
        path.unlink()
        path.symlink_to(self.source / "member/src/lib.rs")
        self.commit()
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tracked link", result.stderr)
        self.assertFalse(self.record.exists())

    def test_source_and_scratch_symlinks_or_nested_scratch_reject(self):
        linked = self.root / "linked-source"
        linked.symlink_to(self.source, target_is_directory=True)
        result = subprocess.run([sys.executable, "-B", str(SCRIPT), "--source", str(linked),
                                 "--scratch-dir", str(self.scratch)], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("canonical", result.stderr)
        for scratch in (self.source, self.root / "linked-scratch"):
            if scratch.name == "linked-scratch": scratch.symlink_to(self.scratch, target_is_directory=True)
            with self.subTest(scratch=scratch), self.assertRaises(caller.InspectionError):
                caller.scratch_parent(scratch, self.source)
        self.assertFalse(self.record.exists())

    def test_parent_workspace_and_cargo_configs_rejected(self):
        for name in ("Cargo.toml", "go.work", ".cargo/config", ".cargo/config.toml"):
            with self.subTest(name=name):
                path = self.root / name
                path.parent.mkdir(exist_ok=True)
                path.write_text("not inherited")
                result = self.invoke()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("inherited workspace", result.stderr)
                self.assertFalse(self.record.exists())
                path.unlink()

    def test_export_attributes_cannot_silently_omit_or_rewrite_inputs(self):
        self.write(".gitattributes", "member/src/lib.rs export-ignore\n")
        self.commit()
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("omitted tracked files", result.stderr)
        self.assertFalse(self.record.exists())

    def test_export_substitution_cannot_change_committed_inputs(self):
        self.write(".gitattributes", "member/src/lib.rs export-subst\n")
        self.write("member/src/lib.rs", '// $Format:%H$\npub fn member() {}\n')
        self.commit()
        result = self.invoke()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("changed committed bytes", result.stderr)
        self.assertFalse(self.record.exists())

    def test_child_cannot_change_patch_manifest_lock_or_original_source(self):
        for mode in ("vendor", "manifest", "lock", "source"):
            with self.subTest(mode=mode):
                result = self.invoke(FRESHNESS_FAKE_MODE=mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("source must be clean" if mode == "source" else "changed", result.stderr)
                self.assertEqual(list(self.scratch.iterdir()), [])
                self.git("restore", "Cargo.lock")

    def test_workflow_keeps_failure_propagation_and_existing_scope(self):
        text = (ROOT / ".github/workflows/deps-freshness.yml").read_text()
        block = text.split("  rust-deps:\n")[1].split("  go-deps:\n")[0]
        self.assertIn('python3 scripts/ci/cargo_outdated.py --source "$GITHUB_WORKSPACE" --scratch-dir "$RUNNER_TEMP"', block)
        self.assertIn('exit "$STATUS"', block)
        self.assertNotIn("continue-on-error", block)
        self.assertNotIn("--ignore", block)
        self.assertNotIn("--exclude", block)


if __name__ == "__main__":
    unittest.main()
