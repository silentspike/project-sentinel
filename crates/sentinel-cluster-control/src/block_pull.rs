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

use sentinel_common::BlockRef;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::envelope::{decode_frame, encode_frame, CodecError};

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
        assert_eq!(back, req, "the request (a BlockRef, never a path) survives the wire");
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
