//! Cluster membership: a lightweight heartbeat-based liveness view (V13).
//!
//! Each node periodically publishes a [`Heartbeat`] (over Zenoh — the transport is
//! wired in a later step); a [`MembershipView`] ingests them and tracks each node's
//! liveness (`Alive → Suspect → Dead`, plus an explicit `Left`).
//!
//! **Membership reports liveness only** — it never decides ownership (V2): "Alive"
//! implies nothing about voting / owner / schedulable. The owner registry and the
//! `VotingConfig` are separate (Track D). N-node-native (`NodeId`-keyed).
//!
//! Time is the **receiver's** monotonic clock (`now_ms`), passed in explicitly so
//! the view is deterministic and unit-testable; remote timestamps are not trusted.

use crate::cluster::NodeId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Liveness state of a node as seen by the membership view (V2/V13). Liveness
/// only — never an ownership/voting signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MembershipState {
    /// Heard from recently.
    Alive,
    /// Missed heartbeats — suspected down, not yet confirmed.
    Suspect,
    /// Confirmed not heard from (a crash/partition — NOT an ownership decision).
    Dead,
    /// Gracefully left (explicit decommission), distinct from a crash (`Dead`).
    Left,
}

/// A liveness heartbeat published by a node (the Zenoh wire message). Carries the
/// ABA guards `boot_id` (fresh per process boot) + `incarnation` (monotonic within
/// a boot). It deliberately carries **no trusted wall-clock** — recency is the
/// receiver's local clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub node_id: NodeId,
    pub alias: String,
    pub boot_id: Uuid,
    pub incarnation: u64,
    /// Reachable endpoints (QUIC control / Zenoh). Informational for the view.
    #[serde(default)]
    pub endpoints: Vec<String>,
}

/// The membership record the view keeps per node (V13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeMembership {
    pub node_id: NodeId,
    pub alias: String,
    pub boot_id: Uuid,
    pub incarnation: u64,
    /// Receiver-stamped monotonic time (ms) of the last **accepted** heartbeat.
    pub last_seen_ms: u64,
    pub state: MembershipState,
    pub endpoints: Vec<String>,
}

/// TTLs for the liveness state machine (receiver clock, milliseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MembershipConfig {
    /// No accepted heartbeat for this long → `Suspect`.
    pub suspect_after_ms: u64,
    /// `Suspect` for this much longer → `Dead`.
    pub dead_after_ms: u64,
}

impl Default for MembershipConfig {
    fn default() -> Self {
        // Heartbeat ~1 Hz; suspect after ~3 missed, dead after ~6 more.
        Self {
            suspect_after_ms: 3_000,
            dead_after_ms: 6_000,
        }
    }
}

/// Outcome of ingesting a heartbeat (observability + tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// First observation of this node.
    Joined,
    /// Same boot, advanced (newer/equal incarnation).
    Updated,
    /// A new `boot_id` → the node restarted (fresh session).
    Restarted,
    /// Rejected as stale (older incarnation within the same boot — ABA guard).
    RejectedStale,
}

/// The chef's membership view: `NodeId → record` plus the liveness state machine.
#[derive(Debug, Clone, Default)]
pub struct MembershipView {
    config: MembershipConfig,
    nodes: HashMap<NodeId, NodeMembership>,
}

impl MembershipView {
    pub fn new(config: MembershipConfig) -> Self {
        Self {
            config,
            nodes: HashMap::new(),
        }
    }

    /// Ingest a heartbeat received at `now_ms` (the receiver's monotonic clock).
    ///
    /// ABA handling (V13): within a boot, a stale (older) incarnation is rejected;
    /// a different `boot_id` is treated as a restart that supersedes the record.
    /// The cross-boot edge (a delayed heartbeat of an *older* boot arriving after a
    /// restart) is a known limitation — the full guard (boot epoch / SWIM
    /// incarnation refutation) is Track-D hardening, not this lightweight view.
    pub fn ingest(&mut self, hb: &Heartbeat, now_ms: u64) -> IngestOutcome {
        match self.nodes.get_mut(&hb.node_id) {
            None => {
                self.nodes.insert(
                    hb.node_id,
                    NodeMembership {
                        node_id: hb.node_id,
                        alias: hb.alias.clone(),
                        boot_id: hb.boot_id,
                        incarnation: hb.incarnation,
                        last_seen_ms: now_ms,
                        state: MembershipState::Alive,
                        endpoints: hb.endpoints.clone(),
                    },
                );
                IngestOutcome::Joined
            }
            Some(rec) => {
                if hb.boot_id != rec.boot_id {
                    rec.boot_id = hb.boot_id;
                    rec.incarnation = hb.incarnation;
                    rec.alias = hb.alias.clone();
                    rec.endpoints = hb.endpoints.clone();
                    rec.last_seen_ms = now_ms;
                    rec.state = MembershipState::Alive;
                    IngestOutcome::Restarted
                } else if hb.incarnation < rec.incarnation {
                    IngestOutcome::RejectedStale
                } else {
                    rec.incarnation = hb.incarnation;
                    rec.alias = hb.alias.clone();
                    rec.endpoints = hb.endpoints.clone();
                    rec.last_seen_ms = now_ms;
                    rec.state = MembershipState::Alive;
                    IngestOutcome::Updated
                }
            }
        }
    }

