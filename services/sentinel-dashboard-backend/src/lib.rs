//! SOTA dashboard backend (#431): axum HTTP(S) + WebTransport/QUIC push with
//! topic+msgpack+zstd framing. Serves the SolidJS bundle (#419), proxies the
//! Operator-API/Gateway control endpoints, and exposes the projection read-models.
//!
//! Module layout (filled in across #431 sub-tasks):
//! - `codec`      — topic+msgpack+zstd frame encode/decode (noaide-compatible wire)
//! - `auth`       — httpOnly session auth (#402/#405 port)
//! - `projection` — read-only projection.db access + read routes
//! - `routes`     — control-proxy to Operator-API/Gateway
//! - `wt`         — WebTransport/QUIC endpoint (self-signed TLS, uni-stream push)

#![forbid(unsafe_code)]

pub mod codec;
