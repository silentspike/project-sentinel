//! Cluster membership wiring (#495): exchange liveness heartbeats over the existing
//! cert-pinned QUIC control plane and track peers in a shared [`MembershipView`].
//!
//! Zenoh remains daemon-local. Cross-node membership uses only explicit
//! `[daemon.cluster].control_peers`, preserving the QUIC trust boundary and avoiding
//! LAN discovery. Membership remains liveness-only: it never grants ownership,
//! voting, or scheduling authority.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sentinel_cluster_control::{
    AuthenticatedPeer, ControlHandler, ControlRequest, ControlResponse,
};
use sentinel_common::{
    Heartbeat, IngestOutcome, MembershipConfig, MembershipState, MembershipView, NodeId,
    NodeIdentity,
};
use tracing::{debug, info};
use uuid::Uuid;

use crate::cluster_control::ClusterControl;

/// A membership state change observed by the receiver's monotonic TTL clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembershipTransition {
    pub node_id: NodeId,
    pub alias: String,
    pub previous: MembershipState,
    pub current: MembershipState,
}

/// Shared membership state and monotonic clock used by both the inbound QUIC handler
/// and the periodic TTL ticker.
pub struct MembershipRuntime {
    started_at: Instant,
    view: Mutex<MembershipView>,
}

impl MembershipRuntime {
    pub fn new(config: MembershipConfig) -> Self {
        Self {
            started_at: Instant::now(),
            view: Mutex::new(MembershipView::new(config)),
        }
    }

    fn now_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    pub fn ingest(&self, heartbeat: &Heartbeat) -> (IngestOutcome, Option<MembershipState>) {
        self.ingest_at(heartbeat, self.now_ms())
    }

    fn ingest_at(
        &self,
        heartbeat: &Heartbeat,
        now_ms: u64,
    ) -> (IngestOutcome, Option<MembershipState>) {
        let mut view = self
            .view
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = view.get(&heartbeat.node_id).map(|record| record.state);
        let outcome = view.ingest(heartbeat, now_ms);
        (outcome, previous)
    }

    pub fn tick(&self) -> Vec<MembershipTransition> {
        self.tick_at(self.now_ms())
    }

    fn tick_at(&self, now_ms: u64) -> Vec<MembershipTransition> {
        let mut view = self
            .view
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before: HashMap<NodeId, MembershipState> = view
            .records()
            .map(|record| (record.node_id, record.state))
            .collect();
        view.tick(now_ms);
        view.records()
            .filter_map(|record| {
                let previous = before.get(&record.node_id).copied()?;
                (previous != record.state).then(|| MembershipTransition {
                    node_id: record.node_id,
                    alias: record.alias.clone(),
                    previous,
                    current: record.state,
                })
            })
            .collect()
    }

    fn state(&self, node_id: &NodeId) -> Option<MembershipState> {
        self.view
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(node_id)
            .map(|record| record.state)
    }

    pub fn is_alive(&self, node_id: &NodeId) -> bool {
        self.state(node_id) == Some(MembershipState::Alive)
    }
}

/// QUIC handler wrapper that owns membership heartbeats and delegates every other
/// control request to the existing owner/block-map handler chain.
pub struct QuicMembershipHandler<H> {
    cluster_id: Uuid,
    local_node_id: NodeId,
    runtime: Arc<MembershipRuntime>,
    inner: H,
}

impl<H> QuicMembershipHandler<H> {
    pub fn new(
        cluster_id: Uuid,
        local_node_id: NodeId,
        runtime: Arc<MembershipRuntime>,
        inner: H,
    ) -> Self {
        Self {
            cluster_id,
            local_node_id,
            runtime,
            inner,
        }
    }
}

impl<H: ControlHandler> ControlHandler for QuicMembershipHandler<H> {
    fn handle(&self, peer: AuthenticatedPeer, request: &ControlRequest) -> ControlResponse {
        let ControlRequest::MembershipHeartbeat {
            cluster_id,
            heartbeat,
        } = request
        else {
            return self.inner.handle(peer, request);
        };

        if *cluster_id != self.cluster_id {
            return ControlResponse::Rejected {
                reason: "membership heartbeat cluster_id mismatch".into(),
            };
        }
        if heartbeat.node_id == self.local_node_id {
            return ControlResponse::Rejected {
                reason: "membership heartbeat claims the local node_id".into(),
            };
        }
        if heartbeat.node_id != peer.node_id {
            return ControlResponse::Rejected {
                reason: format!(
                    "membership node_id {} does not match authenticated peer {}",
                    heartbeat.node_id, peer.node_id
                ),
            };
        }

        let (outcome, previous) = self.runtime.ingest(heartbeat);
        if outcome == IngestOutcome::RejectedStale {
            return ControlResponse::Rejected {
                reason: "stale membership heartbeat incarnation".into(),
            };
        }

        if previous != Some(MembershipState::Alive) {
            info!(
                node_id = %heartbeat.node_id,
                alias = %heartbeat.alias,
                ?previous,
                ?outcome,
                state = ?MembershipState::Alive,
                "Cluster 12: membership peer became Alive over QUIC"
            );
        } else {
            debug!(
                node_id = %heartbeat.node_id,
                incarnation = heartbeat.incarnation,
                "Cluster 12: membership heartbeat accepted over QUIC"
            );
        }

        ControlResponse::MembershipAccepted {
            node_id: heartbeat.node_id,
            incarnation: heartbeat.incarnation,
        }
    }
}

