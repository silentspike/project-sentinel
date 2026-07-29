use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    error::DeliveryError,
    ports::WorkbenchEvidenceReceiptV1,
    schema::{
        AcceptanceV1, ApprovalV1, CandidateState, CustomerFeedbackV1, DeliveryReceiptV1,
        DeliveryState, FindingV1, ProjectCloseoutV1, QaEvaluationPlanV1, QaEvaluationRunReceiptV1,
        QaReleaseGateReceiptV1, QaRunState, ReleaseCandidateV1, ReleaseManifestV1, ReleaseState,
        ReleaseV1, ReviewV1, RollbackV1, TestRunV1, DELIVERY_SCHEMA_V1,
    },
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeliveryAggregateV1 {
    pub schema_version: u16,
    pub tenant_id: String,
    pub project_id: String,
    pub revision: u64,
    pub candidates: BTreeMap<String, ReleaseCandidateV1>,
    pub qa_plans: BTreeMap<String, QaEvaluationPlanV1>,
    pub qa_runs: BTreeMap<String, QaEvaluationRunReceiptV1>,
    pub workbench_receipts: BTreeMap<String, WorkbenchEvidenceReceiptV1>,
    pub reviews: BTreeMap<String, ReviewV1>,
    pub test_runs: BTreeMap<String, TestRunV1>,
    pub findings: BTreeMap<String, FindingV1>,
    pub approvals: BTreeMap<String, ApprovalV1>,
    pub gates: BTreeMap<String, QaReleaseGateReceiptV1>,
    pub manifests: BTreeMap<String, ReleaseManifestV1>,
    pub releases: BTreeMap<String, ReleaseV1>,
    pub deliveries: BTreeMap<String, DeliveryReceiptV1>,
    pub feedback: BTreeMap<String, CustomerFeedbackV1>,
    pub acceptances: BTreeMap<String, AcceptanceV1>,
    pub rollbacks: BTreeMap<String, RollbackV1>,
    pub closeouts: BTreeMap<String, ProjectCloseoutV1>,
    pub active_release_id: Option<String>,
}

impl DeliveryAggregateV1 {
    pub fn new(tenant_id: impl Into<String>, project_id: impl Into<String>) -> Self {
        Self {
            schema_version: DELIVERY_SCHEMA_V1,
            tenant_id: tenant_id.into(),
            project_id: project_id.into(),
            revision: 0,
            candidates: BTreeMap::new(),
            qa_plans: BTreeMap::new(),
            qa_runs: BTreeMap::new(),
            workbench_receipts: BTreeMap::new(),
            reviews: BTreeMap::new(),
            test_runs: BTreeMap::new(),
            findings: BTreeMap::new(),
            approvals: BTreeMap::new(),
            gates: BTreeMap::new(),
            manifests: BTreeMap::new(),
            releases: BTreeMap::new(),
            deliveries: BTreeMap::new(),
            feedback: BTreeMap::new(),
            acceptances: BTreeMap::new(),
            rollbacks: BTreeMap::new(),
            closeouts: BTreeMap::new(),
            active_release_id: None,
        }
    }
}

pub fn transition_qa_run(current: QaRunState, next: QaRunState) -> Result<(), DeliveryError> {
    let valid = matches!(
        (current, next),
        (QaRunState::Planned, QaRunState::Admitted)
            | (QaRunState::Planned, QaRunState::Cancelled)
            | (QaRunState::Planned, QaRunState::Superseded)
            | (QaRunState::Admitted, QaRunState::Running)
            | (QaRunState::Admitted, QaRunState::Cancelled)
            | (QaRunState::Admitted, QaRunState::Superseded)
            | (QaRunState::Running, QaRunState::NeedsHumanReview)
            | (QaRunState::Running, QaRunState::CompletedPass)
            | (QaRunState::Running, QaRunState::CompletedFail)
            | (QaRunState::Running, QaRunState::HarnessError)
            | (QaRunState::Running, QaRunState::Cancelled)
            | (QaRunState::Running, QaRunState::Quarantined)
            | (
                QaRunState::NeedsHumanReview,
                QaRunState::CompletedPass | QaRunState::CompletedFail | QaRunState::Quarantined
            )
    );
    if !valid {
        return Err(DeliveryError::InvalidState {
            entity: "QA run",
            from: format!("{current:?}"),
            to: format!("{next:?}"),
        });
    }
    Ok(())
}

pub fn transition_candidate(
    current: CandidateState,
    next: CandidateState,
) -> Result<(), DeliveryError> {
    let valid = matches!(
        (current, next),
        (CandidateState::Draft, CandidateState::QaAssigned)
            | (CandidateState::QaAssigned, CandidateState::QaRunning)
            | (CandidateState::QaAssigned, CandidateState::Superseded)
            | (CandidateState::QaRunning, CandidateState::GatePassed)
            | (CandidateState::QaRunning, CandidateState::GateFailed)
            | (CandidateState::QaRunning, CandidateState::Superseded)
            | (CandidateState::GateFailed, CandidateState::Superseded)
            | (CandidateState::GatePassed, CandidateState::Promoted)
            | (CandidateState::GatePassed, CandidateState::Superseded)
    );
    if !valid {
        return Err(DeliveryError::InvalidState {
            entity: "release candidate",
            from: format!("{current:?}"),
            to: format!("{next:?}"),
        });
    }
    Ok(())
}

pub fn transition_delivery(
    current: DeliveryState,
    next: DeliveryState,
) -> Result<(), DeliveryError> {
    let valid = matches!(
        (current, next),
        (DeliveryState::PreviewReady, DeliveryState::Delivered)
            | (DeliveryState::PreviewReady, DeliveryState::Expired)
            | (DeliveryState::Delivered, DeliveryState::Accepted)
            | (DeliveryState::Delivered, DeliveryState::Rejected)
            | (DeliveryState::Delivered, DeliveryState::ChangesRequested)
            | (DeliveryState::Delivered, DeliveryState::Expired)
    );
    if !valid {
        return Err(DeliveryError::InvalidState {
            entity: "delivery",
            from: format!("{current:?}"),
            to: format!("{next:?}"),
        });
    }
    Ok(())
}

pub fn transition_release(current: ReleaseState, next: ReleaseState) -> Result<(), DeliveryError> {
    let valid = matches!(
        (current, next),
        (ReleaseState::Approved, ReleaseState::Active)
            | (ReleaseState::Approved, ReleaseState::Superseded)
            | (ReleaseState::Active, ReleaseState::RolledBack)
            | (ReleaseState::Active, ReleaseState::Superseded)
            | (ReleaseState::RolledBack, ReleaseState::Active)
            | (ReleaseState::Superseded, ReleaseState::Active)
    );
    if !valid {
        return Err(DeliveryError::InvalidState {
            entity: "release",
            from: format!("{current:?}"),
            to: format!("{next:?}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qa_terminal_receipts_never_reopen() {
        for terminal in [
            QaRunState::CompletedPass,
            QaRunState::CompletedFail,
            QaRunState::HarnessError,
            QaRunState::Cancelled,
            QaRunState::Superseded,
            QaRunState::Quarantined,
        ] {
            assert!(transition_qa_run(terminal, QaRunState::Running).is_err());
        }
    }

    #[test]
    fn delivery_acceptance_is_terminal() {
        assert!(transition_delivery(DeliveryState::Accepted, DeliveryState::Delivered).is_err());
        assert!(transition_delivery(DeliveryState::Delivered, DeliveryState::Accepted).is_ok());
    }
}
