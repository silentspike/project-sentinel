//! The request-scoped control-plane message + the length-prefixed frame codec.
//!
//! ADR-2: control RPCs travel as a `ControlEnvelope` over a cert-pinned QUIC bidi
//! stream, reusing the dashboard `wt.rs` u32-BE length-prefixed frame format. The
//! The receiver binds `idempotency_key` to authenticated peer, RPC method, and
//! request digest for process-local duplicate suppression (V5/V39).

use sentinel_common::{
    Heartbeat, HolderAdvertisement, LocalOwnerStateSnapshot, NodeId, OwnerSnapshotInstallOutcome,
    OwnerTermSnapshot,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::idempotency::RequestDigest;

/// Hard cap on a single control frame (1 MiB). Control messages are tiny; the cap
/// guards the decoder against a hostile/oversized length prefix.
pub const MAX_FRAME_BYTES: usize = 1 << 20;

/// A request-scoped control message (ADR-2). `idempotency_key` lets the receiver
/// de-duplicate a re-sent RPC to a single effect; `request_id` correlates the reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlEnvelope {
    pub request_id: Uuid,
    pub idempotency_key: String,
    pub request: ControlRequest,
}

impl ControlEnvelope {
    pub fn new(idempotency_key: impl Into<String>, request: ControlRequest) -> Self {
        Self {
            request_id: Uuid::new_v4(),
            idempotency_key: idempotency_key.into(),
            request,
        }
    }
}

/// The cross-node control RPCs (Phase-3a0 skeleton). The owner-handoff / GC payloads
/// are intentionally thin — the real `OwnerTerm` / `BlockRef` fields land with the
/// owner-registry (#496) and the cluster GC (#499); here they are opaque strings so
/// the transport, envelope, idempotency and cert-pinning can be built + verified now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlRequest {
    /// Liveness-only membership heartbeat. Unlike effectful control RPCs, this request
    /// must bypass response deduplication so every arrival refreshes receiver-local time.
    MembershipHeartbeat {
        cluster_id: Uuid,
        heartbeat: Heartbeat,
    },
    /// G1/V1: the chef asks the source to prepare a scoped handoff at an epoch.
    PrepareHandoff { scope: String, epoch: u64 },
    /// V1: the source's durable acknowledgement that it has retired the scope.
    SourceRetiredAck { scope: String, epoch: u64 },
    /// G1: commit a new owner term for a scope.
    OwnerCommit {
        scope: String,
        owner_node: String,
        epoch: u64,
    },
    /// #615: install one complete chef authority snapshot plus the authenticated
    /// recipient's complete local base-state snapshot.
    ReplicateOwnerSnapshot {
        global: OwnerTermSnapshot,
        local: LocalOwnerStateSnapshot,
    },
    /// V8/G7: does any node still reference this block? (GC liveness query)
    RefQuery { block_ref: String },
    /// V20: does any node pin this block? (GC pin query)
    PinQuery { block_ref: String },
    /// #498 V8/V16: push a batch of holder advertisements (block-map gossip). The
    /// receiver merges them into its block map by the conflict-free freshness rule.
    /// Metadata only — block bytes never travel on the control stream (AC-4).
    AdvertiseHolders {
        advertisements: Vec<HolderAdvertisement>,
    },
}

