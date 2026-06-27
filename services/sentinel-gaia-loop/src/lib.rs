//! Reactive Gaia Console runtime for #442.
//!
//! This crate is intentionally separate from deterministic `sentinel-gaia`
//! (company config generation) and from Voice-of-Gaia simulation input.

pub mod config;
pub mod prompts;
pub mod readiness;
pub mod session;
pub mod storage;
pub mod types;

pub const DEFAULT_CONSOLE_DIR: &str = "/opt/sentinel/data/gaia-console";
pub const DEFAULT_EVENTS_DB: &str = "/opt/sentinel/data/events.db";
pub const DEFAULT_NATS_URL: &str = "nats://127.0.0.1:4222";
pub const DEFAULT_HTTP_BIND: &str = "127.0.0.1:8092";
pub const DEFAULT_CLAUDE_BIN: &str = "claude";
pub const DEFAULT_SENTINEL_CTL_BIN: &str = "/opt/sentinel/bin/sentinel-ctl";
pub const DEFAULT_SENTINEL_GAIA_BIN: &str = "/opt/sentinel/bin/sentinel-gaia";
pub const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 120;
pub const DEFAULT_MAX_TURNS: u32 = 1;
pub const DEFAULT_MAX_BUDGET_USD: f64 = 0.05;
pub const ALERTS_FILE_NAME: &str = "alerts.jsonl";
pub const STATE_FILE_NAME: &str = "state.json";
pub const SESSIONS_DIR_NAME: &str = "sessions";
pub const SESSION_INDEX_FILE_NAME: &str = "index.jsonl";
