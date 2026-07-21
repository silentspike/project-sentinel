#!/usr/bin/env python3
"""Validate the M0 virtual-company product contract."""

from __future__ import annotations

import argparse
from collections import Counter
from pathlib import Path
import re
import sys
import tomllib
from typing import Any


REQUIRED_CATEGORIES = {
    "agreement",
    "artifact",
    "authority",
    "collaboration",
    "console",
    "contract",
    "cost",
    "customer",
    "delivery",
    "deployment",
    "gaia",
    "identity",
    "inference",
    "memory",
    "project",
    "qa",
    "recovery",
    "release",
    "runtime",
    "security",
    "stability",
    "tools",
    "workspace",
}

DELIVERY_ISSUES = {75, 472, 650, 693, 694, 695, 696, 698}
ALLOWED_STATUSES = {"blocked", "not_tested", "pass"}

REQUIRED_ROLES = {
    "designer",
    "developer",
    "gaia",
    "project_manager",
    "qa",
    "release_manager",
    "sales",
    "technical_lead",
}

REQUIRED_LIFECYCLE = {
    "customer_intake",
    "qualification",
    "proposal",
    "customer_agreement",
    "project_planning",
    "specialist_execution",
    "independent_qa",
    "release",
    "delivery",
    "customer_acceptance",
    "memory_closeout",
}

REQUIRED_ARTIFACTS = {
    "agreement",
    "customer_acceptance",
    "customer_brief",
    "delivery_receipt",
    "design_specification",
    "project_closeout_memory",
    "project_plan",
    "qa_report",
    "release_manifest",
    "source_tree",
}

REQUIRED_QUALITY_GATES = {
    "agreement_acceptance_criteria",
    "browser_smoke",
    "digest_provenance",
    "html_structure",
    "local_link_integrity",
    "static_security",
}

REQUIREMENT_FIELDS = {
    "category",
    "contract_section",
    "evidence",
    "id",
    "live_probe",
    "owner_issue",
    "status",
    "test_id",
    "title",
    "togaf_anchor",
}

ID_RE = re.compile(r"^M0-[A-Z][A-Z0-9]*-[0-9]{3}$")
TEST_ID_RE = re.compile(r"^m0(?:\.[a-z][a-z0-9_]*){2,}$")
PROBE_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
HEADING_RE = re.compile(r"^#{2,6}\s+(.+?)\s*$", re.MULTILINE)


def load_toml(path: Path, errors: list[str], label: str) -> dict[str, Any]:
    try:
        with path.open("rb") as handle:
            value = tomllib.load(handle)
    except FileNotFoundError:
        errors.append(f"{label}: missing file {path}")
        return {}
    except tomllib.TOMLDecodeError as exc:
        errors.append(f"{label}: invalid TOML in {path}: {exc}")
        return {}

    if not isinstance(value, dict):
        errors.append(f"{label}: expected a TOML table in {path}")
        return {}
    return value


def relative_path(
    repo_root: Path, value: object, field: str, errors: list[str]
) -> Path | None:
    if not isinstance(value, str) or not value.strip():
        errors.append(f"{field}: expected a non-empty repository-relative path")
        return None

    path = Path(value)
    if path.is_absolute() or ".." in path.parts:
        errors.append(f"{field}: path must remain inside the repository: {value!r}")
        return None
    candidate = repo_root / path
    if not candidate.resolve(strict=False).is_relative_to(repo_root.resolve()):
        errors.append(f"{field}: resolved path leaves the repository: {value!r}")
        return None
    return candidate


def duplicate_values(values: list[str]) -> set[str]:
    counts = Counter(values)
    return {value for value, count in counts.items() if count > 1}


