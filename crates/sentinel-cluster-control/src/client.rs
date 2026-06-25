//! The QUIC control client: connect to a cert-pinned peer, send one RPC, get the reply.

use std::net::SocketAddr;

use quinn::Endpoint;

use crate::cert::{CertFingerprint, NodeCertificate};
use crate::envelope::{decode_frame, encode_frame, ControlEnvelope, ControlReply, MAX_FRAME_BYTES};
use crate::tls::{peer_fingerprint, quic_client_config};

/// A QUIC control client bound to an ephemeral local port, presenting `node`'s cert
/// for mutual auth.
pub struct ControlClient {
    endpoint: Endpoint,
}

impl ControlClient {
    pub fn new(node: &NodeCertificate) -> anyhow::Result<Self> {
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse().expect("valid bind addr"))?;
        endpoint.set_default_client_config(quic_client_config(node)?);
        Ok(Self { endpoint })
    }

    /// Connect to `peer_addr`, enforce the server cert pin (V10 — reject if the
    /// server's fingerprint differs from `expected_peer`), send `envelope` over a bidi
    /// stream and return the reply.
    pub async fn rpc(
        &self,
        peer_addr: SocketAddr,
        expected_peer: CertFingerprint,
        envelope: &ControlEnvelope,
    ) -> anyhow::Result<ControlReply> {
        let conn = self.endpoint.connect(peer_addr, "sentinel-node")?.await?;
        let fp = peer_fingerprint(&conn)?;
        if fp != expected_peer {
            conn.close(1u32.into(), b"unpinned server");
            anyhow::bail!("server cert {fp} does not match pin {expected_peer}");
        }
        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(&encode_frame(envelope)?).await?;
        send.finish()?;
        let frame = recv.read_to_end(MAX_FRAME_BYTES + 4).await?;
        let reply: ControlReply = decode_frame(&frame)?;
        conn.close(0u32.into(), b"done");
        Ok(reply)
    }
}
