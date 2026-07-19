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
//!   pull      <peer_addr> <peer_fp_hex> <blob_hash_hex> <size> <cert_dir> <peer_node_id> <dest_data_dir>
//!   integrity <peer_addr> <peer_fp_hex> <blob_hash_hex> <size> <cert_dir> <peer_node_id>
//!   bench     <peer_addr> <peer_fp_hex> <blob_hash_hex> <size> <cert_dir> <peer_node_id> <iters>
//!   resolve   <peer_addr> <peer_fp_hex> <blob_hash_hex> <size> <cert_dir> <peer_node_id> <dest_data_dir>
//! ```
//!
//! `resolve` is the PR3 (4c) AC: it wires a real `BlockResolver` (V9) into a fresh
//! `CasStore` and drives the actual `CasStore::read` API — proving a read of a remote-only
//! blob resolves on a miss (pull + verify + durable store + retry) and a second read is a
//! local hit with no extra pull. This exercises the same resolver code the daemon wires in.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sentinel_cluster_control::{BlockPullClient, CertFingerprint, NodeCertificate, PeerRegistry};
use sentinel_common::{BlockRef, NodeId};
use sentinel_fs::artifact::ChunkHash;
use sentinel_fs::block_resolver::{BlobResolve, BlockResolver, BlockStore, RemotePull};
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
    let peer_node_id = NodeId(uuid::Uuid::parse_str(&a[7]).expect("peer_node_id UUID"));
    let block_ref = BlockRef::blob_sha256(hash, size);

    let node = load_cert(cert_dir);
    let client = BlockPullClient::new(
        &node,
        PeerRegistry::new([(peer_fp, peer_node_id)]).expect("peer registry"),
    )
    .expect("client");

    match mode {
        "pull" => {
            let dest = &a[8];
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
            let iters: usize = a[8].parse().expect("iters");
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
        "resolve" => {
            // PR3 (4c) AC-6: drive the real CasStore::read API with a wired BlockResolver,
            // proving a remote-only blob resolves on a read miss cross-VM (V9 read path).
            let dest = a[8].clone();
            let cas = Arc::new(CasStore::open(Path::new(&dest)).expect("dest cas"));
            let cached_before = cas.contains(&hash);

            // One bridge object that is both the resolver's local-existence `BlockStore` and
            // its `RemotePull`: pull_blob block_on's the QUIC pull on a non-worker thread
            // (mirrors the daemon's DaemonRemotePull / the FUSE sync read path), verifies +
            // durably stores into a fresh CasStore over `dest` (stateless dir wrapper, no
            // Arc cycle with the CasStore the resolver is wired into). Shared pull counter.
            struct Resolve {
                client: BlockPullClient,
                peer_addr: std::net::SocketAddr,
                peer_fp: CertFingerprint,
                block_ref: BlockRef,
                hash: [u8; 32],
                size: u64,
                dest: std::path::PathBuf,
                handle: tokio::runtime::Handle,
                pulls: AtomicUsize,
            }
            impl BlockStore for Resolve {
                fn has_blob(&self, hash: &[u8; 32]) -> bool {
                    CasStore::open(&self.dest).is_ok_and(|c| c.contains(hash))
                }
                fn has_chunk(&self, _hash: &ChunkHash) -> bool {
                    false
                }
            }
            impl RemotePull for Resolve {
                fn pull_blob(&self, _hash: &[u8; 32]) -> bool {
                    self.pulls.fetch_add(1, Ordering::Relaxed);
                    let pulled = self.handle.block_on(self.client.pull(
                        self.peer_addr,
                        self.peer_fp,
                        &self.block_ref,
                    ));
                    let Ok(Some(encoded)) = pulled else {
                        return false;
                    };
                    let Ok(cas) = CasStore::open(&self.dest) else {
                        return false;
                    };
                    cas.store_pulled_blob(&encoded, &self.hash, self.size)
                        .is_ok()
                }
                fn pull_chunk(&self, _hash: &ChunkHash) -> bool {
                    false
                }
            }

            let bridge = Arc::new(Resolve {
                client,
                peer_addr,
                peer_fp,
                block_ref,
                hash,
                size,
                dest: std::path::PathBuf::from(&dest),
                handle: tokio::runtime::Handle::current(),
                pulls: AtomicUsize::new(0),
            });
            let resolver = Arc::new(BlockResolver::new(
                bridge.clone() as Arc<dyn BlockStore>,
                bridge.clone() as Arc<dyn RemotePull>,
            ));
            cas.set_resolver(resolver as Arc<dyn BlobResolve>);

            // The read must run off a runtime worker so the puller's block_on is legal — this
            // is exactly the daemon's FUSE-thread design.
            let read_cas = cas.clone();
            let content = std::thread::spawn(move || read_cas.read(&hash))
                .join()
                .expect("read thread")
                .expect("read resolves the remote-only blob");
            let pulls_after_first = bridge.pulls.load(Ordering::Relaxed);
            println!(
                "RESOLVE ok: hash={} content_len={} cached_before={} pulls={}",
                a[4],
                content.len(),
                cached_before,
                pulls_after_first
            );

            // Second read of the now-local blob: a cache hit, no extra pull.
            let read_cas2 = cas.clone();
            let content2 = std::thread::spawn(move || read_cas2.read(&hash))
                .join()
                .expect("read thread 2")
                .expect("second read");
            println!(
                "SECOND-READ content_len={} pulls={} (no extra pull)",
                content2.len(),
                bridge.pulls.load(Ordering::Relaxed)
            );
        }
        other => eprintln!("unknown mode {other:?}"),
    }
}
