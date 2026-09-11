//! Pre-agreement provider authority. No project or assignment is fabricated.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AuthenticatedCompanyPrincipalV1, CustomerRequestV1, SubscriptionTokenPolicyV1};

pub fn request_provider_allowance_id(
    tenant: &crate::TenantId,
    operation: Uuid,
) -> Result<String, crate::WorkflowError> {
    tenant.validate()?;
    crate::domain::stable_domain_id("subscription", tenant, operation)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestProviderGrantV1 {
    pub schema_version: u16,
    pub request_id: String,
    pub expected_version: u64,
    pub sales_principal: AuthenticatedCompanyPrincipalV1,
    pub provider: String,
    pub model: String,
    pub catalog_digest: String,
    pub total_call_limit: u16,
    pub concurrent_call_limit: u16,
    pub max_duration_ms: u64,
    pub token_policy: SubscriptionTokenPolicyV1,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestProviderDispatchV1 {
    pub request_id: String,
    pub request_digest: String,
    pub context_digest: String,
    pub dispatched_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestProviderCallV1 {
    pub schema_version: u16,
    pub allowance_id: String,
    pub operation_id: Uuid,
    pub granted_by: AuthenticatedCompanyPrincipalV1,
    pub grant: RequestProviderGrantV1,
    pub source_request: CustomerRequestV1,
    pub version: u64,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    /// A possible send consumes authority permanently, including unknown outcomes.
    pub dispatch: Option<RequestProviderDispatchV1>,
    pub question_response: Option<CustomerRequestV1>,
    pub model_response_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRequestProviderCallV1 {
    pub allowance_id: String,
    pub request_id: String,
    pub request_digest: String,
    pub context_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdoptSalesQuestionV1 {
    pub allowance_id: String,
    pub request_digest: String,
    pub model_response_digest: String,
    pub content: String,
}
