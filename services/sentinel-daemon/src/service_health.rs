//! Asynchroner Service-Health-Checker.
//!
//! Separater Thread der periodisch systemd Services prueft.
//! Ergebnisse werden non-blocking via mpsc Channel an den Tick-Loop geliefert.

use std::sync::{mpsc, Arc, RwLock};
use std::time::Duration;

use tracing::warn;

#[derive(Debug, Clone, Default)]
pub struct ServiceHealthWorkerSnapshot {
    pub running: bool,
    pub restart_count: u64,
    pub last_error: Option<String>,
    pub thread_name: String,
}

/// Non-blocking Service-Health-Checker.
///
/// Laeuft in einem separaten Thread und prueft Services via `systemctl is-active`.
/// Restart-Entscheidungen passieren deterministisch im Platform-Controlplane.
pub struct ServiceHealthChecker {
    rx: mpsc::Receiver<Vec<String>>,
    worker_state: Arc<RwLock<ServiceHealthWorkerSnapshot>>,
}

impl ServiceHealthChecker {
    /// Spawnt den Health-Check Thread.
    ///
    /// Der Thread prueft alle `check_interval` die gegebenen Services
    /// und sendet die Liste der failed Services ueber den Channel.
    pub fn spawn(services: Vec<String>, check_interval: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        let worker_state = Arc::new(RwLock::new(ServiceHealthWorkerSnapshot {
            running: false,
            restart_count: 0,
            last_error: None,
            thread_name: "service-health-checker".to_string(),
        }));
        let thread_state = Arc::clone(&worker_state);

        std::thread::Builder::new()
            .name("service-health-checker".into())
            .spawn(move || {
                if let Ok(mut state) = thread_state.write() {
                    state.running = true;
                }
                loop {
                    let mut failed = Vec::new();

                    for service in &services {
                        let active = is_service_active(service);
                        if !active {
                            warn!(
                                service = %service,
                                "Service nicht active — Observation an Platform-Controlplane gemeldet"
                            );
                            failed.push(service.clone());
                        }
                    }

                    // Best-effort: Wenn Receiver gedroppt wurde, Thread beenden
                    if tx.send(failed).is_err() {
                        if let Ok(mut state) = thread_state.write() {
                            state.running = false;
                        }
                        break;
                    }

                    std::thread::sleep(check_interval);
                }
            })
            .expect("service-health-checker Thread starten");

        Self { rx, worker_state }
    }

    /// Non-blocking Poll: Gibt die zuletzt bekannten failed Services zurueck.
    ///
    /// Konsumiert alle aufgestauten Nachrichten und gibt die letzte zurueck.
    /// Leerer Vec wenn keine Nachrichten vorhanden oder alle Services aktiv.
    pub fn poll_failed(&self) -> Vec<String> {
        let mut last = Vec::new();
        while let Ok(failed) = self.rx.try_recv() {
            last = failed;
        }
        last
    }

    pub fn worker_state(&self) -> ServiceHealthWorkerSnapshot {
        self.worker_state
            .read()
            .map(|state| state.clone())
            .unwrap_or_default()
    }
}

/// Prueft ob ein systemd Service active ist.
fn is_service_active(service_name: &str) -> bool {
    std::process::Command::new("systemctl")
        .args(["is-active", "--quiet", service_name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Restartet einen systemd Service. Daemon laeuft als root → kein sudo noetig.
fn restart_service(service_name: &str) -> bool {
    std::process::Command::new("systemctl")
        .args(["restart", service_name])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn restart_service_now(service_name: &str) -> bool {
    restart_service(service_name)
}

pub fn is_service_active_now(service_name: &str) -> bool {
    is_service_active(service_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_poll_failed_empty_when_no_messages() {
        let (_tx, rx) = mpsc::channel::<Vec<String>>();
        let checker = ServiceHealthChecker {
            rx,
            worker_state: Arc::new(RwLock::new(ServiceHealthWorkerSnapshot::default())),
        };
        assert!(checker.poll_failed().is_empty());
    }

    #[test]
    fn test_poll_failed_returns_latest() {
        let (tx, rx) = mpsc::channel();
        let checker = ServiceHealthChecker {
            rx,
            worker_state: Arc::new(RwLock::new(ServiceHealthWorkerSnapshot::default())),
        };

        // Mehrere Nachrichten senden
        tx.send(vec!["service-a".into()]).unwrap();
        tx.send(vec!["service-b".into()]).unwrap();
        tx.send(vec![]).unwrap(); // Letzte: alles OK

        // poll_failed() gibt die LETZTE zurueck
        let result = checker.poll_failed();
        assert!(result.is_empty());
    }

    #[test]
    fn test_poll_failed_returns_failed_services() {
        let (tx, rx) = mpsc::channel();
        let checker = ServiceHealthChecker {
            rx,
            worker_state: Arc::new(RwLock::new(ServiceHealthWorkerSnapshot::default())),
        };

        tx.send(vec!["sentinel-judge".into()]).unwrap();

        let result = checker.poll_failed();
        assert_eq!(result, vec!["sentinel-judge"]);
    }

    #[test]
    fn worker_state_defaults_to_stopped_snapshot() {
        let (_tx, rx) = mpsc::channel::<Vec<String>>();
        let checker = ServiceHealthChecker {
            rx,
            worker_state: Arc::new(RwLock::new(ServiceHealthWorkerSnapshot::default())),
        };
        let state = checker.worker_state();
        assert!(!state.running);
        assert_eq!(state.restart_count, 0);
        assert!(state.last_error.is_none());
    }
}
