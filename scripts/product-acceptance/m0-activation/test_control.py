#!/usr/bin/env python3
"""Deterministic tests for M0 activation and restart control."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import unittest
import uuid
from unittest import mock


HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("m0_activation_control", HERE / "control.py")
assert SPEC is not None and SPEC.loader is not None
control = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = control
SPEC.loader.exec_module(control)
JOURNEY_TEST_SPEC = importlib.util.spec_from_file_location(
    "m0_activation_journey_test_ssot", HERE.parent / "test_run_m0_journey.py"
)
assert JOURNEY_TEST_SPEC is not None and JOURNEY_TEST_SPEC.loader is not None
journey_test = importlib.util.module_from_spec(JOURNEY_TEST_SPEC)
sys.modules[JOURNEY_TEST_SPEC.name] = journey_test
JOURNEY_TEST_SPEC.loader.exec_module(journey_test)


def encoded(value: object) -> bytes:
    return control.canonical(value)


class FakeRunner:
    def __init__(self, fixture: "Fixture") -> None:
        self.fixture = fixture
        self.calls: list[tuple[str, ...]] = []
        self.environments: list[dict[str, str]] = []
        self.states = {
            unit: {"LoadState": "loaded", "ActiveState": "inactive",
                   "SubState": "dead", "Result": "success"}
            for unit in control.INSPECT_UNITS
        }
        self.fail_command: tuple[str, ...] | None = None
        self.partial_start_at: str | None = None
        self.rollback_failure: str | None = None
        self.readiness_failure = False
        self.restart_never_ready = False
        self.nightrun_fails_after_start = False
        self.mutate_during_preflight: str | None = None
        self.journey_effects: dict[str, int] = {}
        self.after_target_start = False
        self.show_failure_unit: str | None = None
        self.target_start_returncode = 0
        self.journey_mutation: str | None = None
        self.journey_mutation_final_only = False

    def __call__(
        self, argv: tuple[str, ...], timeout: float, environment: dict[str, str]
    ) -> control.Result:
        del timeout
        self.calls.append(argv)
        self.environments.append(dict(environment))
        if self.fail_command is not None and argv[:len(self.fail_command)] == self.fail_command:
            return control.Result(1, b"private /work/path secret")
        if argv[0] == str(control.SYSTEMCTL):
            verb = argv[1]
            if verb == "show":
                unit = argv[2]
                if self.after_target_start and unit == self.show_failure_unit:
                    return control.Result(1)
                state = self.states[unit]
                properties = argv[3].removeprefix("--property=").split(",")
                data = "".join(f"{key}={state[key]}\n" for key in properties)
                return control.Result(0, data.encode("ascii"))
            if verb == "daemon-reload":
                return control.Result(0)
            if verb == "start":
                self.after_target_start = True
                for unit in control.ALL_UNITS:
                    if self.partial_start_at == unit:
                        break
                    self._ready(unit)
                if self.nightrun_fails_after_start:
                    self.states["sentinel-nightrun.service"]["Result"] = "failed"
                    self.readiness_failure = True
                return control.Result(self.target_start_returncode)
            if verb == "stop":
                unit = argv[2]
                if self.rollback_failure == unit:
                    return control.Result(1)
                self.states[unit].update(ActiveState="inactive", SubState="dead")
                return control.Result(0)
            if verb == "restart":
                unit = argv[2]
                if not self.restart_never_ready:
                    self._ready(unit)
                else:
                    self.states[unit].update(ActiveState="activating", SubState="start")
                return control.Result(0)
        if argv[:2] == (str(control.PYTHON), str(control.PREFLIGHT_PROGRAM)):
            if self.mutate_during_preflight == "ledger":
                self.fixture.ledger.write_bytes(b"{}\n")
            elif self.mutate_during_preflight == "evidence":
                self.fixture.evidence.write_bytes(b"{}\n")
            if self.readiness_failure:
                return control.Result(1, encoded({"runtime_preflight_pass": False}))
            return control.Result(0, encoded({
                "schema_version": 1, "claim": "runtime_preflight_pass",
                "runtime_preflight_pass": True, "m0_acceptance_pass": False,
                "result_digest": "d" * 64,
            }))
        if argv[:2] == (str(control.PYTHON), str(control.JOURNEY_PROGRAM)):
            checkpoint = None
            if "--stop-after-checkpoint" in argv:
                checkpoint = argv[argv.index("--stop-after-checkpoint") + 1]
            self._journey(checkpoint)
            if self.journey_mutation is not None and (
                not self.journey_mutation_final_only or checkpoint is None
            ):
                self._mutate_journey_state(self.journey_mutation)
                self.journey_mutation = None
            return control.Result(0, b"public journey result\n")
        raise AssertionError(f"unexpected command: {argv}")

    def _ready(self, unit: str) -> None:
        if unit == control.TARGET:
            sub = "active"
        elif unit in control.TIMERS:
            sub = "waiting"
        else:
            sub = "running"
        self.states[unit].update(ActiveState="active", SubState=sub, Result="success")

    def _journey(self, checkpoint: str | None) -> None:
        module = self.fixture.journey_contract.module
        if self.fixture.ledger.exists():
            ledger = json.loads(self.fixture.ledger.read_text())
        else:
            ledger = {
                "schema_version": self.fixture.plan["schema_version"],
                "journey_id": self.fixture.plan["journey_id"],
                "plan_digest": module.digest(self.fixture.plan),
                "target_origin": module.validate_base_url("http://127.0.0.1:8084"),
                "chain_tip": module.ZERO_DIGEST,
                "completed": {},
            }
        replayed: set[str] = set()
        for step in self.fixture.plan["steps"]:
            step_id = step["id"]
            if step_id in ledger["completed"]:
                replayed.add(step_id)
            else:
                captures = {}
                for name, specification in step.get("capture", {}).items():
                    capture_type = specification["type"]
                    if capture_type == "digest":
                        value: object = hashlib.sha256(name.encode("ascii")).hexdigest()
                    elif capture_type == "state":
                        value = "accepted"
                    elif capture_type == "boolean":
                        value = True
                    elif capture_type == "integer":
                        value = 1
                    else:
                        value = f"{name}-1"
                    captures[name] = value
                record = {
                    "captures": captures,
                    "checkpoint": step.get("checkpoint"),
                    "kind": step.get("kind", "positive"),
                    "method": step["method"],
                    "operation_id": module.stable_operation_id(
                        self.fixture.plan["journey_id"], step_id
                    ),
                    "path": step["path"],
                    "phase": step["phase"],
                    "prior_record_digest": ledger["chain_tip"],
                    "query": "",
                    "replay_contract": "server_response_verified",
                    "request_digest": hashlib.sha256(
                        f"request:{step_id}".encode("ascii")
                    ).hexdigest(),
                    "status": step.get("expected_status", [200])[0],
                }
                record["record_digest"] = module.record_digest(record)
                ledger["completed"][step_id] = record
                ledger["chain_tip"] = record["record_digest"]
                self.journey_effects[step_id] = 1
            if checkpoint is not None and step.get("checkpoint") == checkpoint:
                break
        self.fixture.ledger.write_bytes(encoded(ledger))
        evidence = module.build_evidence(
            self.fixture.plan, ledger,
            "checkpoint_reached" if checkpoint else "complete",
            checkpoint, replayed,
        )
        self.fixture.evidence.write_bytes(encoded(evidence))

    def _mutate_journey_state(self, mutation: str) -> None:
        ledger = json.loads(self.fixture.ledger.read_text())
        evidence = json.loads(self.fixture.evidence.read_text())
        if mutation == "empty_ledger":
            self.fixture.ledger.write_bytes(encoded({}))
        elif mutation == "empty_evidence":
            self.fixture.evidence.write_bytes(encoded({}))
        elif mutation == "noncanonical_ledger":
            self.fixture.ledger.write_bytes(self.fixture.ledger.read_bytes() + b" \n")
        elif mutation == "noncanonical_evidence":
            self.fixture.evidence.write_bytes(self.fixture.evidence.read_bytes() + b" \n")
        elif mutation == "reordered_evidence":
            evidence["steps"] = list(reversed(evidence["steps"]))
            self.fixture.evidence.write_bytes(encoded(evidence))
        elif mutation == "reordered_ledger":
            keys = list(ledger["completed"])
            if len(keys) < 2:
                raise AssertionError("reordered ledger fixture needs two records")
            ledger["completed"][keys[0]], ledger["completed"][keys[1]] = (
                ledger["completed"][keys[1]], ledger["completed"][keys[0]]
            )
            self.fixture.ledger.write_bytes(encoded(ledger))
        elif mutation == "foreign_plan":
            ledger["plan_digest"] = "f" * 64
            self.fixture.ledger.write_bytes(encoded(ledger))
        elif mutation == "changed_plan_file":
            plan = copy.deepcopy(self.fixture.plan)
            plan["journey_id"] = "journey-forged"
            self.fixture.plan_path.write_bytes(encoded(plan))
        elif mutation == "foreign_origin":
            ledger["target_origin"] = "http://127.0.0.1:9999"
            self.fixture.ledger.write_bytes(encoded(ledger))
        elif mutation == "semantic_record":
            module = self.fixture.journey_contract.module
            prior = module.ZERO_DIGEST
            for index, step in enumerate(self.fixture.plan["steps"]):
                step_id = step["id"]
                if step_id not in ledger["completed"]:
                    break
                record = ledger["completed"][step_id]
                if index == 0:
                    record["path"] = "/operator/forbidden"
                record["prior_record_digest"] = prior
                record["record_digest"] = module.record_digest(record)
                prior = record["record_digest"]
            ledger["chain_tip"] = prior
            self.fixture.ledger.write_bytes(encoded(ledger))
        else:
            raise AssertionError(f"unknown journey mutation: {mutation}")


class Fixture:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.root.mkdir(mode=0o700, parents=True)
        self.manifest = self.root / "release-manifest.json"
        self.provision = self.root / "provision-receipt.json"
        self.activation = self.root / "activation-receipt.json"
        self.restart_receipt = self.root / "restart-receipt.json"
        self.plan_path = self.root / "journey-plan.json"
        self.control_path = self.root / "restart-control.json"
        self.ledger = self.root / "journey-ledger.json"
        self.evidence = self.root / "journey-evidence.json"
        self.git_sha = "a" * 40
        self.manifest_value = {
            "version": "1.0", "created_at": "2026-08-13T00:00:00Z",
            "git_sha": self.git_sha,
            "artifacts": [{"path": f"/artifact/{index}"} for index in range(111)],
        }
        self.manifest.write_bytes(encoded(self.manifest_value))
        self.manifest_sha = control.digest_bytes(self.manifest.read_bytes())
        unsigned = {
            "schema_version": 1, "status": "COMPLETE", "git_sha": self.git_sha,
            "manifest_sha256": self.manifest_sha, "artifact_count": 111,
            "changed_count": 111, "artifact_set_digest": "b" * 64,
            "services_started": False,
        }
        self.provision.write_bytes(encoded(unsigned))
        self.provision_sha = control.digest_bytes(self.provision.read_bytes())
        self.plan = journey_test.canonical_plan()
        self.plan_path.write_bytes(encoded(self.plan))
        self.plan_sha = control.digest_bytes(self.plan_path.read_bytes())
        self.control_value = {
            "schema_version": 1, "journey_plan_sha256": self.plan_sha,
            "checkpoint_services": {
                "after_customer_request": "sentinel-daemon.service",
                "after_governed_project": "sentinel-projection.service",
                "after_workbench_execution": "sentinel-daemon.service",
                "after_qa_release": "sentinel-judge.service",
                "after_delivery": "sentinel-daemon.service",
                "after_acceptance": "sentinel-daemon.service",
            },
        }
        self.control_path.write_bytes(encoded(self.control_value))
        self.control_sha = control.digest_bytes(self.control_path.read_bytes())
        for path in (self.manifest, self.provision, self.plan_path, self.control_path):
            path.chmod(0o600)
        self.journey_contract = control.load_journey_contract(self.plan_path)

    def preflight(self) -> control.PreflightArgs:
        return control.PreflightArgs(
            self.manifest, Path("/opt/sentinel/config/product-acceptance/m0-contract.toml"),
            Path("/opt/sentinel/config/work-profiles/web-project-v1.toml"),
            Path("/opt/sentinel/config/agents"), Path("/etc/sentinel/operator.env"),
            self.git_sha, self.manifest_sha, 0.15,
        )

    def journey_args(self) -> control.JourneyArgs:
        return control.JourneyArgs(
            self.plan_path, "http://127.0.0.1:8084",
            ("operator=OPERATOR_TOKEN", "customer=CUSTOMER_TOKEN", "agent=AGENT_TOKEN"),
            self.ledger, self.evidence, 0.15,
        )

    def activate(self, runner: FakeRunner) -> dict[str, object]:
        return control.activate(
            runner, self.provision, self.provision_sha, self.manifest,
            self.manifest_sha, self.git_sha, self.preflight(), self.activation, 0.15,
        )

    def restart(self, runner: FakeRunner) -> dict[str, object]:
        return control.restart_journey(
            runner, self.journey_args(), self.control_path, self.control_sha,
            self.preflight(), self.restart_receipt,
        )


class ControlTests(unittest.TestCase):
    def setUp(self) -> None:
        base = Path(os.environ.get("RUNNER_TEMP", "/work/tmp/project-sentinel"))
        self.root = base / "project-sentinel-cdx1-650-activation" / str(uuid.uuid4())
        self.fixture = Fixture(self.root)
        self.runner = FakeRunner(self.fixture)
        self.lock_patch = mock.patch.object(
            control, "CONTROL_LOCK", self.root / ".m0-activation-control.lock"
        )
        self.lock_patch.start()
        self.environment_patch = mock.patch.dict(os.environ, {
            "OPERATOR_TOKEN": "operator-secret-value",
            "CUSTOMER_TOKEN": "customer-secret-value",
            "AGENT_TOKEN": "agent-secret-value",
        })
        self.environment_patch.start()

    def tearDown(self) -> None:
        self.environment_patch.stop()
        self.lock_patch.stop()
        shutil.rmtree(self.root, ignore_errors=True)

    def test_activation_success_uses_only_target_and_preflight(self) -> None:
        result = self.fixture.activate(self.runner)
        self.assertEqual(result["status"], "ACTIVE")
        self.assertEqual(result["started_unit_count"], len(control.ALL_UNITS))
        persisted = json.loads(self.fixture.activation.read_text())
        unsigned = dict(persisted)
        receipt_digest = unsigned.pop("receipt_sha256")
        self.assertEqual(receipt_digest, control.digest(unsigned))
        mutations = [call for call in self.runner.calls if call[1] in
                     {"start", "stop", "restart", "daemon-reload"}]
        self.assertEqual(mutations[0], (str(control.SYSTEMCTL), "daemon-reload"))
        self.assertEqual(mutations[1], (str(control.SYSTEMCTL), "start", control.TARGET))
        self.assertEqual(len(mutations), 2)
        self.assertFalse(result["m0_acceptance_pass"])

    def test_child_environment_is_exact_and_secret_values_never_enter_argv(self) -> None:
        hostile = {
            "LD_PRELOAD": "/private/inject.so",
            "PYTHONPATH": "/private/python",
            "PYTHONSTARTUP": "/private/start.py",
            "HTTP_PROXY": "http://proxy.invalid",
            "PROVIDER_API_KEY": "foreign-provider-secret",
            "UNRELATED_TOKEN": "foreign-secret",
        }
        with mock.patch.dict(os.environ, hostile, clear=False):
            self.fixture.restart(self.runner)
        credential_names = {"OPERATOR_TOKEN", "CUSTOMER_TOKEN", "AGENT_TOKEN"}
        for argv, environment in zip(self.runner.calls, self.runner.environments):
            expected = dict(control.BASE_CHILD_ENV)
            if argv[:2] == (str(control.PYTHON), str(control.JOURNEY_PROGRAM)):
                expected.update({name: os.environ[name] for name in credential_names})
            self.assertEqual(environment, expected)
            joined = "\0".join(argv)
            for secret in (*hostile.values(), *(os.environ[name] for name in credential_names)):
                self.assertNotIn(secret, joined)
        persisted = self.fixture.restart_receipt.read_text()
        for secret in (*hostile.values(), *(os.environ[name] for name in credential_names)):
            self.assertNotIn(secret, persisted)

    def test_repository_target_topology_is_the_controller_authority(self) -> None:
        target = (HERE.parents[2] / "deploy/systemd/sentinel.target").read_text()
        wants = next(line for line in target.splitlines() if line.startswith("Wants="))
        self.assertEqual(tuple(wants.removeprefix("Wants=").split()), control.TOPOLOGY)

    def test_forged_stale_receipt_and_wrong_authority_fail_before_command(self) -> None:
        cases = (
            ("receipt_digest", "0" * 64, self.fixture.manifest_sha, self.fixture.git_sha,
             "provision_receipt_digest_mismatch"),
            ("manifest_digest", self.fixture.provision_sha, "0" * 64, self.fixture.git_sha,
             "manifest_digest_mismatch"),
            ("git", self.fixture.provision_sha, self.fixture.manifest_sha, "c" * 40,
             "provision_receipt_authority_mismatch"),
        )
        for _, receipt_sha, manifest_sha, git_sha, reason in cases:
            with self.subTest(reason=reason):
                self.fixture.activation.unlink(missing_ok=True)
                runner = FakeRunner(self.fixture)
                original = self.fixture.preflight()
                preflight = control.PreflightArgs(
                    original.manifest, original.contract, original.profile,
                    original.agents_dir, original.operator_credential_file,
                    git_sha, manifest_sha, original.timeout,
                )
                with self.assertRaisesRegex(control.ControlError, reason):
                    control.activate(
                        runner, self.fixture.provision, receipt_sha,
                        self.fixture.manifest, manifest_sha, git_sha,
                        preflight, self.fixture.activation, 0.15,
                    )
                self.assertEqual(runner.calls, [])

    def test_activation_preflight_must_use_the_same_manifest_authority(self) -> None:
        preflight = self.fixture.preflight()
        changed = control.PreflightArgs(
            preflight.manifest, preflight.contract, preflight.profile,
            preflight.agents_dir, preflight.operator_credential_file,
            preflight.expected_git_sha, "0" * 64, preflight.timeout,
        )
        with self.assertRaisesRegex(control.ControlError, "preflight_authority_mismatch"):
            control.activate(
                self.runner, self.fixture.provision, self.fixture.provision_sha,
                self.fixture.manifest, self.fixture.manifest_sha, self.fixture.git_sha,
                changed, self.fixture.activation, 0.15,
            )
        self.assertEqual(self.runner.calls, [])

    def test_running_or_failed_unit_stops_before_daemon_reload(self) -> None:
        for field, value in (("ActiveState", "active"), ("Result", "failed")):
            with self.subTest(field=field):
                self.fixture.activation.unlink(missing_ok=True)
                runner = FakeRunner(self.fixture)
                runner.states[control.SERVICES[1]][field] = value
                with self.assertRaisesRegex(control.ControlError, "unit_not_stopped_cleanly"):
                    self.fixture.activate(runner)
                self.assertFalse(any(call[1] == "daemon-reload" for call in runner.calls))

    def test_failed_nightrun_oneshot_is_not_reset_or_accepted(self) -> None:
        self.runner.states["sentinel-nightrun.service"]["Result"] = "failed"
        with self.assertRaisesRegex(control.ControlError, "unit_not_stopped_cleanly"):
            self.fixture.activate(self.runner)
        verbs = [call[1] for call in self.runner.calls]
        self.assertNotIn("reset-failed", verbs)
        self.assertNotIn("daemon-reload", verbs)
        self.assertNotIn("start", verbs)
        persisted = json.loads(self.fixture.activation.read_text())
        self.assertEqual(persisted["status"], "FAILED")

    def test_nightrun_failure_after_parallel_target_start_rolls_back(self) -> None:
        self.runner.nightrun_fails_after_start = True
        with self.assertRaisesRegex(control.ControlError, "readiness_failed"):
            self.fixture.activate(self.runner)
        verbs = [call[1] for call in self.runner.calls]
        self.assertNotIn("reset-failed", verbs)
        self.assertEqual(json.loads(self.fixture.activation.read_text())["status"], "ROLLED_BACK")
        self.assertTrue(all(
            self.runner.states[unit]["ActiveState"] == "inactive"
            for unit in control.ALL_UNITS
        ))

    def test_controller_lock_is_fail_closed_before_commands(self) -> None:
        lock_path = self.root / ".m0-activation-control.lock"
        fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
        try:
            import fcntl
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(control.ControlError, "controller_busy"):
                self.fixture.activate(self.runner)
        finally:
            os.close(fd)
        self.assertEqual(self.runner.calls, [])

    def test_symlinked_input_parent_and_output_parent_fail_before_commands(self) -> None:
        real = self.root / "real"
        real.mkdir(mode=0o700)
        linked = self.root / "linked"
        linked.symlink_to(real, target_is_directory=True)
        manifest = linked / "manifest.json"
        (real / "manifest.json").write_bytes(self.fixture.manifest.read_bytes())
        (real / "manifest.json").chmod(0o600)
        original = self.fixture.preflight()
        preflight = control.PreflightArgs(
            manifest, original.contract, original.profile, original.agents_dir,
            original.operator_credential_file, original.expected_git_sha,
            original.expected_manifest_sha256, original.timeout,
        )
        with self.assertRaisesRegex(control.ControlError, "path_component_unsafe"):
            control.activate(
                self.runner, self.fixture.provision, self.fixture.provision_sha,
                manifest, self.fixture.manifest_sha, self.fixture.git_sha,
                preflight, self.fixture.activation, 0.15,
            )
        with self.assertRaisesRegex(control.ControlError, "path_component_unsafe"):
            control.atomic_json(linked / "receipt.json", {"status": "FAIL"})
        self.assertEqual(self.runner.calls, [])

    def test_first_command_failure_allows_no_non_readback_command(self) -> None:
        self.runner.fail_command = (str(control.SYSTEMCTL), "daemon-reload")
        with self.assertRaisesRegex(control.ControlError, "daemon_reload_failed"):
            self.fixture.activate(self.runner)
        reload_index = self.runner.calls.index((str(control.SYSTEMCTL), "daemon-reload"))
        self.assertEqual(self.runner.calls[reload_index + 1:], [])

    def test_failed_target_start_stops_the_attempted_target(self) -> None:
        self.runner.fail_command = (str(control.SYSTEMCTL), "start", control.TARGET)
        with self.assertRaisesRegex(control.ControlError, "target_start_failed"):
            self.fixture.activate(self.runner)
        stops = [call for call in self.runner.calls if call[1] == "stop"]
        self.assertEqual(
            stops,
            [
                (str(control.SYSTEMCTL), "stop", unit)
                for unit in control.ROLLBACK_ORDER
            ],
        )

    def test_partial_failed_start_and_readback_error_stop_full_topology_target_first(self) -> None:
        cases = ("partial_failed_start", "post_start_readback_error")
        for case in cases:
            with self.subTest(case=case):
                fixture = Fixture(self.root / case)
                runner = FakeRunner(fixture)
                if case == "partial_failed_start":
                    runner.partial_start_at = control.TOPOLOGY[4]
                    runner.target_start_returncode = 1
                    reason = "target_start_failed"
                else:
                    runner.show_failure_unit = control.TOPOLOGY[4]
                    reason = "activation_rollback_failed"
                with self.assertRaisesRegex(control.ControlError, reason):
                    fixture.activate(runner)
                stopped = [call[2] for call in runner.calls if call[1] == "stop"]
                self.assertEqual(stopped, list(control.ROLLBACK_ORDER))
                self.assertEqual(stopped[0], control.TARGET)
                self.assertTrue(all(
                    runner.states[unit]["ActiveState"] == "inactive"
                    for unit in control.ALL_UNITS
                ))

    def test_partial_start_and_readiness_failure_roll_back_full_owned_topology(self) -> None:
        for partial, readiness in ((control.SERVICES[3], False), (None, True)):
            with self.subTest(partial=partial, readiness=readiness):
                self.fixture.activation.unlink(missing_ok=True)
                runner = FakeRunner(self.fixture)
                runner.partial_start_at = partial
                runner.readiness_failure = readiness
                with self.assertRaises(control.ControlError):
                    self.fixture.activate(runner)
                stopped = [call[2] for call in runner.calls if call[1] == "stop"]
                self.assertEqual(stopped, list(control.ROLLBACK_ORDER))
                self.assertNotIn("foreign.service", stopped)
                self.assertEqual(stopped[0], control.TARGET)
                self.assertTrue(all(
                    runner.states[unit]["ActiveState"] == "inactive" for unit in stopped
                ))

    def test_rollback_failure_is_not_hidden(self) -> None:
        self.runner.readiness_failure = True
        self.runner.rollback_failure = control.SERVICES[-1]
        with self.assertRaisesRegex(control.ControlError, "activation_rollback_failed"):
            self.fixture.activate(self.runner)
        receipt = json.loads(self.fixture.activation.read_text())
        self.assertEqual(receipt["status"], "ROLLBACK_FAILED")

    def test_restart_controller_replays_same_ledger_and_ids(self) -> None:
        result = self.fixture.restart(self.runner)
        self.assertEqual(result["status"], "COMPLETE")
        self.assertTrue(result["authoritative_replay_verified"])
        restarts = [call[2] for call in self.runner.calls if call[1] == "restart"]
        self.assertEqual(restarts, list(self.fixture.control_value["checkpoint_services"].values()))
        self.assertEqual(
            self.runner.journey_effects,
            {step["id"]: 1 for step in self.fixture.plan["steps"]},
        )

    def test_journey_ssot_rejects_tampered_ledger_and_evidence_before_restart(self) -> None:
        mutations = (
            "empty_ledger", "empty_evidence", "noncanonical_ledger",
            "noncanonical_evidence", "reordered_evidence", "reordered_ledger",
            "foreign_plan", "changed_plan_file", "foreign_origin",
            "semantic_record",
        )
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                fixture = Fixture(self.root / mutation)
                runner = FakeRunner(fixture)
                runner.journey_mutation = mutation
                with self.assertRaises(control.ControlError):
                    fixture.restart(runner)
                self.assertFalse(any(call[1] == "restart" for call in runner.calls))

    def test_final_empty_evidence_cannot_claim_authoritative_replay(self) -> None:
        self.runner.journey_mutation = "empty_evidence"
        self.runner.journey_mutation_final_only = True
        with self.assertRaises(control.ControlError):
            self.fixture.restart(self.runner)
        self.assertFalse(self.fixture.restart_receipt.exists())

    def test_bounded_process_kills_oversized_and_timed_out_children(self) -> None:
        scripts = (
            ("oversized", "import os; os.write(1, b'x' * (4 * 1024 * 1024 + 1))",
             2.0, "command_output_oversized"),
            ("timeout", "import time; time.sleep(10)", 0.05, "command_timeout"),
        )
        for name, source, timeout, reason in scripts:
            with self.subTest(name=name):
                helper = self.root / f"{name}.py"
                helper.write_text(source, encoding="ascii")
                helper.chmod(0o600)
                with mock.patch.object(control, "PREFLIGHT_PROGRAM", helper):
                    argv = (str(control.PYTHON), str(helper))
                    environment = control.child_environment(argv)
                    started = __import__("time").monotonic()
                    with self.assertRaisesRegex(control.ControlError, reason):
                        control.production_runner(argv, timeout, environment)
                    self.assertLess(__import__("time").monotonic() - started, 3.0)

    def test_restart_control_rejects_unsafe_unit_digest_and_executable(self) -> None:
        bad = copy.deepcopy(self.fixture.control_value)
        bad["checkpoint_services"]["after_customer_request"] = "foreign.service"
        self.fixture.control_path.write_bytes(encoded(bad))
        bad_sha = control.digest_bytes(self.fixture.control_path.read_bytes())
        with self.assertRaisesRegex(control.ControlError, "restart_control_authority_mismatch"):
            control.restart_journey(
                self.runner, self.fixture.journey_args(), self.fixture.control_path,
                bad_sha, self.fixture.preflight(), self.fixture.restart_receipt,
            )
        with self.assertRaisesRegex(control.ControlError, "executable_not_allowed"):
            control.validate_executables(
                Path("/bin/sh"), control.PYTHON, control.PREFLIGHT_PROGRAM,
                control.JOURNEY_PROGRAM,
            )

    def test_changed_ledger_or_evidence_fails_after_restart(self) -> None:
        for mutation, reason in (("ledger", "ledger_changed_during_restart"),
                                 ("evidence", "evidence_changed_during_restart")):
            with self.subTest(mutation=mutation):
                fixture = Fixture(self.root / mutation)
                runner = FakeRunner(fixture)
                runner.mutate_during_preflight = mutation
                with self.assertRaisesRegex(control.ControlError, reason):
                    fixture.restart(runner)

    def test_restart_failure_and_timeout_are_visible(self) -> None:
        self.runner.fail_command = (str(control.SYSTEMCTL), "restart")
        with self.assertRaisesRegex(control.ControlError, "restart_failed"):
            self.fixture.restart(self.runner)
        fixture = Fixture(self.root / "timeout")
        runner = FakeRunner(fixture)
        runner.restart_never_ready = True
        with self.assertRaisesRegex(control.ControlError, "restart_timeout"):
            fixture.restart(runner)

    def test_journey_failure_runs_no_restart_or_preflight(self) -> None:
        self.runner.fail_command = (str(control.PYTHON), str(control.JOURNEY_PROGRAM))
        with self.assertRaisesRegex(control.ControlError, "journey_checkpoint_failed"):
            self.fixture.restart(self.runner)
        self.assertFalse(any(call[1] == "restart" for call in self.runner.calls))
        self.assertFalse(any(
            call[:2] == (str(control.PYTHON), str(control.PREFLIGHT_PROGRAM))
            for call in self.runner.calls
        ))

    def test_public_error_never_contains_secret_or_path(self) -> None:
        class Sink:
            def __init__(self) -> None:
                self.buffer = __import__("io").BytesIO()

        sink = Sink()
        with mock.patch.object(sys, "stderr", sink):
            result = control.main([
                "activate", "--manifest", str(self.fixture.manifest),
                "--contract", "/contract", "--profile", "/profile",
                "--agents-dir", "/agents", "--operator-credential-file", "/secret",
                "--expected-git-sha", self.fixture.git_sha,
                "--expected-manifest-sha256", self.fixture.manifest_sha,
                "--provision-receipt", str(self.fixture.provision),
                "--expected-provision-receipt-sha256", "0" * 64,
                "--output", str(self.fixture.activation),
            ], runner=self.runner)
        self.assertEqual(result, 1)
        self.assertEqual(
            sink.buffer.getvalue(), control.public_failure("provision_receipt_digest_mismatch")
        )
        self.assertNotIn(b"/work/", sink.buffer.getvalue())
        self.assertNotIn(b"secret", sink.buffer.getvalue().lower())


if __name__ == "__main__":
    unittest.main()
