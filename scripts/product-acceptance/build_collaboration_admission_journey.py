#!/usr/bin/env python3
"""Build the token-free #740 collaboration journey from the accepted M0 journey."""

from __future__ import annotations

import argparse
import copy
import hashlib
from pathlib import Path
import sys
from typing import Any

import run_m0_journey as journey


BASE_JOURNEY_ID = "single-node-web-company-v5"
COLLABORATION_JOURNEY_ID = "single-node-collaboration-admission-v1"
ORGANIZATION_DIGEST = "8573b066e5b7205e3af66a58352c4c698387b6f17e657f7f57f6daa9edadb1c6"
DESIGN_CONTRACT_DIGEST = "4a6aac2362da98d08ad589ba5e36a12eda339a3c0f61f73f30af2a6d714dbd93"
SOURCE_CONTRACT_DIGEST = "7af74cf0681ccef86e9b28d2caac337e5f08448435d9eb139ade1590a67ab702"
QA_GATE_DIGEST = "24f492112d520225c3bd63bb6c486afebdf35815a1056f918efe0d86d87c80e0"
REVIEW_CONTRACT_DIGEST = "d6b1c2c4000300e707af509e7f6a5f2858bc4a189029cb82ff1fec42bf1d6492"
RELEASE_CONTRACT_DIGEST = "698a707bff8708584b0a3bf80b2ade0361102a7fedc1e93e888391f9ad1fbd5d"


def _ref(name: str) -> dict[str, str]:
    return {"$ref": name}


def _operation_id() -> dict[str, bool]:
    return {"$operation_id": True}


def _positive(
    *,
    step_id: str,
    phase: str,
    path: str,
    credential_alias: str,
    route_role: str,
    body: dict[str, Any],
    assertions: list[dict[str, Any]],
    capture: dict[str, dict[str, str]],
    checkpoint: str,
) -> dict[str, Any]:
    return {
        "id": step_id,
        "phase": phase,
        "kind": "positive",
        "method": "POST",
        "path": path,
        "credential_alias": credential_alias,
        "route_role": route_role,
        "body": body,
        "expected_status": [200],
        "assertions": assertions,
        "initial_assertions": copy.deepcopy(assertions),
        "replay_assertions": copy.deepcopy(assertions),
        "capture": capture,
        "checkpoint": checkpoint,
    }


def _observe_project(
    *,
    step_id: str,
    phase: str,
    project_id_ref: str,
    admission_index: int,
    expected_state: str,
) -> dict[str, Any]:
    return {
        "id": step_id,
        "phase": phase,
        "kind": "observe",
        "method": "GET",
        "path": "/operator/workflow/projects",
        "credential_alias": "project_manager",
        "route_role": "operator",
        "query": {"project_id": _ref(project_id_ref)},
        "expected_status": [200],
        "assertions": [
            {
                "pointer": f"/collaboration_admissions/{admission_index}/state",
                "equals": expected_state,
            }
        ],
        "capture": {
            "project_version": {"pointer": "/version", "type": "integer"},
            "collaboration_generation": {
                "pointer": "/collaboration_generation",
                "type": "integer",
            },
        },
        "observe": {
            "interval_ms": 50,
            "max_attempts": 5,
            "max_elapsed_ms": 5_000,
            "replay": "exact_status_and_captures",
            "retry_statuses": [404, 409, 425, 429],
        },
    }


def _observe_projection_boundary(project_id_ref: str) -> dict[str, Any]:
    return {
        "id": "observe_pre_panel_projection",
        "phase": "acceptance",
        "kind": "observe",
        "method": "GET",
        "path": "/operator/workflow/projections",
        "credential_alias": "project_manager",
        "route_role": "operator",
        "query": {"project_id": _ref(project_id_ref)},
        "expected_status": [200],
        "assertions": [
            {
                "pointer": "/project/collaboration_admissions/2/state",
                "equals": "completed",
            }
        ],
        "capture": {
            "source_sequence": {"pointer": "/source_sequence", "type": "integer"},
            "projection_digest": {
                "pointer": "/projection_digest",
                "type": "digest",
            },
        },
        "observe": {
            "interval_ms": 50,
            "max_attempts": 5,
            "max_elapsed_ms": 5_000,
            "replay": "exact_status_and_captures",
            "retry_statuses": [404, 409, 425, 429],
        },
    }


