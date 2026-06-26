//! Gaia Console Memory.
//!
//! This crate owns the user-facing Gaia memory layer for #443: a local
//! relational-temporal redb graph, a Markdown memory file, read-only wake-up
//! rehydration, Hippocampus source access, and a crate-local backup path.
//! It is intentionally separate from simulation `WorldSnapshot` state.

pub mod backup;
pub mod cli;
pub mod graph;
pub mod hippocampus_source;
pub mod memory_file;
pub mod rehydrate;

pub const GRAPH_FILE_NAME: &str = "gaia_console_memory.redb";
pub const MEMORY_FILE_NAME: &str = "gaia-memory.md";
pub const BACKUP_FORMAT_VERSION: u32 = 1;
