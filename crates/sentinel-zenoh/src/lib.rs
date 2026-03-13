//! Zenoh Pub/Sub integration for Project Sentinel.
//!
//! Provides the central communication bus (SentinelBus) that connects
//! all components: ECS kernel, Cortex Gateway, Dashboard, Monitoring.
//!
//! Features:
//! - SHM transport mit automatischem Fallback auf TCP (AC2)
//! - Scoped Queries mit UUIDv7, Deadline, min_tick Stale-Filter (AC1, AC3)
//! - In-Flight Limits: global=128, per-agent=8 (AC4)
//! - FlatBuffer-kompatible Payloads (AC5)

pub mod config;
pub mod flatbuf;
pub mod inflight;
pub mod query;
pub mod topics;

use std::sync::Arc;
use std::time::Duration;

use config::BusConfig;
use inflight::{InFlightError, InFlightTracker};
use query::{QueryResponse, QueryScope, ScopedQuery};
use sentinel_common::{AgentId, RoomId, Tick};
use tracing::{info, instrument, warn};
use zenoh::handlers::FifoChannelHandler;
use zenoh::pubsub::Subscriber;
use zenoh::sample::Sample;
use zenoh::Session;

/// Histogram bucket boundaries for Zenoh operation latencies (microseconds).
const LATENCY_BUCKETS: &[f64] = &[10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0, 10000.0];

/// Type alias for the default Zenoh subscriber (FIFO handler).
pub type BusSubscriber = Subscriber<FifoChannelHandler<Sample>>;

/// Transport-Modus des Zenoh-Bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    /// Shared Memory transport (niedrige Latenz, lokal).
    Shm,
    /// Standard TCP/Unix-Socket transport.
    Network,
}

/// Central communication bus wrapping a Zenoh session.
///
/// SentinelBus is Clone (backed by Arc internally) and can be shared
/// across tasks freely.
#[derive(Clone)]
pub struct SentinelBus {
    session: Session,
    config: Arc<BusConfig>,
    transport_mode: TransportMode,
    inflight: Arc<InFlightTracker>,
}

impl SentinelBus {
    /// Create a new SentinelBus with config from environment variables.
    ///
    /// Bei aktiviertem SHM (`SENTINEL_ZENOH_SHM=true`) wird zuerst SHM versucht.
    /// Bei Fehler automatischer Fallback auf Network-Transport (AC2).
    #[instrument(name = "SentinelBus::new", level = "debug")]
    pub async fn new() -> anyhow::Result<Self> {
        let config = BusConfig::from_env();
        Self::with_config(config).await
    }

    /// Create a new SentinelBus with explicit config.
    pub async fn with_config(config: BusConfig) -> anyhow::Result<Self> {
        let (session, transport_mode) = Self::open_session(&config).await?;
        let inflight = Arc::new(InFlightTracker::new(
            config.max_inflight_global,
            config.max_inflight_per_agent,
        ));
        info!(
            "SentinelBus: session opened, transport={:?}, inflight_global={}, inflight_per_agent={}",
            transport_mode, config.max_inflight_global, config.max_inflight_per_agent
        );
        Ok(Self {
            session,
            config: Arc::new(config),
            transport_mode,
            inflight,
        })
    }

    /// Oeffnet Zenoh Session mit SHM-Fallback (AC2).
    async fn open_session(config: &BusConfig) -> anyhow::Result<(Session, TransportMode)> {
        if config.shm_enabled {
            let mut shm_config = zenoh::Config::default();
            if let Err(e) = shm_config.insert_json5("transport/shared_memory/enabled", "true") {
                warn!("SentinelBus: SHM config insertion failed ({e}), using network transport");
            } else {
                match zenoh::open(shm_config).await {
                    Ok(session) => {
                        info!("SentinelBus: SHM transport active");
                        return Ok((session, TransportMode::Shm));
                    }
                    Err(e) => {
                        warn!(
                            "SentinelBus: SHM session open failed ({e}), falling back to network"
                        );
                        #[cfg(feature = "telemetry")]
                        {
                            sentinel_telemetry::MetricsRegistry::global()
                                .counter("sentinel.zenoh.shm_fallback.count")
                                .increment();
                        }
                    }
                }
            }
        }
        // Fallback: Standard Network-Transport
        let default_config = zenoh::Config::default();
        let session = zenoh::open(default_config)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to open Zenoh session: {e}"))?;
        Ok((session, TransportMode::Network))
    }

