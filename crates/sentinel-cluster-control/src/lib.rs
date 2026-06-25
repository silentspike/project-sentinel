//! Cluster-12 cross-node control plane (Phase 3a0, ADR-2).
//!
//! One cert-pinned QUIC control stream carries request-scoped [`ControlEnvelope`]s
//! for the owner-handoff / provision / GC RPCs — the request/response substrate that
//! Zenoh (pub/sub, no queryable) cannot provide and that #496 PR2 / #498 / #499 build
//! on. This crate is the **skeleton**: the envelope, the length-prefixed frame codec
//! (reusing the dashboard `wt.rs` u32-BE format), idempotency dedup (V5/V39),
//! cert-pinning fingerprints (V10), and a [`ControlHandler`] seam with a deterministic
//! [`StubHandler`]. The real owner/GC handler logic lands with #496 / #499; the QUIC
//! server + client transport is wired on top of these types.
//!
//! **Bounded scope (ADR-2):** node→node only, **after** a node has joined; the
//! bare-shell bootstrap (#495) stays on SSH. 0-RTT is off for control (V18).

pub mod cert;
pub mod envelope;
pub mod handler;
pub mod idempotency;

pub use cert::{CertFingerprint, NodeCertificate};
pub use envelope::{
    decode_frame, encode_frame, CodecError, ControlEnvelope, ControlReply, ControlRequest,
    ControlResponse, MAX_FRAME_BYTES,
};
pub use handler::{ControlHandler, StubHandler};
pub use idempotency::IdempotencyCache;
