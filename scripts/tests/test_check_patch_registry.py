from datetime import date
from fnmatch import fnmatchcase
import importlib.util
from pathlib import Path
import re
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).parents[1] / "check-patch-registry.py"
CI_WORKFLOW = Path(__file__).parents[2] / ".github" / "workflows" / "ci.yml"
SPEC = importlib.util.spec_from_file_location("check_patch_registry", SCRIPT)
checker = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = checker
SPEC.loader.exec_module(checker)

TODAY = date(2026, 7, 24)
SOURCE = "git=https://example.invalid/upstream/demo;rev=abc123"
OVERRIDE_KEY = "patch:Cargo.toml:crates-io:demo"
GIT_URL = "https://example.invalid/upstream/demo"
GIT_KEY = "git:Cargo.toml:dependencies:demo"


def manifest(
    *,
    patch: bool = False,
    replace: bool = False,
    git_dependency: bool = False,
    git_url: str = GIT_URL,
) -> str:
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
                f'demo = {{ git = "{git_url}" }}',
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
    if replace:
        lines.extend(
            [
                "",
                "[replace]",
                '"demo:1.2.3" = { git = "https://example.invalid/upstream/demo", rev = "abc123" }',
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


def git_allowlist_entry(**overrides) -> dict:
    value = {
        "id": "cargo-git-demo",
        "dependency_key": GIT_KEY,
        "manifest": "Cargo.toml",
        "table": "dependencies",
        "dependency": "demo",
        "package": "demo",
        "source": f"git={GIT_URL}",
        "owner": "component:test",
        "reason": "Fixture official-upstream dependency",
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


def registry(entries, git_dependencies) -> str:
    lines = [
        "# Fixture Registry",
        "",
        checker.REGISTRY_START,
        "```toml",
        "schema_version = 2",
        "entries = []" if not entries else "",
        "direct_git_dependencies = []" if not git_dependencies else "",
    ]
    for item in entries:
        lines.append("[[entries]]")
        for key, value in item.items():
            lines.append(f"{key} = {toml_value(value)}")
    for item in git_dependencies:
        lines.append("[[direct_git_dependencies]]")
        for key, value in item.items():
            lines.append(f"{key} = {toml_value(value)}")
    lines.extend(["```", checker.REGISTRY_END, ""])
    return "\n".join(line for line in lines if line != "") + "\n"


class Fixture:
    def __init__(
        self,
        manifest_text: str,
        entries,
        git_dependencies=(),
        cargo_config: str | None = None,
    ):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text(manifest_text, encoding="utf-8")
        if cargo_config is not None:
            config_dir = self.root / checker.CARGO_CONFIG_DIR
            config_dir.mkdir()
            (config_dir / "config.toml").write_text(cargo_config, encoding="utf-8")
        (self.root / "docs").mkdir()
        self.registry = self.root / "docs" / "dependency-patches.md"
        self.registry.write_text(
            registry(entries, git_dependencies),
            encoding="utf-8",
        )

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

    def test_allowlisted_official_git_dependency_passes(self):
        fixture = Fixture(
            manifest(git_dependency=True),
            [],
            [git_allowlist_entry()],
        )
        self.addCleanup(fixture.cleanup)
        result = fixture.check()
        self.assertTrue(result.ok, result.errors)
        self.assertEqual(result.overrides, 0)
        self.assertEqual(result.git_dependencies, 1)

    def test_git_dependency_url_change_to_fork_fails(self):
        fixture = Fixture(
            manifest(
                git_dependency=True,
                git_url="https://example.invalid/fork/demo",
            ),
            [],
            [git_allowlist_entry()],
        )
        self.addCleanup(fixture.cleanup)
        self.assert_error(fixture.check(), "GIT_DEPENDENCY_MISMATCH")

    def test_new_unallowlisted_git_dependency_fails(self):
        fixture = Fixture(manifest(git_dependency=True), [])
        self.addCleanup(fixture.cleanup)
        self.assert_error(fixture.check(), "UNALLOWLISTED_GIT_DEPENDENCY")

    def test_stale_git_allowlist_row_fails(self):
        fixture = Fixture(manifest(), [], [git_allowlist_entry()])
        self.addCleanup(fixture.cleanup)
        self.assert_error(fixture.check(), "STALE_GIT_ALLOWLIST_ROW")

    def test_registered_replace_passes(self):
        replacement = entry(
            override_key="replace:Cargo.toml:demo:1.2.3",
            source=SOURCE,
        )
        fixture = Fixture(manifest(replace=True), [replacement])
        self.addCleanup(fixture.cleanup)
        result = fixture.check()
        self.assertTrue(result.ok, result.errors)

    def test_unregistered_replace_fails(self):
        fixture = Fixture(manifest(replace=True), [])
        self.addCleanup(fixture.cleanup)
        self.assert_error(fixture.check(), "UNREGISTERED_OVERRIDE")

    def test_registered_source_replacement_passes(self):
        replacement = entry(
            package="*",
            manifest=f"{checker.CARGO_CONFIG_DIR}/config.toml",
            override_key=(
                f"source:{checker.CARGO_CONFIG_DIR}/config.toml:crates-io"
            ),
            source="replace-with=vendored;directory=vendor",
        )
        fixture = Fixture(
            manifest(),
            [replacement],
            cargo_config=(
                '[source.crates-io]\n'
                'replace-with = "vendored"\n'
                '\n'
                '[source.vendored]\n'
                'directory = "vendor"\n'
            ),
        )
        self.addCleanup(fixture.cleanup)
        result = fixture.check()
        self.assertTrue(result.ok, result.errors)

    def test_unregistered_source_replacement_fails(self):
        fixture = Fixture(
            manifest(),
            [],
            cargo_config=(
                '[source.crates-io]\n'
                'replace-with = "vendored"\n'
                '\n'
                '[source.vendored]\n'
                'directory = "vendor"\n'
            ),
        )
        self.addCleanup(fixture.cleanup)
        self.assert_error(fixture.check(), "UNREGISTERED_OVERRIDE")

    def test_rust_ci_filter_covers_every_checker_cargo_input(self):
        workflow = CI_WORKFLOW.read_text(encoding="utf-8")
        rust_filter = workflow.split("            rust:\n", 1)[1].split(
            "            go:\n",
            1,
        )[0]
        patterns = re.findall(r"- '([^']+)'", rust_filter)
        config_dir = checker.CARGO_CONFIG_DIR
        monitored_paths = (
            "Cargo.toml",
            "deploy/bench/stack-harness/Cargo.toml",
            f"{config_dir}/config",
            f"{config_dir}/config.toml",
            f"nested/{config_dir}/config",
            f"nested/{config_dir}/config.toml",
        )
        for path in monitored_paths:
            self.assertTrue(
                any(fnmatchcase(path, pattern) for pattern in patterns),
                f"Rust CI filter does not match {path}",
            )


if __name__ == "__main__":
    unittest.main()