def table_ids(
    value: object, field: str, errors: list[str], *, allow_external_customer: bool = False
) -> set[str]:
    if not isinstance(value, list) or not value:
        errors.append(f"profile.{field}: expected a non-empty array of tables")
        return set()

    ids: list[str] = []
    for index, item in enumerate(value):
        if not isinstance(item, dict):
            errors.append(f"profile.{field}[{index}]: expected a table")
            continue
        item_id = item.get("id") or item.get("kind")
        if not isinstance(item_id, str) or not item_id:
            errors.append(f"profile.{field}[{index}]: missing id/kind")
            continue
        if item_id == "customer" and not allow_external_customer:
            errors.append(f"profile.{field}[{index}]: customer is not an internal role")
        ids.append(item_id)

    for item_id in sorted(duplicate_values(ids)):
        errors.append(f"profile.{field}: duplicate id {item_id!r}")
    return set(ids)


def validate_profile(profile: dict[str, Any], errors: list[str]) -> None:
    expected_scalars = {
        "schema_version": 1,
        "id": "web-project-v1",
        "runtime_mode": "single_node",
        "cluster_required": False,
        "external_production_deploy": False,
    }
    for field, expected in expected_scalars.items():
        actual = profile.get(field)
        if actual != expected:
            errors.append(
                f"profile.{field}: expected {expected!r}, found {actual!r}"
            )

    lifecycle = profile.get("lifecycle")
    lifecycle_set = set(lifecycle) if isinstance(lifecycle, list) else set()
    missing_lifecycle = REQUIRED_LIFECYCLE - lifecycle_set
    if missing_lifecycle:
        errors.append(
            "profile.lifecycle: missing " + ", ".join(sorted(missing_lifecycle))
        )
    if isinstance(lifecycle, list) and duplicate_values(lifecycle):
        errors.append("profile.lifecycle: duplicate states are not allowed")

    roles = table_ids(profile.get("roles"), "roles", errors)
    missing_roles = REQUIRED_ROLES - roles
    if missing_roles:
        errors.append("profile.roles: missing " + ", ".join(sorted(missing_roles)))
    role_tables = profile.get("roles")
    if isinstance(role_tables, list):
        for index, item in enumerate(role_tables):
            if not isinstance(item, dict):
                continue
            for field in ("authority", "required_capabilities"):
                value = item.get(field)
                if not isinstance(value, list) or not value or not all(
                    isinstance(entry, str) and entry for entry in value
                ):
                    errors.append(
                        f"profile.roles[{index}].{field}: expected non-empty strings"
                    )

    artifacts = table_ids(profile.get("required_artifacts"), "required_artifacts", errors)
    missing_artifacts = REQUIRED_ARTIFACTS - artifacts
    if missing_artifacts:
        errors.append(
            "profile.required_artifacts: missing "
            + ", ".join(sorted(missing_artifacts))
        )
    artifact_tables = profile.get("required_artifacts")
    if isinstance(artifact_tables, list):
        for index, item in enumerate(artifact_tables):
            if not isinstance(item, dict):
                continue
            producer = item.get("producer_role")
            if producer not in roles | {"customer"}:
                errors.append(
                    f"profile.required_artifacts[{index}].producer_role: "
                    f"unknown role {producer!r}"
                )
            if item.get("immutable") is not True:
                errors.append(
                    f"profile.required_artifacts[{index}].immutable: expected true"
                )

    tool_profiles = profile.get("tool_profiles")
    tool_ids = table_ids(tool_profiles, "tool_profiles", errors)
    expected_tool_ids = {"web-authoring-v1", "web-qa-v1", "web-release-v1"}
    if expected_tool_ids - tool_ids:
        errors.append(
            "profile.tool_profiles: missing "
            + ", ".join(sorted(expected_tool_ids - tool_ids))
        )
    if isinstance(tool_profiles, list):
        for index, item in enumerate(tool_profiles):
            if not isinstance(item, dict):
                continue
            item_roles = item.get("roles")
            if not isinstance(item_roles, list) or not item_roles:
                errors.append(f"profile.tool_profiles[{index}]: roles must be non-empty")
            elif set(item_roles) - roles:
                errors.append(
                    f"profile.tool_profiles[{index}]: references unknown roles "
                    + ", ".join(sorted(set(item_roles) - roles))
                )
            tools = item.get("tools")
            if not isinstance(tools, list) or not tools:
                errors.append(f"profile.tool_profiles[{index}]: tools must be non-empty")

    gates = table_ids(profile.get("quality_gates"), "quality_gates", errors)
    missing_gates = REQUIRED_QUALITY_GATES - gates
    if missing_gates:
        errors.append(
            "profile.quality_gates: missing " + ", ".join(sorted(missing_gates))
        )
    gate_tables = profile.get("quality_gates")
    if isinstance(gate_tables, list):
        for index, item in enumerate(gate_tables):
            if not isinstance(item, dict):
                continue
            if item.get("runner") not in tool_ids:
                errors.append(
                    f"profile.quality_gates[{index}].runner: unknown tool profile "
                    f"{item.get('runner')!r}"
                )
            if item.get("required") is not True:
                errors.append(f"profile.quality_gates[{index}].required: expected true")

    runtime = profile.get("runtime")
    if not isinstance(runtime, dict):
        errors.append("profile.runtime: expected a table")
    else:
        if runtime.get("tool_runtime") != "bwrap":
            errors.append("profile.runtime.tool_runtime: expected 'bwrap'")
        if runtime.get("runtime_registry_required") is not True:
            errors.append("profile.runtime.runtime_registry_required: expected true")
        if runtime.get("allow_secure_runtime_fallback") is not False:
            errors.append("profile.runtime.allow_secure_runtime_fallback: expected false")

    security = profile.get("security")
    if not isinstance(security, dict):
        errors.append("profile.security: expected a table")
    else:
        if security.get("network_default") != "deny":
            errors.append("profile.security.network_default: expected 'deny'")
        required_true = {
            "cgroup_limits_required",
            "complete_process_tree_cancellation",
            "environment_allowlist_required",
            "landlock_required",
            "linux_capability_policy_required",
        }
        required_false = {
            "cross_project_workspace_access",
            "host_filesystem_visible",
            "secret_paths_visible",
        }
        for field in sorted(required_true):
            if security.get(field) is not True:
                errors.append(f"profile.security.{field}: expected true")
        for field in sorted(required_false):
            if security.get(field) is not False:
                errors.append(f"profile.security.{field}: expected false")

    expected_bools = {
        ("customer_contract", "explicit_acceptance_required"): True,
        ("customer_contract", "proposal_digest_binding"): True,
        ("project", "evidence_backed_completion"): True,
        ("project", "chat_is_authoritative"): False,
        ("recovery", "durable_request_reservation"): True,
        ("recovery", "outcome_probe_before_retry"): True,
        ("recovery", "outbox_required"): True,
        ("memory", "closeout_after_customer_acceptance"): True,
        ("memory", "source_provenance_required"): True,
        ("memory", "derived_memory_is_not_authority"): True,
        ("memory", "nightrun_direct_workflow_mutation"): False,
        ("acceptance", "explicit_customer_action"): True,
        ("acceptance", "rollback_rehearsal_required"): True,
        ("acceptance", "issue_specific_vm_snapshot"): True,
    }
    for (table_name, field), expected in expected_bools.items():
        table = profile.get(table_name)
        actual = table.get(field) if isinstance(table, dict) else None
        if actual is not expected:
            errors.append(
                f"profile.{table_name}.{field}: expected {expected!r}, found {actual!r}"
            )