    /// Aktueller Transport-Modus (SHM oder Network).
    pub fn transport_mode(&self) -> TransportMode {
        self.transport_mode
    }

    /// Referenz auf die Bus-Konfiguration.
    pub fn config(&self) -> &BusConfig {
        &self.config
    }

    /// Referenz auf den InFlightTracker.
    pub fn inflight(&self) -> &InFlightTracker {
        &self.inflight
    }

    /// Publish a message to a topic.
    #[instrument(skip(self, payload), level = "trace", fields(topic = %topic))]
    pub async fn publish(&self, topic: &str, payload: &[u8]) -> anyhow::Result<()> {
        let start = std::time::Instant::now();
        self.session
            .put(topic, payload)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to publish to {topic}: {e}"))?;
        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.counter("sentinel.zenoh.publish.count").increment();
            reg.histogram("sentinel.zenoh.publish.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
        }
        Ok(())
    }

    /// Subscribe to a topic. Returns a Subscriber that yields samples
    /// via `recv_async().await`.
    #[instrument(skip(self), level = "debug", fields(topic = %topic))]
    pub async fn subscribe(&self, topic: &str) -> anyhow::Result<BusSubscriber> {
        let subscriber = self
            .session
            .declare_subscriber(topic)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to {topic}: {e}"))?;
        #[cfg(feature = "telemetry")]
        {
            sentinel_telemetry::MetricsRegistry::global()
                .counter("sentinel.zenoh.subscribe.count")
                .increment();
        }
        info!("SentinelBus: Subscribed to {topic}");
        Ok(subscriber)
    }

    /// Publish an agent action.
    #[instrument(skip(self, payload), level = "trace", fields(agent_id = %agent_id))]
    pub async fn publish_action(&self, agent_id: AgentId, payload: &[u8]) -> anyhow::Result<()> {
        self.publish(&topics::agent_action(&agent_id.to_string()), payload)
            .await
    }

    /// Subscribe to an agent's perception channel.
    #[instrument(skip(self), level = "debug", fields(agent_id = %agent_id))]
    pub async fn subscribe_perception(&self, agent_id: AgentId) -> anyhow::Result<BusSubscriber> {
        self.subscribe(&topics::agent_perception(&agent_id.to_string()))
            .await
    }

    /// Publish a room event (audio, smell, or presence).
    #[instrument(skip(self, payload), level = "trace", fields(room_id = %room_id, event_type = %event_type))]
    pub async fn publish_room_event(
        &self,
        room_id: RoomId,
        event_type: &str,
        payload: &[u8],
    ) -> anyhow::Result<()> {
        let topic = format!("{}/room/{room_id}/{event_type}", topics::PREFIX);
        self.publish(&topic, payload).await
    }

    /// Publish a global simulation tick.
    #[instrument(skip(self, payload), level = "trace", fields(tick = %tick))]
    pub async fn publish_tick(&self, tick: Tick, payload: &[u8]) -> anyhow::Result<()> {
        self.publish(&topics::physics_tick(tick.0), payload).await
    }

    /// Publish a chaos event.
    #[instrument(skip(self, payload), level = "trace")]
    pub async fn publish_chaos_event(&self, payload: &[u8]) -> anyhow::Result<()> {
        self.publish(topics::CHAOS_EVENT, payload).await
    }

