//! Zenoh Pub/Sub integration for Project Sentinel.
//!
//! Provides the central communication bus (SentinelBus) that connects
//! all components: ECS kernel, Cortex Gateway, Dashboard, Monitoring.

pub mod topics;

use sentinel_common::{AgentId, RoomId, Tick};
use tracing::{info, instrument};
use zenoh::handlers::FifoChannelHandler;
use zenoh::pubsub::Subscriber;
use zenoh::sample::Sample;
use zenoh::Session;

/// Type alias for the default Zenoh subscriber (FIFO handler).
pub type BusSubscriber = Subscriber<FifoChannelHandler<Sample>>;

/// Central communication bus wrapping a Zenoh session.
///
/// SentinelBus is Clone (backed by Arc internally) and can be shared
/// across tasks freely.
#[derive(Clone)]
pub struct SentinelBus {
    session: Session,
}

impl SentinelBus {
    /// Create a new SentinelBus with default Zenoh config.
    /// SHM is prepared but not activated (needs runtime validation first).
    #[instrument(name = "SentinelBus::new", level = "debug")]
    pub async fn new() -> anyhow::Result<Self> {
        let config = zenoh::Config::default();
        // TODO: SHM activation after runtime validation
        // config.insert_json5("transport/shared_memory/enabled", "true")?;
        let session = zenoh::open(config)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to open Zenoh session: {e}"))?;
        info!("SentinelBus: Zenoh session opened");
        Ok(Self { session })
    }

    /// Publish a message to a topic.
    #[instrument(skip(self, payload), level = "trace", fields(topic = %topic))]
    pub async fn publish(&self, topic: &str, payload: &[u8]) -> anyhow::Result<()> {
        self.session
            .put(topic, payload)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to publish to {topic}: {e}"))?;
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
        info!("SentinelBus: Subscribed to {topic}");
        Ok(subscriber)
    }

    /// Publish an agent action.
    #[instrument(skip(self, payload), level = "trace", fields(agent_id = %agent_id))]
    pub async fn publish_action(
        &self,
        agent_id: AgentId,
        payload: &[u8],
    ) -> anyhow::Result<()> {
        self.publish(&topics::agent_action(&agent_id.to_string()), payload)
            .await
    }

    /// Subscribe to an agent's perception channel.
    #[instrument(skip(self), level = "debug", fields(agent_id = %agent_id))]
    pub async fn subscribe_perception(
        &self,
        agent_id: AgentId,
    ) -> anyhow::Result<BusSubscriber> {
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
    /// Uses raw numeric tick value for compact topic paths (e.g. sentinel/physics/tick/42).
    #[instrument(skip(self, payload), level = "trace", fields(tick = %tick))]
    pub async fn publish_tick(
        &self,
        tick: Tick,
        payload: &[u8],
    ) -> anyhow::Result<()> {
        self.publish(&topics::physics_tick(tick.0), payload)
            .await
    }

    /// Publish a chaos event.
    #[instrument(skip(self, payload), level = "trace")]
    pub async fn publish_chaos_event(&self, payload: &[u8]) -> anyhow::Result<()> {
        self.publish(topics::CHAOS_EVENT, payload).await
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
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            subscriber.recv_async(),
        )
        .await;

        assert!(result.is_ok(), "Should receive message within timeout");
        let sample = result.unwrap().unwrap();
        assert_eq!(sample.payload().to_bytes().as_ref(), payload);
    }
}
