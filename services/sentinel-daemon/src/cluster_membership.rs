//! Cluster membership wiring (#495): publish this node's heartbeat over Zenoh and
//! track the cluster's liveness in a shared [`MembershipView`] (V13).
//!
//! The service runs only when `[daemon.cluster]` is configured. Cross-node
//! connectivity uses Zenoh's default LAN peer discovery (the test nodes share a
//! subnet); an explicit seed `connect` endpoint is a robustness follow-up. The
//! view is **liveness only** (V2) — it is not a voting/owner/schedulable set.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sentinel_common::{Heartbeat, MembershipView, NodeId, NodeIdentity};
use sentinel_zenoh::SentinelBus;
use tracing::{debug, info, warn};

const MEMBERSHIP_PREFIX: &str = "sentinel/cluster/membership";

/// The Zenoh topic a node publishes its own heartbeat on.
pub fn membership_topic(node_id: &NodeId) -> String {
    format!("{MEMBERSHIP_PREFIX}/{node_id}")
}

fn encode_heartbeat(hb: &Heartbeat) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(hb)?)
}

fn decode_heartbeat(bytes: &[u8]) -> anyhow::Result<Heartbeat> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Run the membership service: spawn a subscribe task (ingest peer heartbeats) and
/// drive this node's publish + tick cadence. Loops until the bus subscription
/// closes; intended to be `tokio::spawn`ed for the daemon's lifetime.
pub async fn run_cluster_membership(
    bus: SentinelBus,
    identity: NodeIdentity,
    view: Arc<Mutex<MembershipView>>,
    heartbeat_interval: Duration,
) -> anyhow::Result<()> {
    let start = Instant::now();
    let self_id = identity.node_id;

    // Subscribe to every node's membership topic (one chunk after the prefix).
    let subscriber = bus
        .subscribe(&format!("{MEMBERSHIP_PREFIX}/*"))
        .await
        .map_err(|e| anyhow::anyhow!("membership subscribe: {e}"))?;
    let view_rx = view.clone();
    tokio::spawn(async move {
        info!("Cluster 12: Membership-Subscriber aktiv");
        while let Ok(sample) = subscriber.recv_async().await {
            let bytes = sample.payload().to_bytes();
            match decode_heartbeat(bytes.as_ref()) {
                Ok(hb) if hb.node_id != self_id => {
                    let now = start.elapsed().as_millis() as u64;
                    if let Ok(mut v) = view_rx.lock() {
                        let outcome = v.ingest(&hb, now);
                        debug!(node_id = %hb.node_id, ?outcome, "membership heartbeat ingested");
                    }
                }
                Ok(_) => {} // our own heartbeat, ignore
                Err(e) => warn!(error = %e, "membership heartbeat decode failed"),
            }
        }
        warn!("Cluster 12: Membership-Subscriber beendet");
    });

    // Publish our heartbeat + advance the liveness view on a fixed cadence.
    let topic = membership_topic(&identity.node_id);
    let mut incarnation: u64 = 0;
    let mut ticker = tokio::time::interval(heartbeat_interval);
    info!(node_id = %identity.node_id, alias = %identity.alias, "Cluster 12: Membership-Heartbeat gestartet");
    loop {
        ticker.tick().await;
        let hb = Heartbeat {
            node_id: identity.node_id,
            alias: identity.alias.clone(),
            boot_id: identity.boot_id,
            incarnation,
            endpoints: Vec::new(),
        };
        match encode_heartbeat(&hb) {
            Ok(bytes) => {
                if let Err(e) = bus.publish(&topic, &bytes).await {
                    warn!(error = %e, "membership heartbeat publish failed");
                }
            }
            Err(e) => warn!(error = %e, "membership heartbeat encode failed"),
        }
        let now = start.elapsed().as_millis() as u64;
        if let Ok(mut v) = view.lock() {
            v.tick(now);
        }
        incarnation = incarnation.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn heartbeat_encode_decode_roundtrip() {
        let hb = Heartbeat {
            node_id: NodeId::new(),
            alias: "test-node-0".into(),
            boot_id: Uuid::new_v4(),
            incarnation: 3,
            endpoints: vec!["10.0.0.241".into()],
        };
        let bytes = encode_heartbeat(&hb).unwrap();
        let back = decode_heartbeat(&bytes).unwrap();
        assert_eq!(hb, back);
    }

    #[test]
    fn topic_includes_node_id() {
        let id = NodeId::new();
        assert_eq!(
            membership_topic(&id),
            format!("sentinel/cluster/membership/{id}")
        );
    }
}
