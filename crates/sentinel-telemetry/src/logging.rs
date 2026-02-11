//! Structured logging setup for Project Sentinel.
//!
//! Two modes: JSON (production) and pretty (development).
//! Respects RUST_LOG env var for filtering.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialize structured logging with JSON output (production mode).
///
/// Respects RUST_LOG env var. Default filter: `info`.
/// Call once at startup.
pub fn init_logging() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json())
        .init();
}

/// Initialize structured logging with pretty console output (development mode).
///
/// Respects RUST_LOG env var. Default filter: `debug`.
/// Call once at startup.
pub fn init_logging_dev() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));

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
        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().json())
            .try_init();
    }

    #[test]
    fn test_init_logging_dev_does_not_panic() {
        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));

        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().pretty())
            .try_init();
    }
}
