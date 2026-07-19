//! #498 PR2 (4b) — the block-pull wire protocol over QUIC.
//!
//! A node that the block map (4a) tells holds a blob pulls the bytes **by hash** from
//! that peer. The control stream's frame codec caps a frame at 1 MiB (too small for a
//! blob), so block bytes ride their own bidi stream with a minimal framing:
//!
//! ```text
//!   client -> server:  [u32-BE len][ serde(BlockPullRequest{ block_ref }) ]   (one framed request)
//!   server -> client:  [1 byte status]  then, if FOUND, the raw on-disk encoded blob
//!                       bytes streamed until the send side finishes (no per-blob cap).
//! ```
//!
//! Security (V10): the request carries **only a `BlockRef`, never a path** — the server
//! maps it to a canonical CAS path itself, never lists directories, never answers
//! "does path X exist?". The receiver bounds the read (an encoded blob never exceeds the
//! original size + 1 prefix byte, so `size_bytes + 1` is the hard cap) against a hostile
//! over-long stream, and the **content hash is verified after the pull** (by the CAS
//! layer, V28) before anything is published — a corrupt/tampered blob is rejected, never
//! cached.

use std::net::SocketAddr;
use std::sync::Arc;

use quinn::{Connection, Endpoint};
use sentinel_common::BlockRef;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::cert::{CertFingerprint, NodeCertificate};
use crate::envelope::{decode_frame, encode_frame, CodecError};
use crate::server::PeerRegistry;
use crate::tls::{peer_fingerprint, quic_client_config, quic_server_config};

/// Max concurrent block-pull streams served per pinned peer (per-node rate limit, V10).
const MAX_INFLIGHT_PER_PEER: usize = 16;

/// Pull one block by its content id. The server accepts **only** this (a `BlockRef`),
/// never a path (V10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockPullRequest {
    pub block_ref: BlockRef,
}

/// The one-byte status the server writes before the (optional) blob stream.
pub mod status {
    /// The server holds the block; the encoded blob bytes follow until stream end.
    pub const FOUND: u8 = 0x01;
    /// The server does not hold the block; no bytes follow.
    pub const NOT_FOUND: u8 = 0x00;
}

/// Errors on the block-pull wire.
#[derive(Debug, thiserror::Error)]
pub enum BlockPullError {
    #[error("block-pull i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("block-pull frame codec: {0}")]
    Codec(#[from] CodecError),
    #[error("unknown status byte 0x{0:02x}")]
    UnknownStatus(u8),
    /// The peer streamed more than the bound (`size_bytes + 1`) — hostile / corrupt.
    #[error("block-pull response exceeds the {0}-byte bound for the requested ref")]
    TooLarge(u64),
}

/// Write the framed request onto a send stream (client side).
pub async fn write_request<W: AsyncWrite + Unpin>(
    send: &mut W,
    req: &BlockPullRequest,
) -> Result<(), BlockPullError> {
    let frame = encode_frame(req)?;
    send.write_all(&frame).await?;
    Ok(())
}

/// Read the framed request from a recv stream (server side). Bounded by the 1-MiB
/// control-frame cap, so a hostile client cannot make the server allocate unboundedly.
pub async fn read_request<R: AsyncRead + Unpin>(
    recv: &mut R,
) -> Result<BlockPullRequest, BlockPullError> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = crate::envelope::frame_body_len(&len_buf)?;
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await?;
    // Re-prefix so decode_frame validates the same way the control codec does.
    let mut frame = len_buf.to_vec();
    frame.extend_from_slice(&body);
    Ok(decode_frame(&frame)?)
}

/// Write the response (server side): a status byte, then — if `FOUND` — the encoded
/// blob bytes.
pub async fn write_response<W: AsyncWrite + Unpin>(
    send: &mut W,
    encoded: Option<&[u8]>,
) -> Result<(), BlockPullError> {
    match encoded {
        Some(bytes) => {
            send.write_all(&[status::FOUND]).await?;
            send.write_all(bytes).await?;
        }
        None => {
            send.write_all(&[status::NOT_FOUND]).await?;
        }
    }
    Ok(())
}

