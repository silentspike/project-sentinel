//! Konfiguration fuer den Projection Worker.

use std::time::Duration;

/// Konfiguration fuer den ProjectionWorker.
pub struct ProjectionConfig {
    /// Poll-Intervall wenn keine neuen Events vorliegen. Default: 50ms.
    pub poll_interval: Duration,
    /// Anzahl Events pro Batch. Default: 100.
    pub batch_size: usize,
    /// Pfad zur Read-Model SQLite-Datenbank. Default: "data/projection.db".
    pub db_path: String,
}

impl Default for ProjectionConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(50),
            batch_size: 100,
            db_path: "data/projection.db".to_string(),
        }
    }
}
