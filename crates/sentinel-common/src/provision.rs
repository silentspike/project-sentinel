//! `ProvisionNode` saga: absorbing a bare VM shell into a cluster node (G3).
//!
//! See `docs/adr/ADR-0495-G3-provisionnode-threat-model.md`. The seed node runs
//! a persistent, recoverable [`ProvisionOp`] saga (V5/V39): each step is
//! idempotent, so a chef/target restart mid-provision resumes from the persisted
//! state. The host comes from the [`PendingBareNode`] allowlist (V14) — **never**
//! from the operator request — so `ProvisionNode` cannot become a free remote-exec
//! tool.
//!
//! This module is the **saga state machine + allowlist validation** (pure logic,
//! unit-tested). The live execution (SSH host-key pinning, binary push, daemon
//! start) and the `OperatorCommand::ProvisionNode` wiring are a later step.

use crate::cluster::{NodeId, PendingBareNode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// State machine of a node-provisioning operation (a persistent, recoverable saga,
/// V5). The happy path runs `VerifyingTarget → … → Completed`; any step may go to
/// `Failed` (rollback / quarantine, AC-B6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProvisionOpState {
    /// Verify the target against the `PendingBareNode` allowlist (V14).
    VerifyingTarget,
    /// Pin the SSH host key out-of-band (G3: via the Proxmox guest agent).
    PinningHostKey,
    /// Push the sha256-verified `sentinel-daemon` binary.
    PushingBinary,
    /// Generate the node identity on the target; the private key never leaves it.
    IssuingCert,
    /// Render `daemon.toml` + systemd units + the #517 token-gate drop-ins.
    RenderingConfig,
    /// Start `sentinel-daemon` on the target.
    StartingDaemon,
    /// Poll `/operator/health` until ready.
    AwaitingHealth,
    /// Observe the target self-register in membership.
    ObservingJoin,
    /// Done — a `NodeProvisioned` event is emitted.
    Completed,
    /// Failed — the target is rolled back / quarantined (no alive node, AC-B6).
    Failed,
}

impl ProvisionOpState {
    /// The next state on the happy path, or `None` at a terminal state.
    pub fn next(self) -> Option<Self> {
        use ProvisionOpState::*;
        Some(match self {
            VerifyingTarget => PinningHostKey,
            PinningHostKey => PushingBinary,
            PushingBinary => IssuingCert,
            IssuingCert => RenderingConfig,
            RenderingConfig => StartingDaemon,
            StartingDaemon => AwaitingHealth,
            AwaitingHealth => ObservingJoin,
            ObservingJoin => Completed,
            Completed | Failed => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

/// A persistent provisioning operation (ADR-3 `PROVISION_OPS`, V5). Recoverable:
/// on restart the chef resumes from `state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionOp {
    pub op_id: Uuid,
    /// The allowlist target being provisioned (V14 — host from the allowlist).
    pub pending_target_id: String,
    /// Idempotency key from the operator command (re-run = convergent no-op, AC-S2).
    pub idempotency_key: String,
    pub requested_alias: String,
    /// The node id minted for this target (`None` until the target verifies).
    #[serde(default)]
    pub node_id: Option<NodeId>,
    pub state: ProvisionOpState,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub failure_reason: Option<String>,
}

impl ProvisionOp {
    /// Start a new provisioning saga (begins at `VerifyingTarget`).
    pub fn new(
        op_id: Uuid,
        pending_target_id: String,
        requested_alias: String,
        idempotency_key: String,
        now_ms: u64,
    ) -> Self {
        Self {
            op_id,
            pending_target_id,
            idempotency_key,
            requested_alias,
            node_id: None,
            state: ProvisionOpState::VerifyingTarget,
            started_at_ms: now_ms,
            updated_at_ms: now_ms,
            failure_reason: None,
        }
    }

    /// Advance one happy-path step. Idempotent: a terminal op stays terminal and
    /// returns `false`; otherwise advances and returns `true`.
    pub fn advance(&mut self, now_ms: u64) -> bool {
        match self.state.next() {
            Some(next) => {
                self.state = next;
                self.updated_at_ms = now_ms;
                true
            }
            None => false,
        }
    }

    /// Record the node id minted once the target is verified.
    pub fn assign_node(&mut self, node_id: NodeId, now_ms: u64) {
        self.node_id = Some(node_id);
        self.updated_at_ms = now_ms;
    }

    /// Fail the op (rollback / quarantine, AC-B6). Idempotent.
    pub fn fail(&mut self, reason: impl Into<String>, now_ms: u64) {
        self.state = ProvisionOpState::Failed;
        self.failure_reason = Some(reason.into());
        self.updated_at_ms = now_ms;
    }
}

/// Why a `ProvisionNode` request was rejected against the allowlist (V14/G3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionError {
    /// The `pending_target_id` is not on the allowlist.
    UnknownTarget(String),
    /// The allowlist entry expired (single-site NTP, V14).
    Expired(String),
}

impl std::fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTarget(id) => write!(f, "pending target '{id}' not on allowlist"),
            Self::Expired(id) => write!(f, "allowlist entry for '{id}' expired"),
        }
    }
}