/// Read the response (client side): the status byte, then — if `FOUND` — the encoded
/// blob bytes, **bounded** by `max_encoded` (`size_bytes + 1`). Returns `None` for
/// `NOT_FOUND`. The returned bytes are the raw on-disk encoded blob; the content hash is
/// verified afterwards by the CAS layer (V28) before publish.
pub async fn read_response<R: AsyncRead + Unpin>(
    recv: &mut R,
    max_encoded: u64,
) -> Result<Option<Vec<u8>>, BlockPullError> {
    let mut status_buf = [0u8; 1];
    recv.read_exact(&mut status_buf).await?;
    match status_buf[0] {
        status::NOT_FOUND => Ok(None),
        status::FOUND => {
            // Read at most max_encoded+1: one extra byte detects an over-long (hostile)
            // stream without trusting the peer's length.
            let mut buf = Vec::new();
            let mut limited = recv.take(max_encoded + 1);
            limited.read_to_end(&mut buf).await?;
            if buf.len() as u64 > max_encoded {
                return Err(BlockPullError::TooLarge(max_encoded));
            }
            Ok(Some(buf))
        }
        other => Err(BlockPullError::UnknownStatus(other)),
    }
}

/// The hard read bound for a blob's encoded form: an encoded blob is at most the
/// original content size plus the one-byte encoding prefix (compression is only kept
/// when it shrinks the data), so this never truncates a legitimate blob.
pub fn encoded_read_bound(block_ref: &BlockRef) -> u64 {
    block_ref.size_bytes().saturating_add(1)
}

/// A source of blobs the block-pull server serves, addressed **only** by `BlockRef`
/// (V10 — the server never maps anything but a content id to a path, never lists
/// directories). Implementations return the raw on-disk **encoded** blob bytes, or
/// `None` if the block is not held.
pub trait BlockProvider: Send + Sync {
    fn encoded_blob(&self, block_ref: &BlockRef) -> Option<Vec<u8>>;
}

/// Serve exactly one block-pull request on a `(recv, send)` stream pair: read the
/// `BlockRef`, look it up via the provider, write the response. Returns the encoded
/// bytes served (0 = miss). Stream-generic, so it is testable without a live QUIC peer.
pub async fn handle_pull<R, W, P>(
    recv: &mut R,
    send: &mut W,
    provider: &P,
    peer: &str,
) -> Result<usize, BlockPullError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: BlockProvider + ?Sized,
{
    let req = read_request(recv).await?;
    let encoded = provider.encoded_blob(&req.block_ref);
    let bytes = encoded.as_ref().map_or(0, Vec::len);
    write_response(send, encoded.as_deref()).await?;
    debug!(
        peer,
        block_ref = %req.block_ref,
        bytes,
        result = if bytes > 0 { "found" } else { "not_found" },
        "#498 block-pull served"
    );
    Ok(bytes)
}

/// A running QUIC block-pull server. Reuses the cluster-control cert/TLS stack (V10
/// pinned peers); each request carries only a `BlockRef`, never a path.
pub struct BlockPullServer {
    endpoint: Endpoint,
    local_addr: SocketAddr,
}

