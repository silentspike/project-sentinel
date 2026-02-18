//! Graceful Shutdown via SIGINT/SIGTERM.

use tokio::signal::unix::SignalKind;

/// Wartet auf SIGINT (Ctrl+C) oder SIGTERM.
/// Kehrt zurueck sobald eines der Signale empfangen wird.
pub async fn wait_for_shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut sigterm =
        tokio::signal::unix::signal(SignalKind::terminate()).expect("SIGTERM handler registrieren");

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("SIGINT empfangen, fahre herunter...");
        }
        _ = sigterm.recv() => {
            tracing::info!("SIGTERM empfangen, fahre herunter...");
        }
    }
}
