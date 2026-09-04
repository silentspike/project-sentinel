#!/usr/bin/env python3
"""Contract tests for the checked-in single-node M0 journey."""

from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import unittest


HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parents[1]
PLAN_PATH = HERE / "m0-journey-v2.json"
CONTROL_PATH = HERE / "m0-restart-control-v1.json"


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


journey = load_module("m0_journey_contract_runner", HERE / "run_m0_journey.py")
activation = load_module(
    "m0_journey_contract_activation", HERE / "m0-activation" / "control.py"
)


class M0JourneyContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.plan = json.loads(PLAN_PATH.read_text(encoding="utf-8"))

    def test_plan_is_a_complete_v2_product_journey(self) -> None:
        journey.validate_plan(self.plan)
        self.assertEqual(self.plan["schema_version"], 2)
        self.assertEqual(self.plan["journey_id"], "single-node-web-company-v5")
        self.assertEqual(self.plan["provider_mode"], "token_free")
        self.assertEqual(len(self.plan["steps"]), 28)
        self.assertEqual(
            {step["phase"] for step in self.plan["steps"]}, set(journey.PHASES)
        )
        positive = [step for step in self.plan["steps"] if step["kind"] == "positive"]
        self.assertEqual(len(positive), 24)
        self.assertTrue(all(step["initial_assertions"] for step in positive))
        self.assertTrue(all(step["replay_assertions"] for step in positive))
        self.assertTrue(all(step["checkpoint"] for step in positive))

    def test_people_profiles_and_authority_are_pinned_to_release_inputs(self) -> None:
        steps = {step["id"]: step for step in self.plan["steps"]}
        binding = steps["create_proposal"]["body"]["command"]["binding"]
        governance = binding["governance"]
        participants = governance["participants"]
        self.assertEqual(governance["owner"], 9)
        self.assertEqual(len(participants), 7)
        self.assertEqual(
            {participant["role"] for participant in participants},
            {
                "sales",
                "project_manager",
                "technical_lead",
                "designer",
                "developer",
                "qa",
                "release_manager",
            },
        )
        project_profile = REPO_ROOT / "config/work-profiles/web-project-v1.toml"
        authoring_profile = REPO_ROOT / "config/workbench-profiles/web-authoring-v1.toml"
        qa_profile = REPO_ROOT / "config/workbench-profiles/web-qa-v1.toml"
        self.assertEqual(
            governance["project_profile"]["digest"],
            hashlib.sha256(project_profile.read_bytes()).hexdigest(),
        )
        by_role = {participant["role"]: participant for participant in participants}
        authoring_digest = hashlib.sha256(authoring_profile.read_bytes()).hexdigest()
        self.assertEqual(by_role["designer"]["profile"]["digest"], authoring_digest)
        self.assertEqual(by_role["developer"]["profile"]["digest"], authoring_digest)
        self.assertEqual(
            by_role["qa"]["profile"]["digest"],
            hashlib.sha256(qa_profile.read_bytes()).hexdigest(),
        )

    def test_project_manager_mutations_use_its_agent_authority(self) -> None:
        mutations = {
            "plan_work_graph",
            "create_project_room",
            "raise_project_blocker",
            "resolve_project_blocker",
            "activate_project",
            "assign_designer",
        }
        steps = {step["id"]: step for step in self.plan["steps"]}
        for step_id in mutations:
            self.assertEqual(steps[step_id]["credential_alias"], "project_manager")
            self.assertEqual(steps[step_id]["path"], "/agent/workflow/commands")
            self.assertEqual(steps[step_id]["route_role"], "agent")

        self.assertEqual(
            steps["customer_operator_boundary"]["path"],
            "/operator/workflow/commands",
        )
        self.assertEqual(
            steps["observe_design_done"]["path"],
            "/operator/workflow/work-items",
        )
        self.assertEqual(
            steps["observe_source_done"]["path"],
            "/operator/workflow/work-items",
        )
        self.assertEqual(
            steps["observe_design_done"]["observe"]["max_attempts"], 20
        )
        self.assertEqual(
            steps["observe_design_done"]["observe"]["max_elapsed_ms"], 30_000
        )
        self.assertEqual(
            steps["observe_source_done"]["observe"]["max_attempts"], 300
        )
        self.assertEqual(
            steps["observe_source_done"]["observe"]["max_elapsed_ms"], 300_000
        )

    def test_real_work_collaboration_and_delivery_intents_are_present(self) -> None:
        steps = {step["id"]: step for step in self.plan["steps"]}
        step_order = [step["id"] for step in self.plan["steps"]]
        self.assertEqual(
            steps["customer_operator_boundary"]["expected_status"], [403]
        )
        work_items = {
            item["work_item_id"]: item
            for item in steps["plan_work_graph"]["body"]["command"]["items"]
        }
        self.assertEqual(work_items["design-site"]["dependency_ids"], [])
        self.assertEqual(work_items["build-site"]["dependency_ids"], ["design-site"])
        self.assertEqual(
            work_items["build-site"]["inputs"],
            [
                {
                    "name": "design_specification",
                    "producer_work_item_id": "design-site",
                    "producer_output_name": "design_specification",
                    "expected_contract_generation": 1,
                    "expected_contract_digest": work_items["design-site"]["outputs"][0][
                        "contract_digest"
                    ],
                }
            ],
        )
        self.assertLess(
            step_order.index("observe_design_done"),
            step_order.index("assign_developer"),
        )
        self.assertLess(
            step_order.index("assign_developer"), step_order.index("execute_source")
        )
        self.assertEqual(
            steps["assign_developer"]["body"]["command"]["expected_version"], 12
        )
        self.assertEqual(
            steps["execute_source"]["body"]["intent"]["tools"][0],
            {"kind": "inspect_file", "path": "design.md", "max_bytes": 4096},
        )
        source_tools = steps["execute_source"]["body"]["intent"]["tools"]
        self.assertEqual(source_tools[1]["kind"], "write_file")
        self.assertEqual(source_tools[1]["path"], "index.html")
        self.assertIn("<title>Project Sentinel</title>", source_tools[1]["content"])
        self.assertEqual(source_tools[-1]["paths"], ["index.html", "site.js"])
        self.assertEqual(
            steps["execute_design"]["body"]["intent"]["tools"][-1]["artifact_kind"],
            "design_specification",
        )
        self.assertEqual(
            steps["execute_source"]["body"]["intent"]["tools"][-1]["artifact_kind"],
            "source_tree",
        )
        self.assertEqual(
            steps["execute_design"]["assertions"],
            [{"pointer": "/work_item/state", "equals": "claimed"}],
        )
        self.assertEqual(
            steps["execute_source"]["assertions"],
            [{"pointer": "/work_item/state", "equals": "claimed"}],
        )
        self.assertEqual(
            steps["create_source_handoff"]["body"]["command"]["artifact_digests"],
            [{"$ref": "observe_source_done.artifact_digest"}],
        )
        self.assertEqual(
            steps["create_source_handoff"]["body"]["command"]["expected_version"],
            10 + (2 * 3),
        )
        self.assertEqual(
            steps["observe_source_done"]["capture"]["artifact_digest"]["pointer"],
            "/terminal_execution_evidence/artifacts/0/digest",
        )
        self.assertEqual(
            [
                steps[step_id]["body"]["intent"]["action"]
                for step_id in (
                    "prepare_delivery_candidate",
                    "assign_independent_qa",
                    "execute_independent_qa",
                    "release_delivery",
                    "customer_acceptance",
                    "project_closeout",
                )
            ],
            ["prepare_candidate", "assign_qa", "execute_qa", "release", "accept", "closeout"],
        )

    def test_restart_control_selects_one_representative_checkpoint(self) -> None:
        contract = activation.load_journey_contract(PLAN_PATH)
        control_raw = CONTROL_PATH.read_bytes()
        mapping = activation.load_control_plan(
            CONTROL_PATH,
            hashlib.sha256(control_raw).hexdigest(),
            contract.raw_sha256,
            list(contract.checkpoints),
        )
        self.assertEqual(
            mapping,
            {"after_agreement_project": "sentinel-daemon.service"},
        )

    def test_release_and_provisioning_authorities_include_the_journey(self) -> None:
        generator = (REPO_ROOT / "deploy/generate-manifest.sh").read_text(
            encoding="utf-8"
        )
        provisioner = (REPO_ROOT / "deploy/provision-m0-single-node.sh").read_text(
            encoding="utf-8"
        )
        preflight = (HERE / "run_m0_preflight.py").read_text(encoding="utf-8")
        for relative in (
            "scripts/product-acceptance/run_m0_preflight.py",
            "scripts/product-acceptance/run_m0_journey.py",
            "scripts/product-acceptance/build_collaboration_admission_journey.py",
            "scripts/product-acceptance/evaluate_collaboration_admission.py",
            "scripts/product-acceptance/m0-activation/control.py",
            "scripts/product-acceptance/collaboration-admission-study-v1.json",
            "scripts/product-acceptance/m0-journey-v2.json",
            "scripts/product-acceptance/m0-restart-control-v1.json",
        ):
            self.assertIn(relative, generator)
            self.assertIn(relative, provisioner)
            self.assertIn(relative, preflight)


if __name__ == "__main__":
    unittest.main()