impl ControlRequest {
    /// Stable method name for logging / metrics (never the payload).
    pub fn method_name(&self) -> &'static str {
        match self {
            Self::PrepareHandoff { .. } => "prepare_handoff",
            Self::MembershipHeartbeat { .. } => "membership_heartbeat",
            Self::SourceRetiredAck { .. } => "source_retired_ack",
            Self::OwnerCommit { .. } => "owner_commit",
            Self::ReplicateOwnerSnapshot { .. } => "replicate_owner_snapshot",
            Self::RefQuery { .. } => "ref_query",
            Self::PinQuery { .. } => "pin_query",
            Self::AdvertiseHolders { .. } => "advertise_holders",
        }
    }

    /// Whether the server may reuse a cached reply for this request. Membership
    /// heartbeats are observations, not deduplicated effects: every received packet
    /// must reach the handler to refresh liveness and must not grow the dedup cache.
    pub fn cache_response(&self) -> bool {
        !matches!(self, Self::MembershipHeartbeat { .. })
    }

    /// Stable digest used to bind an idempotency key to one exact request body.
    pub fn digest(&self) -> Result<RequestDigest, serde_json::Error> {
        let encoded = serde_json::to_vec(self)?;
        Ok(RequestDigest(Sha256::digest(encoded).into()))
    }

    /// Owner-state mutations are accepted only from the configured chef node.
    pub fn requires_chef_authorization(&self) -> bool {
        matches!(
            self,
            Self::PrepareHandoff { .. }
                | Self::SourceRetiredAck { .. }
                | Self::OwnerCommit { .. }
                | Self::ReplicateOwnerSnapshot { .. }
        )
    }
}

/// The reply to a `ControlEnvelope`, correlated by `request_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlReply {
    pub request_id: Uuid,
    pub response: ControlResponse,
}

/// The typed result of a control RPC. `Rejected` carries the reason for an unknown
/// / unauthorized / failed request (a typed reject, never a panic — AC-3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlResponse {
    MembershipAccepted {
        node_id: NodeId,
        incarnation: u64,
    },
    HandoffPrepared {
        scope: String,
        epoch: u64,
    },
    RetiredAckRecorded {
        scope: String,
        epoch: u64,
    },
    OwnerCommitted {
        scope: String,
        epoch: u64,
    },
    OwnerSnapshotAck {
        outcome: OwnerSnapshotInstallOutcome,
    },
    RefQueryResult {
        block_ref: String,
        referenced: bool,
    },
    PinQueryResult {
        block_ref: String,
        pinned: bool,
    },
    /// #498: how many of the pushed advertisements were newly applied (strictly newer
    /// than the receiver already knew) — the rest were stale/duplicate no-ops.
    HoldersApplied {
        applied: u32,
    },
    /// The same peer/method/idempotency tuple was reused with another payload.
    IdempotencyConflict {
        method: String,
        idempotency_key: String,
    },
    /// Typed reject (unknown/unsupported request, auth failure, or handler error).
    Rejected {
        reason: String,
    },
}

/// Frame-codec errors.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("serialize/deserialize control frame: {0}")]
    Json(#[from] serde_json::Error),
    #[error("control frame too large: {0} bytes (max {MAX_FRAME_BYTES})")]
    TooLarge(usize),
    #[error("control frame truncated")]
    Truncated,
}

/// Encode a value as a single u32-BE length-prefixed JSON frame.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    let body = serde_json::to_vec(value)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(CodecError::TooLarge(body.len()));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// Decode a value from a buffer holding exactly one frame (prefix + body).
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, CodecError> {
    let len = frame_body_len(frame.get(0..4).ok_or(CodecError::Truncated)?)?;
    let body = frame.get(4..4 + len).ok_or(CodecError::Truncated)?;
    Ok(serde_json::from_slice(body)?)
}

