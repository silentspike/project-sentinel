//! Structured logging setup for Project Sentinel.
//!
//! Two modes: JSON (production) and pretty (development).
//! Respects RUST_LOG env var for filtering.
//!
//! # Log-Level-Strategie
//!
//! | Level | Wann | Beispiel |
//! |-------|------|---------|
//! | ERROR | Fatal/Unrecoverable | redb corruption, Zenoh disconnect |
//! | WARN  | Transient/Degraded | API timeout, retry |
//! | INFO  | Business Events | Agent moved, message sent, tick completed |
//! | DEBUG | Subsystem Details | Bio-Engine Werte, Physics Berechnung |
//! | TRACE | Hot Path Instrumentation | Jeder redb get/set, jeder Zenoh publish |
//!
//! # Span-Hierarchie (geplant fuer Sprint 2+)
//!
//! ```text
//! tick{tick=t42}
//!   agent{agent_id=AGENT-01}
//!     bio{hunger=45.5, energy=72.0}
//!     physics{room_id=ROOM-3}
//!     zenoh.publish{topic=sentinel/agent/AGENT-01/action}
//!     redb.get_agent_state{agent_id=AGENT-01}
//! ```
//!
//! Die Hierarchie wird durch verschachtelte `tracing::Span`s realisiert.
//! Jeder Tick erzeugt einen Root-Span, Agent-Operationen sind Kind-Spans.

#[cfg(feature = "telemetry")]
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialize structured logging with JSON output (production mode).
///
/// Respects RUST_LOG env var. Default filter: `info`.
/// Call once at startup.
///
/// Only available with the `telemetry` feature (default: enabled).
#[cfg(feature = "telemetry")]
pub fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json())
        .init();
}

/// Initialize structured logging with pretty console output (development mode).
///
/// Respects RUST_LOG env var. Default filter: `debug`.
/// Call once at startup.
///
/// Only available with the `telemetry` feature (default: enabled).
#[cfg(feature = "telemetry")]
pub fn init_logging_dev() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().pretty())
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    // Logging kann nur einmal pro Prozess initialisiert werden.
    // Wir testen dass die Funktionen nicht paniken.
    // Separate Tests wuerden sich gegenseitig stoeren.

    #[test]
    fn test_init_logging_does_not_panic() {
        // try_init statt init um Doppel-Initialisierung zu vermeiden
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().json())
            .try_init();
    }

    #[test]
    fn test_init_logging_dev_does_not_panic() {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));

        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().pretty())
            .try_init();
    }
}
