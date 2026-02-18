//! Tool runtime for agent capabilities (native handlers + WASM).
//!
//! Provides sandboxed tool execution with capability-based access control.
//! Native handlers for FileRead/FileWrite enforce filesystem restrictions.
//! WASM modules execute via wasmtime (enable with `--features wasm`).

pub mod registry;
pub mod runner;
pub mod sandbox;

pub use runner::{ExecutionContext, ToolDefinition, ToolResult, ToolRuntime, ToolType};
pub use sandbox::SandboxConfig;