impl std::error::Error for ProvisionError {}

/// Validate a `ProvisionNode` request against the allowlist (V14): the target MUST
/// be on the allowlist and not expired. The host/identity come from the returned
/// entry, **never** from the operator request.
pub fn validate_pending_target<'a>(
    allowlist: &'a [PendingBareNode],
    pending_target_id: &str,
    now_unix_s: i64,
) -> Result<&'a PendingBareNode, ProvisionError> {
    let entry = allowlist
        .iter()
        .find(|p| p.pending_target_id == pending_target_id)
        .ok_or_else(|| ProvisionError::UnknownTarget(pending_target_id.to_string()))?;
    if entry.expires_at <= now_unix_s {
        return Err(ProvisionError::Expired(pending_target_id.to_string()));
    }
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(id: &str, expires_at: i64) -> PendingBareNode {
        PendingBareNode {
            pending_target_id: id.into(),
            target_ip: "10.0.0.242".into(),
            expected_host_key: "ssh-ed25519 AAAA...".into(),
            expected_image_id: None,
            expected_hostname: None,
            expected_machine_id: None,
            expires_at,
        }
    }

    #[test]
    fn saga_runs_happy_path_to_completed() {
        let mut op = ProvisionOp::new(
            Uuid::new_v4(),
            "bare-1".into(),
            "test-node-1".into(),
            "idem-1".into(),
            0,
        );
        assert_eq!(op.state, ProvisionOpState::VerifyingTarget);
        let mut steps = 0;
        while op.advance(steps + 1) {
            steps += 1;
            assert!(steps < 20, "saga must terminate");
        }
        assert_eq!(op.state, ProvisionOpState::Completed);
        assert!(op.state.is_terminal());
        // Idempotent: advancing a terminal op is a no-op.
        assert!(!op.advance(100));
        assert_eq!(op.state, ProvisionOpState::Completed);
    }

    #[test]
    fn saga_can_fail_and_is_terminal() {
        let mut op = ProvisionOp::new(
            Uuid::new_v4(),
            "bare-1".into(),
            "n".into(),
            "idem".into(),
            0,
        );
        op.advance(1); // PinningHostKey
        op.fail("host key mismatch", 2);
        assert_eq!(op.state, ProvisionOpState::Failed);
        assert_eq!(op.failure_reason.as_deref(), Some("host key mismatch"));
        assert!(op.state.is_terminal());
        assert!(!op.advance(3));
    }

    #[test]
    fn assign_node_records_id() {
        let mut op = ProvisionOp::new(Uuid::new_v4(), "b".into(), "n".into(), "i".into(), 0);
        assert!(op.node_id.is_none());
        let nid = NodeId::new();
        op.assign_node(nid, 5);
        assert_eq!(op.node_id, Some(nid));
        assert_eq!(op.updated_at_ms, 5);
    }

    #[test]
    fn validate_allowlist_membership_and_expiry() {
        let list = vec![allow("bare-1", 2_000), allow("bare-2", 500)];
        // On the allowlist and not expired.
        assert!(validate_pending_target(&list, "bare-1", 1_000).is_ok());
        assert_eq!(
            validate_pending_target(&list, "bare-1", 1_000)
                .unwrap()
                .target_ip,
            "10.0.0.242"
        );
        // Unknown target → rejected (host never from the request).
        assert_eq!(
            validate_pending_target(&list, "evil-host", 1_000),
            Err(ProvisionError::UnknownTarget("evil-host".into()))
        );
        // Expired entry → rejected.
        assert_eq!(
            validate_pending_target(&list, "bare-2", 1_000),
            Err(ProvisionError::Expired("bare-2".into()))
        );
    }

    #[test]
    fn provision_op_serde_roundtrip() {
        let op = ProvisionOp::new(Uuid::new_v4(), "b".into(), "n".into(), "i".into(), 7);
        let json = serde_json::to_string(&op).unwrap();
        let back: ProvisionOp = serde_json::from_str(&json).unwrap();
        assert_eq!(op, back);
    }
}