    /// Sende eine Scoped Query und warte auf Response mit Deadline.
    ///
    /// Gibt `None` zurueck wenn Deadline ueberschritten oder Query gecancelt.
    /// Stale Responses (response_tick < min_tick) werden automatisch verworfen (AC3).
    /// In-Flight Limits werden via InFlightTracker erzwungen (AC4).
    #[instrument(skip(self, query), level = "debug", fields(
        query_id = %query.query_id,
        agent_id = %query.origin_agent,
        scope = ?query.scope,
        deadline_ms = query.deadline_ms,
        min_tick = query.min_tick,
    ))]
    pub async fn scoped_query(&self, query: ScopedQuery) -> anyhow::Result<Option<QueryResponse>> {
        let start = std::time::Instant::now();
        let deadline = Duration::from_millis(query.deadline_ms);
        let query_id = query.query_id;
        let min_tick = query.min_tick;
        let agent_id = query.origin_agent.0;

        // In-Flight Slot akquirieren (Backpressure, AC4)
        let guard = self
            .inflight
            .try_acquire(query_id, agent_id, min_tick)
            .await
            .map_err(|e| match e {
                InFlightError::GlobalCapacity => {
                    anyhow::anyhow!(
                        "global in-flight capacity exceeded ({})",
                        self.config.max_inflight_global
                    )
                }
                InFlightError::AgentCapacity(id) => {
                    anyhow::anyhow!(
                        "per-agent in-flight capacity exceeded for agent {id} ({})",
                        self.config.max_inflight_per_agent
                    )
                }
            })?;

        #[cfg(feature = "telemetry")]
        {
            sentinel_telemetry::MetricsRegistry::global()
                .gauge("sentinel.zenoh.query.inflight.gauge")
                .set(self.inflight.active_count_sync() as i64);
        }

        // Query auf Request-Topic publishen
        let request_topic = match &query.scope {
            QueryScope::Agent(id) => topics::query_request_agent(&id.to_string()),
            QueryScope::Room(room) => topics::query_request_room(room),
            QueryScope::Global => topics::QUERY_REQUEST_GLOBAL.to_string(),
        };
        let payload = serde_json::to_vec(&query)
            .map_err(|e| anyhow::anyhow!("Failed to serialize query: {e}"))?;
        self.publish(&request_topic, &payload).await?;

        // Auf Response-Topic subscriben (per-Agent fuer Effizienz)
        let response_topic = topics::query_response_agent(&query.origin_agent.to_string());
        let subscriber = self.subscribe(&response_topic).await?;

        // Warten mit Timeout (Query-Cancellation, AC3)
        let result = tokio::time::timeout(deadline, async {
            loop {
                let sample = subscriber
                    .recv_async()
                    .await
                    .map_err(|e| anyhow::anyhow!("Recv failed: {e}"))?;
                let response: QueryResponse =
                    serde_json::from_slice(sample.payload().to_bytes().as_ref())
                        .map_err(|e| anyhow::anyhow!("Failed to deserialize response: {e}"))?;

                // Nur Responses fuer diese Query akzeptieren
                if response.query_id != query_id {
                    continue;
                }

                // Stale-Response-Filter (AC3): response_tick muss >= min_tick sein
                if response.is_stale(min_tick) {
                    #[cfg(feature = "telemetry")]
                    {
                        sentinel_telemetry::MetricsRegistry::global()
                            .counter("sentinel.zenoh.query.stale_drop.count")
                            .increment();
                    }
                    tracing::debug!(
                        query_id = %query_id,
                        response_tick = response.response_tick,
                        min_tick = min_tick,
                        "Stale response dropped"
                    );
                    continue;
                }

                return Ok::<_, anyhow::Error>(Some(response));
            }
        })
        .await;

        // Guard droppen → Slot freigeben
        drop(guard);

        #[cfg(feature = "telemetry")]
        {
            let reg = sentinel_telemetry::MetricsRegistry::global();
            reg.histogram("sentinel.zenoh.query.duration_us", LATENCY_BUCKETS)
                .observe(start.elapsed().as_micros() as f64);
            reg.gauge("sentinel.zenoh.query.inflight.gauge")
                .set(self.inflight.active_count_sync() as i64);
        }

        match result {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Timeout: Query cancelled
                #[cfg(feature = "telemetry")]
                {
                    sentinel_telemetry::MetricsRegistry::global()
                        .counter("sentinel.zenoh.query.timeout.count")
                        .increment();
                }
                tracing::debug!(query_id = %query_id, "Query timed out after {}ms", query.deadline_ms);
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_pub_sub_roundtrip() {
        let bus = SentinelBus::new().await.expect("Failed to create bus");
        let subscriber = bus
            .subscribe("sentinel/test/roundtrip")
            .await
            .expect("Failed to subscribe");

        // Delay for subscription propagation within the Zenoh session
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let payload = b"hello sentinel";
        bus.publish("sentinel/test/roundtrip", payload)
            .await
            .expect("Failed to publish");

        // Receive with timeout
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(5), subscriber.recv_async()).await;

        assert!(result.is_ok(), "Should receive message within timeout");
        let sample = result.unwrap().unwrap();
        assert_eq!(sample.payload().to_bytes().as_ref(), payload);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_transport_mode_default_is_network() {
        let bus = SentinelBus::new().await.expect("Failed to create bus");
        // Ohne SENTINEL_ZENOH_SHM=true sollte Network-Modus gewaehlt werden
        assert_eq!(bus.transport_mode(), TransportMode::Network);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_config_accessible() {
        let bus = SentinelBus::new().await.expect("Failed to create bus");
        assert_eq!(bus.config().max_inflight_global, 128);
        assert_eq!(bus.config().max_inflight_per_agent, 8);
    }
}
