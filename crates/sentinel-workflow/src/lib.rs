//! Durable, dependency-independent workflow core for the M0 virtual company.
//!
//! This crate owns authoritative customer, agreement, project, work, policy,
//! cost, collaboration, event, and projection state. Tool execution is an
//! external effect behind [`WorkExecutionPort`]; no runtime implementation is
//! selected here.

mod engine;
mod error;
mod model;
mod port;
mod store;

pub use engine::{Clock, SystemClock, WorkflowEngine};
pub use error::{WorkflowError, WorkflowErrorCode};
pub use model::*;
pub use port::{
    DependencyReadiness, ExecutionReceipt, OrganizationAgentSnapshot, OrganizationRuntimePort,
    PendingExecution, UnavailableExecutionPort, UnavailableOrganizationRuntimePort,
    WorkExecutionError, WorkExecutionPort,
};
pub use store::{ProjectionCheckpoint, WorkflowBackupManifest, WorkflowStore};

pub const WORKFLOW_SCHEMA_VERSION: u32 = 2;