impl BlockPullServer {
    /// Bind a block-pull server on `bind_addr` with `node`'s identity, serving only
    /// peers whose cert fingerprint is pinned, sourcing blobs from `provider`.
    pub fn bind<P: BlockProvider + 'static>(
        bind_addr: SocketAddr,
        node: &NodeCertificate,
        peers: PeerRegistry,
        provider: Arc<P>,
    ) -> anyhow::Result<Self> {
        let server_cfg = quic_server_config(node)?;
        let endpoint = Endpoint::server(server_cfg, bind_addr)?;
        let local_addr = endpoint.local_addr()?;
        let ep = endpoint.clone();
        tokio::spawn(async move {
            while let Some(incoming) = ep.accept().await {
                let peers = peers.clone();
                let provider = provider.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_pull_connection(incoming, peers, provider).await {
                        warn!(error = %e, "#498 block-pull connection ended with error");
                    }
                });
            }
        });
        info!(%local_addr, "#498 block-pull server listening");
        Ok(Self {
            endpoint,
            local_addr,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stop accepting + close existing connections.
    pub fn close(&self) {
        self.endpoint.close(0u32.into(), b"shutdown");
    }
}

async fn serve_pull_connection<P: BlockProvider + 'static>(
    incoming: quinn::Incoming,
    peers: PeerRegistry,
    provider: Arc<P>,
) -> anyhow::Result<()> {
    let conn = incoming.await?;
    // V10: enforce the cert pin post-handshake (same gate as the control server).
    let fp = peer_fingerprint(&conn)?;
    let Some((_authenticated_peer, connection_id)) = peers.register_connection(fp, &conn) else {
        conn.close(1u32.into(), b"unpinned peer");
        anyhow::bail!("rejected unpinned block-pull peer cert {fp}");
    };
    let peer = fp.to_hex();
    // Per-node rate limit: bound the concurrent pull streams from this pinned peer.
    let limiter = Arc::new(Semaphore::new(MAX_INFLIGHT_PER_PEER));
    debug!(peer = %fp, "#498 block-pull peer accepted (pinned)");
    let result = loop {
        match conn.accept_bi().await {
            Ok((mut send, mut recv)) => {
                let provider = provider.clone();
                let limiter = limiter.clone();
                let peer = peer.clone();
                let peers = peers.clone();
                tokio::spawn(async move {
                    let Ok(_permit) = limiter.acquire().await else {
                        return;
                    };
                    if peers.resolve(fp).is_none() {
                        debug!(peer = %fp, "#498 block-pull peer revoked before request dispatch");
                        return;
                    }
                    if let Err(e) =
                        handle_pull(&mut recv, &mut send, provider.as_ref(), &peer).await
                    {
                        debug!(error = %e, "#498 block-pull request stream error");
                        return;
                    }
                    let _ = send.finish();
                    let _ = send.stopped().await;
                });
            }
            Err(quinn::ConnectionError::ApplicationClosed(_))
            | Err(quinn::ConnectionError::LocallyClosed) => break Ok(()),
            Err(e) => break Err(e.into()),
        }
    };
    peers.unregister_connection(fp, connection_id);
    result
}

/// A QUIC block-pull client bound to an ephemeral local port, presenting `node`'s cert.
pub struct BlockPullClient {
    endpoint: Endpoint,
}

impl BlockPullClient {
    pub fn new(node: &NodeCertificate) -> anyhow::Result<Self> {
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse().expect("valid bind addr"))?;
        endpoint.set_default_client_config(quic_client_config(node)?);
        Ok(Self { endpoint })
    }

    /// Connect to one holder and enforce its server certificate pin. The returned
    /// session can carry multiple pull streams and observes server-side revocation.
    pub async fn connect(
        &self,
        peer_addr: SocketAddr,
        expected_peer: CertFingerprint,
    ) -> anyhow::Result<BlockPullConnection> {
        let conn = self.endpoint.connect(peer_addr, "sentinel-node")?.await?;
        let fp = peer_fingerprint(&conn)?;
        if fp != expected_peer {
            conn.close(1u32.into(), b"unpinned server");
            anyhow::bail!("block-pull server cert {fp} does not match pin {expected_peer}");
        }
        Ok(BlockPullConnection { connection: conn })
    }

    /// Pull one block over a short-lived authenticated session. The content hash is
    /// still verified by the CAS layer before publication (V28).
    pub async fn pull(
        &self,
        peer_addr: SocketAddr,
        expected_peer: CertFingerprint,
        block_ref: &BlockRef,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let connection = self.connect(peer_addr, expected_peer).await?;
        let result = connection.pull(block_ref).await;
        connection.close();
        result
    }
}

/// An authenticated block-pull session that can carry multiple request streams.
pub struct BlockPullConnection {
    connection: Connection,
}

impl BlockPullConnection {
    pub async fn pull(&self, block_ref: &BlockRef) -> anyhow::Result<Option<Vec<u8>>> {
        let (mut send, mut recv) = self.connection.open_bi().await?;
        write_request(
            &mut send,
            &BlockPullRequest {
                block_ref: block_ref.clone(),
            },
        )
        .await?;
        send.finish()?;
        Ok(read_response(&mut recv, encoded_read_bound(block_ref)).await?)
    }

