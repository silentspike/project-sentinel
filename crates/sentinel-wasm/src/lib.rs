//! Tool runtime for agent capabilities (native handlers + future WASM).

pub mod runner;

pub use runner::{ToolDefinition, ToolRuntime, ToolType};
