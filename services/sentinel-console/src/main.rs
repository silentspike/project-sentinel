//! `sentinel-console` — WebTransport/QUIC push server for the CAS console data-plane (#439).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use sentinel_console::{run_ingest, run_server, SharedPlane};
use sentinel_console_plane::ConsolePlane;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let port: u16 = std::env::var("SENTINEL_CONSOLE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4433);
    let events_db = std::env::var("SENTINEL_EVENTS_DB")
        .unwrap_or_else(|_| "/opt/sentinel/data/events.db".to_string());

    let plane: SharedPlane = Arc::new(Mutex::new(ConsolePlane::new()));

    // Ingest from the event store on a dedicated thread (sync EventStore).
    {
        let plane = plane.clone();
        std::thread::spawn(move || run_ingest(plane, events_db, Duration::from_millis(500)));
    }

    // QUIC server on the tokio runtime.
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(run_server(plane, port))
}
