#!/usr/bin/env python3
"""Tests for the predeclared #740 admission comparison."""

from __future__ import annotations

import copy
import importlib.util
import json
from pathlib import Path
import sys
import unittest


HERE = Path(__file__).resolve().parent
CONTRACT_PATH = HERE / "collaboration-admission-study-v1.json"


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


load_module("run_m0_journey", HERE / "run_m0_journey.py")
evaluator = load_module(
    "evaluate_collaboration_admission",
    HERE / "evaluate_collaboration_admission.py",
)


class CollaborationAdmissionEvaluationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.contract = json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))

    def observations(
        self,
        *,
        candidate_delivery: bool = True,
        candidate_risk: bool = True,
        solo_delivery: bool = True,
        solo_risk: bool = False,
        authority_regression_trial: int | None = None,
        candidate_latency_ms: int = 110,
        trial_count: int = 20,
    ) -> dict:
        rows = []
        for scenario_index, scenario in enumerate(self.contract["scenarios"]):
            for trial in range(trial_count):
                trial_id = f"trial-{trial + 1:03d}"
                for arm in ("solo", "candidate"):
                    candidate = arm == "candidate"
                    seed = f"{scenario_index}-{trial}-{arm}"
                    row = {
                            "scenario_id": scenario["scenario_id"],
                            "trial_id": trial_id,
                            "arm": arm,
                            "observed_mode": scenario["candidate_mode"] if candidate else "solo",
                            "participant_count": scenario["max_candidate_participants"] if candidate else 1,
                            "accepted_deliverable": candidate_delivery if candidate else solo_delivery,
                            "risk_control_satisfied": candidate_risk if candidate else solo_risk,
                            "authority_regression": candidate and trial == authority_regression_trial,
                            "privacy_regression": False,
                            "inference_units": 0,
                            "cost_micros": 0,
                            "latency_ms": candidate_latency_ms if candidate else 100,
                            "cpu_millis": 50 + (5 if candidate else 0),
                            "peak_rss_bytes": 1024 + (128 if candidate else 0),
                            "journey_run_id": f"run-{seed}",
                            "journey_plan_sha256": evaluator.hashlib.sha256(
                                f"{seed}-plan".encode("ascii")
                            ).hexdigest(),
                            "journey_ledger_sha256": evaluator.hashlib.sha256(
                                f"{seed}-ledger".encode("ascii")
                            ).hexdigest(),
                            "event_readback_sha256": evaluator.hashlib.sha256(
                                f"{seed}-event".encode("ascii")
                            ).hexdigest(),
                            "projection_readback_sha256": evaluator.hashlib.sha256(
                                f"{seed}-projection".encode("ascii")
                            ).hexdigest(),
                            "result_readback_sha256": evaluator.hashlib.sha256(
                                f"{seed}-result".encode("ascii")
                            ).hexdigest(),
                        }
                    rows.append(row)
        observations = {
            "schema_version": 1,
            "study_id": self.contract["study_id"],
            "policy_generation": self.contract["policy_generation"],
            "release_sha256": "a" * 64,
            "observations": rows,
        }
        for row in rows:
            row["evidence_digest"] = evaluator.sha256(
                {
                    "release_sha256": observations["release_sha256"],
                    "policy_generation": observations["policy_generation"],
                    "observation": row,
                }
            )
        return observations

    def test_hard_risk_gain_adopts_the_bounded_candidate_modes(self) -> None:
        result = evaluator.evaluate(self.contract, self.observations())
        self.assertEqual(result["result"], "complete")
        self.assertEqual(
            [scenario["effective_mode"] for scenario in result["scenarios"]],
            [scenario["candidate_mode"] for scenario in self.contract["scenarios"]],
        )
        self.assertTrue(all(scenario["de_risk"] for scenario in result["scenarios"]))
        self.assertTrue(all(not scenario["quality_win"] for scenario in result["scenarios"]))
        self.assertRegex(result["result_sha256"], r"^[0-9a-f]{64}$")

    def test_no_quality_or_risk_gain_retains_solo(self) -> None:
        result = evaluator.evaluate(
            self.contract,
            self.observations(candidate_risk=False, solo_risk=False),
        )
        self.assertTrue(
            all(scenario["effective_mode"] == "solo" for scenario in result["scenarios"])
        )

    def test_quality_gain_can_adopt_without_a_risk_delta(self) -> None:
        result = evaluator.evaluate(
            self.contract,
            self.observations(
                solo_delivery=False,
                candidate_delivery=True,
                solo_risk=False,
                candidate_risk=False,
            ),
        )
        self.assertTrue(all(scenario["quality_win"] for scenario in result["scenarios"]))
        self.assertTrue(all(scenario["decision"] == "adopt_candidate" for scenario in result["scenarios"]))

    def test_any_authority_regression_rejects_the_candidate(self) -> None:
        result = evaluator.evaluate(
            self.contract,
            self.observations(authority_regression_trial=4),
        )
        self.assertTrue(
            all(scenario["effective_mode"] == "solo" for scenario in result["scenarios"])
        )
        self.assertTrue(
            all(scenario["authority_or_privacy_regressions"] == 1 for scenario in result["scenarios"])
        )

    def test_resource_growth_beyond_participant_limit_rejects_candidate(self) -> None:
        result = evaluator.evaluate(
            self.contract,
            self.observations(candidate_latency_ms=301),
        )
        self.assertTrue(
            all(scenario["decision"] == "retain_solo" for scenario in result["scenarios"])
        )
        self.assertTrue(
            all(not scenario["resource_guard_pass"] for scenario in result["scenarios"])
        )

    def test_resource_guard_does_not_hide_a_one_unit_overrun_in_rounded_means(self) -> None:
        observations = self.observations(candidate_latency_ms=200)
        row = next(
            item
            for item in observations["observations"]
            if item["scenario_id"] == "directed-capability-gap-v1"
            and item["trial_id"] == "trial-001"
            and item["arm"] == "candidate"
        )
        row["latency_ms"] += 1
        material = {key: value for key, value in row.items() if key != "evidence_digest"}
        row["evidence_digest"] = evaluator.sha256(
            {
                "release_sha256": observations["release_sha256"],
                "policy_generation": observations["policy_generation"],
                "observation": material,
            }
        )

        result = evaluator.evaluate(self.contract, observations)
        directed = result["scenarios"][0]
        self.assertEqual(directed["resource_means"]["latency_ms"]["solo"], 100)
        self.assertEqual(directed["resource_means"]["latency_ms"]["candidate"], 200)
        self.assertFalse(directed["resource_guard_pass"])
        self.assertEqual(directed["decision"], "retain_solo")

    def test_incomplete_or_duplicated_pairs_fail_closed(self) -> None:
        with self.assertRaisesRegex(evaluator.StudyError, "required paired trials"):
            evaluator.evaluate(self.contract, self.observations(trial_count=19))

        observations = self.observations()
        observations["observations"].append(copy.deepcopy(observations["observations"][0]))
        with self.assertRaisesRegex(evaluator.StudyError, "duplicated"):
            evaluator.evaluate(self.contract, observations)

    def test_contract_and_observation_authority_are_immutable(self) -> None:
        changed = copy.deepcopy(self.contract)
        changed["confidence_basis_points"] = 9_000
        with self.assertRaisesRegex(evaluator.StudyError, "95 percent"):
            evaluator.evaluate(changed, self.observations())

        changed = copy.deepcopy(self.contract)
        changed["resource_adoption_rule"] = "report_only"
        with self.assertRaisesRegex(evaluator.StudyError, "resource adoption rule"):
            evaluator.evaluate(changed, self.observations())

        observations = self.observations()
        observations["release_sha256"] = "not-a-digest"
        with self.assertRaisesRegex(evaluator.StudyError, "contract or release"):
            evaluator.evaluate(self.contract, observations)

    def test_observation_outcomes_are_bound_to_exact_live_readbacks(self) -> None:
        observations = self.observations()
        observations["observations"][0]["accepted_deliverable"] = False
        with self.assertRaisesRegex(evaluator.StudyError, "does not bind"):
            evaluator.evaluate(self.contract, observations)

    def test_equal_paired_outcomes_do_not_claim_an_advantage(self) -> None:
        result = evaluator.evaluate(
            self.contract,
            self.observations(candidate_risk=True, solo_risk=True),
        )
        self.assertTrue(
            all(
                scenario["risk_control_satisfied"]["paired_advantage"]
                ["discordant_pairs"]
                == 0
                for scenario in result["scenarios"]
            )
        )
        self.assertTrue(all(not scenario["de_risk"] for scenario in result["scenarios"]))


if __name__ == "__main__":
    unittest.main()
