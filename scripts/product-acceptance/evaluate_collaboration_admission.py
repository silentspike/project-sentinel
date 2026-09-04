#!/usr/bin/env python3
"""Evaluate the predeclared #740 solo-versus-team admission study."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import sys
from typing import Any

import run_m0_journey as journey


SCHEMA_VERSION = 1
CONFIDENCE_BASIS_POINTS = 9_500
MINIMUM_PAIRED_TRIALS = 20
MAXIMUM_PAIRED_TRIALS = 1_000
Z_95 = 1.959963984540054
IDENTIFIER_RE = re.compile(r"^[a-z0-9][a-z0-9_-]{0,127}$")
DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")
CANDIDATE_MODES = {
    "directed_handoff",
    "parallel_independent_review",
    "specialist_panel",
}
ARMS = {"solo", "candidate"}
METRICS = (
    "inference_units",
    "cost_micros",
    "latency_ms",
    "cpu_millis",
    "peak_rss_bytes",
)
PAIRED_ADOPTION_RULE = "discordant_wilson_lower_gt_half"
RESOURCE_ADOPTION_RULE = "candidate_mean_lte_solo_mean_times_participant_limit"


class StudyError(ValueError):
    """Raised when study authority or observations are not canonical."""


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=True,
        allow_nan=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("ascii")


def sha256(value: Any) -> str:
    return hashlib.sha256(canonical_json(value)).hexdigest()


def _exact_keys(value: Any, required: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != required:
        raise StudyError(f"{label} fields are invalid")
    return value


def _identifier(value: Any, label: str) -> str:
    if not isinstance(value, str) or not IDENTIFIER_RE.fullmatch(value):
        raise StudyError(f"{label} is invalid")
    return value


def _positive_int(value: Any, label: str, maximum: int | None = None) -> int:
    if (
        not isinstance(value, int)
        or isinstance(value, bool)
        or value <= 0
        or (maximum is not None and value > maximum)
    ):
        raise StudyError(f"{label} is invalid")
    return value


def _nonnegative_int(value: Any, label: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise StudyError(f"{label} is invalid")
    return value


def validate_contract(raw: Any) -> dict[str, Any]:
    contract = _exact_keys(
        raw,
        {
            "schema_version",
            "study_id",
            "policy_generation",
            "confidence_basis_points",
            "minimum_paired_trials",
            "paired_adoption_rule",
            "resource_adoption_rule",
            "maximum_authority_privacy_regressions",
            "scenarios",
        },
        "study contract",
    )
    if contract["schema_version"] != SCHEMA_VERSION:
        raise StudyError("study schema version is unsupported")
    _identifier(contract["study_id"], "study id")
    _positive_int(contract["policy_generation"], "policy generation")
    if contract["confidence_basis_points"] != CONFIDENCE_BASIS_POINTS:
        raise StudyError("study confidence must remain predeclared at 95 percent")
    if contract["paired_adoption_rule"] != PAIRED_ADOPTION_RULE:
        raise StudyError("paired adoption rule is invalid")
    if contract["resource_adoption_rule"] != RESOURCE_ADOPTION_RULE:
        raise StudyError("resource adoption rule is invalid")
    if contract["maximum_authority_privacy_regressions"] != 0:
        raise StudyError("authority and privacy regression tolerance must remain zero")
    minimum = _positive_int(
        contract["minimum_paired_trials"],
        "minimum paired trials",
        MAXIMUM_PAIRED_TRIALS,
    )
    if minimum < MINIMUM_PAIRED_TRIALS:
        raise StudyError("study requires at least 20 paired trials")
    scenarios = contract["scenarios"]
    if not isinstance(scenarios, list) or not scenarios:
        raise StudyError("study scenarios are missing")
    seen = set()
    for scenario in scenarios:
        item = _exact_keys(
            scenario,
            {
                "scenario_id",
                "task_class",
                "candidate_mode",
                "de_risk_dimension",
                "max_candidate_participants",
            },
            "study scenario",
        )
        scenario_id = _identifier(item["scenario_id"], "scenario id")
        if scenario_id in seen:
            raise StudyError("study scenario is duplicated")
        seen.add(scenario_id)
        _identifier(item["task_class"], "task class")
        _identifier(item["de_risk_dimension"], "de-risk dimension")
        if item["candidate_mode"] not in CANDIDATE_MODES:
            raise StudyError("candidate mode is invalid")
        participants = _positive_int(
            item["max_candidate_participants"], "candidate participant limit", 4
        )
        if participants < 2:
            raise StudyError("multi-agent scenario requires at least two participants")
    if not journey.public_safe(contract):
        raise StudyError("study contract is not public-safe")
    return contract


def validate_observations(raw: Any, contract: dict[str, Any]) -> dict[str, Any]:
    evidence = _exact_keys(
        raw,
        {
            "schema_version",
            "study_id",
            "policy_generation",
            "release_sha256",
            "observations",
        },
        "study observations",
    )
    if (
        evidence["schema_version"] != SCHEMA_VERSION
        or evidence["study_id"] != contract["study_id"]
        or evidence["policy_generation"] != contract["policy_generation"]
        or not isinstance(evidence["release_sha256"], str)
        or not DIGEST_RE.fullmatch(evidence["release_sha256"])
    ):
        raise StudyError("study observations do not match the contract or release")
    scenarios = {item["scenario_id"]: item for item in contract["scenarios"]}
    observations = evidence["observations"]
    if not isinstance(observations, list):
        raise StudyError("study observations are invalid")
    seen_pairs: set[tuple[str, str, str]] = set()
    evidence_digests = set()
    for observation in observations:
        item = _exact_keys(
            observation,
            {
                "scenario_id",
                "trial_id",
                "arm",
                "observed_mode",
                "participant_count",
                "accepted_deliverable",
                "risk_control_satisfied",
                "authority_regression",
                "privacy_regression",
                "inference_units",
                "cost_micros",
                "latency_ms",
                "cpu_millis",
                "peak_rss_bytes",
                "journey_run_id",
                "journey_plan_sha256",
                "journey_ledger_sha256",
                "event_readback_sha256",
                "projection_readback_sha256",
                "result_readback_sha256",
                "evidence_digest",
            },
            "study observation",
        )
        scenario_id = _identifier(item["scenario_id"], "observation scenario id")
        trial_id = _identifier(item["trial_id"], "trial id")
        arm = item["arm"]
        if scenario_id not in scenarios or arm not in ARMS:
            raise StudyError("observation scenario or arm is invalid")
        key = (scenario_id, trial_id, arm)
        if key in seen_pairs:
            raise StudyError("study observation arm is duplicated")
        seen_pairs.add(key)
        expected_modes = (
            {"solo"}
            if arm == "solo"
            else {"solo", scenarios[scenario_id]["candidate_mode"]}
        )
        if item["observed_mode"] not in expected_modes:
            raise StudyError("observed mode is outside the predeclared study")
        participant_count = _positive_int(item["participant_count"], "participant count", 4)
        if arm == "solo" and participant_count != 1:
            raise StudyError("solo observation must have exactly one participant")
        if (
            arm == "candidate"
            and participant_count > scenarios[scenario_id]["max_candidate_participants"]
        ):
            raise StudyError("candidate observation exceeds its participant limit")
        for field in (
            "accepted_deliverable",
            "risk_control_satisfied",
            "authority_regression",
            "privacy_regression",
        ):
            if not isinstance(item[field], bool):
                raise StudyError(f"{field} must be boolean")
        for field in METRICS:
            _nonnegative_int(item[field], field)
        _identifier(item["journey_run_id"], "journey run id")
        for field in (
            "journey_plan_sha256",
            "journey_ledger_sha256",
            "event_readback_sha256",
            "projection_readback_sha256",
            "result_readback_sha256",
        ):
            if not isinstance(item[field], str) or not DIGEST_RE.fullmatch(item[field]):
                raise StudyError(f"{field} is invalid")
        digest = item["evidence_digest"]
        if not isinstance(digest, str) or not DIGEST_RE.fullmatch(digest):
            raise StudyError("observation evidence digest is invalid")
        material = {key: value for key, value in item.items() if key != "evidence_digest"}
        expected_digest = sha256(
            {
                "release_sha256": evidence["release_sha256"],
                "policy_generation": evidence["policy_generation"],
                "observation": material,
            }
        )
        if digest != expected_digest:
            raise StudyError("observation evidence digest does not bind its readbacks")
        if digest in evidence_digests:
            raise StudyError("observation evidence digest is reused")
        evidence_digests.add(digest)
    if not journey.public_safe(evidence):
        raise StudyError("study observations are not public-safe")
    return evidence


def wilson_interval(successes: int, total: int) -> dict[str, int]:
    if total <= 0 or successes < 0 or successes > total:
        raise StudyError("Wilson interval counts are invalid")
    probability = successes / total
    denominator = 1.0 + (Z_95 * Z_95 / total)
    center = (probability + (Z_95 * Z_95 / (2.0 * total))) / denominator
    margin = (
        Z_95
        * math.sqrt(
            probability * (1.0 - probability) / total
            + Z_95 * Z_95 / (4.0 * total * total)
        )
        / denominator
    )
    return {
        "lower_ppm": round(max(0.0, center - margin) * 1_000_000),
        "upper_ppm": round(min(1.0, center + margin) * 1_000_000),
    }


def _mean(values: list[int]) -> int:
    return (sum(values) + len(values) // 2) // len(values)


def paired_advantage_interval(
    solo: list[dict[str, Any]],
    candidate: list[dict[str, Any]],
    field: str,
) -> dict[str, int]:
    candidate_wins = sum(
        candidate_item[field] and not solo_item[field]
        for solo_item, candidate_item in zip(solo, candidate, strict=True)
    )
    solo_wins = sum(
        solo_item[field] and not candidate_item[field]
        for solo_item, candidate_item in zip(solo, candidate, strict=True)
    )
    discordant_pairs = candidate_wins + solo_wins
    if discordant_pairs == 0:
        interval = {"lower_ppm": 0, "upper_ppm": 1_000_000}
    else:
        interval = wilson_interval(candidate_wins, discordant_pairs)
    return {
        "candidate_wins": candidate_wins,
        "solo_wins": solo_wins,
        "discordant_pairs": discordant_pairs,
        **interval,
    }


def evaluate(contract_raw: Any, observations_raw: Any) -> dict[str, Any]:
    contract = validate_contract(contract_raw)
    evidence = validate_observations(observations_raw, contract)
    minimum = contract["minimum_paired_trials"]
    result_scenarios = []
    for scenario in contract["scenarios"]:
        scenario_id = scenario["scenario_id"]
        rows = [
            item
            for item in evidence["observations"]
            if item["scenario_id"] == scenario_id
        ]
        by_trial: dict[str, dict[str, dict[str, Any]]] = {}
        for row in rows:
            by_trial.setdefault(row["trial_id"], {})[row["arm"]] = row
        if (
            len(by_trial) < minimum
            or len(by_trial) > MAXIMUM_PAIRED_TRIALS
            or any(set(arms) != ARMS for arms in by_trial.values())
        ):
            raise StudyError("scenario does not contain the required paired trials")
        solo = [arms["solo"] for _, arms in sorted(by_trial.items())]
        candidate = [arms["candidate"] for _, arms in sorted(by_trial.items())]
        solo_delivery = wilson_interval(
            sum(item["accepted_deliverable"] for item in solo), len(solo)
        )
        candidate_delivery = wilson_interval(
            sum(item["accepted_deliverable"] for item in candidate), len(candidate)
        )
        solo_risk = wilson_interval(
            sum(item["risk_control_satisfied"] for item in solo), len(solo)
        )
        candidate_risk = wilson_interval(
            sum(item["risk_control_satisfied"] for item in candidate), len(candidate)
        )
        delivery_advantage = paired_advantage_interval(
            solo, candidate, "accepted_deliverable"
        )
        risk_advantage = paired_advantage_interval(
            solo, candidate, "risk_control_satisfied"
        )
        mode_selected = wilson_interval(
            sum(
                item["observed_mode"] == scenario["candidate_mode"]
                for item in candidate
            ),
            len(candidate),
        )
        regressions = sum(
            item["authority_regression"] or item["privacy_regression"]
            for item in candidate
        )
        quality_win = delivery_advantage["lower_ppm"] > 500_000
        de_risk = risk_advantage["lower_ppm"] > 500_000
        exact_mode = all(
            item["observed_mode"] == scenario["candidate_mode"] for item in candidate
        )
        resource_means = {}
        for field in METRICS:
            solo_values = [item[field] for item in solo]
            candidate_values = [item[field] for item in candidate]
            solo_sum = sum(solo_values)
            candidate_sum = sum(candidate_values)
            maximum_candidate_sum = (
                solo_sum * scenario["max_candidate_participants"]
            )
            resource_means[field] = {
                "solo": _mean(solo_values),
                "candidate": _mean(candidate_values),
                "added": _mean(candidate_values) - _mean(solo_values),
                "solo_sum": solo_sum,
                "candidate_sum": candidate_sum,
                "maximum_candidate_sum": maximum_candidate_sum,
                "within_limit": candidate_sum <= maximum_candidate_sum,
            }
        resource_guard_pass = all(
            values["within_limit"] for values in resource_means.values()
        )
        adopted = (
            regressions == 0
            and exact_mode
            and resource_guard_pass
            and (quality_win or de_risk)
        )
        result_scenarios.append(
            {
                "scenario_id": scenario_id,
                "task_class": scenario["task_class"],
                "candidate_mode": scenario["candidate_mode"],
                "de_risk_dimension": scenario["de_risk_dimension"],
                "paired_trials": len(by_trial),
                "accepted_deliverable": {
                    "solo": solo_delivery,
                    "candidate": candidate_delivery,
                    "paired_advantage": delivery_advantage,
                },
                "risk_control_satisfied": {
                    "solo": solo_risk,
                    "candidate": candidate_risk,
                    "paired_advantage": risk_advantage,
                },
                "candidate_mode_selected": mode_selected,
                "authority_or_privacy_regressions": regressions,
                "quality_win": quality_win,
                "de_risk": de_risk,
                "resource_means": resource_means,
                "resource_guard_pass": resource_guard_pass,
                "decision": "adopt_candidate" if adopted else "retain_solo",
                "effective_mode": scenario["candidate_mode"] if adopted else "solo",
            }
        )
    result = {
        "schema_version": SCHEMA_VERSION,
        "study_id": contract["study_id"],
        "contract_sha256": sha256(contract),
        "observations_sha256": sha256(evidence),
        "release_sha256": evidence["release_sha256"],
        "policy_generation": contract["policy_generation"],
        "confidence_basis_points": CONFIDENCE_BASIS_POINTS,
        "resource_adoption_rule": RESOURCE_ADOPTION_RULE,
        "result": "complete",
        "scenarios": result_scenarios,
    }
    result["result_sha256"] = sha256(result)
    if not journey.public_safe(result):
        raise StudyError("study result is not public-safe")
    return result


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--contract", type=Path, required=True)
    parser.add_argument("--observations", type=Path, required=True)
    parser.add_argument("--output", required=True)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        contract = journey.load_json(args.contract, "collaboration study contract")
        observations = journey.load_json(args.observations, "collaboration observations")
        output = journey.safe_output_path(args.output, "collaboration study result")
        journey.atomic_json_write(output, evaluate(contract, observations))
    except (StudyError, journey.JourneyError) as exc:
        print(f"collaboration study failed: {exc}", file=sys.stderr)
        return 1
    print(f"collaboration study complete: {contract['study_id']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
