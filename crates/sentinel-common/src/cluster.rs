//! Cluster identity & lifecycle types for the TOGAF Cluster-12 N-node platform.
//!
//! Foundation for Genesis (`docs/adr/ADR-0495-G-GENESIS-first-seed-bootstrap.md`)
//! and `ProvisionNode` (`docs/adr/ADR-0495-G3-provisionnode-threat-model.md`).
//! All types are **N-node-native** (`NodeId`-keyed sets/maps, never a hard
//! source/target pair as the cluster model) — two nodes are the first test, not
//! the ceiling.
//!
//! In Track A these are **identity + config only**: the `[daemon.cluster]` section
//! is absent by default, so a daemon without it stays single-node (the current
//! production behavior). Membership, the owner registry and `ProvisionNode` build
//! on these types in later steps.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable cluster-unique node identifier (V15) — **not** the legacy `u16` agent
/// alias. A node keeps its `NodeId` across reboots; `boot_id`/`incarnation` change.
///
/// `Ord`/`PartialOrd` (over the underlying `Uuid`'s total order) exist so N-node
/// sets/maps iterate deterministically — never rely on `HashMap` order in cluster
/// state (avoids order-dependent flakiness; same lesson as the determinism hash).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct NodeId(pub Uuid);

impl NodeId {
    /// Mint a fresh random node id (used at Genesis / provisioning).
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for NodeId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Full node identity (V15). The cluster never identifies a node by a bare `u16`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeIdentity {
    pub node_id: NodeId,
    pub alias: String,
    /// Pinned QUIC peer cert fingerprint (Track-A pinned-trust, V10/V35). `None`
    /// until a cert is issued for the node.
    #[serde(default)]
    pub cert_fingerprint: Option<[u8; 32]>,
    /// Fresh per process boot — guards against ABA (heartbeats of a previous boot
    /// must not overwrite newer state; V13/V16).
    pub boot_id: Uuid,
    /// Monotonic per node — the SWIM/membership incarnation (V13). Starts at 0.
    pub incarnation: u64,
}

impl NodeIdentity {
    /// Build the runtime identity from the persistent `[daemon.cluster]` config:
    /// the `node_id` and alias are stable, `boot_id` is fresh per boot, and the
    /// incarnation starts at 0 (membership advances it later).
    pub fn from_config(cfg: &ClusterConfig) -> Self {
        Self {
            node_id: cfg.node_id,
            alias: cfg.alias.clone().unwrap_or_else(|| cfg.node_id.to_string()),
            cert_fingerprint: None,
            boot_id: Uuid::new_v4(),
            incarnation: 0,
        }
    }
}

/// Node lifecycle (G-N0 object model / G-D2 lifecycle). `GenesisSeed` is the
/// distinguished first node — the single permitted manual deploy (G-GENESIS);
/// nodes provisioned via `ProvisionNode` walk `PendingBare → … → Active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeLifecycleState {
    /// A bare VM shell on the allowlist, not yet provisioned (V14).
    PendingBare,
    /// `ProvisionNode` is deploying the binary/config to the target.
    Provisioning,
    /// The daemon started and is joining membership.
    Joining,
    /// In membership, but not yet a voting/schedulable member (Track D learner).
    NonVoting,
    /// Fully active member.
    Active,
    /// Drained of new placements, before drain (D2).
    Cordoned,
    /// Draining its owners/containers away (D2).
    Draining,
    /// Being removed after drain (D2).
    Decommissioning,
    /// Removed from the cluster.
    Removed,
    /// Compromised / fenced off — recover only from trusted RecoveryPoints (D2).
    Quarantined,
    /// The first seed node — the single permitted manual Sentinel deploy.
    GenesisSeed,
}

impl NodeLifecycleState {
    /// Whether the node may currently own/run containers (cooperative Track-A view).
    pub fn is_operational(&self) -> bool {
        matches!(self, Self::Active | Self::GenesisSeed | Self::Cordoned)
    }
}

/// Cluster role of this daemon instance, derived from `[daemon.cluster].seed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClusterRole {
    /// The Genesis seed / chef-controller node.
    Seed,
    /// A provisioned member node.
    Member,
}

/// The `[daemon.cluster]` config section. **Absent = single-node** (Track-A
/// default; the daemon behaves exactly as today). Present = the node participates
/// in the Cluster-12 platform with the given identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// Stable identity of this node (persisted in the config, V15).
    pub node_id: NodeId,
    /// The cluster this node belongs to (Genesis fixes it; one Genesis per id).
    pub cluster_id: Uuid,
    /// `true` only on the Genesis seed (G-GENESIS).
    #[serde(default)]
    pub seed: bool,
    /// Human-readable alias (defaults to the `node_id` string).
    #[serde(default)]
    pub alias: Option<String>,
    /// Seed endpoint to join (Zenoh `connect`). `None` on the seed itself.
    #[serde(default)]
    pub seed_endpoint: Option<String>,
    /// Bare targets this seed may provision (V14 allowlist). The host/identity of a
    /// `ProvisionNode` request come from here, never from the request. Empty on a
    /// non-seed node.
    #[serde(default)]
    pub pending_targets: Vec<PendingBareNode>,
    /// Path to the sha256-verified `sentinel-daemon` binary the seed pushes to a new
    /// node. `None` = the seed's own deployed binary (`/opt/sentinel/bin/sentinel-daemon`).
    /// The determinism profile (#494) requires an identical binary on every node.
    #[serde(default)]
    pub provision_binary_path: Option<String>,
    /// Bind address for this node's QUIC control stream (ADR-2, e.g. `"0.0.0.0:8085"`).
    /// `None` = the control stream is not started on this node.
    #[serde(default)]
    pub control_bind: Option<String>,
    /// Pinned control-plane peers (V10): each carries the peer's control address +
    /// SHA-256 cert fingerprint, exchanged out-of-band (like the SSH host-key pin).
    #[serde(default)]
    pub control_peers: Vec<ControlPeer>,
}