def validate_requirement(
    requirement: object,
    index: int,
    repo_root: Path,
    contract_headings: set[str],
    delivery_issues: set[int],
    errors: list[str],
) -> tuple[str | None, str | None, str | None]:
    prefix = f"requirements[{index}]"
    if not isinstance(requirement, dict):
        errors.append(f"{prefix}: expected a table")
        return None, None, None

    missing = REQUIREMENT_FIELDS - requirement.keys()
    if missing:
        errors.append(f"{prefix}: missing fields {', '.join(sorted(missing))}")

    requirement_id = requirement.get("id")
    if not isinstance(requirement_id, str) or not ID_RE.fullmatch(requirement_id):
        errors.append(f"{prefix}.id: invalid M0 requirement ID {requirement_id!r}")
        requirement_id = None

    category = requirement.get("category")
    if not isinstance(category, str) or category not in REQUIRED_CATEGORIES:
        errors.append(f"{prefix}.category: unknown category {category!r}")
        category = None
    elif isinstance(requirement_id, str):
        id_category = requirement_id.split("-", 2)[1].lower()
        if id_category != category:
            errors.append(
                f"{prefix}: ID category {id_category!r} does not match {category!r}"
            )

    for field in ("title", "togaf_anchor"):
        value = requirement.get(field)
        if not isinstance(value, str) or not value.strip():
            errors.append(f"{prefix}.{field}: expected non-empty text")

    section = requirement.get("contract_section")
    if not isinstance(section, str) or section not in contract_headings:
        errors.append(f"{prefix}.contract_section: unknown heading {section!r}")

    owner = requirement.get("owner_issue")
    if not isinstance(owner, int) or isinstance(owner, bool) or owner <= 0:
        errors.append(f"{prefix}.owner_issue: expected a positive issue number")
    elif owner not in delivery_issues:
        errors.append(f"{prefix}.owner_issue: #{owner} is not in delivery_issues")

    test_id = requirement.get("test_id")
    if not isinstance(test_id, str) or not TEST_ID_RE.fullmatch(test_id):
        errors.append(f"{prefix}.test_id: invalid stable test ID {test_id!r}")
        test_id = None

    probe = requirement.get("live_probe")
    if not isinstance(probe, str) or not PROBE_RE.fullmatch(probe):
        errors.append(f"{prefix}.live_probe: invalid probe slug {probe!r}")
        probe = None

    evidence = relative_path(repo_root, requirement.get("evidence"), f"{prefix}.evidence", errors)
    if evidence is not None:
        evidence_rel = evidence.relative_to(repo_root)
        expected_prefix = Path("console/evidence")
        if not evidence_rel.is_relative_to(expected_prefix):
            errors.append(f"{prefix}.evidence: must be under console/evidence")

    status = requirement.get("status")
    if status not in ALLOWED_STATUSES:
        errors.append(f"{prefix}.status: unknown status {status!r}")
    elif status == "blocked":
        blocker = requirement.get("blocker")
        blocked_by = requirement.get("blocked_by")
        if not isinstance(blocker, str) or not blocker.strip():
            errors.append(f"{prefix}.blocker: blocked requirements need a reason")
        if not isinstance(blocked_by, list) or not blocked_by:
            errors.append(f"{prefix}.blocked_by: blocked requirements need issue IDs")
        elif any(
            not isinstance(issue, int) or isinstance(issue, bool) or issue not in delivery_issues
            for issue in blocked_by
        ):
            errors.append(f"{prefix}.blocked_by: contains an unknown delivery issue")
    elif status == "pass" and evidence is not None:
        if not evidence.is_file() or evidence.stat().st_size == 0:
            errors.append(f"{prefix}.evidence: pass requires a non-empty evidence file")
        else:
            text = evidence.read_text(encoding="utf-8")
            if re.search(r"\bNOT[ _-]?TESTED\b", text, re.IGNORECASE):
                errors.append(f"{prefix}.evidence: pass evidence still says NOT TESTED")

    return requirement_id, test_id, probe


