//! Background-Task der Outbox-Eintraege pollt und via Transport publiziert.
//!
//! Verbindet den append-only EventStore (SQLite) mit einem externen
//! Pub/Sub-System (z.B. Zenoh). Polling-basiert mit konfigurierbarem
//! Intervall und Batch-Groesse.
//!
//! Graceful Shutdown via `tokio::sync::watch` Channel.

use std::time::Duration;

use tracing::{debug, info, warn};

use crate::event_store::{EventStore, OutboxTransport};

/// Default-Polling-Intervall in Millisekunden.
const DEFAULT_POLL_INTERVAL_MS: u64 = 100;

/// Default Batch-Groesse pro Poll-Zyklus.
const DEFAULT_BATCH_SIZE: usize = 50;

/// Konfiguration fuer den OutboxPublisher.
#[derive(Debug, Clone)]
pub struct OutboxPublisherConfig {
    /// Polling-Intervall zwischen Outbox-Abfragen.
    pub poll_interval: Duration,
    /// Maximale Anzahl Eintraege pro Poll-Zyklus.
    pub batch_size: usize,
}

impl Default for OutboxPublisherConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(DEFAULT_POLL_INTERVAL_MS),
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }
}

impl OutboxPublisherConfig {
    /// Liest Konfiguration aus Umgebungsvariablen.
    ///
    /// - `SENTINEL_OUTBOX_POLL_INTERVAL_MS` (default: 100)
    /// - `SENTINEL_OUTBOX_BATCH_SIZE` (default: 50)
    pub fn from_env() -> Self {
        let poll_interval_ms: u64 = std::env::var("SENTINEL_OUTBOX_POLL_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_POLL_INTERVAL_MS);

        let batch_size: usize = std::env::var("SENTINEL_OUTBOX_BATCH_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BATCH_SIZE);

        Self {
            poll_interval: Duration::from_millis(poll_interval_ms),
            batch_size,
        }
    }
}

/// Background-Publisher der Outbox-Events an einen Transport weiterleitet.
///
/// Pollt den EventStore in regelmaessigen Intervallen nach pending Outbox-Eintraegen,
/// publiziert sie via Transport und markiert sie als published.
///
/// Fehlgeschlagene Publishes bleiben als pending erhalten und werden beim
/// naechsten Zyklus erneut versucht (at-least-once Semantik).
pub struct OutboxPublisher<T: OutboxTransport> {
    store: EventStore,
    transport: T,
    config: OutboxPublisherConfig,
}

/// Statistiken eines einzelnen Poll-Zyklus.
#[derive(Debug, Default)]
pub struct PublishCycleStats {
    /// Anzahl erfolgreich publizierter Eintraege.
    pub published: usize,
    /// Anzahl fehlgeschlagener Publishes (bleiben pending).
    pub failed: usize,
}

impl<T: OutboxTransport> OutboxPublisher<T> {
    /// Erstellt einen neuen OutboxPublisher.
    pub fn new(store: EventStore, transport: T, config: OutboxPublisherConfig) -> Self {
        Self {
            store,
            transport,
            config,
        }
    }

    /// Startet die Publish-Loop bis ein Shutdown-Signal empfangen wird.
    ///
    /// Blockiert den aktuellen Task. Typische Nutzung:
    /// ```ignore
    /// let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    /// tokio::spawn(async move { publisher.run(shutdown_rx).await });
    /// // Spaeter: shutdown_tx.send(true) zum Beenden
    /// ```
    pub async fn run(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        info!(
            poll_interval_ms = self.config.poll_interval.as_millis() as u64,
            batch_size = self.config.batch_size,
            "outbox publisher started"
        );

        loop {
            tokio::select! {
                () = tokio::time::sleep(self.config.poll_interval) => {
                    let stats = self.process_batch().await;
                    if stats.published > 0 || stats.failed > 0 {
                        debug!(
                            published = stats.published,
                            failed = stats.failed,
                            "outbox publish cycle completed"
                        );
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("outbox publisher shutting down");
                        // Drain remaining entries before exit
                        let stats = self.process_batch().await;
                        if stats.published > 0 {
                            info!(published = stats.published, "drained remaining outbox entries");
                        }
                        break;
                    }
                }
            }
        }
    }

    /// Verarbeitet einen Batch von Outbox-Eintraegen.
    ///
    /// Kann auch direkt aufgerufen werden (ohne run-Loop) fuer Tests
    /// oder einmalige Verarbeitung.
    pub async fn process_batch(&self) -> PublishCycleStats {
        let mut stats = PublishCycleStats::default();

        let entries = match self.store.poll_outbox(self.config.batch_size) {
            Ok(entries) => entries,
            Err(e) => {
                warn!(error = %e, "outbox poll failed");
                return stats;
            }
        };

        if entries.is_empty() {
            return stats;
        }

        for entry in &entries {
            match self
                .transport
                .publish(&entry.topic, entry.payload.as_bytes())
                .await
            {
                Ok(()) => {
                    if let Err(e) = self.store.mark_published(&entry.event_id) {
                        warn!(
                            outbox_id = entry.id,
                            event_id = %entry.event_id,
                            error = %e,
                            "failed to mark outbox entry as published"
                        );
                        stats.failed += 1;
                    } else {
                        stats.published += 1;
                    }
                }
                Err(e) => {
                    warn!(
                        outbox_id = entry.id,
                        event_id = %entry.event_id,
                        topic = %entry.topic,
                        error = %e,
                        "outbox publish failed, will retry next cycle"
                    );
                    stats.failed += 1;
                }
            }
        }

        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            for _ in 0..stats.published {
                reg.counter("sentinel.limbo.outbox.publish.count")
                    .increment();
            }
            for _ in 0..stats.failed {
                reg.counter("sentinel.limbo.outbox.publish.errors")
                    .increment();
            }
        }

        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EventStore;
    use sentinel_common::DomainEvent;
    use std::sync::{Arc, Mutex};

    type PublishedPayloads = Vec<(String, Vec<u8>)>;

    /// Mock-Transport der published Payloads sammelt.
    #[derive(Clone, Default)]
    struct MockTransport {
        published: Arc<Mutex<PublishedPayloads>>,
        fail_next: Arc<Mutex<bool>>,
    }

    impl OutboxTransport for MockTransport {
        async fn publish(&self, topic: &str, payload: &[u8]) -> anyhow::Result<()> {
            if *self.fail_next.lock().unwrap() {
                *self.fail_next.lock().unwrap() = false;
                return Err(anyhow::anyhow!("simulated transport failure"));
            }
            self.published
                .lock()
                .unwrap()
                .push((topic.to_string(), payload.to_vec()));
            Ok(())
        }
    }

    fn test_event(i: u64) -> DomainEvent {
        DomainEvent::new(
            "test_event",
            &format!("AGG-{i}"),
            &format!("{{\"seq\": {i}}}"),
            "corr-001",
            i,
        )
    }

    #[tokio::test]
    async fn test_process_batch_publishes_pending_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("test.db").to_str().unwrap()).unwrap();
        let transport = MockTransport::default();

        // 3 Events mit Outbox einfuegen
        for i in 0..3 {
            store
                .append_with_outbox(&test_event(i), &format!("topic/test/{i}"))
                .unwrap();
        }

        let publisher = OutboxPublisher::new(
            store.clone(),
            transport.clone(),
            OutboxPublisherConfig {
                batch_size: 10,
                ..Default::default()
            },
        );

        let stats = publisher.process_batch().await;

        assert_eq!(stats.published, 3);
        assert_eq!(stats.failed, 0);

        // Transport hat alle 3 empfangen
        let published = transport.published.lock().unwrap();
        assert_eq!(published.len(), 3);
        assert_eq!(published[0].0, "topic/test/0");
        assert_eq!(published[1].0, "topic/test/1");
        assert_eq!(published[2].0, "topic/test/2");

        // Outbox ist leer (alle published)
        let remaining = store.poll_outbox(10).unwrap();
        assert!(remaining.is_empty(), "outbox should be empty after publish");
    }

