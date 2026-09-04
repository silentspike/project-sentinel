//! Tool runtime for agent capabilities (native handlers + WASM Component Model).
//!
//! Provides sandboxed tool execution with capability-based access control.
//! Native handlers for FileRead/FileWrite enforce filesystem restrictions.
//! WASM Component Model plugins execute via wasmtime 46.0.3 (enable with `--features wasm`).

pub mod registry;
pub mod runner;
pub mod sandbox;

#[cfg(feature = "wasm")]
pub mod host;
#[cfg(feature = "wasm")]
pub mod nano;
#[cfg(feature = "wasm")]
pub mod plugin;

pub use runner::{ExecutionContext, ToolDefinition, ToolResult, ToolRuntime, ToolType};
pub use sandbox::SandboxConfig;

#[cfg(feature = "wasm")]
pub use host::{AgentSnapshot, PluginState, RoomSnapshot};
#[cfg(feature = "wasm")]
pub use nano::{wasm_conformance_metadata, WasmtimeNanoRuntime};
#[cfg(feature = "wasm")]
pub use plugin::{PluginConfig, PluginHost, PluginMeta};
