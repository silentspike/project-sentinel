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


def preflight_check(status: str, reason: str, evidence: dict[str, object]) -> dict[str, object]:
    digest_input: object = evidence
    if status == "FAIL":
        digest_input = {"id": "runtime", "reason": reason}
    return {
        "id": "runtime", "status": status, "reason": reason,
        "evidence_digest": control.wire_digest(digest_input), "evidence": evidence,
    }


def preflight_payload(passed: bool, reason: str = "ok") -> dict[str, object]:
    check = preflight_check("PASS", "ok", {"ready": True}) if passed else preflight_check(
        "FAIL", reason, {}
    )
    value: dict[str, object] = {
        "schema_version": 1,
        "claim": "runtime_preflight_pass" if passed else "runtime_preflight_fail",
        "runtime_preflight_pass": passed,
        "m0_acceptance_pass": False,
        "checks": [check],
    }
    seal_preflight(value)
    return value


def seal_preflight(value: dict[str, object]) -> None:
    value.pop("result_digest", None)
    value["result_digest"] = control.wire_digest(value)


class FakeClock:
    def __init__(self) -> None:
        self.now = 0.0
        self.sleeps: list[float] = []

    def monotonic(self) -> float:
        return self.now

    def sleep(self, duration: float) -> None:
        self.sleeps.append(duration)
        self.now += duration