def _observe_final_projection(project_id_ref: str) -> dict[str, Any]:
    return {
        "id": "observe_final_admission_projection",
        "phase": "acceptance",
        "kind": "observe",
        "method": "GET",
        "path": "/operator/workflow/projections",
        "credential_alias": "project_manager",
        "route_role": "operator",
        "query": {"project_id": _ref(project_id_ref)},
        "expected_status": [200],
        "assertions": [
            {
                "pointer": f"/project/collaboration_admissions/{index}/mode",
                "equals": mode,
            }
            for index, mode in enumerate(
                (
                    "parallel_independent_review",
                    "solo",
                    "directed_handoff",
                    "specialist_panel",
                )
            )
        ]
        + [
            {
                "pointer": f"/project/collaboration_admissions/{index}/state",
                "equals": "completed",
            }
            for index in range(4)
        ],
        "capture": {
            "source_sequence": {"pointer": "/source_sequence", "type": "integer"},
            "projection_digest": {
                "pointer": "/projection_digest",
                "type": "digest",
            },
        },
        "observe": {
            "interval_ms": 50,
            "max_attempts": 5,
            "max_elapsed_ms": 5_000,
            "replay": "exact_status_and_captures",
            "retry_statuses": [404, 409, 425, 429],
        },
    }


def _observe_final_admission_events() -> dict[str, Any]:
    return {
        "id": "observe_final_admission_events",
        "phase": "acceptance",
        "kind": "observe",
        "method": "GET",
        "path": "/operator/workflow/events",
        "credential_alias": "project_manager",
        "route_role": "operator",
        "query": {
            "after": _ref("observe_pre_panel_projection.source_sequence"),
            "limit": 10,
        },
        "expected_status": [200],
        "assertions": [
            {
                "pointer": "/1/event_type",
                "equals": "project_collaboration_admission_recorded",
            },
            {
                "pointer": "/1/project/collaboration_admissions/3/state",
                "equals": "admitted",
            },
            {
                "pointer": "/2/event_type",
                "equals": "project_collaboration_admission_recorded",
            },
            {
                "pointer": "/2/project/collaboration_admissions/3/state",
                "equals": "completed",
            },
        ],
        "capture": {
            "admitted_event_id": {"pointer": "/1/event_id", "type": "id"},
            "completed_event_id": {"pointer": "/2/event_id", "type": "id"},
        },
        "observe": {
            "interval_ms": 50,
            "max_attempts": 5,
            "max_elapsed_ms": 5_000,
            "replay": "exact_status_and_captures",
            "retry_statuses": [404, 409, 425, 429],
        },
    }


def _assignment_step(
    *,
    step_id: str,
    phase: str,
    project_id_ref: str,
    previous_version: str,
    work_item_id: str,
    agent_id: int,
    reason_ref: str,
) -> dict[str, Any]:
    return _positive(
        step_id=step_id,
        phase=phase,
        path="/agent/workflow/commands",
        credential_alias="project_manager",
        route_role="agent",
        body={
            "operation_id": _operation_id(),
            "command": {
                "command": "assign_work",
                "project_id": _ref(project_id_ref),
                "expected_version": _ref(previous_version),
                "work_item_id": work_item_id,
                "agent_id": agent_id,
                "organization_generation": 1,
                "organization_digest": ORGANIZATION_DIGEST,
                "reason_ref": reason_ref,
            },
        },
        assertions=[
            {
                "pointer": f"/response/value/work_items/{work_item_id}/state",
                "equals": "assigned",
            }
        ],
        capture={
            "project_version": {
                "pointer": "/response/value/version",
                "type": "integer",
            }
        },
        checkpoint=f"after_{step_id}",
    )