/// A pinned control-plane peer (V10): where to reach it + which cert to trust. The
/// fingerprint is exchanged out-of-band (single trust domain, V21); cert rotation /
/// distribution via membership is Track-D2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlPeer {
    /// Human-readable alias of the peer node.
    pub alias: String,
    /// The peer's control-stream socket address (e.g. `"10.0.0.242:8085"`).
    pub addr: String,
    /// The peer's SHA-256 cert fingerprint (64-hex), pinned out-of-band (V10).
    pub cert_fingerprint: String,
}

impl ClusterConfig {
    pub fn role(&self) -> ClusterRole {
        if self.seed {
            ClusterRole::Seed
        } else {
            ClusterRole::Member
        }
    }

    /// The lifecycle state a freshly-started daemon assumes from this config: the
    /// seed comes up as `GenesisSeed`, a member as `Joining` (it then joins
    /// membership; promotion to `Active`/voting is a later step).
    pub fn initial_lifecycle(&self) -> NodeLifecycleState {
        if self.seed {
            NodeLifecycleState::GenesisSeed
        } else {
            NodeLifecycleState::Joining
        }
    }
}

/// A pre-approved bare target for `ProvisionNode` (V14 allowlist). The host comes
/// from **here**, never from the operator request — so `ProvisionNode` cannot be
/// turned into a free remote-exec tool. (Materialized/used in the ProvisionNode
/// step; defined here as part of the cluster identity model.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingBareNode {
    pub pending_target_id: String,
    pub target_ip: String,
    /// SSH host key pinned out-of-band (G3: via the Proxmox guest agent).
    pub expected_host_key: String,
    #[serde(default)]
    pub expected_image_id: Option<String>,
    #[serde(default)]
    pub expected_hostname: Option<String>,
    #[serde(default)]
    pub expected_machine_id: Option<String>,
    /// Unix seconds; the allowlist entry is rejected after this (single-site NTP).
    pub expires_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_roundtrips_and_displays() {
        let id = NodeId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: NodeId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
        assert_eq!(id.to_string(), id.0.to_string());
    }

    #[test]
    fn node_identity_from_config_is_stable_id_fresh_boot() {
        let cfg = ClusterConfig {
            node_id: NodeId::new(),
            cluster_id: Uuid::new_v4(),
            seed: true,
            alias: Some("test-node-0".into()),
            seed_endpoint: None,
            pending_targets: Vec::new(),
            provision_binary_path: None,
            control_bind: None,
            control_peers: Vec::new(),
        };
        let a = NodeIdentity::from_config(&cfg);
        let b = NodeIdentity::from_config(&cfg);
        // Stable identity across "boots"...
        assert_eq!(a.node_id, b.node_id);
        assert_eq!(a.alias, "test-node-0");
        assert_eq!(a.incarnation, 0);
        // ...but a fresh boot_id each time (ABA guard).
        assert_ne!(a.boot_id, b.boot_id);
    }

    #[test]
    fn seed_vs_member_role_and_lifecycle() {
        let mut cfg = ClusterConfig {
            node_id: NodeId::new(),
            cluster_id: Uuid::new_v4(),
            seed: true,
            alias: None,
            seed_endpoint: None,
            pending_targets: Vec::new(),
            provision_binary_path: None,
            control_bind: None,
            control_peers: Vec::new(),
        };
        assert_eq!(cfg.role(), ClusterRole::Seed);
        assert_eq!(cfg.initial_lifecycle(), NodeLifecycleState::GenesisSeed);
        cfg.seed = false;
        assert_eq!(cfg.role(), ClusterRole::Member);
        assert_eq!(cfg.initial_lifecycle(), NodeLifecycleState::Joining);
    }

    #[test]
    fn lifecycle_operational_classification() {
        assert!(NodeLifecycleState::GenesisSeed.is_operational());
        assert!(NodeLifecycleState::Active.is_operational());
        assert!(!NodeLifecycleState::PendingBare.is_operational());
        assert!(!NodeLifecycleState::Quarantined.is_operational());
        assert!(!NodeLifecycleState::Draining.is_operational());
    }

    #[test]
    fn cluster_config_parses_from_toml_and_is_optional() {
        // Present: a seed node config.
        let toml_src = r#"
            node_id = "550e8400-e29b-41d4-a716-446655440000"
            cluster_id = "550e8400-e29b-41d4-a716-446655440001"
            seed = true
            alias = "test-node-0"
        "#;
        let cfg: ClusterConfig = toml::from_str(toml_src).unwrap();
        assert!(cfg.seed);
        assert_eq!(cfg.alias.as_deref(), Some("test-node-0"));
        assert_eq!(cfg.role(), ClusterRole::Seed);

        // Minimal: only the required fields (seed defaults to false).
        let minimal = r#"
            node_id = "550e8400-e29b-41d4-a716-446655440002"
            cluster_id = "550e8400-e29b-41d4-a716-446655440001"
        "#;
        let cfg2: ClusterConfig = toml::from_str(minimal).unwrap();
        assert!(!cfg2.seed);
        assert_eq!(cfg2.role(), ClusterRole::Member);
    }
}
