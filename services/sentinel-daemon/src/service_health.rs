//! Asynchroner Service-Health-Checker.
//!
//! Separater Thread der periodisch systemd Services prueft.
//! Ergebnisse werden non-blocking via mpsc Channel an den Tick-Loop geliefert.

use std::panic::{self, AssertUnwindSafe};
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
    control_tx: mpsc::Sender<ServiceHealthControl>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceHealthControl {
    PanicTest,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceHealthWorkerExit {
    ResultChannelClosed,
    ControlChannelClosed,
    ShutdownRequested,
}

impl ServiceHealthChecker {
    /// Spawnt den Health-Check Thread.
    ///
    /// Der Thread prueft alle `check_interval` die gegebenen Services
    /// und sendet die Liste der failed Services ueber den Channel.
    pub fn spawn(services: Vec<String>, check_interval: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        let (control_tx, control_rx) = mpsc::channel();
        let worker_state = Arc::new(RwLock::new(ServiceHealthWorkerSnapshot {
            running: false,
            restart_count: 0,
            last_error: None,
            thread_name: "service-health-checker".to_string(),
        }));
        let thread_state = Arc::clone(&worker_state);

        std::thread::Builder::new()
            .name("service-health-checker".into())
            .spawn(move || loop {
                if let Ok(mut state) = thread_state.write() {
                    state.running = true;
                    state.thread_name = "service-health-checker".to_string();
                }

                let worker_run = panic::catch_unwind(AssertUnwindSafe(|| {
                    run_service_health_worker(&services, check_interval, &tx, &control_rx)
                }));

                match worker_run {
                    Ok(
                        ServiceHealthWorkerExit::ResultChannelClosed
                        | ServiceHealthWorkerExit::ControlChannelClosed
                        | ServiceHealthWorkerExit::ShutdownRequested,
                    ) => {
                        if let Ok(mut state) = thread_state.write() {
                            state.running = false;
                        }
                        break;
                    }
                    Err(payload) => {
                        let error = panic_payload_to_string(payload);
                        warn!(
                            error = %error,
                            "service-health-checker panicked, respawn im selben Daemon-Prozess"
                        );
                        if let Ok(mut state) = thread_state.write() {
                            state.running = false;
                            state.restart_count = state.restart_count.saturating_add(1);
                            state.last_error = Some(error);
                        }
                    }
                }
            })
            .expect("service-health-checker Thread starten");

        Self {
            rx,
            worker_state,
            control_tx,
        }
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

    pub fn trigger_panic_test(&self) -> bool {
        self.control_tx
            .send(ServiceHealthControl::PanicTest)
            .is_ok()
    }
}

impl Drop for ServiceHealthChecker {
    fn drop(&mut self) {
        let _ = self.control_tx.send(ServiceHealthControl::Shutdown);
    }
}

fn run_service_health_worker(
    services: &[String],
    check_interval: Duration,
    tx: &mpsc::Sender<Vec<String>>,
    control_rx: &mpsc::Receiver<ServiceHealthControl>,
) -> ServiceHealthWorkerExit {
    loop {
        let mut failed = Vec::new();

        for service in services {
            let active = is_service_active(service);
            if !active {
                warn!(
                    service = %service,
                    "Service nicht active — Observation an Platform-Controlplane gemeldet"
                );
                failed.push(service.clone());
            }
        }

        if tx.send(failed).is_err() {
            return ServiceHealthWorkerExit::ResultChannelClosed;
        }

        match control_rx.recv_timeout(check_interval) {
            Ok(ServiceHealthControl::PanicTest) => {
                panic!("panic-test requested for service_health");
            }
            Ok(ServiceHealthControl::Shutdown) => {
                return ServiceHealthWorkerExit::ShutdownRequested;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return ServiceHealthWorkerExit::ControlChannelClosed;
            }
        }
    }
}

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_string(),
            Err(_) => "service-health-checker panic".to_string(),
        },
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
            control_tx: mpsc::channel().0,
        };
        assert!(checker.poll_failed().is_empty());
    }

    #[test]
    fn test_poll_failed_returns_latest() {
        let (tx, rx) = mpsc::channel();
        let checker = ServiceHealthChecker {
            rx,
            worker_state: Arc::new(RwLock::new(ServiceHealthWorkerSnapshot::default())),
            control_tx: mpsc::channel().0,
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
            control_tx: mpsc::channel().0,
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
            control_tx: mpsc::channel().0,
        };
        let state = checker.worker_state();
        assert!(!state.running);
        assert_eq!(state.restart_count, 0);
        assert!(state.last_error.is_none());
    }

    #[test]
    fn panic_test_restarts_worker_in_process() {
        let checker = ServiceHealthChecker::spawn(Vec::new(), Duration::from_millis(10));

        for _ in 0..50 {
            if checker.worker_state().running {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(checker.worker_state().running);

        assert!(checker.trigger_panic_test());

        let mut recovered = false;
        for _ in 0..100 {
            let state = checker.worker_state();
            if state.running && state.restart_count >= 1 {
                recovered = true;
                assert_eq!(state.thread_name, "service-health-checker");
                assert!(state
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.contains("panic-test requested")));
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(
            recovered,
            "service-health worker wurde nach panic-test nicht neu gestartet"
        );
    }
}