def _admission_steps(
    *,
    prefix: str,
    phase: str,
    project_id_ref: str,
    previous_version: str,
    work_item_id: str,
    mode: str,
    selected_agents: list[int],
    admission_index: int,
    expected_benefit_ref: str,
) -> list[dict[str, Any]]:
    admit_id = f"admit_{prefix}"
    observe_admit_id = f"observe_{prefix}_admitted"
    progress_id = f"complete_{prefix}"
    observe_complete_id = f"observe_{prefix}_completed"
    novelty_digest = hashlib.sha256(f"{prefix}-novelty-v1".encode("ascii")).hexdigest()
    milestone_digest = hashlib.sha256(
        f"{prefix}-milestone-v1".encode("ascii")
    ).hexdigest()
    work_digest = hashlib.sha256(f"{prefix}-work-v1".encode("ascii")).hexdigest()
    admission_assertions = [
        {"pointer": "/mode", "equals": mode},
        {"pointer": "/selected_agents", "equals": selected_agents},
        {"pointer": "/state", "equals": "admitted"},
        {"pointer": "/transition_sequence", "equals": 1},
    ]
    completion_assertions = [
        {"pointer": "/mode", "equals": mode},
        {"pointer": "/selected_agents", "equals": selected_agents},
        {"pointer": "/state", "equals": "completed"},
        {"pointer": "/transition_sequence", "equals": 2},
    ]
    return [
        _positive(
            step_id=admit_id,
            phase=phase,
            path="/company/workflow/collaboration/admissions",
            credential_alias="project_manager",
            route_role="company",
            body={
                "operation_id": _operation_id(),
                "command": {
                    "command": "admit",
                    "project_id": _ref(project_id_ref),
                    "work_item_id": work_item_id,
                    "expected_version": _ref(previous_version),
                    "expected_benefit_ref": expected_benefit_ref,
                },
            },
            assertions=admission_assertions,
            capture={
                "admission_id": {"pointer": "/admission_id", "type": "id"},
                "transition_sequence": {
                    "pointer": "/transition_sequence",
                    "type": "integer",
                },
                "decision_digest": {
                    "pointer": "/decision_digest",
                    "type": "digest",
                },
            },
            checkpoint=f"after_{admit_id}",
        ),
        _observe_project(
            step_id=observe_admit_id,
            phase=phase,
            project_id_ref=project_id_ref,
            admission_index=admission_index,
            expected_state="admitted",
        ),
        _positive(
            step_id=progress_id,
            phase=phase,
            path="/company/workflow/collaboration/admissions",
            credential_alias="project_manager",
            route_role="company",
            body={
                "operation_id": _operation_id(),
                "command": {
                    "command": "progress",
                    "project_id": _ref(project_id_ref),
                    "expected_version": _ref(
                        f"{observe_admit_id}.project_version"
                    ),
                    "admission_id": _ref(f"{admit_id}.admission_id"),
                    "progress": {
                        "expected_transition_sequence": _ref(
                            f"{admit_id}.transition_sequence"
                        ),
                        "novelty_micros": 1_000_000,
                        "novelty_digest": novelty_digest,
                        "milestone_digest": milestone_digest,
                        "work_digest": work_digest,
                        "disposition": "complete",
                        "reason_ref": f"{prefix}-scenario-completed",
                    },
                },
            },
            assertions=completion_assertions,
            capture={
                "transition_sequence": {
                    "pointer": "/transition_sequence",
                    "type": "integer",
                },
                "decision_digest": {
                    "pointer": "/decision_digest",
                    "type": "digest",
                },
            },
            checkpoint=f"after_{progress_id}",
        ),
        _observe_project(
            step_id=observe_complete_id,
            phase=phase,
            project_id_ref=project_id_ref,
            admission_index=admission_index,
            expected_state="completed",
        ),
    ]


def _review_work_item() -> dict[str, Any]:
    return {
        "work_item_id": "review-admission",
        "title": "Review the collaboration admission policy",
        "objective": "Independently verify the bounded admission decision.",
        "required_role": "qa",
        "required_specialties": [
            "browser_validation",
            "quality_assurance",
            "security_validation",
        ],
        "dependency_ids": [],
        "owner": 55,
        "inputs": [],
        "outputs": [
            {
                "name": "review_report",
                "media_type": "text/markdown",
                "digest_algorithm": "sha256",
                "contract_generation": 1,
                "contract_digest": REVIEW_CONTRACT_DIGEST,
            }
        ],
        "quality_gate": {
            "gate_id": "web-work-item-qa-v1",
            "generation": 1,
            "digest": QA_GATE_DIGEST,
        },
        "budget_micros": 250_000,
    }


def _solo_work_item() -> dict[str, Any]:
    return {
        "work_item_id": "solo-admission",
        "title": "Prepare the solo admission decision",
        "objective": "Confirm that one capable owner remains the complete team.",
        "required_role": "designer",
        "required_specialties": ["artifact_authoring", "web_design"],
        "dependency_ids": [],
        "owner": 3,
        "inputs": [],
        "outputs": [
            {
                "name": "design_specification",
                "media_type": "text/markdown",
                "digest_algorithm": "sha256",
                "contract_generation": 1,
                "contract_digest": DESIGN_CONTRACT_DIGEST,
            }
        ],
        "quality_gate": {
            "gate_id": "web-work-item-qa-v1",
            "generation": 1,
            "digest": QA_GATE_DIGEST,
        },
        "budget_micros": 250_000,
    }


