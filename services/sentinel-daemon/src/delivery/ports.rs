use serde::{Deserialize, Serialize};

use super::{
    digest::ContentDigest,
    error::DeliveryError,
    schema::{
        ArtifactRefV1, AuthorityRole, PrincipalV1, QaHarnessOutcome, VersionedRefV1,
        DELIVERY_SCHEMA_V1,
    },
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterReadiness {
    Ready {
        contract_version: u16,
        authority_generation: u64,
        contract_digest: ContentDigest,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateAuthorityQueryV1 {
    pub tenant_id: String,
    pub agreement: VersionedRefV1,
    pub project: VersionedRefV1,
    pub work_items_digest: ContentDigest,
    pub candidate_digest: ContentDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateAuthoritySnapshotV1 {
    pub schema_version: u16,
    pub authority_generation: u64,
    pub agreement: VersionedRefV1,
    pub project: VersionedRefV1,
    pub work_items_digest: ContentDigest,
    pub current_candidate_generation: u64,
    pub current_candidate_digest: ContentDigest,
    pub participant_principals: Vec<PrincipalV1>,
    pub snapshot_digest: ContentDigest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowLineageKindV1 {
    CustomerRequest,
    Agreement,
    Project,
    WorkItem,
    Participant,
    Decision,
    Handoff,
    Blocker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowLineageStateV1 {
    Requested,
    Active,
    Completed,
    Approved,
    HandedOff,
    Blocked,
    Clear,
}

/// Internal, server-redacted workflow node supplied by the future #695 adapter.
/// `node_ordinal` is used only to reconstruct topology and is remapped before
/// the public DTO is returned. Arbitrary labels and source authority identifiers
/// are not representable in this contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowLineageNodeV1 {
    pub node_ordinal: u32,
    pub kind: WorkflowLineageKindV1,
    pub state: WorkflowLineageStateV1,
    pub generation: u64,
    pub digest: ContentDigest,
    pub participant_role: Option<AuthorityRole>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowLineageEdgeV1 {
    pub from_ordinal: u32,
    pub to_ordinal: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowLineageQueryV1 {
    pub schema_version: u16,
    pub tenant_id: String,
    pub project: VersionedRefV1,
    pub candidate: VersionedRefV1,
    pub authority_generation: u64,
    pub authority_identity_digest: ContentDigest,
    pub query_digest: ContentDigest,
}

impl WorkflowLineageQueryV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.query_digest = ContentDigest::zero();
        ContentDigest::of_domain("workflow-lineage-query", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.query_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowLineageSnapshotV1 {
    pub schema_version: u16,
    pub server_redacted: bool,
    pub tenant_id: String,
    pub project: VersionedRefV1,
    pub candidate: VersionedRefV1,
    pub authority_generation: u64,
    pub authority_identity_digest: ContentDigest,
    pub query_digest: ContentDigest,
    pub snapshot_generation: u64,
    pub nodes: Vec<WorkflowLineageNodeV1>,
    pub edges: Vec<WorkflowLineageEdgeV1>,
    pub snapshot_digest: ContentDigest,
}

impl WorkflowLineageSnapshotV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.snapshot_digest = ContentDigest::zero();
        ContentDigest::of_domain("workflow-lineage-snapshot", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.snapshot_digest = self.computed_digest()?;
        Ok(self)
    }
}

impl CandidateAuthoritySnapshotV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.snapshot_digest = ContentDigest::zero();
        ContentDigest::of_domain(
            "candidate-authority-snapshot",
            DELIVERY_SCHEMA_V1,
            &unsigned,
        )
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.snapshot_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityValidationRequestV1 {
    pub schema_version: u16,
    pub tenant_id: String,
    pub principal_id: String,
    pub claimed_authority_generation: u64,
    pub required_role: AuthorityRole,
    pub operation: String,
    pub contract_version: u16,
    pub contract_digest: ContentDigest,
    pub validated_at_ms: u64,
    pub request_digest: ContentDigest,
}

impl AuthorityValidationRequestV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.request_digest = ContentDigest::zero();
        ContentDigest::of_domain(
            "authority-validation-request",
            DELIVERY_SCHEMA_V1,
            &unsigned,
        )
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        if self.validated_at_ms == 0 {
            return Err(DeliveryError::Validation(
                "authority validation time is invalid".to_string(),
            ));
        }
        self.request_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityReceiptV1 {
    pub schema_version: u16,
    pub request_digest: ContentDigest,
    pub principal: PrincipalV1,
    pub contract_version: u16,
    pub contract_authority_generation: u64,
    pub contract_digest: ContentDigest,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub issuer: String,
    pub receipt_digest: ContentDigest,
}

impl AuthorityReceiptV1 {
    /// Stable authority lineage used by durable external-operation identities.
    ///
    /// Short-lived receipt metadata is deliberately excluded: renewing the same
    /// authority must not create a second workbench invocation or delivery
    /// effect. Contract, issuer, or principal changes must create a new identity.
    pub fn stable_identity_digest(&self) -> Result<ContentDigest, DeliveryError> {
        ContentDigest::of_domain(
            "authority-identity",
            DELIVERY_SCHEMA_V1,
            &(
                &self.principal,
                self.contract_version,
                self.contract_authority_generation,
                &self.contract_digest,
                &self.issuer,
            ),
        )
    }

    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.receipt_digest = ContentDigest::zero();
        ContentDigest::of_domain("authority-receipt", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.receipt_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchEvidenceRequestV1 {
    pub schema_version: u16,
    pub tenant_id: String,
    pub project: VersionedRefV1,
    pub candidate: VersionedRefV1,
    pub qa_plan: VersionedRefV1,
    pub qa_run: VersionedRefV1,
    pub candidate_artifacts: Vec<ArtifactRefV1>,
    pub assigned_qa: PrincipalV1,
    pub authority_receipt_digest: ContentDigest,
    pub authority_identity_digest: ContentDigest,
    pub invocation: VersionedRefV1,
    pub started_at_ms: u64,
    pub request_digest: ContentDigest,
}

impl WorkbenchEvidenceRequestV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        // Authority receipts are short-lived authorization proofs. They are not
        // part of the stable workbench idempotency identity.
        unsigned.authority_receipt_digest = ContentDigest::zero();
        unsigned.request_digest = ContentDigest::zero();
        ContentDigest::of_domain("workbench-evidence-request", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        if self.schema_version != DELIVERY_SCHEMA_V1
            || !valid_wire_id(&self.tenant_id)
            || !valid_versioned_ref(&self.project)
            || !valid_versioned_ref(&self.candidate)
            || !valid_versioned_ref(&self.qa_plan)
            || !valid_versioned_ref(&self.qa_run)
            || self.candidate_artifacts.is_empty()
            || self.candidate_artifacts.len() > 64
            || self.candidate_artifacts.iter().any(|artifact| {
                !valid_wire_id(&artifact.artifact_id)
                    || artifact.generation == 0
                    || artifact.digest == ContentDigest::zero()
                    || artifact.media_type.is_empty()
                    || artifact.media_type.len() > 128
                    || !artifact.media_type.contains('/')
                    || !valid_wire_id(&artifact.owner_principal_id)
            })
            || !valid_principal(&self.assigned_qa)
            || self.assigned_qa.tenant_id != self.tenant_id
            || !self.assigned_qa.has_role(AuthorityRole::Qa)
            || self.authority_receipt_digest == ContentDigest::zero()
            || self.authority_identity_digest == ContentDigest::zero()
            || !valid_versioned_ref(&self.invocation)
            || self.started_at_ms == 0
        {
            return Err(DeliveryError::Validation(
                "workbench request identity or authority binding is invalid".to_string(),
            ));
        }
        self.request_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbenchEvidenceReceiptV1 {
    pub schema_version: u16,
    pub invocation: VersionedRefV1,
    pub assignment: VersionedRefV1,
    pub qa_run: VersionedRefV1,
    pub assigned_qa: PrincipalV1,
    pub authority_receipt_digest: ContentDigest,
    pub authority_identity_digest: ContentDigest,
    pub input_digest: ContentDigest,
    pub output_digest: ContentDigest,
    pub artifact_ownership_digest: ContentDigest,
    pub result_inventory_digest: ContentDigest,
    pub logs_digest: ContentDigest,
    pub screenshots_digest: Option<ContentDigest>,
    pub failure_classification_digest: ContentDigest,
    pub harness_outcome: QaHarnessOutcome,
    pub required_cases_complete: bool,
    pub contaminated: bool,
    pub needs_human_review: bool,
    pub flaky_unresolved: bool,
    pub cleanup_receipt: VersionedRefV1,
    pub receipt_digest: ContentDigest,
}

impl WorkbenchEvidenceReceiptV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.receipt_digest = ContentDigest::zero();
        ContentDigest::of_domain("workbench-evidence-receipt", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.receipt_digest = self.computed_digest()?;
        Ok(self)
    }
}

/// Narrow, fail-closed seam implemented later by the productive #694/#695 adapter.
///
/// The dependency-independent core calls only this versioned contract. A daemon may
/// construct the core with an unavailable adapter and still start normally; every
/// command that needs workflow or workbench authority returns a typed error.
pub trait DeliveryIntegrationPort: Send + Sync {
    fn readiness(&self) -> AdapterReadiness;

    /// Readiness for the durable execution-saga contract. A Ready adapter must
    /// durably claim the stable request digest before invoking the workbench,
    /// persist the opaque outcome before returning, and return that same
    /// outcome on retry/reconcile after caller crash or disconnect.
    fn execution_saga_readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Unavailable {
            reason: "productive #694 workbench execution saga is not provisioned".to_string(),
        }
    }

    fn candidate_authority(
        &self,
        query: &CandidateAuthorityQueryV1,
    ) -> Result<CandidateAuthoritySnapshotV1, DeliveryError>;

    fn workflow_lineage(
        &self,
        _query: &WorkflowLineageQueryV1,
    ) -> Result<WorkflowLineageSnapshotV1, DeliveryError> {
        Err(DeliveryError::AdapterUnavailable {
            dependency: "workflow_lineage",
            reason: "productive #695 workflow lineage adapter is not provisioned".to_string(),
        })
    }

    fn authorize(
        &self,
        request: &AuthorityValidationRequestV1,
    ) -> Result<AuthorityReceiptV1, DeliveryError>;

    fn execute_qa(
        &self,
        request: &WorkbenchEvidenceRequestV1,
    ) -> Result<WorkbenchEvidenceReceiptV1, DeliveryError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationRequestV1 {
    pub schema_version: u16,
    pub operation_id: String,
    pub event_type: String,
    pub aggregate_id: String,
    pub row_identity: String,
    pub payload_digest: ContentDigest,
    pub payload: Vec<u8>,
    pub occurred_at_ms: u64,
    pub request_digest: ContentDigest,
}

impl PublicationRequestV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.request_digest = ContentDigest::zero();
        ContentDigest::of_domain("publication-request", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        if self.occurred_at_ms == 0 {
            return Err(DeliveryError::Validation(
                "publication occurrence time is invalid".to_string(),
            ));
        }
        self.request_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationReceiptV1 {
    pub schema_version: u16,
    pub operation_id: String,
    pub event_id: String,
    pub aggregate_id: String,
    pub row_identity: String,
    pub payload_digest: ContentDigest,
    pub request_digest: ContentDigest,
}

/// Adapter from the local durable #696 outbox into the application event chain.
/// Receipt authority belongs to the injected publisher and is adopted only
/// after exact request, aggregate, row, and payload-digest readback.
/// `operation_id` plus `request_digest` is the stable idempotency key: after a
/// successful publish every replay must return the identical receipt for the
/// same effective event. This closes caller-crash reconciliation; it does not
/// claim exactly-once transport.
pub trait DeliveryPublicationPort: Send + Sync {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Unavailable {
            reason: "productive delivery event publisher is not provisioned".to_string(),
        }
    }

    fn publish(
        &self,
        request: &PublicationRequestV1,
    ) -> Result<PublicationReceiptV1, DeliveryError>;
}

#[derive(Clone, Debug, Default)]
pub struct UnavailableDeliveryPublication;

impl DeliveryPublicationPort for UnavailableDeliveryPublication {
    fn publish(
        &self,
        _request: &PublicationRequestV1,
    ) -> Result<PublicationReceiptV1, DeliveryError> {
        Err(DeliveryError::AdapterUnavailable {
            dependency: "delivery_publication",
            reason: "productive delivery event publisher is not provisioned".to_string(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryEffectKind {
    Rollout,
    Rollback,
    GovernedRework,
    MemoryPublication,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloseoutMemorySourceV1 {
    pub acceptance: VersionedRefV1,
    pub decisions_digest: ContentDigest,
    pub artifact_inventory_digest: ContentDigest,
    pub failures_digest: ContentDigest,
    pub lessons_digest: ContentDigest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryEffectRequestV1 {
    pub schema_version: u16,
    pub operation_id: String,
    pub kind: DeliveryEffectKind,
    pub tenant_id: String,
    pub project: VersionedRefV1,
    pub candidate: Option<VersionedRefV1>,
    pub subject: VersionedRefV1,
    pub target: Option<VersionedRefV1>,
    #[serde(default)]
    pub feedback_digest: Option<ContentDigest>,
    #[serde(default)]
    pub closeout_memory: Option<CloseoutMemorySourceV1>,
    pub occurred_at_ms: u64,
    pub actor: PrincipalV1,
    pub actor_authority_receipt_digest: ContentDigest,
    pub actor_authority_identity_digest: ContentDigest,
    pub request_digest: ContentDigest,
}

impl DeliveryEffectRequestV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        // Renewal of the same authority must not change the effect's durable
        // operation identity or cause the real effect to run twice.
        unsigned.actor_authority_receipt_digest = ContentDigest::zero();
        unsigned.request_digest = ContentDigest::zero();
        ContentDigest::of_domain("delivery-effect-request", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        let legal_effect_shape = match self.kind {
            DeliveryEffectKind::Rollout => {
                self.candidate.is_some()
                    && self.target.is_none()
                    && self.feedback_digest.is_none()
                    && self.closeout_memory.is_none()
                    && self.actor.has_role(AuthorityRole::ReleaseManager)
            }
            DeliveryEffectKind::Rollback => {
                self.candidate.is_some()
                    && self
                        .target
                        .as_ref()
                        .is_some_and(|target| target != &self.subject)
                    && self.feedback_digest.is_none()
                    && self.closeout_memory.is_none()
                    && self.actor.has_role(AuthorityRole::ReleaseManager)
            }
            DeliveryEffectKind::GovernedRework => {
                self.candidate.is_some()
                    && self.target.is_none()
                    && self.feedback_digest.as_ref().is_some_and(|value| *value != ContentDigest::zero())
                    && self.closeout_memory.is_none()
                    && self.actor.has_role(AuthorityRole::Customer)
            }
            DeliveryEffectKind::MemoryPublication => {
                self.candidate.is_some()
                    && self.target.is_none()
                    && self.feedback_digest.is_none()
                    && self.closeout_memory.as_ref().is_some_and(|source| {
                        valid_versioned_ref(&source.acceptance)
                            && source.decisions_digest != ContentDigest::zero()
                            && source.artifact_inventory_digest != ContentDigest::zero()
                            && source.failures_digest != ContentDigest::zero()
                            && source.lessons_digest != ContentDigest::zero()
                    })
                    && self.actor.has_role(AuthorityRole::ReleaseManager)
            }
        };
        if self.schema_version != DELIVERY_SCHEMA_V1
            || !valid_wire_id(&self.operation_id)
            || !valid_wire_id(&self.tenant_id)
            || !valid_versioned_ref(&self.project)
            || self
                .candidate
                .as_ref()
                .is_some_and(|value| !valid_versioned_ref(value))
            || !valid_versioned_ref(&self.subject)
            || !valid_principal(&self.actor)
            || self.actor.tenant_id != self.tenant_id
            || self.actor_authority_receipt_digest == ContentDigest::zero()
            || self.actor_authority_identity_digest == ContentDigest::zero()
            || self.occurred_at_ms == 0
            || self
                .target
                .as_ref()
                .is_some_and(|value| !valid_versioned_ref(value))
            || !legal_effect_shape
        {
            return Err(DeliveryError::Validation(
                "delivery effect request identity or binding is invalid".to_string(),
            ));
        }
        self.request_digest = self.computed_digest()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryEffectReceiptV1 {
    pub schema_version: u16,
    pub operation_id: String,
    pub kind: DeliveryEffectKind,
    pub tenant_id: String,
    pub project: VersionedRefV1,
    pub candidate: Option<VersionedRefV1>,
    pub subject: VersionedRefV1,
    pub target: Option<VersionedRefV1>,
    pub actor: PrincipalV1,
    pub request_digest: ContentDigest,
    pub actor_authority_receipt_digest: ContentDigest,
    pub actor_authority_identity_digest: ContentDigest,
    pub effect_ref: VersionedRefV1,
    #[serde(default)]
    pub affected_refs: Vec<VersionedRefV1>,
    pub issuer: String,
    pub issued_at_ms: u64,
    pub receipt_digest: ContentDigest,
}

impl DeliveryEffectReceiptV1 {
    pub fn computed_digest(&self) -> Result<ContentDigest, DeliveryError> {
        let mut unsigned = self.clone();
        unsigned.receipt_digest = ContentDigest::zero();
        ContentDigest::of_domain("delivery-effect-receipt", DELIVERY_SCHEMA_V1, &unsigned)
    }

    pub fn seal(mut self) -> Result<Self, DeliveryError> {
        self.receipt_digest = self.computed_digest()?;
        Ok(self)
    }
}

pub trait DeliveryEffectPort: Send + Sync {
    /// A Ready implementation owns the #710 durable intent/outcome/reconcile
    /// boundary. It must claim `(operation_id, request_digest)` before the
    /// external effect and return the same sealed outcome on every retry.
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Unavailable {
            reason: "productive #710 effect saga is not provisioned".to_string(),
        }
    }

    fn apply(
        &self,
        request: &DeliveryEffectRequestV1,
    ) -> Result<DeliveryEffectReceiptV1, DeliveryError>;
}

#[derive(Clone, Debug, Default)]
pub struct UnavailableDeliveryIntegration;

impl DeliveryIntegrationPort for UnavailableDeliveryIntegration {
    fn readiness(&self) -> AdapterReadiness {
        AdapterReadiness::Unavailable {
            reason: "productive #694/#695 adapter is not provisioned".to_string(),
        }
    }

    fn candidate_authority(
        &self,
        _query: &CandidateAuthorityQueryV1,
    ) -> Result<CandidateAuthoritySnapshotV1, DeliveryError> {
        Err(DeliveryError::AdapterUnavailable {
            dependency: "workflow_authority",
            reason: "productive #695 adapter is not provisioned".to_string(),
        })
    }

    fn authorize(
        &self,
        _request: &AuthorityValidationRequestV1,
    ) -> Result<AuthorityReceiptV1, DeliveryError> {
        Err(DeliveryError::AdapterUnavailable {
            dependency: "authenticated_authority",
            reason: "productive authenticated principal adapter is not provisioned".to_string(),
        })
    }

    fn execute_qa(
        &self,
        _request: &WorkbenchEvidenceRequestV1,
    ) -> Result<WorkbenchEvidenceReceiptV1, DeliveryError> {
        Err(DeliveryError::AdapterUnavailable {
            dependency: "workbench",
            reason: "productive #694 adapter is not provisioned".to_string(),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct UnavailableDeliveryEffects;

impl DeliveryEffectPort for UnavailableDeliveryEffects {
    fn apply(
        &self,
        request: &DeliveryEffectRequestV1,
    ) -> Result<DeliveryEffectReceiptV1, DeliveryError> {
        let dependency = match request.kind {
            DeliveryEffectKind::Rollout | DeliveryEffectKind::Rollback => "release_effects",
            DeliveryEffectKind::GovernedRework => "governed_rework",
            DeliveryEffectKind::MemoryPublication => "memory_publication",
        };
        Err(DeliveryError::AdapterUnavailable {
            dependency,
            reason: "productive effect adapter is not provisioned".to_string(),
        })
    }
}

pub fn expected_integration_contract_digest() -> ContentDigest {
    ContentDigest::of_domain(
        "integration-contract",
        DELIVERY_SCHEMA_V1,
        &"delivery-integration-v1",
    )
    .expect("constant integration contract must be canonical")
}

pub fn expected_effect_saga_contract_digest() -> ContentDigest {
    ContentDigest::of_domain(
        "effect-saga-contract",
        DELIVERY_SCHEMA_V1,
        &"delivery-effect-saga-v2",
    )
    .expect("constant effect saga contract must be canonical")
}

pub fn expected_workbench_execution_saga_contract_digest() -> ContentDigest {
    ContentDigest::of_domain(
        "workbench-execution-saga-contract",
        DELIVERY_SCHEMA_V1,
        &"workbench-execution-saga-v1",
    )
    .expect("constant workbench execution saga contract must be canonical")
}

pub fn expected_publication_contract_digest() -> ContentDigest {
    ContentDigest::of_domain(
        "publication-contract",
        DELIVERY_SCHEMA_V1,
        &"delivery-publication-v1",
    )
    .expect("constant publication contract must be canonical")
}

fn valid_wire_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 240
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn valid_versioned_ref(value: &VersionedRefV1) -> bool {
    valid_wire_id(&value.id) && value.generation > 0 && value.digest != ContentDigest::zero()
}

fn valid_principal(value: &PrincipalV1) -> bool {
    valid_wire_id(&value.tenant_id)
        && valid_wire_id(&value.principal_id)
        && value.authority_generation > 0
        && !value.roles.is_empty()
}
