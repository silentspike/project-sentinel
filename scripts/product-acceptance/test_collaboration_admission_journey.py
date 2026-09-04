#!/usr/bin/env python3
"""Contract tests for the #740 token-free collaboration admission journey."""

from __future__ import annotations

import copy
import importlib.util
import json
from pathlib import Path
import sys
import tomllib
import unittest


HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parents[1]
BASE_PLAN_PATH = HERE / "m0-journey-v2.json"


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


journey = load_module("run_m0_journey", HERE / "run_m0_journey.py")
builder = load_module(
    "build_collaboration_admission_journey",
    HERE / "build_collaboration_admission_journey.py",
)


class CollaborationAdmissionJourneyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.base = json.loads(BASE_PLAN_PATH.read_text(encoding="utf-8"))
        self.plan = builder.build_plan(self.base)
        self.steps = {step["id"]: step for step in self.plan["steps"]}

    def test_plan_extends_the_production_m0_path_without_provider_calls(self) -> None:
        journey.validate_plan(self.plan)
        self.assertEqual(
            self.plan["journey_id"], builder.COLLABORATION_JOURNEY_ID
        )
        self.assertEqual(self.plan["provider_mode"], "token_free")
        self.assertEqual(len(self.plan["steps"]), 63)
        self.assertEqual(len(self.steps), 63)
        self.assertTrue(all(not step.get("provider_call") for step in self.plan["steps"]))
        self.assertEqual(self.plan["steps"][:28], self.base["steps"])

    def test_work_graph_materializes_all_four_policy_modes(self) -> None:
        items = {
            item["work_item_id"]: item
            for item in self.steps["admission_plan_work_graph"]["body"]["command"]["items"]
        }
        self.assertEqual(set(items), {
            "review-admission",
            "solo-admission",
            "directed-admission",
            "release-admission",
        })
        self.assertEqual(items["review-admission"]["required_role"], "qa")
        self.assertEqual(items["review-admission"]["dependency_ids"], [])
        self.assertEqual(items["solo-admission"]["required_role"], "designer")
        self.assertEqual(items["directed-admission"]["required_role"], "developer")
        self.assertEqual(items["release-admission"]["dependency_ids"], [])
        self.assertEqual(items["release-admission"]["required_role"], "developer")
        self.assertEqual(
            items["release-admission"]["required_specialties"],
            [
                "artifact_authoring",
                "technical_design",
                "test_execution",
                "web_design",
                "web_development",
            ],
        )
        self.assertEqual(
            self.steps["assign_release_panel_owner"]["body"]["command"]["agent_id"],
            6,
        )
        expected = {
            "admit_independent_review": (
                "parallel_independent_review",
                [55, 56],
            ),
            "admit_solo": ("solo", [3]),
            "admit_directed_handoff": ("directed_handoff", [3, 6]),
            "admit_specialist_panel": ("specialist_panel", [3, 5, 6]),
        }
        for step_id, (mode, selected_agents) in expected.items():
            self.assertIn(
                {"pointer": "/mode", "equals": mode},
                self.steps[step_id]["assertions"],
            )
            self.assertIn(
                {"pointer": "/selected_agents", "equals": selected_agents},
                self.steps[step_id]["assertions"],
            )

    def test_admission_api_accepts_no_caller_supplied_policy_or_roster(self) -> None:
        forbidden = {
            "candidates",
            "task_risk",
            "reversibility",
            "ambiguity",
            "uncertainty",
            "separation_requirements",
            "budget",
            "organization_generation",
            "collaboration_generation",
        }
        admission_steps = [
            step for step in self.plan["steps"] if step["id"].startswith("admit_")
        ]
        self.assertEqual(len(admission_steps), 4)
        for step in admission_steps:
            command = step["body"]["command"]
            self.assertEqual(
                set(command),
                {
                    "command",
                    "project_id",
                    "work_item_id",
                    "expected_version",
                    "expected_benefit_ref",
                },
            )
            self.assertTrue(forbidden.isdisjoint(command))

    def test_canonical_events_and_projection_are_live_read_back(self) -> None:
        boundary = self.steps["observe_pre_panel_projection"]
        self.assertEqual(boundary["path"], "/operator/workflow/projections")
        self.assertEqual(
            boundary["capture"]["source_sequence"]["pointer"], "/source_sequence"
        )

        events = self.steps["observe_final_admission_events"]
        self.assertEqual(events["path"], "/operator/workflow/events")
        self.assertEqual(
            events["query"]["after"],
            {"$ref": "observe_pre_panel_projection.source_sequence"},
        )
        self.assertIn(
            {
                "pointer": "/2/project/collaboration_admissions/3/state",
                "equals": "completed",
            },
            events["assertions"],
        )

        projection = self.steps["observe_final_admission_projection"]
        self.assertEqual(projection["path"], "/operator/workflow/projections")
        self.assertEqual(len(projection["assertions"]), 8)
        self.assertEqual(
            projection["capture"]["projection_digest"]["type"], "digest"
        )

    def test_project_versions_are_observed_instead_of_predicted(self) -> None:
        self.assertEqual(
            self.steps["assign_solo_owner"]["body"]["command"]["expected_version"],
            {"$ref": "observe_independent_review_completed.project_version"},
        )
        self.assertEqual(
            self.steps["assign_directed_owner"]["body"]["command"]["expected_version"],
            {"$ref": "observe_solo_completed.project_version"},
        )
        self.assertEqual(
            self.steps["assign_release_panel_owner"]["body"]["command"][
                "expected_version"
            ],
            {"$ref": "observe_directed_handoff_completed.project_version"},
        )

    def test_mutable_admission_observations_replay_monotonically(self) -> None:
        for prefix in (
            "independent_review",
            "solo",
            "directed_handoff",
            "specialist_panel",
        ):
            admitted = self.steps[f"observe_{prefix}_admitted"]
            self.assertEqual(
                admitted["observe"]["replay"],
                "monotone_status_and_captures",
            )
            self.assertEqual(
                admitted["replay_assertions"][0]["one_of"],
                ["admitted", "completed"],
            )
            completed = self.steps[f"observe_{prefix}_completed"]
            self.assertEqual(
                completed["replay_assertions"][0]["one_of"], ["completed"]
            )

    def test_m0_designer_and_qa_match_their_pinned_tool_profiles(self) -> None:
        authoring = tomllib.loads(
            (REPO_ROOT / "config/workbench-profiles/web-authoring-v1.toml").read_text(
                encoding="utf-8"
            )
        )
        qa = tomllib.loads(
            (REPO_ROOT / "config/workbench-profiles/web-qa-v1.toml").read_text(
                encoding="utf-8"
            )
        )
        designer = tomllib.loads(
            (REPO_ROOT / "config/agents/AGENT-03-MAX-DESIGN.toml").read_text(
                encoding="utf-8"
            )
        )
        qa_agent = tomllib.loads(
            (REPO_ROOT / "config/agents/AGENT-55-LAURA-QA.toml").read_text(
                encoding="utf-8"
            )
        )
        self.assertTrue(
            set(authoring["capabilities"]).issubset(designer["capabilities"]["tools"])
        )
        self.assertTrue(set(qa["capabilities"]).issubset(qa_agent["capabilities"]["tools"]))

    def test_base_drift_fails_closed(self) -> None:
        changed = copy.deepcopy(self.base)
        changed["journey_id"] = "changed-base"
        with self.assertRaisesRegex(journey.JourneyError, "base M0 journey contract changed"):
            builder.build_plan(changed)


if __name__ == "__main__":
    unittest.main()
