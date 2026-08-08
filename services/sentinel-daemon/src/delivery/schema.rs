use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::digest::ContentDigest;

pub const DELIVERY_SCHEMA_V1: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionedRefV1 {
    pub id: String,
    pub generation: u64,
    pub digest: ContentDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityRole {
    Customer,
    Developer,
    Qa,
    ReleaseManager,
    Auditor,
    GaiaObserver,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalV1 {
    pub tenant_id: String,
    pub principal_id: String,
    pub authority_generation: u64,
    pub roles: BTreeSet<AuthorityRole>,
}

impl PrincipalV1 {
    pub fn has_role(&self, role: AuthorityRole) -> bool {
        self.roles.contains(&role)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataControlV1 {
    pub classification: String,
    pub encryption_key_owner: String,
    pub access_policy_digest: ContentDigest,
    pub redaction_policy_digest: ContentDigest,
    pub retention_frontier: VersionedRefV1,
    pub audit_policy_digest: ContentDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceTupleV1 {
    pub owner: String,
    pub source_type: String,
    pub id: String,
    pub generation: u64,
    pub digest: ContentDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRefV1 {
    pub artifact_id: String,
    pub generation: u64,
    pub digest: ContentDigest,
    pub media_type: String,
    pub owner_principal_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostRefV1 {
    pub ledger_id: String,
    pub generation: u64,
    pub digest: ContentDigest,
    pub currency: String,
    pub amount_minor: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetSplit {
    Development,
    Calibration,
    HiddenHoldout,
    AdversarialCanary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaEvaluationPlanV1 {
    pub schema_version: u16,
    pub plan_id: String,
    pub generation: u64,
    pub request: VersionedRefV1,
    pub candidate: VersionedRefV1,
    pub agreement: VersionedRefV1,
    pub project: VersionedRefV1,
    pub work_items_digest: ContentDigest,
    pub acceptance_criteria_digest: ContentDigest,
    pub required_case_ids: BTreeSet<String>,
    pub optional_case_ids: BTreeSet<String>,
    pub fixture_inventory_digest: ContentDigest,
    pub evaluator_policy_digest: ContentDigest,
    pub aggregation_policy_digest: ContentDigest,
    pub release_policy_digest: ContentDigest,
    pub runner_binary_digest: ContentDigest,
    pub toolchain_digest: ContentDigest,
    pub sandbox_profile_digest: ContentDigest,
    pub capability_digest: ContentDigest,
    pub environment_digest: ContentDigest,
    pub credential_policy_digest: ContentDigest,
    pub declared_seeds: BTreeSet<u64>,
    pub retry_limit: u16,
    pub retryable_classes: BTreeSet<String>,
    pub data_control: DataControlV1,
    pub plan_digest: ContentDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaDatasetCaseV1 {
    pub schema_version: u16,
    pub case_id: String,
    pub generation: u64,
    pub split: DatasetSplit,
    pub required: bool,
    pub required_class: String,
    pub slices: BTreeMap<String, String>,
    pub input_digest: ContentDigest,
    pub oracle_digest: ContentDigest,
    pub provenance: Vec<SourceTupleV1>,
    pub license: String,
    pub access_policy_digest: ContentDigest,
    pub contamination_policy_digest: ContentDigest,
    pub retired_at_ms: Option<u64>,
    pub superseded_by: Option<VersionedRefV1>,
    pub data_control: DataControlV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QaRunState {
    Planned,
    Admitted,
    Running,
    NeedsHumanReview,
    CompletedPass,
    CompletedFail,
    HarnessError,
    Cancelled,
    Superseded,
    Quarantined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QaHarnessOutcome {
    Pass,
    Fail,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaAggregateOutcomesV1 {
    pub required_cases_complete: bool,
    pub contaminated: bool,
    pub needs_human_review: bool,
    pub flaky_unresolved: bool,
}

impl QaRunState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::CompletedPass
                | Self::CompletedFail
                | Self::HarnessError
                | Self::Cancelled
                | Self::Superseded
                | Self::Quarantined
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaEvaluationRunReceiptV1 {
    pub schema_version: u16,
    pub run_id: String,
    pub generation: u64,
    pub plan: VersionedRefV1,
    pub request_digest: ContentDigest,
    pub state: QaRunState,
    pub retry_of: Option<VersionedRefV1>,
    pub supersedes: Option<VersionedRefV1>,
    pub actors: Vec<PrincipalV1>,
    pub durable_event_generation: u64,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub attempts: u16,
    pub harness_outcome: Option<QaHarnessOutcome>,
    pub cleanup_receipt: Option<VersionedRefV1>,
    pub aggregate_outcomes: Option<QaAggregateOutcomesV1>,
    pub gate_receipt: Option<VersionedRefV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QaCaseOutcome {
    Pass,
    Fail,
    Error,
    Unscored,
    Skipped,
    NeedsHumanReview,
    FlakyUnresolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QaCaseReasonCode {
    Verified,
    AssertionFailed,
    HarnessError,
    ModelRejected,
    SkippedByPolicy,
    NeedsHumanReview,
    FlakyUnresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaCaseResultV1 {
    pub schema_version: u16,
    pub result_id: String,
    pub generation: u64,
    pub run: VersionedRefV1,
    pub case_ref: VersionedRefV1,
    pub outcome: QaCaseOutcome,
    pub required: bool,
    pub reason_code: QaCaseReasonCode,
    pub sources: Vec<SourceTupleV1>,
    pub assertion_refs: Vec<VersionedRefV1>,
    pub grader_refs: Vec<VersionedRefV1>,
    pub slices: BTreeMap<String, String>,
    pub attempts: u16,
    pub disposition: Option<VersionedRefV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaDeterministicAssertionResultV1 {
    pub schema_version: u16,
    pub assertion_id: String,
    pub generation: u64,
    pub plan_digest: ContentDigest,
    pub case_digest: ContentDigest,
    pub assertion_digest: ContentDigest,
    pub oracle_digest: ContentDigest,
    pub input_digest: ContentDigest,
    pub evidence_digest: ContentDigest,
    pub actual_digest: ContentDigest,
    pub passed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaModelGradeEvidenceV1 {
    pub schema_version: u16,
    pub evidence_id: String,
    pub generation: u64,
    pub provider_endpoint_class: String,
    pub api_version: String,
    pub requested_model_id: String,
    pub reported_model_id: Option<String>,
    pub model_fingerprint: Option<String>,
    pub model_identity_status: QaModelIdentityStatus,
    pub model_family: String,
    pub model_version: String,
    pub system_digest: ContentDigest,
    pub rubric_digest: ContentDigest,
    pub prompt_digest: ContentDigest,
    pub response_schema_digest: ContentDigest,
    pub sampling_parameters: BTreeMap<String, String>,
    pub seed_supported: bool,
    pub request_id: String,
    pub response_id: Option<String>,
    pub raw_output_digest: ContentDigest,
    pub parse_outcome: QaModelParseOutcome,
    pub verdict: QaModelVerdict,
    pub attempts: u16,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost: Option<CostRefV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QaModelIdentityStatus {
    Verified,
    Unverified,
    Mismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QaModelParseOutcome {
    Valid,
    InvalidSchema,
    Truncated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QaModelVerdict {
    Pass,
    Fail,
    NeedsHumanReview,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QaFlakeClassification {
    Infrastructure,
    ModelVariance,
    TestHarness,
    ProductDefect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QaFlakeReason {
    RetryPassed,
    KnownInfrastructure,
    EvaluatorVariance,
    Unresolved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaFlakeDispositionV1 {
    pub schema_version: u16,
    pub disposition_id: String,
    pub generation: u64,
    pub result: VersionedRefV1,
    pub owner: PrincipalV1,
    pub classification: QaFlakeClassification,
    pub reason: QaFlakeReason,
    pub policy_revision: u64,
    pub expires_at_ms: u64,
    pub defect_ref: VersionedRefV1,
    pub deterministic_regression_fixture: VersionedRefV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaEvidenceGraphV1 {
    pub schema_version: u16,
    pub run: VersionedRefV1,
    pub workbench_receipt: VersionedRefV1,
    pub dataset_cases: Vec<QaDatasetCaseV1>,
    pub case_results: Vec<QaCaseResultV1>,
    pub deterministic_results: Vec<QaDeterministicAssertionResultV1>,
    pub model_results: Vec<QaModelGradeEvidenceV1>,
    pub flake_dispositions: Vec<QaFlakeDispositionV1>,
    pub graph_digest: ContentDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QaReleaseGateReceiptV1 {
    pub schema_version: u16,
    pub gate_id: String,
    pub generation: u64,
    pub candidate: VersionedRefV1,
    pub plan: VersionedRefV1,
    pub case_inventory_digest: ContentDigest,
    pub deterministic_evidence_digest: ContentDigest,
    pub model_evidence_digest: Option<ContentDigest>,
    pub calibration_digest: Option<ContentDigest>,
    pub source_evidence_digest: ContentDigest,
    pub flake_disposition_digest: Option<ContentDigest>,
    pub policy_digest: ContentDigest,
    pub release_manifest_digest: ContentDigest,
    pub actor: PrincipalV1,
    pub passed: bool,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateState {
    Draft,
    QaAssigned,
    QaRunning,
    GatePassed,
    GateFailed,
    Promoted,
    Superseded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseCandidateV1 {
    pub schema_version: u16,
    pub candidate_id: String,
    pub generation: u64,
    pub tenant_id: String,
    pub agreement: VersionedRefV1,
    pub project: VersionedRefV1,
    pub work_items_digest: ContentDigest,
    pub source_digest: ContentDigest,
    pub artifacts: Vec<ArtifactRefV1>,
    pub toolchain_digest: ContentDigest,
    pub runtime_profile_digest: ContentDigest,
    pub acceptance_criteria_digest: ContentDigest,
    pub implementer_principal_ids: BTreeSet<String>,
    pub cost: CostRefV1,
    pub state: CandidateState,
    pub candidate_digest: ContentDigest,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewV1 {
    pub schema_version: u16,
    pub review_id: String,
    pub generation: u64,
    pub candidate: VersionedRefV1,
    pub reviewer: PrincipalV1,
    pub findings_digest: ContentDigest,
    pub approved: bool,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestRunV1 {
    pub schema_version: u16,
    pub test_run_id: String,
    pub generation: u64,
    pub candidate: VersionedRefV1,
    pub qa_plan: VersionedRefV1,
    pub runner_receipt: VersionedRefV1,
    pub result_inventory_digest: ContentDigest,
    pub logs_digest: ContentDigest,
    pub screenshots_digest: Option<ContentDigest>,
    pub passed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingClassification {
    Correctness,
    Security,
    Reliability,
    Performance,
    Accessibility,
    Compliance,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingV1 {
    pub schema_version: u16,
    pub finding_id: String,
    pub generation: u64,
    pub candidate: VersionedRefV1,
    pub severity: FindingSeverity,
    pub classification: FindingClassification,
    pub evidence: Vec<SourceTupleV1>,
    pub resolved_by: Option<VersionedRefV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalV1 {
    pub schema_version: u16,
    pub approval_id: String,
    pub generation: u64,
    pub candidate: VersionedRefV1,
    pub gate: VersionedRefV1,
    pub approver: PrincipalV1,
    pub policy_digest: ContentDigest,
    pub approved_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifestV1 {
    pub schema_version: u16,
    pub manifest_id: String,
    pub generation: u64,
    pub tenant_id: String,
    pub agreement: VersionedRefV1,
    pub project: VersionedRefV1,
    pub candidate: VersionedRefV1,
    pub work_items_digest: ContentDigest,
    pub source_digest: ContentDigest,
    pub artifacts: Vec<ArtifactRefV1>,
    pub toolchain_digest: ContentDigest,
    pub runtime_profile_digest: ContentDigest,
    pub qa_gate: VersionedRefV1,
    pub qa_evidence_digest: ContentDigest,
    pub sbom_digest: ContentDigest,
    pub dependency_snapshot_digest: ContentDigest,
    pub provenance_digest: ContentDigest,
    pub release_actor: PrincipalV1,
    pub cost: CostRefV1,
    pub rollback_release: Option<VersionedRefV1>,
    pub manifest_digest: ContentDigest,
    pub created_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseState {
    Approved,
    Active,
    RolledBack,
    Superseded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseV1 {
    pub schema_version: u16,
    pub release_id: String,
    pub generation: u64,
    pub manifest: VersionedRefV1,
    pub state: ReleaseState,
    pub activated_at_ms: Option<u64>,
    pub rollout_receipt: Option<VersionedRefV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    PreviewReady,
    Delivered,
    Accepted,
    Rejected,
    ChangesRequested,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryReceiptV1 {
    pub schema_version: u16,
    pub delivery_id: String,
    pub generation: u64,
    pub tenant_id: String,
    pub release: VersionedRefV1,
    pub customer_principal_id: String,
    pub preview_digest: ContentDigest,
    pub receipt_digest: ContentDigest,
    pub state: DeliveryState,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomerAction {
    Accept,
    Reject,
    RequestChanges,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomerFeedbackV1 {
    pub schema_version: u16,
    pub feedback_id: String,
    pub generation: u64,
    pub delivery: VersionedRefV1,
    pub customer: PrincipalV1,
    pub action: CustomerAction,
    pub feedback_digest: ContentDigest,
    pub requested_work_item_refs: Vec<VersionedRefV1>,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceV1 {
    pub schema_version: u16,
    pub acceptance_id: String,
    pub generation: u64,
    pub delivery: VersionedRefV1,
    pub release: VersionedRefV1,
    pub customer: PrincipalV1,
    pub acceptance_digest: ContentDigest,
    pub accepted_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RollbackV1 {
    pub schema_version: u16,
    pub rollback_id: String,
    pub generation: u64,
    pub from_release: VersionedRefV1,
    pub to_release: VersionedRefV1,
    pub actor: PrincipalV1,
    pub reason_digest: ContentDigest,
    pub effect_receipt: Option<VersionedRefV1>,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCloseoutV1 {
    pub schema_version: u16,
    pub closeout_id: String,
    pub generation: u64,
    pub project: VersionedRefV1,
    pub accepted_release: VersionedRefV1,
    pub acceptance: VersionedRefV1,
    pub decisions_digest: ContentDigest,
    pub artifact_inventory_digest: ContentDigest,
    pub failures_digest: ContentDigest,
    pub lessons_digest: ContentDigest,
    pub memory_publication: Option<VersionedRefV1>,
    pub closed_by: PrincipalV1,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedReleasePublicationV1 {
    pub schema_version: u16,
    pub release: VersionedRefV1,
    pub product_commit: String,
    pub role_indexed_artifacts: BTreeMap<String, ArtifactRefV1>,
    pub sbom_digest: ContentDigest,
    pub provenance_digest: ContentDigest,
    pub compatibility_profile_digest: ContentDigest,
    pub signer_key_id: String,
    pub authority_generation: u64,
    pub signature: String,
}

impl QaEvaluationPlanV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, super::error::DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.plan_digest = ContentDigest::zero();
        ContentDigest::of_domain("qa-evaluation-plan", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, super::error::DeliveryError> {
        self.plan_digest = self.computed_digest()?;
        Ok(self)
    }
}

impl ReleaseCandidateV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, super::error::DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.candidate_digest = ContentDigest::zero();
        ContentDigest::of_domain("release-candidate", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, super::error::DeliveryError> {
        self.candidate_digest = self.computed_digest()?;
        Ok(self)
    }
}

impl ReleaseManifestV1 {
    pub fn gate_input_digest(&self) -> Result<ContentDigest, super::error::DeliveryError> {
        let mut input = self.clone();
        input.manifest_digest = ContentDigest::zero();
        input.qa_gate.digest = ContentDigest::zero();
        ContentDigest::of_domain("release-manifest-gate-input", DELIVERY_SCHEMA_V1, &input)
    }

    pub fn computed_digest(&self) -> Result<ContentDigest, super::error::DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.manifest_digest = ContentDigest::zero();
        ContentDigest::of_domain("release-manifest", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, super::error::DeliveryError> {
        self.manifest_digest = self.computed_digest()?;
        Ok(self)
    }
}

impl DeliveryReceiptV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, super::error::DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.receipt_digest = ContentDigest::zero();
        ContentDigest::of_domain("delivery-receipt", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, super::error::DeliveryError> {
        self.receipt_digest = self.computed_digest()?;
        Ok(self)
    }
}

impl CustomerFeedbackV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, super::error::DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.feedback_digest = ContentDigest::zero();
        ContentDigest::of_domain("customer-feedback", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, super::error::DeliveryError> {
        self.feedback_digest = self.computed_digest()?;
        Ok(self)
    }
}

impl AcceptanceV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, super::error::DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.acceptance_digest = ContentDigest::zero();
        ContentDigest::of_domain("customer-acceptance", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, super::error::DeliveryError> {
        self.acceptance_digest = self.computed_digest()?;
        Ok(self)
    }
}

impl QaEvidenceGraphV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, super::error::DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.graph_digest = ContentDigest::zero();
        ContentDigest::of_domain("qa-evidence-graph", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, super::error::DeliveryError> {
        self.graph_digest = self.computed_digest()?;
        Ok(self)
    }
}