def _directed_work_item() -> dict[str, Any]:
    return {
        "work_item_id": "directed-admission",
        "title": "Prepare the directed handoff decision",
        "objective": "Add exactly one designer to close the owner's capability gap.",
        "required_role": "developer",
        "required_specialties": [
            "artifact_authoring",
            "test_execution",
            "web_design",
            "web_development",
        ],
        "dependency_ids": [],
        "owner": 6,
        "inputs": [],
        "outputs": [
            {
                "name": "source_tree",
                "media_type": "application/vnd.sentinel.source-tree",
                "digest_algorithm": "sha256",
                "contract_generation": 1,
                "contract_digest": SOURCE_CONTRACT_DIGEST,
            }
        ],
        "quality_gate": {
            "gate_id": "web-work-item-qa-v1",
            "generation": 1,
            "digest": QA_GATE_DIGEST,
        },
        "budget_micros": 250_000,
    }


def _panel_work_item() -> dict[str, Any]:
    return {
        "work_item_id": "release-admission",
        "title": "Integrate the design and source decisions",
        "objective": "Review both dependency-owner decisions as one technical integration.",
        "required_role": "developer",
        "required_specialties": [
            "artifact_authoring",
            "technical_design",
            "test_execution",
            "web_design",
            "web_development",
        ],
        "dependency_ids": [],
        "owner": 6,
        "inputs": [],
        "outputs": [
            {
                "name": "release_plan",
                "media_type": "text/markdown",
                "digest_algorithm": "sha256",
                "contract_generation": 1,
                "contract_digest": RELEASE_CONTRACT_DIGEST,
            }
        ],
        "quality_gate": {
            "gate_id": "web-work-item-qa-v1",
            "generation": 1,
            "digest": QA_GATE_DIGEST,
        },
        "budget_micros": 250_000,
    }


def _rewrite_setup_refs(value: Any, mapping: dict[str, str]) -> Any:
    if isinstance(value, list):
        return [_rewrite_setup_refs(item, mapping) for item in value]
    if isinstance(value, dict):
        rewritten = {
            key: _rewrite_setup_refs(item, mapping) for key, item in value.items()
        }
        reference = rewritten.get("$ref")
        if len(rewritten) == 1 and isinstance(reference, str):
            step_id, separator, suffix = reference.partition(".")
            if step_id in mapping:
                rewritten["$ref"] = mapping[step_id] + (separator + suffix if separator else "")
        return rewritten
    return value


def _admission_project_setup(base: dict[str, Any]) -> list[dict[str, Any]]:
    setup_ids = [
        "submit_customer_request",
        "clarify_customer_request",
        "qualify_customer_request",
        "create_proposal",
        "accept_proposal",
        "plan_work_graph",
        "record_architecture_decision",
        "create_project_room",
        "raise_project_blocker",
        "escalate_project_blocker",
        "resolve_project_blocker",
        "activate_project",
    ]
    by_id = {step["id"]: step for step in base["steps"]}
    if any(step_id not in by_id for step_id in setup_ids):
        raise journey.JourneyError("base M0 admission setup changed")
    mapping = {step_id: f"admission_{step_id}" for step_id in setup_ids}
    setup = []
    for step_id in setup_ids:
        step = _rewrite_setup_refs(copy.deepcopy(by_id[step_id]), mapping)
        step["id"] = mapping[step_id]
        # The admission project is an independent post-acceptance validation
        # surface. Keep it in the runner's final canonical phase so extending
        # the accepted M0 prefix cannot move the state machine backwards.
        step["phase"] = "acceptance"
        if "checkpoint" in step:
            step["checkpoint"] = f"collaboration_{step['checkpoint']}"
        setup.append(step)

    graph = next(step for step in setup if step["id"] == mapping["plan_work_graph"])
    graph["body"]["command"]["items"] = [
        _review_work_item(),
        _solo_work_item(),
        _directed_work_item(),
        _panel_work_item(),
    ]
    graph["assertions"] = [
        {"pointer": "/response/value/lifecycle_state", "equals": "planning"},
        *[
            {
                "pointer": f"/response/value/work_items/{work_item_id}/state",
                "equals": "ready",
            }
            for work_item_id in (
                "review-admission",
                "solo-admission",
                "directed-admission",
                "release-admission",
            )
        ],
    ]
    return setup


