//! #496 PR2b-2a — V26 steady-state overhead of the owner fence on the hot write path.
//!
//! Every fenced store write calls `OwnerRegistry::validate`/`current_owner`. PR2b-2a made
//! the registry mutable (a committed-term map behind a `RwLock`), so this measures that
//! the **single-node fast path is lock-free**: an atomic `Relaxed` load of the mode flag
//! short-circuits the lock/map entirely while single-node, vs. the cluster-mode path that
//! takes the `RwLock` read + map lookup. Standalone (`harness = false`, no criterion dep)
//! so the binary runs self-contained on a test VM — **never** under `cargo remote`.

use sentinel_common::{NodeId, OwnerRegistry, OwnerTerm, StateTransferScope};
use std::hint::black_box;
use std::time::Instant;

fn bench(name: &str, iters: u64, mut f: impl FnMut()) {
    for _ in 0..100_000 {
        f();
    }
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    let elapsed = start.elapsed();
    let ns = elapsed.as_nanos() as f64 / iters as f64;
    println!("{name:<44} {ns:7.2} ns/op   ({iters} iters in {elapsed:?})");
}

fn main() {
    let iters: u64 = std::env::var("ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(50_000_000);

    let reg = OwnerRegistry::global(); // process-global, single-node default
    let scope = StateTransferScope::World;
    let guard = reg.issue(scope.clone());

    println!("== #496 PR2b-2a owner-fence V26 overhead (iters={iters}) ==");
    assert!(
        !reg.is_cluster_mode(),
        "registry must start single-node for the fast-path measurement"
    );

    // Single-node fast path: a Relaxed atomic load short-circuits to the synthesized
    // seed term — no RwLock, no map lookup. This is the live prod write path.
    bench("validate  single-node (fast-path, no lock)", iters, || {
        black_box(reg.validate(black_box(&guard))).ok();
    });
    bench("owner     single-node (fast-path, no lock)", iters, || {
        black_box(reg.current_owner(black_box(&scope)));
    });

    // Flip to cluster mode (a cross-node commit on an unrelated scope sets the flag), so
    // validate now consults the term map under the RwLock read lock.
    reg.commit_owner(OwnerTerm {
        scope: StateTransferScope::NanoContainer("AGENT-99".into()),
        owner_node: NodeId::default(),
        epoch: 2,
    });
    assert!(reg.is_cluster_mode());

    bench("validate  cluster-mode (RwLock read+lookup)", iters, || {
        black_box(reg.validate(black_box(&guard))).ok();
    });
    bench("owner     cluster-mode (RwLock read+lookup)", iters, || {
        black_box(reg.current_owner(black_box(&scope)));
    });

    println!("(single-node = the prod path; the fast-path delta vs cluster-mode is the lock cost avoided)");
}