    #[tokio::test]
    async fn test_process_batch_retries_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("test.db").to_str().unwrap()).unwrap();
        let transport = MockTransport::default();

        store
            .append_with_outbox(&test_event(0), "topic/retry")
            .unwrap();

        // Ersten Publish fehlschlagen lassen
        *transport.fail_next.lock().unwrap() = true;

        let publisher = OutboxPublisher::new(
            store.clone(),
            transport.clone(),
            OutboxPublisherConfig::default(),
        );

        // Erster Versuch: fehlgeschlagen
        let stats = publisher.process_batch().await;
        assert_eq!(stats.published, 0);
        assert_eq!(stats.failed, 1);

        // Entry bleibt pending
        let pending = store.poll_outbox(10).unwrap();
        assert_eq!(pending.len(), 1, "failed entry should remain pending");

        // Zweiter Versuch: erfolgreich (fail_next wurde zurueckgesetzt)
        let stats = publisher.process_batch().await;
        assert_eq!(stats.published, 1);
        assert_eq!(stats.failed, 0);

        // Jetzt leer
        let remaining = store.poll_outbox(10).unwrap();
        assert!(remaining.is_empty());
    }

    #[tokio::test]
    async fn test_process_batch_empty_outbox() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("test.db").to_str().unwrap()).unwrap();
        let transport = MockTransport::default();

        let publisher =
            OutboxPublisher::new(store, transport.clone(), OutboxPublisherConfig::default());

        let stats = publisher.process_batch().await;
        assert_eq!(stats.published, 0);
        assert_eq!(stats.failed, 0);
        assert!(transport.published.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_run_with_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("test.db").to_str().unwrap()).unwrap();
        let transport = MockTransport::default();

        store
            .append_with_outbox(&test_event(0), "topic/shutdown")
            .unwrap();

        let publisher = OutboxPublisher::new(
            store.clone(),
            transport.clone(),
            OutboxPublisherConfig {
                poll_interval: Duration::from_millis(10),
                batch_size: 10,
            },
        );

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Publisher in Background starten
        let handle = tokio::spawn(async move {
            publisher.run(shutdown_rx).await;
        });

        // Kurz warten damit mindestens ein Poll-Zyklus durchlaeuft
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Shutdown senden
        shutdown_tx.send(true).unwrap();
        handle.await.unwrap();

        // Event sollte publiziert worden sein
        let published = transport.published.lock().unwrap();
        assert!(
            !published.is_empty(),
            "should have published at least one entry"
        );
        assert_eq!(published[0].0, "topic/shutdown");
    }

    #[tokio::test]
    async fn test_batch_size_respected() {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path().join("test.db").to_str().unwrap()).unwrap();
        let transport = MockTransport::default();

        // 5 Events einfuegen
        for i in 0..5 {
            store
                .append_with_outbox(&test_event(i), &format!("topic/batch/{i}"))
                .unwrap();
        }

        let publisher = OutboxPublisher::new(
            store.clone(),
            transport.clone(),
            OutboxPublisherConfig {
                batch_size: 2, // Nur 2 pro Zyklus
                ..Default::default()
            },
        );

        // Erster Zyklus: 2 von 5
        let stats = publisher.process_batch().await;
        assert_eq!(stats.published, 2);

        // Zweiter Zyklus: 2 von 3
        let stats = publisher.process_batch().await;
        assert_eq!(stats.published, 2);

        // Dritter Zyklus: 1 von 1
        let stats = publisher.process_batch().await;
        assert_eq!(stats.published, 1);

        // Vierter Zyklus: leer
        let stats = publisher.process_batch().await;
        assert_eq!(stats.published, 0);

        assert_eq!(transport.published.lock().unwrap().len(), 5);
    }

    #[test]
    fn test_config_from_env_defaults() {
        // Ohne ENV-Variablen: Defaults
        let config = OutboxPublisherConfig::default();
        assert_eq!(config.poll_interval, Duration::from_millis(100));
        assert_eq!(config.batch_size, 50);
    }
}
