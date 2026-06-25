//! The control-RPC handler seam.
//!
//! The transport (server) decodes a `ControlRequest`, de-duplicates it, and calls a
//! `ControlHandler` for the response. The Phase-3a0 skeleton ships a deterministic
//! `StubHandler`; the real owner-registry (#496) and cluster-GC (#499) handlers
//! replace it without touching the transport.

use crate::envelope::{ControlRequest, ControlResponse};

/// Maps a `ControlRequest` to a `ControlResponse`. Implementations MUST be total
/// (return a typed `Rejected` rather than panic on anything they cannot serve).
pub trait ControlHandler: Send + Sync {
    fn handle(&self, request: &ControlRequest) -> ControlResponse;
}

/// A deterministic stub for the skeleton + tests. Acknowledges the owner RPCs and
/// reports **no** refs/pins for the GC queries (a fresh skeleton holds no cluster
/// references); the real liveness answers come from #496/#499.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubHandler;

impl ControlHandler for StubHandler {
    fn handle(&self, request: &ControlRequest) -> ControlResponse {
        match request {
            ControlRequest::PrepareHandoff { scope, epoch } => ControlResponse::HandoffPrepared {
                scope: scope.clone(),
                epoch: *epoch,
            },
            ControlRequest::SourceRetiredAck { scope, epoch } => {
                ControlResponse::RetiredAckRecorded {
                    scope: scope.clone(),
                    epoch: *epoch,
                }
            }
            ControlRequest::OwnerCommit { scope, epoch, .. } => ControlResponse::OwnerCommitted {
                scope: scope.clone(),
                epoch: *epoch,
            },
            ControlRequest::RefQuery { block_ref } => ControlResponse::RefQueryResult {
                block_ref: block_ref.clone(),
                referenced: false,
            },
            ControlRequest::PinQuery { block_ref } => ControlResponse::PinQueryResult {
                block_ref: block_ref.clone(),
                pinned: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_acknowledges_owner_rpcs_and_reports_no_refs() {
        let h = StubHandler;
        assert_eq!(
            h.handle(&ControlRequest::OwnerCommit {
                scope: "agent:7".into(),
                owner_node: "node-1".into(),
                epoch: 5,
            }),
            ControlResponse::OwnerCommitted {
                scope: "agent:7".into(),
                epoch: 5
            }
        );
        assert_eq!(
            h.handle(&ControlRequest::RefQuery {
                block_ref: "cas-blob:v1:sha256:ab".into()
            }),
            ControlResponse::RefQueryResult {
                block_ref: "cas-blob:v1:sha256:ab".into(),
                referenced: false,
            }
        );
    }
}
