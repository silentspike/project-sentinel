use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::{
    ALERTS_FILE_NAME, DEFAULT_BUDGET_WINDOW_SECS, DEFAULT_BUDGET_WINDOW_USD, DEFAULT_CLAUDE_BIN,
    DEFAULT_COMPANY_CONTEXT_PATH, DEFAULT_CONSOLE_DIR, DEFAULT_EVENTS_DB, DEFAULT_HTTP_BIND,
    DEFAULT_MAX_BUDGET_USD, DEFAULT_NATS_URL, DEFAULT_READINESS_SCAN_INTERVAL_SECS,
    DEFAULT_SENTINEL_CTL_BIN, DEFAULT_SENTINEL_GAIA_BIN, DEFAULT_SESSION_TIMEOUT_SECS,
    SESSIONS_DIR_NAME, SESSION_ACTIVE_LOCK_FILE_NAME, SESSION_INDEX_FILE_NAME, STATE_FILE_NAME,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GaiaLoopConfig {
    pub console_dir: PathBuf,
    pub events_db: PathBuf,
    pub nats_url: String,
    pub http_bind: String,
    pub claude_bin: PathBuf,
    pub sentinel_ctl_bin: PathBuf,
    pub sentinel_gaia_bin: PathBuf,
    pub company_context_path: PathBuf,
    pub model: Option<String>,
    pub max_budget_usd: f64,
    pub budget_window_secs: u64,
    pub budget_window_usd: f64,
    pub session_timeout_secs: u64,
    pub readiness_scan_interval_secs: u64,
}

impl GaiaLoopConfig {
    pub fn from_env() -> Result<Self> {
        let cfg = Self {
            console_dir: path_env("SENTINEL_GAIA_CONSOLE_DIR", DEFAULT_CONSOLE_DIR),
            events_db: path_env("SENTINEL_EVENTS_DB", DEFAULT_EVENTS_DB),
            nats_url: string_env("SENTINEL_NATS_URL", DEFAULT_NATS_URL),
            http_bind: string_env("SENTINEL_GAIA_HTTP_BIND", DEFAULT_HTTP_BIND),
            claude_bin: path_env("SENTINEL_GAIA_CLAUDE_BIN", DEFAULT_CLAUDE_BIN),
            sentinel_ctl_bin: path_env("SENTINEL_CTL_BIN", DEFAULT_SENTINEL_CTL_BIN),
            sentinel_gaia_bin: path_env("SENTINEL_GAIA_BIN", DEFAULT_SENTINEL_GAIA_BIN),
            company_context_path: path_env(
                "SENTINEL_GAIA_COMPANY_CONTEXT",
                DEFAULT_COMPANY_CONTEXT_PATH,
            ),
            model: optional_string_env("SENTINEL_GAIA_MODEL"),
            max_budget_usd: parse_env("SENTINEL_GAIA_MAX_BUDGET_USD", DEFAULT_MAX_BUDGET_USD)?,
            budget_window_secs: parse_env(
                "SENTINEL_GAIA_BUDGET_WINDOW_SECS",
                DEFAULT_BUDGET_WINDOW_SECS,
            )?,
            budget_window_usd: parse_env(
                "SENTINEL_GAIA_BUDGET_WINDOW_USD",
                DEFAULT_BUDGET_WINDOW_USD,
            )?,
            session_timeout_secs: parse_env(
                "SENTINEL_GAIA_SESSION_TIMEOUT_SECS",
                DEFAULT_SESSION_TIMEOUT_SECS,
            )?,
            readiness_scan_interval_secs: parse_env(
                "SENTINEL_GAIA_SCAN_INTERVAL_SECS",
                DEFAULT_READINESS_SCAN_INTERVAL_SECS,
            )?,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if !self.max_budget_usd.is_finite() || self.max_budget_usd <= 0.0 {
            bail!("SENTINEL_GAIA_MAX_BUDGET_USD must be > 0");
        }
        if self.budget_window_secs == 0 {
            bail!("SENTINEL_GAIA_BUDGET_WINDOW_SECS must be > 0");
        }
        if !self.budget_window_usd.is_finite() || self.budget_window_usd < self.max_budget_usd {
            bail!("SENTINEL_GAIA_BUDGET_WINDOW_USD must be finite and >= SENTINEL_GAIA_MAX_BUDGET_USD");
        }
        if self.session_timeout_secs == 0 {
            bail!("SENTINEL_GAIA_SESSION_TIMEOUT_SECS must be > 0");
        }
        if self.readiness_scan_interval_secs == 0 {
            bail!("SENTINEL_GAIA_SCAN_INTERVAL_SECS must be > 0");
        }
        if self.http_bind.trim().is_empty() {
            bail!("SENTINEL_GAIA_HTTP_BIND must not be empty");
        }
        if self.nats_url.trim().is_empty() {
            bail!("SENTINEL_NATS_URL must not be empty");
        }
        Ok(())
    }

    pub fn session_timeout(&self) -> Duration {
        Duration::from_secs(self.session_timeout_secs)
    }

    pub fn readiness_scan_interval(&self) -> Duration {
        Duration::from_secs(self.readiness_scan_interval_secs)
    }

    pub fn claude_budget_args(&self) -> Vec<String> {
        vec![
            "--max-budget-usd".to_string(),
            self.max_budget_usd.to_string(),
        ]
    }

    pub fn alerts_path(&self) -> PathBuf {
        self.console_dir.join(ALERTS_FILE_NAME)
    }

    pub fn state_path(&self) -> PathBuf {
        self.console_dir.join(STATE_FILE_NAME)
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.console_dir.join(SESSIONS_DIR_NAME)
    }

    pub fn session_index_path(&self) -> PathBuf {
        self.sessions_dir().join(SESSION_INDEX_FILE_NAME)
    }

    pub fn session_active_lock_path(&self) -> PathBuf {
        self.sessions_dir().join(SESSION_ACTIVE_LOCK_FILE_NAME)
    }
}

fn string_env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn optional_string_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn path_env(name: &str, default: &str) -> PathBuf {
    PathBuf::from(string_env(name, default))
}

fn parse_env<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Ok(raw) => raw
            .parse::<T>()
            .map_err(|err| anyhow::anyhow!("parse {name}={raw}: {err}")),
        Err(_) => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> GaiaLoopConfig {
        GaiaLoopConfig {
            console_dir: PathBuf::from("/tmp/gaia-console"),
            events_db: PathBuf::from("/tmp/events.db"),
            nats_url: DEFAULT_NATS_URL.to_string(),
            http_bind: DEFAULT_HTTP_BIND.to_string(),
            claude_bin: PathBuf::from(DEFAULT_CLAUDE_BIN),
            sentinel_ctl_bin: PathBuf::from(DEFAULT_SENTINEL_CTL_BIN),
            sentinel_gaia_bin: PathBuf::from(DEFAULT_SENTINEL_GAIA_BIN),
            company_context_path: PathBuf::from(DEFAULT_COMPANY_CONTEXT_PATH),
            model: None,
            max_budget_usd: DEFAULT_MAX_BUDGET_USD,
            budget_window_secs: DEFAULT_BUDGET_WINDOW_SECS,
            budget_window_usd: DEFAULT_BUDGET_WINDOW_USD,
            session_timeout_secs: DEFAULT_SESSION_TIMEOUT_SECS,
            readiness_scan_interval_secs: DEFAULT_READINESS_SCAN_INTERVAL_SECS,
        }
    }

    #[test]
    fn defaults_include_mandatory_caps() {
        let cfg = cfg();
        cfg.validate().unwrap();
        assert!(cfg.max_budget_usd > 0.0);
        assert_eq!(cfg.budget_window_secs, DEFAULT_BUDGET_WINDOW_SECS);
        assert_eq!(cfg.budget_window_usd, DEFAULT_BUDGET_WINDOW_USD);
        assert!(cfg.session_timeout().as_secs() > 0);
        assert_eq!(
            cfg.readiness_scan_interval().as_secs(),
            DEFAULT_READINESS_SCAN_INTERVAL_SECS
        );
        assert_eq!(cfg.claude_budget_args(), vec!["--max-budget-usd", "0.05",]);
    }

    #[test]
    fn rejects_missing_caps() {
        let mut cfg = cfg();
        cfg.max_budget_usd = 0.0;
        assert!(cfg.validate().is_err());
        cfg.max_budget_usd = DEFAULT_MAX_BUDGET_USD;
        cfg.budget_window_secs = 0;
        assert!(cfg.validate().is_err());
        cfg.budget_window_secs = DEFAULT_BUDGET_WINDOW_SECS;
        cfg.budget_window_usd = cfg.max_budget_usd / 2.0;
        assert!(cfg.validate().is_err());
        cfg.budget_window_usd = DEFAULT_BUDGET_WINDOW_USD;
        cfg.session_timeout_secs = 0;
        assert!(cfg.validate().is_err());
        cfg.session_timeout_secs = DEFAULT_SESSION_TIMEOUT_SECS;
        cfg.readiness_scan_interval_secs = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn derives_storage_paths_from_console_dir() {
        let cfg = cfg();
        assert_eq!(
            cfg.alerts_path(),
            PathBuf::from("/tmp/gaia-console/alerts.jsonl")
        );
        assert_eq!(
            cfg.state_path(),
            PathBuf::from("/tmp/gaia-console/state.json")
        );
        assert_eq!(
            cfg.sessions_dir(),
            PathBuf::from("/tmp/gaia-console/sessions")
        );
        assert_eq!(
            cfg.session_index_path(),
            PathBuf::from("/tmp/gaia-console/sessions/index.jsonl")
        );
    }
}