    /// Advance the liveness state machine to `now_ms` (`Alive → Suspect → Dead` by
    /// TTL since the last accepted heartbeat). `Left` is terminal.
    pub fn tick(&mut self, now_ms: u64) {
        let suspect = self.config.suspect_after_ms;
        let dead = self
            .config
            .suspect_after_ms
            .saturating_add(self.config.dead_after_ms);
        for rec in self.nodes.values_mut() {
            if matches!(rec.state, MembershipState::Left) {
                continue;
            }
            let since = now_ms.saturating_sub(rec.last_seen_ms);
            rec.state = if since >= dead {
                MembershipState::Dead
            } else if since >= suspect {
                MembershipState::Suspect
            } else {
                MembershipState::Alive
            };
        }
    }

    /// Mark a node as having gracefully left (decommission); terminal.
    pub fn mark_left(&mut self, node_id: &NodeId) -> bool {
        if let Some(rec) = self.nodes.get_mut(node_id) {
            rec.state = MembershipState::Left;
            true
        } else {
            false
        }
    }

    pub fn get(&self, node_id: &NodeId) -> Option<&NodeMembership> {
        self.nodes.get(node_id)
    }

    /// Nodes currently `Alive` (NOT a schedulable/voting set — V38; liveness only).
    pub fn alive(&self) -> Vec<&NodeMembership> {
        self.nodes
            .values()
            .filter(|n| n.state == MembershipState::Alive)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hb(node: NodeId, boot: Uuid, inc: u64) -> Heartbeat {
        Heartbeat {
            node_id: node,
            alias: "n".into(),
            boot_id: boot,
            incarnation: inc,
            endpoints: vec![],
        }
    }

    #[test]
    fn ingest_join_update_and_stale_within_boot() {
        let mut v = MembershipView::new(MembershipConfig::default());
        let n = NodeId::new();
        let boot = Uuid::new_v4();

        assert_eq!(v.ingest(&hb(n, boot, 0), 1000), IngestOutcome::Joined);
        assert_eq!(v.get(&n).unwrap().state, MembershipState::Alive);
        // Newer incarnation, same boot → updated.
        assert_eq!(v.ingest(&hb(n, boot, 2), 2000), IngestOutcome::Updated);
        assert_eq!(v.get(&n).unwrap().incarnation, 2);
        // Older incarnation, same boot → rejected, record unchanged.
        assert_eq!(
            v.ingest(&hb(n, boot, 1), 3000),
            IngestOutcome::RejectedStale
        );
        assert_eq!(v.get(&n).unwrap().incarnation, 2);
        assert_eq!(v.get(&n).unwrap().last_seen_ms, 2000);
    }

    #[test]
    fn different_boot_id_is_a_restart() {
        let mut v = MembershipView::new(MembershipConfig::default());
        let n = NodeId::new();
        let boot_a = Uuid::new_v4();
        let boot_b = Uuid::new_v4();
        v.ingest(&hb(n, boot_a, 5), 1000);
        assert_eq!(v.ingest(&hb(n, boot_b, 0), 2000), IngestOutcome::Restarted);
        assert_eq!(v.get(&n).unwrap().boot_id, boot_b);
        assert_eq!(v.get(&n).unwrap().incarnation, 0);
    }

    #[test]
    fn liveness_state_machine_suspect_then_dead_then_revive() {
        let cfg = MembershipConfig {
            suspect_after_ms: 3_000,
            dead_after_ms: 6_000,
        };
        let mut v = MembershipView::new(cfg);
        let n = NodeId::new();
        let boot = Uuid::new_v4();
        v.ingest(&hb(n, boot, 0), 0);

        v.tick(2_000);
        assert_eq!(v.get(&n).unwrap().state, MembershipState::Alive);
        v.tick(4_000); // >= 3s since last_seen
        assert_eq!(v.get(&n).unwrap().state, MembershipState::Suspect);
        v.tick(10_000); // >= 9s since last_seen
        assert_eq!(v.get(&n).unwrap().state, MembershipState::Dead);

        // A fresh heartbeat revives it.
        v.ingest(&hb(n, boot, 1), 10_500);
        assert_eq!(v.get(&n).unwrap().state, MembershipState::Alive);
        assert_eq!(v.alive().len(), 1);
    }

    #[test]
    fn left_is_terminal() {
        let mut v = MembershipView::new(MembershipConfig::default());
        let n = NodeId::new();
        v.ingest(&hb(n, Uuid::new_v4(), 0), 0);
        assert!(v.mark_left(&n));
        v.tick(1_000_000); // would otherwise be Dead
        assert_eq!(v.get(&n).unwrap().state, MembershipState::Left);
        assert!(v.alive().is_empty());
        // Unknown node → false.
        assert!(!v.mark_left(&NodeId::new()));
    }

    #[test]
    fn alive_set_excludes_non_alive() {
        let mut v = MembershipView::new(MembershipConfig::default());
        let a = NodeId::new();
        let b = NodeId::new();
        v.ingest(&hb(a, Uuid::new_v4(), 0), 0);
        v.ingest(&hb(b, Uuid::new_v4(), 0), 0);
        v.tick(100); // both Alive
        assert_eq!(v.alive().len(), 2);
        v.mark_left(&b);
        assert_eq!(v.alive().len(), 1);
        assert_eq!(v.len(), 2);
    }
}
