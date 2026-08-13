#!/usr/bin/env python3
"""Deterministic fake-root tests for the stopped M0 host provisioner."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import uuid


REPO_ROOT = Path(__file__).resolve().parents[2]
PROVISIONER = REPO_ROOT / "deploy/provision-m0-single-node.sh"
PREFLIGHT = REPO_ROOT / "scripts/product-acceptance/run_m0_preflight.py"
GIT_SHA = "a" * 40


def load_preflight():
    spec = importlib.util.spec_from_file_location("m0_preflight_for_provision_test", PREFLIGHT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


PREFLIGHT_MODULE = load_preflight()
AUTHORITY = PREFLIGHT_MODULE.CANONICAL_RELEASE_ARTIFACTS
STOPPED_UNITS = (
    PREFLIGHT_MODULE.REQUIRED_SERVICES
    | PREFLIGHT_MODULE.REQUIRED_TIMERS
    | set(PREFLIGHT_MODULE.TIMER_SERVICES.values())
    | {PREFLIGHT_MODULE.TARGET_UNIT}
)
MODES = {"binary": 0o755, "script": 0o755, "config": 0o644, "systemd": 0o644}


def encoded(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode("ascii")


class Fixture:
    def __init__(self, base: Path) -> None:
        self.base = base
        self.source = base / "source"
        self.target = base / "target"
        self.stage = base / "stage"
        self.manifest_path = base / "release-manifest.json"
        self.services_path = base / "service-state.json"
        for path in (self.source, self.target):
            path.mkdir(mode=0o700, parents=True)
        self.manifest: dict[str, object] = {
            "version": "1.0",
            "created_at": "2026-08-13T00:00:00Z",
            "git_sha": GIT_SHA,
            "artifacts": [],
        }
        artifacts: list[dict[str, str]] = []
        for destination, (source, kind) in sorted(AUTHORITY.items()):
            path = self.source / source
            path.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
            data = f"artifact:{source}\n".encode("ascii")
            path.write_bytes(data)
            path.chmod(MODES[kind])
            artifacts.append({
                "path": destination,
                "source": source,
                "sha256": hashlib.sha256(data).hexdigest(),
                "type": kind,
            })
        self.manifest["artifacts"] = artifacts
        self.services_path.write_bytes(encoded({unit: "inactive" for unit in sorted(STOPPED_UNITS)}))
        self.services_path.chmod(0o644)
        self.write_manifest()

    @property
    def artifacts(self) -> list[dict[str, str]]:
        value = self.manifest["artifacts"]
        assert isinstance(value, list)
        return value

    def write_manifest(self) -> None:
        self.manifest_path.write_bytes(encoded(self.manifest))
        self.manifest_path.chmod(0o644)
        self.manifest_sha = hashlib.sha256(self.manifest_path.read_bytes()).hexdigest()

    def command(self, *extra: str) -> list[str]:
        return [
            "bash", str(PROVISIONER),
            "--manifest", str(self.manifest_path),
            "--expected-manifest-sha256", self.manifest_sha,
            "--expected-git-sha", GIT_SHA,
            "--source-root", str(self.source),
            "--target-root", str(self.target),
            "--stage-root", str(self.stage),
            "--install-uid", str(os.geteuid()),
            "--install-gid", str(os.getegid()),
            "--service-state-file", str(self.services_path),
            *extra,
        ]

    def run(self, *extra: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            self.command(*extra), cwd=REPO_ROOT, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30, check=False,
        )

    def target_for(self, artifact: dict[str, str]) -> Path:
        return self.target / artifact["path"].lstrip("/")


class ProvisionM0SingleNodeTests(unittest.TestCase):
    def setUp(self) -> None:
        runner_root = os.environ.get("RUNNER_TEMP")
        if runner_root:
            parent = Path(runner_root) / "project-sentinel-cdx1-650-provision-tests"
        else:
            parent = Path("/work/tmp/project-sentinel/cdx1-650-provision-tests")
        parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        parent.chmod(0o700)
        self.case = parent / str(uuid.uuid4())
        self.case.mkdir(mode=0o700)
        self.fixture = Fixture(self.case)

    def tearDown(self) -> None:
        shutil.rmtree(self.case, ignore_errors=True)

    def reason(self, result: subprocess.CompletedProcess[str]) -> str:
        value = json.loads(result.stderr)
        return str(value["reason"])

    def assert_failed(self, result: subprocess.CompletedProcess[str], reason: str) -> None:
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.reason(result), reason)

    def assert_public_pre_mutation_failure(
        self, result: subprocess.CompletedProcess[str], reason: str
    ) -> None:
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual(
            result.stderr,
            encoded({"schema_version": 1, "status": "FAIL", "reason": reason}).decode("ascii"),
        )
        self.assertNotIn("Traceback", result.stderr)
        self.assertNotIn(str(self.case), result.stderr)
        self.assertEqual(list(self.fixture.target.iterdir()), [])
        self.assertFalse((self.fixture.stage / "provision-receipt.json").exists())

    def test_complete_fake_root_install_and_receipt(self) -> None:
        result = self.fixture.run()
        self.assertEqual(result.returncode, 0, result.stderr)
        output = json.loads(result.stdout)
        self.assertEqual(output["status"], "COMPLETE")
        self.assertEqual(output["artifact_count"], len(AUTHORITY))
        self.assertEqual(output["changed_count"], len(AUTHORITY))
        self.assertFalse(output["services_started"])
        self.assertTrue(self.fixture.target_for(next(
            row for row in self.fixture.artifacts if row["source"] == "external/nats-server"
        )).is_file())
        self.assertEqual(len(list((self.fixture.target / "opt/sentinel/config/agents").glob("*.toml"))), 60)
        for artifact in self.fixture.artifacts:
            target = self.fixture.target_for(artifact)
            self.assertEqual(hashlib.sha256(target.read_bytes()).hexdigest(), artifact["sha256"])
            self.assertEqual(target.stat().st_mode & 0o777, MODES[artifact["type"]])
        receipt = json.loads((self.fixture.stage / "provision-receipt.json").read_text())
        self.assertEqual(receipt["manifest_sha256"], self.fixture.manifest_sha)
        self.assertNotIn("source", receipt)
        self.assertNotIn("target", receipt)

    def test_idempotent_retry_has_no_mutation(self) -> None:
        first = self.fixture.run()
        self.assertEqual(first.returncode, 0, first.stderr)
        before = {
            row["path"]: (self.fixture.target_for(row).stat().st_ino,
                          self.fixture.target_for(row).stat().st_mtime_ns)
            for row in self.fixture.artifacts
        }
        second = self.fixture.run()
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(json.loads(second.stdout)["changed_count"], 0)
        after = {
            row["path"]: (self.fixture.target_for(row).stat().st_ino,
                          self.fixture.target_for(row).stat().st_mtime_ns)
            for row in self.fixture.artifacts
        }
        self.assertEqual(before, after)

    def test_symlinked_stage_lock_is_public_safe_and_has_no_target_effect(self) -> None:
        self.fixture.stage.mkdir(mode=0o700)
        (self.fixture.stage / ".provision.lock").symlink_to(self.fixture.manifest_path)
        self.assert_public_pre_mutation_failure(
            self.fixture.run(), "stage_lock_unsafe"
        )

    def test_unsafe_and_stale_stage_operation_fail_without_target_effect(self) -> None:
        for state, reason in (
            ("symlink", "stage_operation_unsafe"),
            ("stale", "stage_operation_stale"),
        ):
            with self.subTest(state=state):
                fixture = Fixture(self.case / "cases" / f"operation-{state}")
                fixture.stage.mkdir(mode=0o700)
                operation = fixture.stage / "operation"
                if state == "symlink":
                    operation.symlink_to(fixture.source)
                else:
                    operation.mkdir(mode=0o700)
                result = fixture.run()
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(
                    result.stderr,
                    encoded({"schema_version": 1, "status": "FAIL", "reason": reason}).decode("ascii"),
                )
                self.assertNotIn("Traceback", result.stderr)
                self.assertNotIn(str(fixture.base), result.stderr)
                self.assertEqual(list(fixture.target.iterdir()), [])
                self.assertFalse((fixture.stage / "provision-receipt.json").exists())

    def test_unexpected_pre_mutation_error_is_static_and_public_safe(self) -> None:
        self.assert_public_pre_mutation_failure(
            self.fixture.run("--inject-pre-mutation-error"), "internal_failure"
        )

    def test_missing_nats_and_other_required_artifact_fail(self) -> None:
        for source in ("external/nats-server", "config/agents/AGENT-60-KATRIN-DELIVERY.toml"):
            with self.subTest(source=source):
                fixture = Fixture(self.case / "cases" / source.replace("/", "-"))
                fixture.manifest["artifacts"] = [row for row in fixture.artifacts if row["source"] != source]
                fixture.write_manifest()
                self.assert_failed(fixture.run(), "manifest_artifact_count")

    def test_tampered_source_fails_before_target_mutation(self) -> None:
        row = self.fixture.artifacts[0]
        path = self.fixture.source / row["source"]
        path.write_bytes(b"tampered\n")
        path.chmod(MODES[row["type"]])
        self.assert_failed(self.fixture.run(), "source_hash_mismatch")

    def test_duplicate_and_unexpected_or_state_artifact_fail(self) -> None:
        cases = []
        duplicate = copy.deepcopy(self.fixture.manifest)
        duplicate["artifacts"][-1] = copy.deepcopy(duplicate["artifacts"][0])
        cases.append((duplicate, "manifest_artifact_duplicate"))
        unexpected = copy.deepcopy(self.fixture.manifest)
        unexpected["artifacts"][0]["path"] = "/opt/sentinel/data/events.db"
        cases.append((unexpected, "manifest_artifact_authority_mismatch"))
        secret = copy.deepcopy(self.fixture.manifest)
        secret["artifacts"][0]["path"] = "/etc/sentinel/dashboard.env"
        cases.append((secret, "manifest_artifact_authority_mismatch"))
        for manifest, reason in cases:
            with self.subTest(reason=reason):
                fixture = Fixture(self.case / "cases" / f"{reason}-{len(list((self.case / 'cases').glob('*'))) if (self.case / 'cases').exists() else 0}")
                fixture.manifest = manifest
                fixture.write_manifest()
                self.assert_failed(fixture.run(), reason)

    def test_manifest_source_type_git_and_digest_authority_fail(self) -> None:
        cases = ("source", "type")
        for field in cases:
            with self.subTest(field=field):
                fixture = Fixture(self.case / "cases" / field)
                fixture.artifacts[0][field] = "wrong" if field == "source" else "script"
                fixture.write_manifest()
                self.assert_failed(fixture.run(), "manifest_artifact_authority_mismatch")
        bad_git = self.fixture.run("--expected-git-sha", "b" * 40)
        self.assert_failed(bad_git, "manifest_git_sha_mismatch")
        command = self.fixture.command()
        command[command.index(self.fixture.manifest_sha)] = "0" * 64
        result = subprocess.run(command, cwd=REPO_ROOT, text=True, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=30, check=False)
        self.assert_failed(result, "manifest_authority_digest_mismatch")

    def test_source_symlink_hardlink_and_mode_attacks_fail(self) -> None:
        for attack, reason in (("symlink", "source_path_unsafe"),
                               ("hardlink", "source_file_authority_invalid"),
                               ("mode", "source_file_mode_invalid")):
            with self.subTest(attack=attack):
                fixture = Fixture(self.case / "cases" / attack)
                row = fixture.artifacts[0]
                path = fixture.source / row["source"]
                if attack == "symlink":
                    path.unlink()
                    path.symlink_to(fixture.manifest_path)
                elif attack == "hardlink":
                    other = fixture.base / "other"
                    os.link(path, other)
                else:
                    path.chmod(0o666)
                self.assert_failed(fixture.run(), reason)

    def test_parent_symlink_target_mode_and_owner_contract_fail(self) -> None:
        outside = self.case / "outside"
        outside.mkdir()
        (self.fixture.target / "opt").symlink_to(outside)
        self.assert_failed(self.fixture.run(), "target_parent_symlink")

        mode_fixture = Fixture(self.case / "cases" / "target-mode")
        row = mode_fixture.artifacts[0]
        target = mode_fixture.target_for(row)
        target.parent.mkdir(parents=True)
        target.write_bytes(b"old")
        target.chmod(0o666)
        self.assert_failed(mode_fixture.run(), "target_file_owner_or_mode_invalid")

        owner_result = Fixture(self.case / "cases" / "owner").run("--install-uid", str(os.geteuid() + 1))
        self.assert_failed(owner_result, "install_owner_invalid")

    def test_running_failed_or_incomplete_service_state_fails(self) -> None:
        for mutation, reason in (("running", "service_running_or_failed"),
                                 ("failed", "service_running_or_failed"),
                                 ("missing", "service_state_incomplete")):
            with self.subTest(mutation=mutation):
                fixture = Fixture(self.case / "cases" / mutation)
                states = {unit: "inactive" for unit in sorted(STOPPED_UNITS)}
                unit = sorted(states)[0]
                if mutation == "missing":
                    states.pop(unit)
                else:
                    states[unit] = mutation
                fixture.services_path.write_bytes(encoded(states))
                fixture.services_path.chmod(0o644)
                self.assert_failed(fixture.run(), reason)

    def test_partial_failure_restores_old_files_and_removes_new_files(self) -> None:
        first, second = self.fixture.artifacts[:2]
        old = self.fixture.target_for(first)
        old.parent.mkdir(parents=True)
        old.write_bytes(b"previous-release\n")
        old.chmod(MODES[first["type"]])
        result = self.fixture.run("--fail-after", "2")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.reason(result), "injected_install_failure")
        self.assertEqual(old.read_bytes(), b"previous-release\n")
        self.assertFalse(self.fixture.target_for(second).exists())
        receipt = json.loads((self.fixture.stage / "provision-receipt.json").read_text())
        self.assertEqual(receipt["status"], "ROLLED_BACK")
        self.assertFalse(receipt["services_started"])

    def test_manifest_and_source_path_ambiguity_fail(self) -> None:
        for field, value in (("source", "../secret"), ("path", "/opt/sentinel/../data")):
            with self.subTest(field=field):
                fixture = Fixture(self.case / "cases" / field)
                fixture.artifacts[0][field] = value
                fixture.write_manifest()
                self.assert_failed(fixture.run(), "path_invalid")


if __name__ == "__main__":
    unittest.main()
