//! Durable workflow execution core for the bounded M0 company journey.
//!
//! This crate owns plans and work-item state. The Workbench, organization, and
//! independent gate implementations remain behind narrow authority ports.

mod adaptive;
mod adaptive_accounting_reconsideration;
mod adaptive_core;
mod adaptive_leadership_local_adoption;
mod adaptive_leadership_recovery;
mod adaptive_leadership_review;
mod adaptive_resume_policy;
mod adaptive_work_funding;
mod admission;
mod collaboration;
mod digest;
mod domain;
mod domain_store;
mod engine;
mod error;
mod model;
mod port;
mod project_planning;
mod request_provider;
mod store;

pub use adaptive::*;
pub use adaptive_accounting_reconsideration::*;
pub use adaptive_core::*;
pub use adaptive_leadership_local_adoption::*;
pub use adaptive_leadership_recovery::*;
pub use adaptive_leadership_review::*;
pub use adaptive_resume_policy::*;
pub use adaptive_work_funding::*;
pub use admission::*;
pub use collaboration::*;
pub use domain::*;
pub use domain_store::{
    AdaptiveBudgetReviewExtensionReceiptV1, AdaptiveBudgetReviewExtensionRequestV1,
};
pub use engine::WorkflowCore;
pub use error::{WorkflowError, WorkflowErrorCode};
pub use model::*;
pub use port::*;
pub use project_planning::*;
pub use request_provider::*;
pub use sentinel_common::AgentId;
pub use store::{
    AdaptiveHealthReadError, ExecutionRevisionV1, WorkflowStore, WORKFLOW_STORE_SCHEMA_VERSION,
};

pub const WORKFLOW_SCHEMA_VERSION: u16 = 1;