def validate(repo_root: Path, matrix_path: Path | None = None) -> list[str]:
    repo_root = repo_root.resolve()
    errors: list[str] = []
    if matrix_path is None:
        matrix_path = repo_root / "scripts/product-acceptance/m0-contract.toml"
    elif not matrix_path.is_absolute():
        matrix_path = repo_root / matrix_path

    matrix = load_toml(matrix_path, errors, "matrix")
    if not matrix:
        return errors

    if matrix.get("schema_version") != 1:
        errors.append("matrix.schema_version: expected 1")
    if matrix.get("profile") != "web-project-v1":
        errors.append("matrix.profile: expected 'web-project-v1'")
    if matrix.get("epic") != 650:
        errors.append("matrix.epic: expected 650")

    configured_issues = matrix.get("delivery_issues")
    if (
        not isinstance(configured_issues, list)
        or set(configured_issues) != DELIVERY_ISSUES
        or len(configured_issues) != len(DELIVERY_ISSUES)
    ):
        errors.append(
            "matrix.delivery_issues: expected exactly "
            + ", ".join(str(issue) for issue in sorted(DELIVERY_ISSUES))
        )
        delivery_issues = DELIVERY_ISSUES
    else:
        delivery_issues = set(configured_issues)

    configured_categories = matrix.get("required_categories")
    if (
        not isinstance(configured_categories, list)
        or set(configured_categories) != REQUIRED_CATEGORIES
        or len(configured_categories) != len(REQUIRED_CATEGORIES)
    ):
        errors.append(
            "matrix.required_categories: expected exactly "
            + ", ".join(sorted(REQUIRED_CATEGORIES))
        )

    profile_path = relative_path(
        repo_root, matrix.get("profile_path"), "matrix.profile_path", errors
    )
    contract_path = relative_path(
        repo_root, matrix.get("contract_path"), "matrix.contract_path", errors
    )

    profile = load_toml(profile_path, errors, "profile") if profile_path else {}
    if profile:
        validate_profile(profile, errors)
        if profile.get("id") != matrix.get("profile"):
            errors.append("matrix.profile does not match profile.id")

    contract_headings: set[str] = set()
    if contract_path is not None:
        try:
            contract_text = contract_path.read_text(encoding="utf-8")
        except FileNotFoundError:
            errors.append(f"contract: missing file {contract_path}")
        else:
            contract_headings = set(HEADING_RE.findall(contract_text))

    requirements = matrix.get("requirements")
    if not isinstance(requirements, list) or not requirements:
        errors.append("matrix.requirements: expected a non-empty array of tables")
        return errors

    requirement_ids: list[str] = []
    test_ids: list[str] = []
    probes: list[str] = []
    categories: list[str] = []
    for index, requirement in enumerate(requirements):
        requirement_id, test_id, probe = validate_requirement(
            requirement,
            index,
            repo_root,
            contract_headings,
            delivery_issues,
            errors,
        )
        if requirement_id:
            requirement_ids.append(requirement_id)
        if test_id:
            test_ids.append(test_id)
        if probe:
            probes.append(probe)
        if isinstance(requirement, dict) and isinstance(requirement.get("category"), str):
            categories.append(requirement["category"])

    for label, values in (
        ("requirement ID", requirement_ids),
        ("test ID", test_ids),
        ("live probe", probes),
    ):
        for value in sorted(duplicate_values(values)):
            errors.append(f"matrix.requirements: duplicate {label} {value!r}")

    missing_categories = REQUIRED_CATEGORIES - set(categories)
    if missing_categories:
        errors.append(
            "matrix.requirements: missing categories "
            + ", ".join(sorted(missing_categories))
        )

    return errors


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="validate the committed contract (the default operation)",
    )
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path(__file__).resolve().parents[2],
        help="repository root (used by tests)",
    )
    parser.add_argument(
        "--matrix",
        type=Path,
        default=None,
        help="matrix path, relative to the repository root unless absolute",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    errors = validate(args.repo_root, args.matrix)
    if errors:
        print(f"M0 contract validation failed with {len(errors)} error(s):")
        for error in errors:
            print(f"- {error}")
        return 1

    matrix_path = args.matrix or Path("scripts/product-acceptance/m0-contract.toml")
    print(f"M0 contract validation passed: {matrix_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
