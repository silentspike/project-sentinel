#!/usr/bin/env python3
"""Focused tests for the deterministic M0 release package."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
from types import SimpleNamespace
import unittest
import uuid


REPO_ROOT = Path(__file__).resolve().parents[3]
MODULE_PATH = Path(__file__).with_name("release_package.py")
PREFLIGHT_PATH = REPO_ROOT / "scripts/product-acceptance/run_m0_preflight.py"
PROVISIONER = REPO_ROOT / "deploy/provision-m0-single-node.sh"


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


PACKAGE = load_module("m0_release_package_test_subject", MODULE_PATH)
PREFLIGHT = load_module("m0_release_package_preflight_authority", PREFLIGHT_PATH)
AUTHORITY = PREFLIGHT.CANONICAL_RELEASE_ARTIFACTS
STOPPED_UNITS = (
    PREFLIGHT.REQUIRED_SERVICES
    | PREFLIGHT.REQUIRED_TIMERS
    | set(PREFLIGHT.TIMER_SERVICES.values())
    | {PREFLIGHT.TARGET_UNIT}
)
SOURCE_MODES = {"binary": 0o700, "script": 0o700, "config": 0o600, "systemd": 0o600}
GIT_ENV = {
    "PATH": os.environ.get("PATH", ""),
    "HOME": "/nonexistent",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_AUTHOR_NAME": "M0 Release Test",
    "GIT_AUTHOR_EMAIL": "m0-release-test@example.invalid",
    "GIT_COMMITTER_NAME": "M0 Release Test",
    "GIT_COMMITTER_EMAIL": "m0-release-test@example.invalid",
    "GIT_AUTHOR_DATE": "2026-08-13T00:00:00Z",
    "GIT_COMMITTER_DATE": "2026-08-13T00:00:00Z",
    "LC_ALL": "C",
}


def run_git(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(root), *args],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=20,
        check=False,
        env=GIT_ENV,
    )


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode("ascii")


class Fixture:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.source = root / "source"
        self.external = root / "external-input"
        self.output = root / "output"
        self.stage = root / "stage"
        self.target = root / "target"
        self.provision_stage = root / "provision-stage"
        for path in (self.source, self.external, self.output, self.stage, self.target):
            path.mkdir(mode=0o700, parents=True)
        self.nats = self.external / "nats-server"
        for _, (source, kind) in sorted(AUTHORITY.items()):
            if source == "external/nats-server":
                path = self.nats
            else:
                path = self.source / source
            path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            data = f"m0-release-fixture:{source}\n".encode("ascii")
            path.write_bytes(data)
            path.chmod(SOURCE_MODES[kind])
        self.nats.chmod(0o700)
        self._commit_source()

    def _commit_source(self) -> None:
        self.assert_git("init", "--quiet")
        self.assert_git("add", "-f", ".")
        self.assert_git("commit", "--quiet", "-m", "test: release inputs")
        result = self.assert_git("rev-parse", "HEAD")
        self.git_sha = result.stdout.strip()

    def assert_git(self, *args: str) -> subprocess.CompletedProcess[str]:
        result = run_git(self.source, *args)
        if result.returncode != 0:
            raise AssertionError(result.stderr)
        return result

    @property
    def package(self) -> Path:
        return self.output / PACKAGE.package_name(self.git_sha)

    def build(self, **kwargs):
        return PACKAGE.build_package(
            self.source,
            self.nats,
            self.output,
            self.stage,
            self.git_sha,
            **kwargs,
        )

    def thaw_package(self) -> None:
        self.package.chmod(0o700)
        for path in self.package.rglob("*"):
            if path.is_dir() and not path.is_symlink():
                path.chmod(0o700)

    def provision(self) -> subprocess.CompletedProcess[str]:
        manifest = self.package / PACKAGE.MANIFEST_NAME
        services = self.root / "services.json"
        services.write_bytes(canonical({unit: "inactive" for unit in sorted(STOPPED_UNITS)}))
        services.chmod(0o600)
        digest = hashlib.sha256(manifest.read_bytes()).hexdigest()
        command = [
            "bash",
            str(PROVISIONER),
            "--manifest",
            str(manifest),
            "--expected-manifest-sha256",
            digest,
            "--expected-git-sha",
            self.git_sha,
            "--source-root",
            str(self.package),
            "--target-root",
            str(self.target),
            "--stage-root",
            str(self.provision_stage),
            "--install-uid",
            str(os.geteuid()),
            "--install-gid",
            str(os.getegid()),
            "--service-state-file",
            str(services),
        ]
        return subprocess.run(
            command,
            cwd=REPO_ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=60,
            check=False,
        )


class ReleasePackageTests(unittest.TestCase):
    def setUp(self) -> None:
        runner_temp = Path(os.environ.get("RUNNER_TEMP", "/work/tmp/project-sentinel/cdx3-650-release"))
        runner_temp.mkdir(mode=0o700, parents=True, exist_ok=True)
        runner_temp.chmod(0o700)
        self.case = runner_temp / f"release-{uuid.uuid4()}"
        self.case.mkdir(mode=0o700)
        self.fixture = Fixture(self.case)

    def tearDown(self) -> None:
        try:
            PACKAGE.remove_owned_tree(self.case)
        except (OSError, PACKAGE.PackageError):
            shutil.rmtree(self.case, ignore_errors=True)

    def assert_error(self, code: str, callable_, *args, **kwargs) -> None:
        with self.assertRaises(PACKAGE.PackageError) as context:
            callable_(*args, **kwargs)
        self.assertEqual(str(context.exception), code)

    def test_complete_111_artifact_package_is_canonical_and_provisionable(self) -> None:
        result = self.fixture.build()
        self.assertEqual(result["status"], "COMPLETE")
        self.assertEqual(result["artifact_count"], 111)
        manifest_raw = (self.fixture.package / PACKAGE.MANIFEST_NAME).read_bytes()
        manifest = json.loads(manifest_raw)
        self.assertEqual(manifest_raw, canonical(manifest))
        self.assertEqual(len(manifest["artifacts"]), 111)
        self.assertEqual(manifest["git_sha"], self.fixture.git_sha)
        self.assertEqual(stat.S_IMODE(self.fixture.package.stat().st_mode), 0o500)
        verified = PACKAGE.verify_package(self.fixture.package, self.fixture.git_sha)
        self.assertEqual(verified["manifest_sha256"], result["manifest_sha256"])

        provisioned = self.fixture.provision()
        self.assertEqual(provisioned.returncode, 0, provisioned.stderr)
        receipt = json.loads(provisioned.stdout)
        self.assertEqual(receipt["status"], "COMPLETE")
        self.assertEqual(receipt["artifact_count"], 111)
        self.assertEqual(receipt["changed_count"], 111)

    def test_retry_reuses_identical_immutable_package(self) -> None:
        first = self.fixture.build()
        manifest = self.fixture.package / PACKAGE.MANIFEST_NAME
        before = (manifest.stat().st_ino, manifest.stat().st_mtime_ns, manifest.read_bytes())
        second = self.fixture.build()
        after = (manifest.stat().st_ino, manifest.stat().st_mtime_ns, manifest.read_bytes())
        self.assertEqual(second["status"], "REUSED")
        self.assertEqual(first["manifest_sha256"], second["manifest_sha256"])
        self.assertEqual(before, after)

    def test_interrupted_build_leaves_no_authoritative_package_and_retry_succeeds(self) -> None:
        def interrupt(index: int) -> None:
            if index == 7:
                raise RuntimeError("private injected detail")

        self.assert_error("internal_failure", self.fixture.build, failure_hook=interrupt)
        self.assertFalse(self.fixture.package.exists())
        self.assertFalse((self.fixture.stage / f".build-{self.fixture.git_sha}").exists())
        result = self.fixture.build()
        self.assertEqual(result["status"], "COMPLETE")

    def test_missing_symlinked_hardlinked_and_bad_mode_nats_fail_closed(self) -> None:
        cases = ("missing", "symlink", "hardlink", "mode")
        for case in cases:
            with self.subTest(case=case):
                fixture = Fixture(self.case / case)
                if case == "missing":
                    fixture.nats.unlink()
                    expected = "source_missing_or_unsafe"
                elif case == "symlink":
                    fixture.nats.unlink()
                    fixture.nats.symlink_to(fixture.source / "config/daemon.toml")
                    expected = "source_missing_or_unsafe"
                elif case == "hardlink":
                    os.link(fixture.nats, fixture.external / "nats-copy")
                    expected = "source_authority_invalid"
                else:
                    fixture.nats.chmod(0o777)
                    expected = "source_authority_invalid"
                self.assert_error(expected, fixture.build)
                self.assertFalse(fixture.package.exists())

    def test_source_path_replacement_after_copy_is_rejected(self) -> None:
        target = self.fixture.source / "config/daemon.toml"
        replacement = self.fixture.source / "config/.daemon-replacement"
        replacement.write_bytes(b"replacement\n")
        replacement.chmod(0o600)

        swapped = False

        def replace_after_first(index: int) -> None:
            nonlocal swapped
            if not swapped:
                os.replace(replacement, target)
                swapped = True

        self.assert_error("source_changed", self.fixture.build, failure_hook=replace_after_first)
        self.assertFalse(self.fixture.package.exists())

    def test_wrong_git_sha_and_dirty_tracked_source_fail(self) -> None:
        self.assert_error(
            "git_sha_mismatch",
            PACKAGE.build_package,
            self.fixture.source,
            self.fixture.nats,
            self.fixture.output,
            self.fixture.stage,
            "f" * 40,
        )
        tracked = self.fixture.source / "config/daemon.toml"
        tracked.write_bytes(b"dirty\n")
        tracked.chmod(0o600)
        self.assert_error("git_source_dirty", self.fixture.build)

    def test_wrong_source_owner_and_public_cli_failure_are_fail_closed(self) -> None:
        source = self.fixture.nats
        fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
        info = os.fstat(fd)
        forged_owner = SimpleNamespace(
            st_mode=info.st_mode,
            st_nlink=info.st_nlink,
            st_uid=os.geteuid() + 1,
            st_dev=info.st_dev,
            st_ino=info.st_ino,
            st_size=info.st_size,
            st_mtime_ns=info.st_mtime_ns,
        )
        self.assert_error(
            "source_authority_invalid",
            PACKAGE.pin_source,
            fd,
            forged_owner,
            "external/nats-server",
            "/usr/local/bin/nats-server",
            "binary",
        )

        missing = self.case / "missing-package"
        result = subprocess.run(
            [
                sys.executable,
                str(MODULE_PATH),
                "verify",
                "--package",
                str(missing),
                "--expected-git-sha",
                self.fixture.git_sha,
            ],
            cwd=REPO_ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=20,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            result.stderr,
            canonical({"reason": "package_root_unsafe", "schema_version": 1, "status": "FAIL"}).decode(
                "ascii"
            ),
        )
        self.assertNotIn(str(self.case), result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_tamper_extra_type_swap_and_hardlink_package_fail(self) -> None:
        attacks = ("tamper", "extra", "type", "hardlink")
        for attack in attacks:
            with self.subTest(attack=attack):
                fixture = Fixture(self.case / attack)
                fixture.build()
                fixture.thaw_package()
                manifest_path = fixture.package / PACKAGE.MANIFEST_NAME
                if attack == "tamper":
                    row = json.loads(manifest_path.read_bytes())["artifacts"][0]
                    artifact = fixture.package / row["source"]
                    artifact.chmod(0o600)
                    artifact.write_bytes(b"tampered\n")
                    artifact.chmod(PACKAGE.FILE_MODES[row["type"]])
                    expected = "package_artifact_digest_mismatch"
                elif attack == "extra":
                    extra = fixture.package / "unexpected"
                    extra.write_bytes(b"extra\n")
                    extra.chmod(0o400)
                    expected = "package_file_set_mismatch"
                elif attack == "type":
                    manifest = json.loads(manifest_path.read_bytes())
                    manifest["artifacts"][0]["type"] = "script"
                    manifest_path.chmod(0o600)
                    manifest_path.write_bytes(canonical(manifest))
                    manifest_path.chmod(0o400)
                    expected = "manifest_artifact_authority_mismatch"
                else:
                    row = json.loads(manifest_path.read_bytes())["artifacts"][0]
                    artifact = fixture.package / row["source"]
                    os.link(artifact, fixture.package / "artifact-hardlink")
                    expected = "package_artifact_authority_invalid"
                for directory in (path for path in fixture.package.rglob("*") if path.is_dir()):
                    directory.chmod(0o500)
                fixture.package.chmod(0o500)
                self.assert_error(expected, PACKAGE.verify_package, fixture.package, fixture.git_sha)

    def test_stale_output_is_never_replaced(self) -> None:
        first = self.fixture.build()
        self.fixture.thaw_package()
        manifest = self.fixture.package / PACKAGE.MANIFEST_NAME
        manifest.chmod(0o600)
        manifest.write_bytes(b"{}\n")
        manifest.chmod(0o400)
        self.fixture.package.chmod(0o500)
        self.assert_error("stale_output_conflict", self.fixture.build)
        self.assertEqual(manifest.read_bytes(), b"{}\n")
        self.assertEqual(first["status"], "COMPLETE")

    def test_missing_package_artifact_and_non_owner_only_roots_fail_closed(self) -> None:
        self.fixture.build()
        manifest = json.loads((self.fixture.package / PACKAGE.MANIFEST_NAME).read_bytes())
        artifact = self.fixture.package / manifest["artifacts"][0]["source"]
        self.fixture.thaw_package()
        artifact.unlink()
        for directory in (path for path in self.fixture.package.rglob("*") if path.is_dir()):
            directory.chmod(0o500)
        self.fixture.package.chmod(0o500)
        self.assert_error(
            "package_artifact_missing_or_unsafe",
            PACKAGE.verify_package,
            self.fixture.package,
            self.fixture.git_sha,
        )

        other = Fixture(self.case / "root-mode")
        other.output.chmod(0o750)
        self.assert_error("directory_mode_invalid", other.build)
        self.assertFalse(other.package.exists())

    def test_inventory_rejects_state_secret_database_and_duplicate_sources(self) -> None:
        for source in ("state/events.db", "config/dashboard.env", "secrets/key.txt"):
            with self.subTest(source=source):
                self.assert_error(
                    "inventory_forbidden_source",
                    PACKAGE.validate_inventory,
                    {
                        "/opt/sentinel/test": (source, "config"),
                        "/usr/local/bin/nats-server": ("external/nats-server", "binary"),
                    },
                )
        self.assert_error(
            "inventory_duplicate",
            PACKAGE.validate_inventory,
            {
                "/opt/sentinel/a": ("config/a.toml", "config"),
                "/opt/sentinel/b": ("config/a.toml", "config"),
                "/usr/local/bin/nats-server": ("external/nats-server", "binary"),
            },
        )

    def test_package_manifest_bytes_are_repeatable_across_independent_roots(self) -> None:
        first = self.fixture.build()
        other = Fixture(self.case / "repeat")
        self.assertEqual(other.git_sha, self.fixture.git_sha)
        second = other.build()
        self.assertEqual(first["manifest_sha256"], second["manifest_sha256"])
        self.assertEqual(
            (self.fixture.package / PACKAGE.MANIFEST_NAME).read_bytes(),
            (other.package / PACKAGE.MANIFEST_NAME).read_bytes(),
        )


if __name__ == "__main__":
    unittest.main()