class FakeRunner:
    def __init__(self, fixture: "Fixture") -> None:
        self.fixture = fixture
        self.calls: list[tuple[str, ...]] = []
        self.timeouts: list[float] = []
        self.environments: list[dict[str, str]] = []
        self.states = {
            unit: {"LoadState": "loaded", "ActiveState": "inactive",
                   "SubState": "dead", "Result": "success"}
            for unit in control.INSPECT_UNITS
        }
        self.states[control.TARGET].pop("Result")
        self.fail_command: tuple[str, ...] | None = None
        self.partial_start_at: str | None = None
        self.rollback_failure: str | None = None
        self.readiness_failure = False
        self.restart_never_ready = False
        self.restart_never_ready_unit: str | None = None
        self.unclean_stop_unit: str | None = None
        self.nightrun_fails_after_start = False
        self.mutate_during_preflight: str | None = None
        self.journey_effects: dict[str, int] = {}
        self.after_target_start = False
        self.show_failure_unit: str | None = None
        self.target_start_returncode = 0
        self.journey_mutation: str | None = None
        self.journey_mutation_final_only = False
        self.ready_after_rounds: dict[str, int] = {}
        self.show_rounds: dict[str, int] = {}
        self.terminal_failure_unit: str | None = None
        self.terminal_failure_result = "exit-code"
        self.auth_init_fails = False
        self.active_oneshot: str | None = None
        self.oneshot_finish_after_rounds: dict[str, int] = {}
        self.oneshot_failure = False
        self.preflight_failures_remaining = 0
        self.preflight_failure_reason = "http_readiness_failed"
        self.preflight_override: tuple[int, dict[str, object]] | None = None

    def __call__(
        self, argv: tuple[str, ...], timeout: float, environment: dict[str, str]
    ) -> control.Result:
        self.calls.append(argv)
        self.timeouts.append(timeout)
        self.environments.append(dict(environment))
        if self.fail_command is not None and argv[:len(self.fail_command)] == self.fail_command:
            return control.Result(1, b"private /work/path secret")
        if argv[0] == str(control.SYSTEMCTL):
            verb = argv[1]
            if verb == "show":
                unit = argv[2]
                if self.after_target_start and unit == self.show_failure_unit:
                    return control.Result(1)
                if self.after_target_start and unit in self.ready_after_rounds:
                    self.show_rounds[unit] = self.show_rounds.get(unit, 0) + 1
                    if self.show_rounds[unit] >= self.ready_after_rounds[unit]:
                        self._ready(unit)
                if (
                    self.after_target_start
                    and unit in self.oneshot_finish_after_rounds
                    and self.states[unit]["ActiveState"] in {"active", "activating"}
                ):
                    self.show_rounds[unit] = self.show_rounds.get(unit, 0) + 1
                    if self.show_rounds[unit] >= self.oneshot_finish_after_rounds[unit]:
                        if self.oneshot_failure:
                            self.states[unit].update(
                                ActiveState="failed", SubState="failed", Result="failed"
                            )
                        else:
                            self.states[unit].update(
                                ActiveState="inactive", SubState="dead", Result="success"
                            )
                state = self.states[unit]
                properties = argv[3].removeprefix("--property=").split(",")
                data = "".join(f"{key}={state[key]}\n" for key in properties)
                return control.Result(0, data.encode("ascii"))
            if verb == "daemon-reload":
                return control.Result(0)
            if verb == "start":
                unit = argv[-1]
                self.after_target_start = True
                if unit in control.ONESHOTS:
                    if unit in self.oneshot_finish_after_rounds:
                        self.states[unit].update(
                            ActiveState="activating", SubState="start", Result="success"
                        )
                    else:
                        self.states[unit].update(
                            ActiveState="inactive", SubState="dead", Result="success"
                        )
                    if self.nightrun_fails_after_start and unit == "sentinel-nightrun.service":
                        self.states[unit].update(
                            ActiveState="failed", SubState="failed", Result="failed"
                        )
                        self.readiness_failure = True
                    return control.Result(0)
                if unit in control.SERVICES:
                    if self.restart_never_ready:
                        self.restart_never_ready_unit = unit
                        self.states[unit].update(
                            ActiveState="activating", SubState="start", Result="success"
                        )
                    else:
                        self._ready(unit)
                    return control.Result(0)
                if unit != control.TARGET:
                    raise AssertionError(f"unexpected start unit: {unit}")
                if self.auth_init_fails:
                    self.states[control.AUTH_INIT].update(
                        ActiveState="failed", SubState="failed", Result="exit-code"
                    )
                    self.states[control.TARGET].update(
                        ActiveState="failed", SubState="failed", Result="dependency"
                    )
                    return control.Result(self.target_start_returncode)
                for unit in control.ALL_UNITS:
                    if self.partial_start_at == unit:
                        break
                    if unit in self.ready_after_rounds:
                        self.states[unit].update(
                            ActiveState="activating", SubState="start", Result="success"
                        )
                    else:
                        self._ready(unit)
                if self.restart_never_ready_unit is not None:
                    self.states[self.restart_never_ready_unit].update(
                        ActiveState="activating", SubState="start", Result="success"
                    )
                if self.terminal_failure_unit is not None:
                    self.states[self.terminal_failure_unit].update(
                        ActiveState="failed", SubState="failed",
                        Result=self.terminal_failure_result,
                    )
                return control.Result(self.target_start_returncode)
            if verb == "stop":
                unit = argv[2]
                if self.rollback_failure == unit:
                    return control.Result(1)
                result = "exit-code" if self.unclean_stop_unit == unit else "success"
                self.states[unit].update(
                    ActiveState="inactive", SubState="dead", Result=result
                )
                return control.Result(0)
        if argv[:2] == (str(control.PYTHON), str(control.PREFLIGHT_PROGRAM)):
            if self.mutate_during_preflight == "ledger":
                self.fixture.ledger.write_bytes(b"{}\n")
            elif self.mutate_during_preflight == "evidence":
                self.fixture.evidence.write_bytes(b"{}\n")
            if self.preflight_override is not None:
                returncode, value = self.preflight_override
                return control.Result(returncode, encoded(value))
            if self.readiness_failure:
                return control.Result(1, encoded({"runtime_preflight_pass": False}))
            if self.preflight_failures_remaining > 0:
                self.preflight_failures_remaining -= 1
                return control.Result(
                    1, encoded(preflight_payload(False, self.preflight_failure_reason))
                )
            return control.Result(0, encoded(preflight_payload(True)))
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
        elif unit == control.AUTH_INIT:
            sub = "exited"
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
        processed_count = 0
        for index, step in enumerate(self.fixture.plan["steps"]):
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
            processed_count = index + 1
            if checkpoint is not None and step.get("checkpoint") == checkpoint:
                break
        self.fixture.ledger.write_bytes(encoded(ledger))
        evidence_ledger = (
            module.evidence_ledger_prefix(
                self.fixture.plan, ledger, processed_count
            )
            if checkpoint is not None
            else ledger
        )
        evidence = module.build_evidence(
            self.fixture.plan, evidence_ledger,
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
        artifact_digests = [f"{index:064x}" for index in range(111)]
        self.manifest_value = {
            "version": "1.0", "created_at": "2026-08-13T00:00:00Z",
            "git_sha": self.git_sha,
            "artifacts": [
                {
                    "path": f"/artifact/{index}", "source": f"artifact/{index}",
                    "sha256": artifact_digests[index], "type": "config",
                }
                for index in range(111)
            ],
        }
        self.manifest.write_bytes(encoded(self.manifest_value))
        self.manifest_sha = control.digest_bytes(self.manifest.read_bytes())
        unsigned = {
            "schema_version": 1, "status": "COMPLETE", "git_sha": self.git_sha,
            "manifest_sha256": self.manifest_sha, "artifact_count": 111,
            "changed_count": 111,
            "artifact_set_digest": control.digest(sorted(artifact_digests)),
            "legacy_migration": {
                "status": "COMPLETE", "directory_count": 7, "file_count": 17,
            },
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

    def rewrite_provision(self, artifact_count: int, changed_count: int | None = None) -> None:
        self.manifest_value["artifacts"] = self.manifest_value["artifacts"][:artifact_count]
        self.manifest.write_bytes(encoded(self.manifest_value))
        self.manifest_sha = control.digest_bytes(self.manifest.read_bytes())
        digests = [item["sha256"] for item in self.manifest_value["artifacts"]]
        receipt = {
            "schema_version": 1, "status": "COMPLETE", "git_sha": self.git_sha,
            "manifest_sha256": self.manifest_sha, "artifact_count": artifact_count,
            "changed_count": artifact_count if changed_count is None else changed_count,
            "artifact_set_digest": control.digest(sorted(digests)),
            "legacy_migration": {
                "status": "COMPLETE", "directory_count": 7, "file_count": 17,
            },
            "services_started": False,
        }
        self.provision.write_bytes(encoded(receipt))
        self.provision_sha = control.digest_bytes(self.provision.read_bytes())
        self.manifest.chmod(0o600)
        self.provision.chmod(0o600)

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

    def activate(self, runner: FakeRunner, **kwargs: object) -> dict[str, object]:
        return control.activate(
            runner, self.provision, self.provision_sha, self.manifest,
            self.manifest_sha, self.git_sha, self.preflight(), self.activation, 0.15,
            **kwargs,
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

    def test_activation_success_starts_target_and_current_oneshots(self) -> None:
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
        self.assertEqual(
            mutations[1],
            (str(control.SYSTEMCTL), "start", "--no-block", control.TARGET),
        )
        self.assertEqual(
            mutations[2:4],
            [
                (str(control.SYSTEMCTL), "start", "--no-block", unit)
                for unit in control.ONESHOTS
            ],
        )
        self.assertEqual(len(mutations), 4)
        self.assertFalse(result["m0_acceptance_pass"])

    def test_rejected_oneshot_start_rolls_back_before_preflight(self) -> None:
        unit = control.ONESHOTS[0]
        self.runner.fail_command = (
            str(control.SYSTEMCTL), "start", "--no-block", unit,
        )
        with self.assertRaisesRegex(control.ControlError, "oneshot_start_failed"):
            self.fixture.activate(self.runner)
        self.assertFalse(any(
            call[:2] == (str(control.PYTHON), str(control.PREFLIGHT_PROGRAM))
            for call in self.runner.calls
        ))
        self.assertEqual(
            [call[2] for call in self.runner.calls if call[1] == "stop"],
            list(control.ROLLBACK_ORDER),
        )

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
        requires = next(
            line for line in target.splitlines() if line.startswith("Requires=")
        )
        self.assertEqual(tuple(wants.removeprefix("Wants=").split()), control.TOPOLOGY)
        self.assertEqual(requires, f"Requires={control.AUTH_INIT}")

    def test_auth_init_failure_prevents_credential_consumers_and_rolls_back(self) -> None:
        self.runner.auth_init_fails = True
        with self.assertRaisesRegex(control.ControlError, "activation_unit_failed"):
            self.fixture.activate(self.runner)
        consumers = {
            "sentinel-daemon.service",
            "sentinel-gateway.service",
            "sentinel-dashboard-backend.service",
        }
        self.assertTrue(
            all(
                self.runner.states[unit]["ActiveState"] == "inactive"
                for unit in consumers
            )
        )
        self.assertFalse(
            any(
                call[:2] == (str(control.PYTHON), str(control.PREFLIGHT_PROGRAM))
                for call in self.runner.calls
            )
        )
        self.assertEqual(
            json.loads(self.fixture.activation.read_text())["status"], "ROLLED_BACK"
        )

    def test_forged_stale_receipt_and_wrong_authority_fail_before_command(self) -> None:
        cases = (
            ("receipt_digest", "0" * 64, self.fixture.manifest_sha, self.fixture.git_sha,
             "provision_receipt_digest_mismatch"),
            ("manifest_digest", self.fixture.provision_sha, "0" * 64, self.fixture.git_sha,
             "manifest_digest_mismatch"),
            ("git", self.fixture.provision_sha, self.fixture.manifest_sha, "c" * 40,
             "manifest_authority_mismatch"),
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

    def test_provision_authority_uses_dynamic_manifest_count_and_digest(self) -> None:
        fixture = Fixture(self.root / "dynamic-provision")
        fixture.rewrite_provision(7, changed_count=3)
        result = control.validate_provision_authority(
            fixture.provision, fixture.provision_sha, fixture.manifest,
            fixture.manifest_sha, fixture.git_sha,
        )
        self.assertEqual(result["artifact_count"], 7)
        self.assertEqual(result["changed_count"], 3)
        self.assertEqual(result["legacy_migration"]["status"], "COMPLETE")

        for field, value in (
            ("artifact_count", 8),
            ("changed_count", 8),
            ("artifact_set_digest", "0" * 64),
        ):
            with self.subTest(field=field):
                bad = Fixture(self.root / f"bad-provision-{field}")
                bad.rewrite_provision(7, changed_count=3)
                receipt = json.loads(bad.provision.read_text())
                receipt[field] = value
                bad.provision.write_bytes(encoded(receipt))
                bad.provision_sha = control.digest_bytes(bad.provision.read_bytes())
                with self.assertRaisesRegex(
                    control.ControlError, "provision_receipt_authority_mismatch"
                ):
                    control.validate_provision_authority(
                        bad.provision, bad.provision_sha, bad.manifest,
                        bad.manifest_sha, bad.git_sha,
                    )

        for field, value in (
            ("status", "ROLLED_BACK"),
            ("directory_count", -1),
            ("file_count", True),
        ):
            with self.subTest(legacy_migration=field):
                bad = Fixture(self.root / f"bad-provision-legacy-{field}")
                bad.rewrite_provision(7, changed_count=3)
                receipt = json.loads(bad.provision.read_text())
                receipt["legacy_migration"][field] = value
                bad.provision.write_bytes(encoded(receipt))
                bad.provision_sha = control.digest_bytes(bad.provision.read_bytes())
                with self.assertRaisesRegex(
                    control.ControlError, "provision_receipt_authority_mismatch"
                ):
                    control.validate_provision_authority(
                        bad.provision, bad.provision_sha, bad.manifest,
                        bad.manifest_sha, bad.git_sha,
                    )

        missing = Fixture(self.root / "bad-provision-legacy-missing")
        receipt = json.loads(missing.provision.read_text())
        del receipt["legacy_migration"]
        missing.provision.write_bytes(encoded(receipt))
        missing.provision_sha = control.digest_bytes(missing.provision.read_bytes())
        with self.assertRaisesRegex(control.ControlError, "provision_receipt_shape"):
            control.validate_provision_authority(
                missing.provision, missing.provision_sha, missing.manifest,
                missing.manifest_sha, missing.git_sha,
            )

    def test_running_or_failed_unit_stops_before_daemon_reload(self) -> None:
        for field, value in (("ActiveState", "active"), ("Result", "exit-code")):
            with self.subTest(field=field):
                self.fixture.activation.unlink(missing_ok=True)
                runner = FakeRunner(self.fixture)
                runner.states[control.SERVICES[1]][field] = value
                with self.assertRaisesRegex(control.ControlError, "unit_not_stopped_cleanly"):
                    self.fixture.activate(runner)
                self.assertFalse(any(call[1] == "daemon-reload" for call in runner.calls))

    def test_target_readback_does_not_require_unsupported_result_property(self) -> None:
        values = control.systemctl_show(self.runner, control.TARGET, 5.0)
        self.assertEqual(
            values,
            {"LoadState": "loaded", "ActiveState": "inactive", "SubState": "dead"},
        )
        self.assertFalse(control.unit_terminal_failure(control.TARGET, values))
        self.assertEqual(
            self.runner.calls[-1][3],
            "--property=LoadState,ActiveState,SubState",
        )

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
        with self.assertRaisesRegex(control.ControlError, "activation_oneshot_failed"):
            self.fixture.activate(self.runner)
        verbs = [call[1] for call in self.runner.calls]
        self.assertNotIn("reset-failed", verbs)
        self.assertEqual(json.loads(self.fixture.activation.read_text())["status"], "ROLLED_BACK")
        self.assertTrue(all(
            self.runner.states[unit]["ActiveState"] == "inactive"
            for unit in control.ALL_UNITS
        ))

    def test_activation_waits_for_gradual_units_without_busy_loop(self) -> None:
        clock = FakeClock()
        for index, unit in enumerate(control.ALL_UNITS):
            self.runner.ready_after_rounds[unit] = 1 + index % 3
        result = self.fixture.activate(
            self.runner, activation_deadline=10.0,
            monotonic=clock.monotonic, sleeper=clock.sleep,
        )
        self.assertEqual(result["status"], "ACTIVE")
        self.assertEqual(clock.sleeps, [1.0, 1.0])
        self.assertTrue(all(
            self.runner.states[unit]["ActiveState"] == "active"
            for unit in control.ALL_UNITS
        ))

    def test_activation_allows_long_temporal_preflight_recovery(self) -> None:
        clock = FakeClock()
        self.runner.preflight_failures_remaining = 149
        result = self.fixture.activate(
            self.runner, activation_deadline=300.0,
            monotonic=clock.monotonic, sleeper=clock.sleep,
        )
        self.assertEqual(result["status"], "ACTIVE")
        self.assertEqual(clock.now, 149.0)
        self.assertGreater(clock.now, control.MAX_TIMEOUT_SECONDS)

    def test_activation_retries_transient_systemd_exec_identity_race(self) -> None:
        clock = FakeClock()
        self.runner.preflight_failures_remaining = 1
        self.runner.preflight_failure_reason = "running_executable_identity_mismatch"

        result = self.fixture.activate(
            self.runner, activation_deadline=10.0,
            monotonic=clock.monotonic, sleeper=clock.sleep,
        )

        self.assertEqual(result["status"], "ACTIVE")
        self.assertEqual(clock.sleeps, [1.0])

    def test_activation_deadline_rechecks_slow_readback_and_preflight(self) -> None:
        readback_clock = FakeClock()
        readback_runner = FakeRunner(self.fixture)
        readback_delayed = False

        def slow_readback(
            argv: tuple[str, ...], timeout: float, environment: dict[str, str]
        ) -> control.Result:
            nonlocal readback_delayed
            result = readback_runner(argv, timeout, environment)
            if (
                readback_runner.after_target_start
                and argv[1] == "show"
                and not readback_delayed
            ):
                readback_delayed = True
                readback_clock.sleep(6.0)
            return result

        with self.assertRaisesRegex(control.ControlError, "activation_timeout"):
            self.fixture.activate(
                slow_readback, activation_deadline=5.0,
                monotonic=readback_clock.monotonic, sleeper=readback_clock.sleep,
            )
        self.assertEqual(readback_clock.now, 6.0)

        fixture = Fixture(self.root / "slow-preflight")
        preflight_clock = FakeClock()
        preflight_runner = FakeRunner(fixture)
        preflight_timeouts: list[float] = []
        delayed_oneshot = False

        def slow_preflight(
            argv: tuple[str, ...], timeout: float, environment: dict[str, str]
        ) -> control.Result:
            nonlocal delayed_oneshot
            result = preflight_runner(argv, timeout, environment)
            if (
                preflight_runner.after_target_start
                and argv[:3] == (
                    str(control.SYSTEMCTL), "show", "sentinel-nightrun.service"
                )
                and not delayed_oneshot
            ):
                delayed_oneshot = True
                preflight_clock.sleep(4.9)
            elif argv[:2] == (str(control.PYTHON), str(control.PREFLIGHT_PROGRAM)):
                preflight_timeouts.append(timeout)
                preflight_clock.sleep(0.2)
            return result

        with self.assertRaisesRegex(control.ControlError, "activation_timeout"):
            fixture.activate(
                slow_preflight, activation_deadline=5.0,
                monotonic=preflight_clock.monotonic, sleeper=preflight_clock.sleep,
            )
        self.assertEqual(len(preflight_timeouts), 1)
        self.assertLessEqual(preflight_timeouts[0], 0.11)
        self.assertGreater(preflight_clock.now, 5.0)

    def test_preflight_wire_contract_rejects_semantic_and_digest_forgery(self) -> None:
        cases: list[tuple[str, dict[str, object]]] = []
        pass_with_fail = preflight_payload(True)
        pass_with_fail["checks"].append(preflight_check("FAIL", "http_timeout", {}))
        seal_preflight(pass_with_fail)
        cases.append(("pass_with_fail", pass_with_fail))
        pass_with_fatal = preflight_payload(True)
        pass_with_fatal["fatal_reason"] = "http_timeout"
        seal_preflight(pass_with_fatal)
        cases.append(("pass_with_fatal", pass_with_fatal))
        non_bool = preflight_payload(True)
        non_bool["runtime_preflight_pass"] = 1
        seal_preflight(non_bool)
        cases.append(("non_bool", non_bool))
        wrong_digest = preflight_payload(True)
        wrong_digest["result_digest"] = "0" * 64
        cases.append(("wrong_digest", wrong_digest))
        for name, value in cases:
            with self.subTest(name=name):
                runner = FakeRunner(self.fixture)
                runner.preflight_override = (0, value)
                with self.assertRaisesRegex(control.ControlError, "readiness_failed"):
                    control.run_preflight_attempt(runner, self.fixture.preflight())

    def test_preflight_probe_budget_is_separate_from_process_deadline(self) -> None:
        original = self.fixture.preflight()
        preflight = control.PreflightArgs(
            original.manifest, original.contract, original.profile,
            original.agents_dir, original.operator_credential_file,
            original.expected_git_sha, original.expected_manifest_sha256,
            control.MAX_TIMEOUT_SECONDS,
        )
        runner = FakeRunner(self.fixture)

        digest, retryable = control.run_preflight_attempt(runner, preflight)

        self.assertRegex(digest or "", r"^[0-9a-f]{64}$")
        self.assertFalse(retryable)
        call = runner.calls[-1]
        self.assertEqual(
            float(call[call.index("--timeout-seconds") + 1]),
            control.MAX_PREFLIGHT_PROBE_TIMEOUT_SECONDS,
        )
        self.assertEqual(runner.timeouts[-1], control.MAX_TIMEOUT_SECONDS)

    def test_no_block_target_job_can_complete_after_command_timeout(self) -> None:
        clock = FakeClock()
        self.runner.ready_after_rounds[control.TARGET] = 149
        result = self.fixture.activate(
            self.runner, activation_deadline=300.0,
            monotonic=clock.monotonic, sleeper=clock.sleep,
        )
        self.assertEqual(result["status"], "ACTIVE")
        self.assertGreater(clock.now, control.MAX_TIMEOUT_SECONDS)
        start_index = self.runner.calls.index(
            (str(control.SYSTEMCTL), "start", "--no-block", control.TARGET)
        )
        self.assertEqual(self.runner.timeouts[start_index], 0.15)
        with self.assertRaisesRegex(control.ControlError, "command_not_allowed"):
            control.validate_command((str(control.SYSTEMCTL), "start", control.TARGET))
        with self.assertRaisesRegex(control.ControlError, "command_not_allowed"):
            control.validate_command(
                (str(control.SYSTEMCTL), "restart", control.SERVICES[0])
            )

    def test_activation_never_ready_timer_stops_at_monotonic_deadline(self) -> None:
        clock = FakeClock()
        timer = "sentinel-nightrun.timer"
        self.runner.ready_after_rounds[timer] = 10_000
        with self.assertRaisesRegex(control.ControlError, "activation_timeout"):
            self.fixture.activate(
                self.runner, activation_deadline=5.0,
                monotonic=clock.monotonic, sleeper=clock.sleep,
            )
        self.assertEqual(clock.now, 5.0)
        self.assertEqual(
            [call[2] for call in self.runner.calls if call[1] == "stop"],
            list(control.ROLLBACK_ORDER),
        )

    def test_activation_terminal_unit_failure_is_immediate(self) -> None:
        clock = FakeClock()
        self.runner.terminal_failure_unit = "nats-server.service"
        with self.assertRaisesRegex(control.ControlError, "activation_unit_failed"):
            self.fixture.activate(
                self.runner, activation_deadline=300.0,
                monotonic=clock.monotonic, sleeper=clock.sleep,
            )
        self.assertEqual(clock.sleeps, [])
        receipt = json.loads(self.fixture.activation.read_text())
        self.assertEqual(receipt["reason"], "activation_unit_failed")

    def test_post_start_failure_rolls_back_running_oneshots(self) -> None:
        self.runner.active_oneshot = "sentinel-nightrun.service"
        self.runner.terminal_failure_unit = "nats-server.service"
        with self.assertRaisesRegex(control.ControlError, "activation_unit_failed"):
            self.fixture.activate(self.runner)
        stops = [call[2] for call in self.runner.calls if call[1] == "stop"]
        self.assertEqual(stops, list(control.ROLLBACK_ORDER))
        self.assertEqual(stops[:3], [control.TARGET, *control.ONESHOTS])
        self.assertLess(
            stops.index("sentinel-daemon.service"),
            stops.index("sentinel-gateway.service"),
        )
        self.assertLess(
            stops.index("sentinel-gateway.service"),
            stops.index("nats-server.service"),
        )
        self.assertEqual(
            self.runner.states["sentinel-nightrun.service"]["ActiveState"],
            "inactive",
        )

    def test_rollback_uses_independent_systemd_stop_budget(self) -> None:
        self.runner.terminal_failure_unit = "nats-server.service"
        with self.assertRaisesRegex(control.ControlError, "activation_unit_failed"):
            self.fixture.activate(self.runner)

        first_stop = next(
            index for index, call in enumerate(self.runner.calls)
            if call[1] == "stop"
        )
        self.assertTrue(all(
            timeout == control.ROLLBACK_COMMAND_TIMEOUT_SECONDS
            for call, timeout in zip(
                self.runner.calls[first_stop:], self.runner.timeouts[first_stop:]
            )
            if call[1] in {"stop", "show"}
        ))
        self.assertGreater(
            control.ROLLBACK_COMMAND_TIMEOUT_SECONDS,
            self.fixture.preflight().timeout,
        )
        self.assertGreater(
            control.ROLLBACK_COMMAND_TIMEOUT_SECONDS,
            240.0,
        )

    def test_running_oneshot_that_later_fails_never_reaches_preflight(self) -> None:
        clock = FakeClock()
        unit = "sentinel-nightrun.service"
        self.runner.active_oneshot = unit
        self.runner.oneshot_finish_after_rounds[unit] = 2
        self.runner.oneshot_failure = True
        with self.assertRaisesRegex(control.ControlError, "activation_oneshot_failed"):
            self.fixture.activate(
                self.runner, activation_deadline=10.0,
                monotonic=clock.monotonic, sleeper=clock.sleep,
            )
        self.assertEqual(clock.sleeps, [1.0])
        self.assertFalse(any(
            call[:2] == (str(control.PYTHON), str(control.PREFLIGHT_PROGRAM))
            for call in self.runner.calls
        ))
        self.assertEqual(json.loads(self.fixture.activation.read_text())["status"], "ROLLED_BACK")

    def test_active_receipt_waits_for_running_oneshot_success(self) -> None:
        clock = FakeClock()
        unit = "sentinel-health-monitor.service"
        self.runner.active_oneshot = unit
        self.runner.oneshot_finish_after_rounds[unit] = 2
        result = self.fixture.activate(
            self.runner, activation_deadline=10.0,
            monotonic=clock.monotonic, sleeper=clock.sleep,
        )
        self.assertEqual(result["status"], "ACTIVE")
        self.assertEqual(clock.sleeps, [1.0])
        self.assertEqual(self.runner.states[unit]["ActiveState"], "inactive")
        preflight_index = next(
            index for index, call in enumerate(self.runner.calls)
            if call[:2] == (str(control.PYTHON), str(control.PREFLIGHT_PROGRAM))
        )
        completion_index = max(
            index for index, call in enumerate(self.runner.calls[:preflight_index])
            if call[:3] == (str(control.SYSTEMCTL), "show", unit)
        )
        self.assertLess(completion_index, preflight_index)

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
        self.runner.fail_command = (
            str(control.SYSTEMCTL), "start", "--no-block", control.TARGET,
        )
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

    def test_rollback_stops_nats_consumers_before_nats(self) -> None:
        daemon_index = control.ROLLBACK_ORDER.index("sentinel-daemon.service")
        nats_index = control.ROLLBACK_ORDER.index("nats-server.service")
        self.assertLess(daemon_index, nats_index)
        for consumer in (
            "sentinel-daemon.service",
            "sentinel-judge.service",
            "sentinel-nats-bridge.service",
        ):
            self.assertLess(control.ROLLBACK_ORDER.index(consumer), nats_index)

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
                clock = FakeClock()
                with self.assertRaises(control.ControlError):
                    self.fixture.activate(
                        runner, activation_deadline=0.2,
                        monotonic=clock.monotonic, sleeper=clock.sleep,
                    )
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
        self.assertEqual(
            receipt["rollback_failures"],
            [{"unit": control.SERVICES[-1], "reason": "stop_rejected"}],
        )

    def test_restart_controller_replays_same_ledger_and_ids(self) -> None:
        result = self.fixture.restart(self.runner)
        self.assertEqual(result["status"], "COMPLETE")
        self.assertTrue(result["authoritative_replay_verified"])
        expected_units = list(self.fixture.control_value["checkpoint_services"].values())
        stopped = [call[2] for call in self.runner.calls if call[1] == "stop"]
        started = [
            call[2] for call in self.runner.calls
            if call[1] == "start" and len(call) == 3
        ]
        topology_starts = [
            call for call in self.runner.calls
            if call == (str(control.SYSTEMCTL), "start", "--no-block", control.TARGET)
        ]
        self.assertEqual(stopped, expected_units)
        self.assertEqual(started, expected_units)
        self.assertEqual(len(topology_starts), len(expected_units))
        self.assertEqual(
            self.runner.journey_effects,
            {step["id"]: 1 for step in self.fixture.plan["steps"]},
        )

    def test_restart_controller_resumes_an_existing_checkpoint_prefix(self) -> None:
        checkpoints = [
            step["checkpoint"]
            for step in self.fixture.plan["steps"]
            if step.get("checkpoint") is not None
        ]
        self.runner._journey(checkpoints[2])
        evidence = json.loads(self.fixture.evidence.read_text())
        self.assertEqual(evidence["replay_verified_steps"], [])

        result = self.fixture.restart(self.runner)

        self.assertEqual(result["status"], "COMPLETE")
        self.assertTrue(result["authoritative_replay_verified"])
        self.assertEqual(
            self.runner.journey_effects,
            {step["id"]: 1 for step in self.fixture.plan["steps"]},
        )
        stopped = [call[2] for call in self.runner.calls if call[1] == "stop"]
        self.assertEqual(
            stopped,
            list(self.fixture.control_value["checkpoint_services"].values()),
        )

    def test_schema_v2_alias_credentials_are_forwarded_without_secret_disclosure(self) -> None:
        reference = "customer_primary:customer=M0_TEST_CUSTOMER_CREDENTIAL"
        argv = (
            str(control.PYTHON),
            str(control.JOURNEY_PROGRAM),
            "--credential",
            reference,
        )
        with mock.patch.dict(
            os.environ,
            {"M0_TEST_CUSTOMER_CREDENTIAL": "c" * 32},
            clear=False,
        ):
            environment = control.child_environment(argv)
        self.assertEqual(environment["M0_TEST_CUSTOMER_CREDENTIAL"], "c" * 32)
        self.assertNotIn("customer_primary", environment)
        self.assertEqual(
            control.journey_command(
                control.JourneyArgs(
                    self.fixture.plan_path,
                    "http://127.0.0.1:8084",
                    (reference,),
                    self.fixture.ledger,
                    self.fixture.evidence,
                    5.0,
                ),
                "after_customer_request",
            )[-3:],
            (reference, "--stop-after-checkpoint", "after_customer_request"),
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
                self.assertFalse(any(
                    call[1] in {"stop", "start", "restart"} for call in runner.calls
                ))

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

    def test_successful_process_does_not_signal_its_reaped_process_group(self) -> None:
        helper = self.root / "success.py"
        helper.write_text("print('{}')\n", encoding="ascii")
        helper.chmod(0o600)
        with (
            mock.patch.object(control, "PREFLIGHT_PROGRAM", helper),
            mock.patch.object(control.os, "killpg") as killpg,
        ):
            argv = (str(control.PYTHON), str(helper))
            result = control.production_runner(argv, 1.0, control.child_environment(argv))
        self.assertEqual(result, control.Result(0, b"{}\n"))
        killpg.assert_not_called()

    def test_abnormal_process_kills_its_grandchild_before_return(self) -> None:
        for mode, reason in (
            ("timeout", "command_timeout"),
            ("overflow", "command_output_oversized"),
        ):
            with self.subTest(mode=mode):
                pid_path = self.root / f"{mode}-grandchild.pid"
                marker = self.root / f"{mode}-grandchild-survived"
                helper = self.root / f"{mode}-process-tree.py"
                child = (
                    "import os,time,pathlib;"
                    f"pathlib.Path({str(pid_path)!r}).write_text(str(os.getpid()));"
                    "time.sleep(1.0);"
                    f"pathlib.Path({str(marker)!r}).write_text('survived')"
                )
                trigger = (
                    "time.sleep(10)"
                    if mode == "timeout"
                    else "os.write(1, b'x' * (4 * 1024 * 1024 + 1))"
                )
                helper.write_text(
                    "import os,pathlib,subprocess,sys,time\n"
                    f"subprocess.Popen([sys.executable, '-c', {child!r}])\n"
                    f"pid_path = pathlib.Path({str(pid_path)!r})\n"
                    "deadline = time.monotonic() + 2.0\n"
                    "while not pid_path.exists() and time.monotonic() < deadline:\n"
                    "    time.sleep(0.01)\n"
                    "if not pid_path.exists():\n"
                    "    raise RuntimeError('child did not start')\n"
                    f"{trigger}\n",
                    encoding="ascii",
                )
                helper.chmod(0o600)
                with mock.patch.object(control, "PREFLIGHT_PROGRAM", helper):
                    argv = (str(control.PYTHON), str(helper))
                    with self.assertRaisesRegex(control.ControlError, reason):
                        control.production_runner(
                            argv, 0.5 if mode == "timeout" else 2.0,
                            control.child_environment(argv),
                        )
                self.assertTrue(pid_path.exists())
                grandchild_pid = int(pid_path.read_text())
                with self.assertRaises(ProcessLookupError):
                    os.kill(grandchild_pid, 0)
                __import__("time").sleep(1.05)
                self.assertFalse(marker.exists())

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
        self.runner.fail_command = (str(control.SYSTEMCTL), "stop")
        with self.assertRaisesRegex(control.ControlError, "restart_stop_failed"):
            self.fixture.restart(self.runner)
        fixture = Fixture(self.root / "timeout")
        runner = FakeRunner(fixture)
        runner.restart_never_ready = True
        with self.assertRaisesRegex(control.ControlError, "restart_timeout"):
            fixture.restart(runner)

    def test_restart_rejects_unclean_old_invocation_before_start(self) -> None:
        unit = next(iter(self.fixture.control_value["checkpoint_services"].values()))
        self.runner.unclean_stop_unit = unit
        with self.assertRaisesRegex(control.ControlError, "restart_stop_unclean"):
            self.fixture.restart(self.runner)
        self.assertFalse(any(
            call[:3] == (str(control.SYSTEMCTL), "start", unit)
            for call in self.runner.calls
        ))

    def test_restart_uses_independent_systemd_and_journey_process_budgets(self) -> None:
        self.fixture.restart(self.runner)

        restart_timeouts = [
            timeout
            for call, timeout in zip(self.runner.calls, self.runner.timeouts)
            if call[:2] in {
                (str(control.SYSTEMCTL), "stop"),
                (str(control.SYSTEMCTL), "start"),
            }
        ]
        journey_timeouts = [
            timeout
            for call, timeout in zip(self.runner.calls, self.runner.timeouts)
            if call[:2] == (str(control.PYTHON), str(control.JOURNEY_PROGRAM))
        ]
        self.assertTrue(restart_timeouts)
        self.assertTrue(journey_timeouts)
        self.assertTrue(all(
            timeout == control.RESTART_COMMAND_TIMEOUT_SECONDS
            for timeout in restart_timeouts
        ))
        expected_journey_timeouts = [
            control.journey_command_timeout(
                self.fixture.journey_contract,
                self.fixture.journey_args(),
                checkpoint,
            )
            for checkpoint in self.fixture.journey_contract.checkpoints
        ]
        expected_journey_timeouts.extend([
            control.journey_command_timeout(
                self.fixture.journey_contract, self.fixture.journey_args(), None
            ),
            control.journey_command_timeout(
                self.fixture.journey_contract, self.fixture.journey_args(), None
            ),
        ])
        self.assertEqual(journey_timeouts, expected_journey_timeouts)
        self.assertGreater(control.RESTART_COMMAND_TIMEOUT_SECONDS, 260.0)
        self.assertGreater(
            control.RESTART_COMMAND_TIMEOUT_SECONDS,
            self.fixture.journey_args().timeout,
        )
        self.assertTrue(all(
            timeout > self.fixture.journey_args().timeout
            for timeout in journey_timeouts
        ))
        self.assertTrue(all(
            timeout <= control.MAX_JOURNEY_COMMAND_TIMEOUT_SECONDS
            for timeout in journey_timeouts
        ))

    def test_journey_process_budget_covers_replayed_prefix_and_observe_bounds(self) -> None:
        journey = self.fixture.journey_args()
        contract = self.fixture.journey_contract
        first_checkpoint = contract.checkpoints[0]
        first_index = next(
            index
            for index, step in enumerate(contract.plan["steps"])
            if step.get("checkpoint") == first_checkpoint
        )
        expected_prefix = sum(
            step.get("observe", {}).get("max_elapsed_ms", journey.timeout * 1_000)
            / 1_000
            for step in contract.plan["steps"][:first_index + 1]
        ) + control.JOURNEY_COMMAND_GRACE_SECONDS
        self.assertEqual(
            control.journey_command_timeout(contract, journey, first_checkpoint),
            expected_prefix,
        )
        self.assertGreater(
            control.journey_command_timeout(contract, journey, None),
            expected_prefix,
        )
        oversized = copy.deepcopy(contract.plan)
        oversized["steps"] = oversized["steps"] * 10_000
        oversized_contract = control.JourneyContract(
            contract.raw_sha256,
            contract.module,
            oversized,
            contract.checkpoints,
            contract.step_ids,
        )
        with self.assertRaisesRegex(
            control.ControlError, "journey_command_budget_exceeded"
        ):
            control.journey_command_timeout(oversized_contract, journey, None)

    def test_canonical_m0_plan_fits_the_bounded_journey_process_budget(self) -> None:
        plan_path = HERE.parent / "m0-journey-v2.json"
        raw = plan_path.read_bytes()
        plan = json.loads(raw)
        contract = control.JourneyContract(
            control.digest_bytes(raw),
            self.fixture.journey_contract.module,
            plan,
            tuple(
                step["checkpoint"]
                for step in plan["steps"]
                if step.get("checkpoint") is not None
            ),
            tuple(step["id"] for step in plan["steps"]),
        )
        journey = control.JourneyArgs(
            plan_path,
            "http://127.0.0.1:8084",
            (),
            self.fixture.ledger,
            self.fixture.evidence,
            control.MAX_TIMEOUT_SECONDS,
        )
        self.assertEqual(control.journey_command_timeout(contract, journey, None), 1140.0)
        self.assertLess(
            control.journey_command_timeout(contract, journey, None),
            control.MAX_JOURNEY_COMMAND_TIMEOUT_SECONDS,
        )

    def test_restart_readiness_retries_only_temporal_failures(self) -> None:
        clock = FakeClock()
        self.runner.preflight_failures_remaining = 2
        digest = control.wait_for_restart_readiness(
            self.runner, self.fixture.preflight(), deadline_seconds=10.0,
            monotonic=clock.monotonic, sleeper=clock.sleep,
        )
        self.assertRegex(digest, r"^[0-9a-f]{64}$")
        self.assertEqual(clock.sleeps, [1.0, 1.0])

        fatal_runner = FakeRunner(self.fixture)
        fatal_runner.preflight_failures_remaining = 1
        fatal_runner.preflight_failure_reason = "artifact_hash_mismatch"
        with self.assertRaisesRegex(control.ControlError, "readiness_failed"):
            control.wait_for_restart_readiness(
                fatal_runner, self.fixture.preflight(), deadline_seconds=10.0,
                monotonic=clock.monotonic, sleeper=clock.sleep,
            )

    def test_restart_readiness_temporal_failure_is_deadline_bounded(self) -> None:
        clock = FakeClock()
        self.runner.preflight_failures_remaining = 10
        with self.assertRaisesRegex(
            control.ControlError, "restart_readiness_timeout"
        ):
            control.wait_for_restart_readiness(
                self.runner, self.fixture.preflight(), deadline_seconds=2.0,
                monotonic=clock.monotonic, sleeper=clock.sleep,
            )
        self.assertEqual(clock.sleeps, [1.0, 1.0])

    def test_wait_service_binds_each_readback_and_terminal_result(self) -> None:
        unit = control.SERVICES[0]
        slow_clock = FakeClock()
        slow_runner = FakeRunner(self.fixture)
        slow_runner.states[unit].update(
            ActiveState="activating", SubState="start", Result="success"
        )

        def delayed(
            argv: tuple[str, ...], timeout: float, environment: dict[str, str]
        ) -> control.Result:
            result = slow_runner(argv, timeout, environment)
            slow_clock.sleep(6.0)
            return result

        with self.assertRaisesRegex(control.ControlError, "restart_timeout"):
            control.wait_service(
                delayed, unit, 5.0,
                monotonic=slow_clock.monotonic, sleeper=slow_clock.sleep,
            )
        self.assertEqual(slow_runner.timeouts[-1], 5.0)

        terminal_runner = FakeRunner(self.fixture)
        terminal_runner.states[unit].update(
            ActiveState="inactive", SubState="dead", Result="signal"
        )
        terminal_clock = FakeClock()
        with self.assertRaisesRegex(control.ControlError, "restart_unit_failed"):
            control.wait_service(
                terminal_runner, unit, 5.0,
                monotonic=terminal_clock.monotonic, sleeper=terminal_clock.sleep,
            )
        self.assertEqual(terminal_clock.sleeps, [])

    def test_journey_failure_runs_no_restart_or_preflight(self) -> None:
        self.runner.fail_command = (str(control.PYTHON), str(control.JOURNEY_PROGRAM))
        with self.assertRaisesRegex(control.ControlError, "journey_checkpoint_failed"):
            self.fixture.restart(self.runner)
        self.assertFalse(any(
            call[1] in {"stop", "start", "restart"} for call in self.runner.calls
        ))
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
