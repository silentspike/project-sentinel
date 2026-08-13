//! Durable workflow execution core for the bounded M0 company journey.
//!
//! This crate owns plans and work-item state. The Workbench, organization, and
//! independent gate implementations remain behind narrow authority ports.

mod digest;
mod domain;
mod domain_store;
mod engine;
mod error;
mod model;
mod port;
mod store;

pub use domain::*;
pub use engine::WorkflowCore;
pub use error::{WorkflowError, WorkflowErrorCode};
pub use model::*;
pub use port::*;
pub use sentinel_common::AgentId;
pub use store::{WorkflowStore, WORKFLOW_STORE_SCHEMA_VERSION};

pub const WORKFLOW_SCHEMA_VERSION: u16 = 1;