/// Publish this node's heartbeat to every explicitly pinned QUIC peer and advance
/// receiver-local TTL state. Runs for the daemon lifetime.
pub async fn run_cluster_membership(
    cluster_control: Arc<ClusterControl>,
    cluster_id: Uuid,
    identity: NodeIdentity,
    runtime: Arc<MembershipRuntime>,
    heartbeat_interval: Duration,
) {
    let mut incarnation = 0u64;
    let mut ticker = tokio::time::interval(heartbeat_interval);
    info!(
        node_id = %identity.node_id,
        alias = %identity.alias,
        "Cluster 12: QUIC membership heartbeat started"
    );

    loop {
        ticker.tick().await;

        for transition in runtime.tick() {
            info!(
                node_id = %transition.node_id,
                alias = %transition.alias,
                previous = ?transition.previous,
                current = ?transition.current,
                "Cluster 12: membership peer state changed"
            );
        }

        let heartbeat = Heartbeat {
            node_id: identity.node_id,
            alias: identity.alias.clone(),
            boot_id: identity.boot_id,
            incarnation,
            endpoints: Vec::new(),
        };
        let delivered = cluster_control
            .broadcast_membership_heartbeat(cluster_id, heartbeat)
            .await;
        debug!(
            incarnation,
            delivered, "Cluster 12: QUIC membership heartbeat round complete"
        );
        incarnation = incarnation.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_cluster_control::StubHandler;

    fn heartbeat(node_id: NodeId, boot_id: Uuid, incarnation: u64) -> Heartbeat {
        Heartbeat {
            node_id,
            alias: "peer".into(),
            boot_id,
            incarnation,
            endpoints: vec!["quic/peer:8085".into()],
        }
    }

    fn authenticated(node_id: NodeId) -> AuthenticatedPeer {
        AuthenticatedPeer {
            fingerprint: sentinel_cluster_control::CertFingerprint([2; 32]),
            node_id,
        }
    }

    #[test]
    fn quic_handler_accepts_only_its_cluster_and_remote_node() {
        let cluster_id = Uuid::new_v4();
        let local_node = NodeId::new();
        let peer_node = NodeId::new();
        let runtime = Arc::new(MembershipRuntime::new(MembershipConfig::default()));
        let handler =
            QuicMembershipHandler::new(cluster_id, local_node, Arc::clone(&runtime), StubHandler);
        let hb = heartbeat(peer_node, Uuid::new_v4(), 7);

        assert_eq!(
            handler.handle(
                authenticated(peer_node),
                &ControlRequest::MembershipHeartbeat {
                    cluster_id,
                    heartbeat: hb.clone(),
                }
            ),
            ControlResponse::MembershipAccepted {
                node_id: peer_node,
                incarnation: 7,
            }
        );
        assert_eq!(runtime.state(&peer_node), Some(MembershipState::Alive));

        assert!(matches!(
            handler.handle(
                authenticated(peer_node),
                &ControlRequest::MembershipHeartbeat {
                    cluster_id: Uuid::new_v4(),
                    heartbeat: hb,
                }
            ),
            ControlResponse::Rejected { .. }
        ));
        assert!(matches!(
            handler.handle(
                authenticated(peer_node),
                &ControlRequest::MembershipHeartbeat {
                    cluster_id,
                    heartbeat: heartbeat(local_node, Uuid::new_v4(), 0),
                }
            ),
            ControlResponse::Rejected { .. }
        ));
        assert!(matches!(
            handler.handle(
                authenticated(NodeId::new()),
                &ControlRequest::MembershipHeartbeat {
                    cluster_id,
                    heartbeat: heartbeat(peer_node, Uuid::new_v4(), 8),
                },
            ),
            ControlResponse::Rejected { reason } if reason.contains("authenticated peer")
        ));
    }

    #[test]
    fn ttl_transitions_to_suspect_dead_and_fresh_heartbeat_revives() {
        let runtime = MembershipRuntime::new(MembershipConfig {
            suspect_after_ms: 3_000,
            dead_after_ms: 6_000,
        });
        let node = NodeId::new();
        let boot = Uuid::new_v4();
        runtime.ingest_at(&heartbeat(node, boot, 0), 0);

        let suspect = runtime.tick_at(3_000);
        assert_eq!(suspect[0].previous, MembershipState::Alive);
        assert_eq!(suspect[0].current, MembershipState::Suspect);

        let dead = runtime.tick_at(9_000);
        assert_eq!(dead[0].previous, MembershipState::Suspect);
        assert_eq!(dead[0].current, MembershipState::Dead);

        let (_, previous) = runtime.ingest_at(&heartbeat(node, boot, 1), 9_500);
        assert_eq!(previous, Some(MembershipState::Dead));
        assert_eq!(runtime.state(&node), Some(MembershipState::Alive));
    }
}
