//! Konfiguration fuer den Projection Worker.

use std::path::Path;
use std::time::Duration;

/// Konfiguration fuer den ProjectionWorker.
pub struct ProjectionConfig {
    /// Poll-Intervall wenn keine neuen Events vorliegen. Default: 50ms.
    pub poll_interval: Duration,
    /// Anzahl Events pro Batch. Default: 100.
    pub batch_size: usize,
    /// Pfad zur Read-Model SQLite-Datenbank. Default: "data/projection.db".
    pub db_path: String,
    /// Dateipfad fuer Full-Rebuild-Requests aus daemon-seitigen Repair-Pfaden.
    pub rebuild_request_path: String,
    /// Intervall fuer das Polling der Rebuild-Request-Datei.
    pub rebuild_request_poll_interval: Duration,
}

impl Default for ProjectionConfig {
    fn default() -> Self {
        let db_path = "data/projection.db".to_string();
        let rebuild_request_path = Path::new(&db_path)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".projection-rebuild-request")
            .to_string_lossy()
            .to_string();
        Self {
            poll_interval: Duration::from_millis(50),
            batch_size: 100,
            db_path,
            rebuild_request_path,
            rebuild_request_poll_interval: Duration::from_secs(1),
        }
    }
}
