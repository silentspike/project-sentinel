use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::GaiaLoopConfig;
use crate::types::GaiaAlert;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GaiaLoopState {
    pub last_event_row_id: i64,
    pub alerts_created: u64,
    pub last_alert_timestamp_ms: Option<u64>,
}

impl Default for GaiaLoopState {
    fn default() -> Self {
        Self {
            last_event_row_id: 0,
            alerts_created: 0,
            last_alert_timestamp_ms: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AlertStore {
    alerts_path: PathBuf,
    state_path: PathBuf,
    sessions_dir: PathBuf,
}

impl AlertStore {
    pub fn from_config(config: &GaiaLoopConfig) -> Self {
        Self {
            alerts_path: config.alerts_path(),
            state_path: config.state_path(),
            sessions_dir: config.sessions_dir(),
        }
    }

    pub fn ensure_layout(&self) -> Result<()> {
        if let Some(parent) = self.alerts_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create Gaia Console dir {}", parent.display()))?;
        }
        fs::create_dir_all(&self.sessions_dir)
            .with_context(|| format!("create Gaia sessions dir {}", self.sessions_dir.display()))?;
        Ok(())
    }

    pub fn load_state(&self) -> Result<GaiaLoopState> {
        if !self.state_path.exists() {
            return Ok(GaiaLoopState::default());
        }
        let raw = fs::read_to_string(&self.state_path)
            .with_context(|| format!("read {}", self.state_path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("parse {}", self.state_path.display()))
    }

    pub fn save_state(&self, state: &GaiaLoopState) -> Result<()> {
        self.ensure_layout()?;
        let tmp = self.state_path.with_extension("json.tmp");
        let raw = serde_json::to_vec_pretty(state).context("serialize Gaia loop state")?;
        fs::write(&tmp, raw).with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, &self.state_path).with_context(|| {
            format!(
                "replace Gaia loop state {} from {}",
                self.state_path.display(),
                tmp.display()
            )
        })?;
        Ok(())
    }

    pub fn append_alert(&self, alert: &GaiaAlert) -> Result<()> {
        self.ensure_layout()?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.alerts_path)
            .with_context(|| format!("open {}", self.alerts_path.display()))?;
        serde_json::to_writer(&mut file, alert).context("serialize Gaia alert")?;
        file.write_all(b"\n")
            .with_context(|| format!("append {}", self.alerts_path.display()))?;
        Ok(())
    }

    pub fn load_alert_dedupe_keys(&self) -> Result<HashSet<String>> {
        if !self.alerts_path.exists() {
            return Ok(HashSet::new());
        }
        let file = fs::File::open(&self.alerts_path)
            .with_context(|| format!("open {}", self.alerts_path.display()))?;
        let mut keys = HashSet::new();
        for line in BufReader::new(file).lines() {
            let line = line.with_context(|| format!("read {}", self.alerts_path.display()))?;
            if line.trim().is_empty() {
                continue;
            }
            let alert: GaiaAlert = serde_json::from_str(&line)
                .with_context(|| format!("parse alert line in {}", self.alerts_path.display()))?;
            keys.insert(alert.dedupe_key());
        }
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, AlertStore) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = GaiaLoopConfig {
            console_dir: dir.path().join("gaia-console"),
            events_db: dir.path().join("events.db"),
            nats_url: crate::DEFAULT_NATS_URL.to_string(),
            http_bind: crate::DEFAULT_HTTP_BIND.to_string(),
            claude_bin: crate::DEFAULT_CLAUDE_BIN.into(),
            sentinel_ctl_bin: crate::DEFAULT_SENTINEL_CTL_BIN.into(),
            sentinel_gaia_bin: crate::DEFAULT_SENTINEL_GAIA_BIN.into(),
            model: None,
            max_budget_usd: crate::DEFAULT_MAX_BUDGET_USD,
            session_timeout_secs: crate::DEFAULT_SESSION_TIMEOUT_SECS,
            max_turns: crate::DEFAULT_MAX_TURNS,
        };
        (dir, AlertStore::from_config(&cfg))
    }

    fn alert(source_event_id: &str) -> GaiaAlert {
        GaiaAlert {
            alert_id: format!("gaia-alert-{source_event_id}"),
            source_event_id: source_event_id.to_string(),
            tick: 7,
            timestamp_ms: 42,
            trigger: "unresolved_escalation".to_string(),
            severity: "warning".to_string(),
            target: "system".to_string(),
            summary: "summary".to_string(),
            recommendation: "recommendation".to_string(),
            unresolved_keys: vec!["projection_lag".to_string()],
        }
    }

    #[test]
    fn appends_alerts_and_loads_dedupe_keys() {
        let (_dir, store) = store();
        store.append_alert(&alert("event-1")).unwrap();
        store.append_alert(&alert("event-2")).unwrap();

        let keys = store.load_alert_dedupe_keys().unwrap();
        assert!(keys.contains("platform_analysis:event-1"));
        assert!(keys.contains("platform_analysis:event-2"));
    }

    #[test]
    fn saves_state_atomically() {
        let (_dir, store) = store();
        let state = GaiaLoopState {
            last_event_row_id: 11,
            alerts_created: 3,
            last_alert_timestamp_ms: Some(99),
        };
        store.save_state(&state).unwrap();

        assert_eq!(store.load_state().unwrap(), state);
    }
}
