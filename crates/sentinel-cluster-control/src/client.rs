//! The QUIC control client: connect to a cert-pinned peer, send one RPC, get the reply.

use std::net::SocketAddr;

use quinn::{Connection, Endpoint};

use crate::cert::{CertFingerprint, NodeCertificate};
use crate::envelope::{decode_frame, encode_frame, ControlEnvelope, ControlReply, MAX_FRAME_BYTES};
use crate::server::PeerRegistry;
use crate::tls::{peer_fingerprint, quic_client_config};

/// A QUIC control client bound to an ephemeral local port, presenting `node`'s cert
/// for mutual auth.
#[derive(Clone)]
pub struct ControlClient {
    endpoint: Endpoint,
    peers: PeerRegistry,
}

impl ControlClient {
    pub fn new(node: &NodeCertificate, peers: PeerRegistry) -> anyhow::Result<Self> {
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse().expect("valid bind addr"))?;
        endpoint.set_default_client_config(quic_client_config(node)?);
        Ok(Self { endpoint, peers })
    }

    /// Connect to `peer_addr` and enforce the server certificate pin. The returned
    /// session can carry multiple request streams and observes local peer revocation.
    pub async fn connect(
        &self,
        peer_addr: SocketAddr,
        expected_peer: CertFingerprint,
    ) -> anyhow::Result<ControlConnection> {
        let conn = self.endpoint.connect(peer_addr, "sentinel-node")?.await?;
        let fp = peer_fingerprint(&conn)?;
        if fp != expected_peer {
            conn.close(1u32.into(), b"unpinned server");
            anyhow::bail!("server cert {fp} does not match pin {expected_peer}");
        }
        let Some((_, connection_id)) = self.peers.register_connection(fp, &conn) else {
            conn.close(2u32.into(), b"peer revoked");
            anyhow::bail!("server cert {fp} is not authorized");
        };
        Ok(ControlConnection {
            connection: conn,
            peers: self.peers.clone(),
            fingerprint: fp,
            connection_id,
        })
    }

    /// Connect, send one request, and close the session. Call [`Self::connect`] when a
    /// caller needs to reuse one authenticated QUIC connection across requests.
    pub async fn rpc(
        &self,
        peer_addr: SocketAddr,
        expected_peer: CertFingerprint,
        envelope: &ControlEnvelope,
    ) -> anyhow::Result<ControlReply> {
        let connection = self.connect(peer_addr, expected_peer).await?;
        let reply = connection.rpc(envelope).await;
        connection.close();
        reply
    }
}

/// An authenticated, cert-pinned control session that can carry multiple RPC streams.
pub struct ControlConnection {
    connection: Connection,
    peers: PeerRegistry,
    fingerprint: CertFingerprint,
    connection_id: u64,
}

impl ControlConnection {
    pub async fn rpc(&self, envelope: &ControlEnvelope) -> anyhow::Result<ControlReply> {
        let (mut send, mut recv) = self.connection.open_bi().await?;
        send.write_all(&encode_frame(envelope)?).await?;
        send.finish()?;
        let frame = recv.read_to_end(MAX_FRAME_BYTES + 4).await?;
        Ok(decode_frame(&frame)?)
    }

    pub fn close(self) {
        self.connection.close(0u32.into(), b"done");
    }
}

impl Drop for ControlConnection {
    fn drop(&mut self) {
        self.peers
            .unregister_connection(self.fingerprint, self.connection_id);
        self.connection.close(0u32.into(), b"session dropped");
    }
}
