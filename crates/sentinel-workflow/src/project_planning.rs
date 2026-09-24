//! Durable provider authority for the first project-planning decision.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    AuthenticatedCompanyPrincipalV1, ProjectV1, RequestProviderDispatchV1,
    SubscriptionTokenPolicyV1,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectPlanningGrantV1 {
    pub schema_version: u16,
    pub project_id: crate::ProjectId,
    pub expected_version: u64,
    pub planner_principal: AuthenticatedCompanyPrincipalV1,
    pub provider: String,
    pub model: String,
    pub catalog_digest: String,
    pub max_duration_ms: u64,
    pub token_policy: SubscriptionTokenPolicyV1,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectPlanningCallV1 {
    pub schema_version: u16,
    pub allowance_id: String,
    pub operation_id: Uuid,
    pub grant: ProjectPlanningGrantV1,
    pub source_project: ProjectV1,
    pub version: u64,
    pub created_at_unix_ms: u64,
    pub grant_issued_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub dispatch: Option<RequestProviderDispatchV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_project: Option<ProjectV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_response_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimProjectPlanningCallV1 {
    pub allowance_id: String,
    pub project_id: crate::ProjectId,
    pub request_id: String,
    pub request_digest: String,
    pub context_digest: String,
}