/// Read + validate the declared body length from a 4-byte prefix (streaming reads
/// read the prefix, then exactly `frame_body_len` more bytes).
pub fn frame_body_len(prefix: &[u8]) -> Result<usize, CodecError> {
    let bytes: [u8; 4] = prefix
        .get(0..4)
        .ok_or(CodecError::Truncated)?
        .try_into()
        .unwrap();
    let len = u32::from_be_bytes(bytes) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(CodecError::TooLarge(len));
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_frame_roundtrip() {
        let env = ControlEnvelope::new(
            "idem-1",
            ControlRequest::OwnerCommit {
                scope: "agent:7".into(),
                owner_node: "node-1".into(),
                epoch: 3,
            },
        );
        let frame = encode_frame(&env).unwrap();
        // u32-BE prefix matches the body length.
        let declared = frame_body_len(&frame[0..4]).unwrap();
        assert_eq!(declared, frame.len() - 4);
        let back: ControlEnvelope = decode_frame(&frame).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn reply_frame_roundtrip() {
        let reply = ControlReply {
            request_id: Uuid::new_v4(),
            response: ControlResponse::RefQueryResult {
                block_ref: "cas-blob:v1:sha256:ab".into(),
                referenced: false,
            },
        };
        let frame = encode_frame(&reply).unwrap();
        let back: ControlReply = decode_frame(&frame).unwrap();
        assert_eq!(reply, back);
    }

    #[test]
    fn owner_snapshot_outcomes_roundtrip_without_losing_typed_detail() {
        let outcomes = [
            OwnerSnapshotInstallOutcome::Installed,
            OwnerSnapshotInstallOutcome::AlreadyInstalled,
            OwnerSnapshotInstallOutcome::StaleSnapshot {
                installed_revision: 8,
                received_revision: 7,
            },
            OwnerSnapshotInstallOutcome::GenerationMismatch {
                installed_generation: 1,
                received_generation: 2,
            },
            OwnerSnapshotInstallOutcome::SnapshotConflict,
        ];
        for outcome in outcomes {
            let reply = ControlReply {
                request_id: Uuid::new_v4(),
                response: ControlResponse::OwnerSnapshotAck {
                    outcome: outcome.clone(),
                },
            };
            let decoded: ControlReply = decode_frame(&encode_frame(&reply).unwrap()).unwrap();
            assert_eq!(decoded, reply);
        }
    }

    #[test]
    fn advertise_holders_frame_roundtrip() {
        use sentinel_common::{BlockRef, HolderAction, HolderAdvertisement, NodeId};
        let adv = HolderAdvertisement {
            block_ref: BlockRef::blob_sha256([7; 32], 2048),
            node_id: NodeId::new(),
            node_boot_id: Uuid::new_v4(),
            node_incarnation: 2,
            node_cas_generation: 9,
            action: HolderAction::Add,
            expires_after: u64::MAX,
        };
        let env = ControlEnvelope::new(
            "idem-adv",
            ControlRequest::AdvertiseHolders {
                advertisements: vec![adv],
            },
        );
        assert_eq!(env.request.method_name(), "advertise_holders");
        let frame = encode_frame(&env).unwrap();
        let back: ControlEnvelope = decode_frame(&frame).unwrap();
        assert_eq!(env, back, "holder gossip survives the wire codec");
    }

    #[test]
    fn membership_heartbeat_frame_roundtrip_and_cache_policy() {
        use sentinel_common::{Heartbeat, NodeId};

        let request = ControlRequest::MembershipHeartbeat {
            cluster_id: Uuid::new_v4(),
            heartbeat: Heartbeat {
                node_id: NodeId::new(),
                alias: "node-1".into(),
                boot_id: Uuid::new_v4(),
                incarnation: 4,
                endpoints: vec![],
            },
        };
        assert!(!request.cache_response());
        let envelope = ControlEnvelope::new("same-key", request.clone());
        let frame = encode_frame(&envelope).unwrap();
        let decoded: ControlEnvelope = decode_frame(&frame).unwrap();
        assert_eq!(decoded.request, request);
    }

    #[test]
    fn method_names_are_stable() {
        assert_eq!(
            ControlRequest::RefQuery {
                block_ref: "x".into()
            }
            .method_name(),
            "ref_query"
        );
        assert_eq!(
            ControlRequest::PrepareHandoff {
                scope: "s".into(),
                epoch: 1
            }
            .method_name(),
            "prepare_handoff"
        );
    }

    #[test]
    fn truncated_frame_is_rejected_not_panicked() {
        assert!(matches!(
            decode_frame::<ControlReply>(&[0, 0]),
            Err(CodecError::Truncated)
        ));
        // prefix declares 100 bytes but body is empty.
        let mut f = 100u32.to_be_bytes().to_vec();
        f.truncate(4);
        assert!(matches!(
            decode_frame::<ControlReply>(&f),
            Err(CodecError::Truncated)
        ));
    }

    #[test]
    fn oversized_length_prefix_is_rejected() {
        let huge = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes();
        assert!(matches!(
            frame_body_len(&huge),
            Err(CodecError::TooLarge(_))
        ));
    }
}
