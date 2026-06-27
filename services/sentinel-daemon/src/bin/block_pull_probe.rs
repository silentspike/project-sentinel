//! #498 4b — block-pull probe / benchmark harness.
//!
//! Pulls a blob **by hash** from a peer's QUIC block-pull server (the running daemon's
//! port = control_port + 1), enforcing the cert pin, then verifies + (optionally) durably
//! publishes it. Used for the 2-VM live ACs; never touches the running daemon's state
//! beyond an explicit `--dest` CAS dir. Presents `cert_dir`'s control cert (which the
//! peer pins, V10).
//!
//! Modes (positional args):
//!
//! ```text
//!   pull      <peer_addr> <peer_fp_hex> <blob_hash_hex> <size> <cert_dir> <dest_data_dir>
//!   integrity <peer_addr> <peer_fp_hex> <blob_hash_hex> <size> <cert_dir>
//!   bench     <peer_addr> <peer_fp_hex> <blob_hash_hex> <size> <cert_dir> <iters>
//! ```

use std::path::Path;
use std::time::Instant;

use sentinel_cluster_control::{BlockPullClient, CertFingerprint, NodeCertificate};
use sentinel_common::BlockRef;
use sentinel_fs::cas::CasStore;

fn hex_decode_32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("hex");
    }
    out
}

fn load_cert(cert_dir: &str) -> NodeCertificate {
    let dir = Path::new(cert_dir);
    NodeCertificate::load_or_generate(
        &dir.join("control-node-cert.der"),
        &dir.join("control-node-key.der"),
        "block-pull-probe",
    )
    .expect("load node cert")
}

fn summarize(mut us: Vec<u64>) -> String {
    us.sort_unstable();
    let p = |q: f64| us[(((us.len() - 1) as f64) * q) as usize];
    format!(
        "p50={} p95={} p99={} max={} us",
        p(0.50),
        p(0.95),
        p(0.99),
        us[us.len() - 1]
    )
}

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mode = a.get(1).map(String::as_str).unwrap_or("pull");
    let peer_addr: std::net::SocketAddr = a[2].parse().expect("peer_addr");
    let peer_fp = CertFingerprint::from_hex(&a[3]).expect("peer fingerprint hex");
    let hash = hex_decode_32(&a[4]);
    let size: u64 = a[5].parse().expect("size");
    let cert_dir = &a[6];
    let block_ref = BlockRef::blob_sha256(hash, size);

    let node = load_cert(cert_dir);
    let client = BlockPullClient::new(&node).expect("client");

    match mode {
        "pull" => {
            let dest = &a[7];
            let cas = CasStore::open(Path::new(dest)).expect("dest cas");
            let already = cas.contains(&hash);
            let pulled = client
                .pull(peer_addr, peer_fp, &block_ref)
                .await
                .expect("pull rpc");
            match pulled {
                Some(encoded) => {
                    cas.store_pulled_blob(&encoded, &hash, size)
                        .expect("verify + durable publish");
                    println!(
                        "PULL ok: hash={} wire_bytes={} verified+durable=true cached_before={}",
                        a[4],
                        encoded.len(),
                        already
                    );
                    // Second read must NOT pull (local cache hit).
                    let second = cas.contains(&hash);
                    println!("SECOND-READ local_hit={second} (no pull needed)");
                }
                None => println!("PULL miss: peer does not hold {}", a[4]),
            }
        }
        "integrity" => {
            // Pull the real bytes, tamper one content byte, then verify -> must reject.
            let encoded = client
                .pull(peer_addr, peer_fp, &block_ref)
                .await
                .expect("pull rpc")
                .expect("peer holds the block");
            let mut corrupt = encoded.clone();
            corrupt[1] ^= 0xFF;
            let dir =
                std::env::temp_dir().join(format!("blockpull-probe-int-{}", std::process::id()));
            let cas = CasStore::open(&dir).expect("tmp cas");
            match cas.store_pulled_blob(&corrupt, &hash, size) {
                Ok(()) => println!("INTEGRITY FAIL: a corrupt blob was published!"),
                Err(e) => println!("INTEGRITY ok: corrupt blob rejected, not published: {e}"),
            }
        }
        "bench" => {
            let iters: usize = a[7].parse().expect("iters");
            let dir =
                std::env::temp_dir().join(format!("blockpull-probe-bench-{}", std::process::id()));
            let cas = CasStore::open(&dir).expect("tmp cas");
            let mut pull_us = Vec::with_capacity(iters);
            let mut wire = 0usize;
            for _ in 0..iters {
                let t0 = Instant::now();
                let encoded = client
                    .pull(peer_addr, peer_fp, &block_ref)
                    .await
                    .expect("pull")
                    .expect("found");
                pull_us.push(t0.elapsed().as_micros() as u64);
                wire = encoded.len();
                // verify (the durable publish is dedup'd after the first)
                let _ = cas.store_pulled_blob(&encoded, &hash, size);
            }
            println!(
                "BENCH pull-by-hash size={size} wire_bytes={wire} iters={iters}: {}",
                summarize(pull_us)
            );
        }
        other => eprintln!("unknown mode {other:?}"),
    }
}
