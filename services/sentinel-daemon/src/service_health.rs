//! Asynchroner Service-Health-Checker.
//!
//! Separater Thread der periodisch systemd Services prueft und bei Ausfall
//! automatisch restartet (max 3 Versuche). Ergebnisse werden non-blocking
//! via mpsc Channel an den Tick-Loop geliefert.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use tracing::{info, warn};

/// Non-blocking Service-Health-Checker.
///
/// Laeuft in einem separaten Thread, prueft Services via `systemctl is-active`
/// und restartet ausgefallene Services (max 3 Versuche pro Service).
pub struct ServiceHealthChecker {
    rx: mpsc::Receiver<Vec<String>>,
}

/// Max Restart-Versuche bevor nur noch Alert.
const MAX_RESTART_ATTEMPTS: u32 = 3;

impl ServiceHealthChecker {
    /// Spawnt den Health-Check Thread.
    ///
    /// Der Thread prueft alle `check_interval` die gegebenen Services
    /// und sendet die Liste der failed Services ueber den Channel.
    pub fn spawn(services: Vec<String>, check_interval: Duration) -> Self {
        let (tx, rx) = mpsc::channel();

        std::thread::Builder::new()
            .name("service-health-checker".into())
            .spawn(move || {
                let mut restart_counts: HashMap<String, u32> = HashMap::new();

                loop {
                    let mut failed = Vec::new();

                    for service in &services {
                        let active = is_service_active(service);
                        if !active {
                            let count = restart_counts.entry(service.clone()).or_insert(0);
                            if *count < MAX_RESTART_ATTEMPTS {
                                info!(service = %service, attempt = *count + 1,
                                    "Service nicht active — Restart wird versucht");
                                let restart_ok = restart_service(service);
                                *count += 1;
                                if restart_ok {
                                    info!(service = %service, "Service erfolgreich restartet");
                                } else {
                                    warn!(service = %service, "Service-Restart fehlgeschlagen");
                                }
                            } else {
                                warn!(service = %service, attempts = MAX_RESTART_ATTEMPTS,
                                    "Max Restart-Versuche erreicht — nur noch Alert");
                            }
                            failed.push(service.clone());
                        } else {
                            // Service laeuft → Counter zuruecksetzen
                            if restart_counts.remove(service).is_some() {
                                info!(service = %service, "Service wieder active — Counter zurueckgesetzt");
                            }
                        }
                    }

                    // Best-effort: Wenn Receiver gedroppt wurde, Thread beenden
                    if tx.send(failed).is_err() {
                        break;
                    }

                    std::thread::sleep(check_interval);
                }
            })
            .expect("service-health-checker Thread starten");

        Self { rx }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_poll_failed_empty_when_no_messages() {
        let (_tx, rx) = mpsc::channel::<Vec<String>>();
        let checker = ServiceHealthChecker { rx };
        assert!(checker.poll_failed().is_empty());
    }

    #[test]
    fn test_poll_failed_returns_latest() {
        let (tx, rx) = mpsc::channel();
        let checker = ServiceHealthChecker { rx };

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
        let checker = ServiceHealthChecker { rx };

        tx.send(vec!["sentinel-judge".into()]).unwrap();

        let result = checker.poll_failed();
        assert_eq!(result, vec!["sentinel-judge"]);
    }
}
