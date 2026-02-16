//! Persistente Job-Queue fuer Nightrun Crash-Recovery.
//!
//! Tracks welche Agents bereits konsolidiert wurden, damit nach einem
//! Crash der Run fortgesetzt werden kann (--resume).

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

/// Status eines Konsolidierungs-Jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Skipped,
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Skipped => write!(f, "skipped"),
        }
    }
}

impl FromStr for JobStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "skipped" => Ok(Self::Skipped),
            other => anyhow::bail!("Unknown JobStatus: {other}"),
        }
    }
}

/// Einzelner Job-Eintrag.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct JobEntry {
    pub run_id: String,
    pub agent_name: String,
    pub status: JobStatus,
    pub error: Option<String>,
    pub episodes_processed: u32,
    pub episodes_consolidated: u32,
}

/// Zusammenfassung eines Runs.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct RunSummary {
    pub total: u32,
    pub completed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub pending: u32,
    pub total_episodes: u32,
}

/// Persistente Job-Queue backed by SQLite.
pub struct JobQueue {
    conn: Connection,
}

impl JobQueue {
    pub fn open(path: &str) -> Result<Self> {
        let conn =
            Connection::open(path).with_context(|| format!("Failed to open job queue: {path}"))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS nightrun_jobs (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 run_id TEXT NOT NULL,
                 agent_name TEXT NOT NULL,
                 status TEXT NOT NULL DEFAULT 'pending',
                 error TEXT,
                 episodes_processed INTEGER DEFAULT 0,
                 episodes_consolidated INTEGER DEFAULT 0,
                 started_at INTEGER,
                 completed_at INTEGER,
                 created_at INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_nightrun_run_status
                 ON nightrun_jobs(run_id, status);",
        )
        .context("Failed to initialize job queue schema")?;

        Ok(Self { conn })
    }

    /// Erstellt einen neuen Run mit allen zu konsolidierenden Agents.
    pub fn create_run(&self, run_id: &str, agents: &[String]) -> Result<()> {
        let now = now_secs();
        let mut stmt = self.conn.prepare(
            "INSERT INTO nightrun_jobs (run_id, agent_name, status, created_at) VALUES (?1, ?2, 'pending', ?3)",
        )?;
        for agent in agents {
            stmt.execute(params![run_id, agent, now])?;
        }
        Ok(())
    }

    /// Gibt alle ausstehenden Jobs fuer einen Run zurueck.
    pub fn get_pending(&self, run_id: &str) -> Result<Vec<JobEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, agent_name, status, error, episodes_processed, episodes_consolidated
             FROM nightrun_jobs WHERE run_id = ?1 AND status IN ('pending', 'in_progress')
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
            let status_str: String = row.get(2)?;
            Ok(JobEntry {
                run_id: row.get(0)?,
                agent_name: row.get(1)?,
                status: status_str
                    .parse::<JobStatus>()
                    .unwrap_or(JobStatus::Pending),
                error: row.get(3)?,
                episodes_processed: row.get(4)?,
                episodes_consolidated: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Sucht nach einem unvollstaendigen Run (fuer --resume).
    pub fn get_incomplete_run(&self) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT run_id FROM nightrun_jobs
             WHERE status IN ('pending', 'in_progress')
             ORDER BY id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Markiert einen Job als in_progress.
    pub fn mark_in_progress(&self, run_id: &str, agent: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE nightrun_jobs SET status = 'in_progress', started_at = ?3
             WHERE run_id = ?1 AND agent_name = ?2 AND status = 'pending'",
            params![run_id, agent, now_secs()],
        )?;
        Ok(())
    }

    /// Markiert einen Job als completed.
    pub fn mark_completed(
        &self,
        run_id: &str,
        agent: &str,
        episodes_processed: u32,
        episodes_consolidated: u32,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE nightrun_jobs SET status = 'completed', completed_at = ?3,
             episodes_processed = ?4, episodes_consolidated = ?5
             WHERE run_id = ?1 AND agent_name = ?2",
            params![
                run_id,
                agent,
                now_secs(),
                episodes_processed,
                episodes_consolidated
            ],
        )?;
        Ok(())
    }