    pub fn close(&self) {
        self.connection.close(0u32.into(), b"done");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentinel_common::BlockRef;
    use std::io::Cursor;

    fn blob(n: u8, size: u64) -> BlockRef {
        BlockRef::blob_sha256([n; 32], size)
    }

    #[tokio::test]
    async fn request_roundtrip_over_a_stream() {
        let req = BlockPullRequest {
            block_ref: blob(7, 4096),
        };
        let mut wire = Vec::new();
        write_request(&mut wire, &req).await.unwrap();
        let mut cur = Cursor::new(wire);
        let back = read_request(&mut cur).await.unwrap();
        assert_eq!(
            back, req,
            "the request (a BlockRef, never a path) survives the wire"
        );
    }

    #[tokio::test]
    async fn found_response_roundtrip_streams_the_encoded_bytes() {
        let encoded = vec![0u8, 1, 2, 3, 4, 5]; // a tiny encoded blob (raw prefix + data)
        let mut wire = Vec::new();
        write_response(&mut wire, Some(&encoded)).await.unwrap();
        let mut cur = Cursor::new(wire);
        let got = read_response(&mut cur, 1024).await.unwrap();
        assert_eq!(got, Some(encoded));
    }

    #[tokio::test]
    async fn not_found_response_is_typed_none() {
        let mut wire = Vec::new();
        write_response(&mut wire, None).await.unwrap();
        let mut cur = Cursor::new(wire);
        assert_eq!(read_response(&mut cur, 1024).await.unwrap(), None);
    }

    struct OneBlock {
        want: BlockRef,
        encoded: Vec<u8>,
    }
    impl BlockProvider for OneBlock {
        fn encoded_blob(&self, block_ref: &BlockRef) -> Option<Vec<u8>> {
            (block_ref == &self.want).then(|| self.encoded.clone())
        }
    }

    #[tokio::test]
    async fn handle_pull_serves_a_held_block_and_misses_an_unknown_one() {
        let want = blob(3, 6);
        let provider = OneBlock {
            want: want.clone(),
            encoded: vec![0u8, 1, 2, 3, 4, 5],
        };

        // Hit: the requested ref is held -> FOUND + the encoded bytes.
        let mut req = Vec::new();
        write_request(
            &mut req,
            &BlockPullRequest {
                block_ref: want.clone(),
            },
        )
        .await
        .unwrap();
        let mut resp = Vec::new();
        let served = handle_pull(&mut Cursor::new(req), &mut resp, &provider, "test-peer")
            .await
            .unwrap();
        assert_eq!(served, 6);
        assert_eq!(
            read_response(&mut Cursor::new(resp), 1024).await.unwrap(),
            Some(vec![0u8, 1, 2, 3, 4, 5])
        );

        // Miss: an unknown ref -> NOT_FOUND, no path leak, no bytes.
        let mut req2 = Vec::new();
        write_request(
            &mut req2,
            &BlockPullRequest {
                block_ref: blob(9, 6),
            },
        )
        .await
        .unwrap();
        let mut resp2 = Vec::new();
        let served2 = handle_pull(&mut Cursor::new(req2), &mut resp2, &provider, "test-peer")
            .await
            .unwrap();
        assert_eq!(served2, 0);
        assert_eq!(
            read_response(&mut Cursor::new(resp2), 1024).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn an_over_long_stream_is_rejected_not_buffered_unboundedly() {
        // The server (or a hostile peer) streams more than the bound for the ref.
        let mut wire = vec![status::FOUND];
        wire.extend_from_slice(&vec![0xAB; 4096]);
        let mut cur = Cursor::new(wire);
        // bound = size_bytes(10)+1 = 11 < 4096 -> rejected.
        let bound = encoded_read_bound(&blob(1, 10));
        let err = read_response(&mut cur, bound).await.unwrap_err();
        assert!(matches!(err, BlockPullError::TooLarge(_)));
    }
}