def build_plan(base: dict[str, Any]) -> dict[str, Any]:
    journey.validate_plan(base)
    if base.get("journey_id") != BASE_JOURNEY_ID or len(base.get("steps", [])) != 28:
        raise journey.JourneyError("base M0 journey contract changed")
    base_ids = [step.get("id") for step in base["steps"]]
    required_order = [
        "activate_project",
        "assign_designer",
        "execute_design",
        "observe_design_done",
        "assign_developer",
        "execute_source",
        "observe_source_done",
        "create_source_handoff",
    ]
    positions = [base_ids.index(step_id) for step_id in required_order]
    if positions != sorted(positions):
        raise journey.JourneyError("base M0 workbench order changed")

    plan = copy.deepcopy(base)
    plan["journey_id"] = COLLABORATION_JOURNEY_ID
    original_steps = copy.deepcopy(plan["steps"])
    setup = _admission_project_setup(base)
    project_id_ref = "admission_accept_proposal.project_id"

    independent_assignment = _assignment_step(
        step_id="assign_independent_reviewer",
        phase="acceptance",
        project_id_ref=project_id_ref,
        previous_version="admission_activate_project.project_version",
        work_item_id="review-admission",
        agent_id=55,
        reason_ref="independent-review-admission",
    )
    independent = _admission_steps(
        prefix="independent_review",
        phase="acceptance",
        project_id_ref=project_id_ref,
        previous_version="assign_independent_reviewer.project_version",
        work_item_id="review-admission",
        mode="parallel_independent_review",
        selected_agents=[55, 56],
        admission_index=0,
        expected_benefit_ref="required-independent-verification",
    )
    solo_assignment = _assignment_step(
        step_id="assign_solo_owner",
        phase="acceptance",
        project_id_ref=project_id_ref,
        previous_version="observe_independent_review_completed.project_version",
        work_item_id="solo-admission",
        agent_id=3,
        reason_ref="solo-admission-owner",
    )
    solo = _admission_steps(
        prefix="solo",
        phase="acceptance",
        project_id_ref=project_id_ref,
        previous_version="assign_solo_owner.project_version",
        work_item_id="solo-admission",
        mode="solo",
        selected_agents=[3],
        admission_index=1,
        expected_benefit_ref="strong-owner-baseline",
    )

    directed_assignment = _assignment_step(
        step_id="assign_directed_owner",
        phase="acceptance",
        project_id_ref=project_id_ref,
        previous_version="observe_solo_completed.project_version",
        work_item_id="directed-admission",
        agent_id=6,
        reason_ref="directed-admission-owner",
    )
    directed = _admission_steps(
        prefix="directed_handoff",
        phase="acceptance",
        project_id_ref=project_id_ref,
        previous_version="assign_directed_owner.project_version",
        work_item_id="directed-admission",
        mode="directed_handoff",
        selected_agents=[3, 6],
        admission_index=2,
        expected_benefit_ref="single-specialist-capability-transfer",
    )

    panel_assignment = _assignment_step(
        step_id="assign_release_panel_owner",
        phase="acceptance",
        project_id_ref=project_id_ref,
        previous_version="observe_directed_handoff_completed.project_version",
        work_item_id="release-admission",
        agent_id=6,
        reason_ref="technical-integration-panel-admission",
    )
    panel = _admission_steps(
        prefix="specialist_panel",
        phase="acceptance",
        project_id_ref=project_id_ref,
        previous_version="assign_release_panel_owner.project_version",
        work_item_id="release-admission",
        mode="specialist_panel",
        selected_agents=[3, 5, 6],
        admission_index=3,
        expected_benefit_ref="two-specialist-capability-coverage",
    )

    plan["steps"] = [
        *original_steps,
        *setup,
        independent_assignment,
        *independent,
        solo_assignment,
        *solo,
        directed_assignment,
        *directed,
        _observe_projection_boundary(project_id_ref),
        panel_assignment,
        *panel,
        _observe_final_projection(project_id_ref),
        _observe_final_admission_events(),
    ]
    journey.validate_plan(plan)
    return plan


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", type=Path, required=True)
    parser.add_argument("--output", required=True)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        base = journey.load_json(args.base, "base M0 journey")
        plan = build_plan(base)
        output = journey.safe_output_path(args.output, "collaboration journey")
        journey.atomic_json_write(output, plan)
    except journey.JourneyError as exc:
        print(f"collaboration journey build failed: {exc}", file=sys.stderr)
        return 1
    print(f"collaboration journey built: {plan['journey_id']} steps={len(plan['steps'])}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