    /// Markiert einen Job als failed.
    pub fn mark_failed(&self, run_id: &str, agent: &str, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE nightrun_jobs SET status = 'failed', error = ?3, completed_at = ?4
             WHERE run_id = ?1 AND agent_name = ?2",
            params![run_id, agent, error, now_secs()],
        )?;
        Ok(())
    }

    /// Markiert einen Job als skipped.
    pub fn mark_skipped(&self, run_id: &str, agent: &str, reason: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE nightrun_jobs SET status = 'skipped', error = ?3, completed_at = ?4
             WHERE run_id = ?1 AND agent_name = ?2",
            params![run_id, agent, reason, now_secs()],
        )?;
        Ok(())
    }

    /// Zusammenfassung eines Runs.
    #[allow(dead_code)]
    pub fn get_summary(&self, run_id: &str) -> Result<RunSummary> {
        let mut summary = RunSummary::default();
        let mut stmt = self.conn.prepare(
            "SELECT status, COUNT(*), COALESCE(SUM(episodes_processed), 0)
             FROM nightrun_jobs WHERE run_id = ?1 GROUP BY status",
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, u32>(2)?,
            ))
        })?;
        for row in rows {
            let (status, count, episodes) = row?;
            summary.total += count;
            summary.total_episodes += episodes;
            match status.as_str() {
                "completed" => summary.completed = count,
                "failed" => summary.failed = count,
                "skipped" => summary.skipped = count,
                "pending" | "in_progress" => summary.pending += count,
                _ => {}
            }
        }
        Ok(summary)
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_queue() -> JobQueue {
        JobQueue::open(":memory:").unwrap()
    }

    #[test]
    fn create_and_list_pending() {
        let q = temp_queue();
        let agents = vec!["Thomas".into(), "Lisa".into(), "Max".into()];
        q.create_run("run-1", &agents).unwrap();
        let pending = q.get_pending("run-1").unwrap();
        assert_eq!(pending.len(), 3);
        assert_eq!(pending[0].agent_name, "Thomas");
        assert_eq!(pending[0].status, JobStatus::Pending);
    }

    #[test]
    fn mark_progress_transitions() {
        let q = temp_queue();
        q.create_run("run-1", &["Thomas".into()]).unwrap();

        q.mark_in_progress("run-1", "Thomas").unwrap();
        let pending = q.get_pending("run-1").unwrap();
        assert_eq!(pending[0].status, JobStatus::InProgress);

        q.mark_completed("run-1", "Thomas", 10, 5).unwrap();
        let pending = q.get_pending("run-1").unwrap();
        assert!(pending.is_empty()); // completed is not pending
    }

    #[test]
    fn resume_finds_incomplete() {
        let q = temp_queue();
        q.create_run("run-1", &["Thomas".into(), "Lisa".into()])
            .unwrap();
        q.mark_completed("run-1", "Thomas", 5, 3).unwrap();

        let incomplete = q.get_incomplete_run().unwrap();
        assert_eq!(incomplete.as_deref(), Some("run-1"));

        // Complete all
        q.mark_completed("run-1", "Lisa", 8, 4).unwrap();
        let incomplete = q.get_incomplete_run().unwrap();
        assert!(incomplete.is_none());
    }

    #[test]
    fn summary_counts() {
        let q = temp_queue();
        let agents: Vec<String> = vec!["A".into(), "B".into(), "C".into(), "D".into()];
        q.create_run("run-1", &agents).unwrap();
        q.mark_completed("run-1", "A", 10, 5).unwrap();
        q.mark_completed("run-1", "B", 8, 4).unwrap();
        q.mark_failed("run-1", "C", "timeout").unwrap();
        q.mark_skipped("run-1", "D", "backlog").unwrap();

        let s = q.get_summary("run-1").unwrap();
        assert_eq!(s.total, 4);
        assert_eq!(s.completed, 2);
        assert_eq!(s.failed, 1);
        assert_eq!(s.skipped, 1);
        assert_eq!(s.pending, 0);
        assert_eq!(s.total_episodes, 18);
    }

    #[test]
    fn mark_failed_stores_error() {
        let q = temp_queue();
        q.create_run("run-1", &["Thomas".into()]).unwrap();
        q.mark_in_progress("run-1", "Thomas").unwrap();
        q.mark_failed("run-1", "Thomas", "redb write error")
            .unwrap();

        let mut stmt = q
            .conn
            .prepare("SELECT error FROM nightrun_jobs WHERE agent_name = 'Thomas'")
            .unwrap();
        let error: String = stmt.query_row([], |r| r.get(0)).unwrap();
        assert_eq!(error, "redb write error");
    }
}
