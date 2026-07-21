from __future__ import annotations

from pathlib import Path
import shutil
import tempfile
import unittest

import check_contract


SOURCE_ROOT = Path(__file__).resolve().parents[2]
MATRIX_REL = Path("scripts/product-acceptance/m0-contract.toml")
PROFILE_REL = Path("config/work-profiles/web-project-v1.toml")
CONTRACT_REL = Path("docs/virtual-company-work-execution.md")
EVIDENCE_REL = Path("console/evidence/issue-693-live/contract-gate.md")


class ContractValidatorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        for relative in (MATRIX_REL, PROFILE_REL, CONTRACT_REL, EVIDENCE_REL):
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(SOURCE_ROOT / relative, destination)

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def validate(self) -> list[str]:
        return check_contract.validate(self.root, MATRIX_REL)

    def replace(self, relative: Path, old: str, new: str, count: int = 1) -> None:
        path = self.root / relative
        text = path.read_text(encoding="utf-8")
        self.assertIn(old, text)
        path.write_text(text.replace(old, new, count), encoding="utf-8")

    def assert_error(self, errors: list[str], fragment: str) -> None:
        self.assertTrue(
            any(fragment in error for error in errors),
            f"expected {fragment!r} in errors: {errors}",
        )

    def test_committed_contract_is_valid(self) -> None:
        self.assertEqual(self.validate(), [])

    def test_duplicate_requirement_id_is_rejected(self) -> None:
        self.replace(
            MATRIX_REL,
            'id = "M0-CONTRACT-002"',
            'id = "M0-CONTRACT-001"',
        )
        self.assert_error(self.validate(), "duplicate requirement ID")

    def test_duplicate_required_category_is_rejected(self) -> None:
        self.replace(MATRIX_REL, '  "contract",', '  "contract",\n  "contract",')
        self.assert_error(self.validate(), "matrix.required_categories")

    def test_duplicate_delivery_issue_is_rejected(self) -> None:
        self.replace(
            MATRIX_REL,
            "delivery_issues = [75, 472, 650, 693, 694, 695, 696, 698]",
            "delivery_issues = [75, 75, 472, 650, 693, 694, 695, 696, 698]",
        )
        self.assert_error(self.validate(), "matrix.delivery_issues")

    def test_requirement_id_category_mismatch_is_rejected(self) -> None:
        self.replace(MATRIX_REL, 'id = "M0-CONTRACT-001"', 'id = "M0-TOOLS-099"')
        self.assert_error(self.validate(), "ID category 'tools' does not match 'contract'")

    def test_missing_required_field_is_rejected(self) -> None:
        self.replace(MATRIX_REL, 'test_id = "m0.contract.document"\n', "")
        self.assert_error(self.validate(), "missing fields test_id")

    def test_missing_requirement_category_is_rejected(self) -> None:
        self.replace(MATRIX_REL, 'category = "gaia"', 'category = "memory"')
        self.assert_error(self.validate(), "missing categories gaia")

    def test_unknown_owner_issue_is_rejected(self) -> None:
        self.replace(MATRIX_REL, "owner_issue = 693", "owner_issue = 999")
        self.assert_error(self.validate(), "#999 is not in delivery_issues")

    def test_unknown_status_is_rejected(self) -> None:
        self.replace(MATRIX_REL, 'status = "not_tested"', 'status = "done"')
        self.assert_error(self.validate(), "unknown status 'done'")

    def test_pass_requires_existing_evidence(self) -> None:
        self.replace(MATRIX_REL, 'status = "not_tested"', 'status = "pass"')
        self.assert_error(self.validate(), "pass requires a non-empty evidence file")

    def test_pass_accepts_nonempty_evidence_without_not_tested_marker(self) -> None:
        self.replace(MATRIX_REL, 'status = "not_tested"', 'status = "pass"')
        evidence = self.root / "console/evidence/issue-650-live/ac-01-readiness.md"
        evidence.parent.mkdir(parents=True, exist_ok=True)
        evidence.write_text("Contract validator evidence: PASS\n", encoding="utf-8")
        self.assertEqual(self.validate(), [])

    def test_blocked_requirement_needs_reason(self) -> None:
        self.replace(
            MATRIX_REL,
            'blocker = "Issue #472 is open and its production-daemon selection path is not yet verified for M0."\n',
            "",
        )
        self.assert_error(self.validate(), "blocked requirements need a reason")

    def test_unknown_contract_heading_is_rejected(self) -> None:
        self.replace(
            CONTRACT_REL,
            "## Purpose and Claim Boundary",
            "## Renamed Purpose",
        )
        self.assert_error(self.validate(), "unknown heading 'Purpose and Claim Boundary'")

    def test_profile_cannot_require_cluster(self) -> None:
        self.replace(PROFILE_REL, "cluster_required = false", "cluster_required = true")
        self.assert_error(self.validate(), "profile.cluster_required: expected False")

    def test_profile_role_needs_capabilities(self) -> None:
        self.replace(
            PROFILE_REL,
            'required_capabilities = ["customer_intake", "scope_analysis"]',
            "required_capabilities = []",
        )
        self.assert_error(self.validate(), "required_capabilities: expected non-empty")

    def test_quality_gate_runner_must_exist(self) -> None:
        self.replace(
            PROFILE_REL,
            'runner = "web-qa-v1"',
            'runner = "missing-runner"',
        )
        self.assert_error(self.validate(), "unknown tool profile 'missing-runner'")

    def test_evidence_path_cannot_escape_repository(self) -> None:
        self.replace(
            MATRIX_REL,
            'evidence = "console/evidence/issue-693-live/contract-gate.md"',
            'evidence = "../private/contract-gate.md"',
        )
        self.assert_error(self.validate(), "path must remain inside the repository")

    def test_evidence_symlink_cannot_escape_repository(self) -> None:
        outside = self.root.parent / f"{self.root.name}-outside"
        outside.mkdir(exist_ok=True)
        link = self.root / "console/evidence/escape"
        link.parent.mkdir(parents=True, exist_ok=True)
        link.symlink_to(outside, target_is_directory=True)
        self.addCleanup(shutil.rmtree, outside, True)
        self.replace(
            MATRIX_REL,
            'evidence = "console/evidence/issue-650-live/ac-01-readiness.md"',
            'evidence = "console/evidence/escape/readiness.md"',
        )
        self.assert_error(self.validate(), "resolved path leaves the repository")


if __name__ == "__main__":
    unittest.main()
