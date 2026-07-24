from datetime import date
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).parents[1] / "check-patch-registry.py"
SPEC = importlib.util.spec_from_file_location("check_patch_registry", SCRIPT)
checker = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = checker
SPEC.loader.exec_module(checker)

TODAY = date(2026, 7, 24)
SOURCE = "git=https://example.invalid/upstream/demo;rev=abc123"
OVERRIDE_KEY = "patch:Cargo.toml:crates-io:demo"


def manifest(*, patch: bool = False, git_dependency: bool = False) -> str:
    lines = [
        "[package]",
        'name = "fixture"',
        'version = "0.1.0"',
    ]
    if git_dependency:
        lines.extend(
            [
                "",
                "[dependencies]",
                'demo = { git = "https://example.invalid/upstream/demo" }',
            ]
        )
    if patch:
        lines.extend(
            [
                "",
                "[patch.crates-io]",
                'demo = { git = "https://example.invalid/upstream/demo", rev = "abc123" }',
            ]
        )
    return "\n".join(lines) + "\n"


def entry(**overrides) -> dict:
    value = {
        "id": "cargo-demo-patch",
        "ecosystem": "cargo",
        "package": "demo",
        "version": "1.2.3",
        "kind": "PATCH_UPSTREAM",
        "manifest": "Cargo.toml",
        "override_key": OVERRIDE_KEY,
        "source": SOURCE,
        "reason": "Fixture reason",
        "evidence": "https://example.invalid/issues/1",
        "upstream_basis": "demo 1.2.3",
        "diff_lines": 4,
        "upstream_pr": "https://example.invalid/pulls/1",
        "owner": "component:test",
        "status": "ACTIVE",
        "expires_on": "2027-01-01",
        "revisit_condition": "Upstream release contains the change",
        "advisory_ids": [],
        "rollback": "Remove the patch table and restore the registry row",
    }
    value.update(overrides)
    return value


def toml_value(value):
    if isinstance(value, str):
        return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'
    if isinstance(value, int):
        return str(value)
    if isinstance(value, list):
        return "[" + ", ".join(toml_value(item) for item in value) + "]"
    raise TypeError(value)


def registry(entries) -> str:
    lines = [
        "# Fixture Registry",
        "",
        checker.REGISTRY_START,
        "```toml",
        "schema_version = 1",
        "entries = []" if not entries else "",
    ]
    for item in entries:
        lines.append("[[entries]]")
        for key, value in item.items():
            lines.append(f"{key} = {toml_value(value)}")
    lines.extend(["```", checker.REGISTRY_END, ""])
    return "\n".join(line for line in lines if line != "") + "\n"


class Fixture:
    def __init__(self, manifest_text: str, entries):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text(manifest_text, encoding="utf-8")
        (self.root / "docs").mkdir()
        self.registry = self.root / "docs" / "dependency-patches.md"
        self.registry.write_text(registry(entries), encoding="utf-8")

    def check(self):
        return checker.check_repository(self.root, self.registry, TODAY)

    def cleanup(self):
        self.temp.cleanup()


class PatchRegistryTests(unittest.TestCase):
    def assert_error(self, result, code):
        self.assertFalse(result.ok)
        self.assertTrue(
            any(f"ERROR[{code}]" in error for error in result.errors),
            result.errors,
        )

    def test_empty_registry_matches_tree_without_overrides(self):
        fixture = Fixture(manifest(), [])
        self.addCleanup(fixture.cleanup)
        result = fixture.check()
        self.assertTrue(result.ok, result.errors)
        self.assertEqual(result.overrides, 0)
        self.assertEqual(result.registry_entries, 0)

    def test_registered_patch_passes_exact_source_match(self):
        fixture = Fixture(manifest(patch=True), [entry()])
        self.addCleanup(fixture.cleanup)
        result = fixture.check()
        self.assertTrue(result.ok, result.errors)
        self.assertEqual(result.overrides, 1)

    def test_unregistered_patch_fails(self):
        fixture = Fixture(manifest(patch=True), [])
        self.addCleanup(fixture.cleanup)
        self.assert_error(fixture.check(), "UNREGISTERED_OVERRIDE")

    def test_stale_registry_row_fails(self):
        fixture = Fixture(manifest(), [entry()])
        self.addCleanup(fixture.cleanup)
        self.assert_error(fixture.check(), "STALE_REGISTRY_ROW")

    def test_missing_required_field_fails(self):
        value = entry()
        del value["owner"]
        fixture = Fixture(manifest(patch=True), [value])
        self.addCleanup(fixture.cleanup)
        self.assert_error(fixture.check(), "MISSING_FIELD")

    def test_expired_temporary_fork_fails(self):
        value = entry(kind="FORK_TEMPORARY", expires_on="2026-07-24")
        fixture = Fixture(manifest(patch=True), [value])
        self.addCleanup(fixture.cleanup)
        self.assert_error(fixture.check(), "EXPIRED_TEMPORARY_FORK")

    def test_ordinary_git_dependency_is_not_a_patch_or_fork(self):
        fixture = Fixture(manifest(git_dependency=True), [])
        self.addCleanup(fixture.cleanup)
        result = fixture.check()
        self.assertTrue(result.ok, result.errors)
        self.assertEqual(result.overrides, 0)
        self.assertEqual(result.git_dependencies, 1)


if __name__ == "__main__":
    unittest.main()
