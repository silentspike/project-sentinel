use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::config::GaiaLoopConfig;
use crate::types::GaiaAlert;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GaiaLoopState {
    pub last_event_row_id: i64,
    pub alerts_created: u64,
    pub last_alert_timestamp_ms: Option<u64>,
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
            ensure_private_dir(parent)?;
        }
        ensure_private_dir(&self.sessions_dir)?;
        Ok(())
    }

    pub fn load_state(&self) -> Result<GaiaLoopState> {
        if !self.state_path.exists() {
            return Ok(GaiaLoopState::default());
        }
        let raw = read_locked(&self.state_path)?;
        serde_json::from_str(&raw).with_context(|| format!("parse {}", self.state_path.display()))
    }

    pub fn save_state(&self, state: &GaiaLoopState) -> Result<()> {
        self.ensure_layout()?;
        let tmp = self.state_path.with_extension("json.tmp");
        let raw = serde_json::to_vec_pretty(state).context("serialize Gaia loop state")?;
        write_private(&tmp, &raw)?;
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
        append_jsonl_locked(&self.alerts_path, alert)
    }

    pub fn load_alert_dedupe_keys(&self) -> Result<HashSet<String>> {
        if !self.alerts_path.exists() {
            return Ok(HashSet::new());
        }
        let mut keys = HashSet::new();
        for alert in read_jsonl_locked::<GaiaAlert>(&self.alerts_path)? {
            keys.insert(alert.dedupe_key());
        }
        Ok(keys)
    }
}

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("create private dir {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("chmod 0700 {}", path.display()))
}

pub fn harden_private_tree(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect private path {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!("refuse symlink in Gaia private tree: {}", path.display());
    }
    let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {mode:o} {}", path.display()))?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path).with_context(|| format!("read dir {}", path.display()))? {
            harden_private_tree(&entry?.path())?;
        }
    }
    Ok(())
}

pub fn create_private_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create private file {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))?;
    Ok(file)
}

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = create_private_file(path)?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))?;
    file.sync_data()
        .with_context(|| format!("sync {}", path.display()))
}

pub struct ExclusiveFileLock {
    file: File,
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn try_exclusive_file_lock(path: &Path) -> Result<Option<ExclusiveFileLock>> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open lock {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))?;
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(ExclusiveFileLock { file })),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error).with_context(|| format!("lock {}", path.display())),
    }
}

pub fn append_jsonl_locked<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let mut line = serde_json::to_vec(value).context("serialize JSONL entry")?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))?;
    FileExt::lock_exclusive(&file).with_context(|| format!("lock {}", path.display()))?;
    file.write_all(&line)
        .with_context(|| format!("append {}", path.display()))?;
    file.sync_data()
        .with_context(|| format!("sync {}", path.display()))?;
    FileExt::unlock(&file).with_context(|| format!("unlock {}", path.display()))
}

pub fn read_jsonl_locked<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    FileExt::lock_shared(&file).with_context(|| format!("lock {}", path.display()))?;
    let mut values = Vec::new();
    for (line_number, line) in BufReader::new(&file).lines().enumerate() {
        let line = line.with_context(|| format!("read {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        values.push(
            serde_json::from_str(&line)
                .with_context(|| format!("parse {} line {}", path.display(), line_number + 1))?,
        );
    }
    FileExt::unlock(&file).with_context(|| format!("unlock {}", path.display()))?;
    Ok(values)
}

fn read_locked(path: &Path) -> Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    FileExt::lock_shared(&file).with_context(|| format!("lock {}", path.display()))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .with_context(|| format!("read {}", path.display()))?;
    FileExt::unlock(&file).with_context(|| format!("unlock {}", path.display()))?;
    Ok(raw)
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
            company_context_path: crate::DEFAULT_COMPANY_CONTEXT_PATH.into(),
            model: None,
            max_budget_usd: crate::DEFAULT_MAX_BUDGET_USD,
            budget_window_secs: crate::DEFAULT_BUDGET_WINDOW_SECS,
            budget_window_usd: crate::DEFAULT_BUDGET_WINDOW_USD,
            session_timeout_secs: crate::DEFAULT_SESSION_TIMEOUT_SECS,
            readiness_scan_interval_secs: crate::DEFAULT_READINESS_SCAN_INTERVAL_SECS,
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

    #[test]
    fn concurrent_jsonl_appends_remain_complete_and_parseable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parallel.jsonl");
        let mut workers = Vec::new();
        for worker in 0..8_u32 {
            let path = path.clone();
            workers.push(std::thread::spawn(move || {
                for item in 0..25_u32 {
                    append_jsonl_locked(&path, &(worker, item)).unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }

        let mut values = read_jsonl_locked::<(u32, u32)>(&path).unwrap();
        values.sort_unstable();
        assert_eq!(values.len(), 200);
        assert_eq!(values.first(), Some(&(0, 0)));
        assert_eq!(values.last(), Some(&(7, 24)));
    }
}
