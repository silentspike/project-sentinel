//! Model-checked proof of the `OwnerRegistry` publish ordering (#496 PR2b-2bi).
//!
//! `loom` exhaustively explores thread interleavings **and** weak-memory reorderings, so
//! it fails if the commit/observe ordering allows a reader to see cluster mode while
//! missing the just-committed term. Gated behind the `loom-test` feature so normal builds
//! and CI never compile it; run with:
//!
//! ```text
//! cargo remote -c -- test -p sentinel-common --features loom-test --test loom_owner_ordering
//! ```
#![cfg(feature = "loom-test")]

use loom::sync::atomic::{AtomicU8, Ordering};
use loom::sync::{Arc, RwLock};
use std::collections::HashMap;

const MODE_SINGLE_NODE: u8 = 0;
const MODE_CLUSTER: u8 = 1;

/// Mirrors `OwnerRegistry::commit_owner` (insert the term under the write lock, **then**
/// `mode.store(Release)`) racing `current_owner` (`mode.load(Acquire)`, then `terms.read()`).
///
/// Invariant under test: a reader that observes `MODE_CLUSTER` MUST see the committed term
/// — it can never read cluster mode, miss the term, fall back to the seed term and wrongly
/// accept/reject a write (a silent split-brain window). With the reverse order
/// (mode-before-insert / `Relaxed`) loom finds the violating interleaving and this fails.
#[test]
fn observing_cluster_mode_implies_the_term_is_visible() {
    loom::model(|| {
        let mode = Arc::new(AtomicU8::new(MODE_SINGLE_NODE));
        let terms = Arc::new(RwLock::new(HashMap::<u8, u64>::new()));

        let committer = {
            let mode = mode.clone();
            let terms = terms.clone();
            loom::thread::spawn(move || {
                // commit_owner: term first (under the lock), then publish the mode flag.
                {
                    let mut g = terms.write().unwrap();
                    g.insert(7u8, 2u64); // scope 7 committed at epoch 2
                }
                mode.store(MODE_CLUSTER, Ordering::Release);
            })
        };

        // current_owner hot path: acquire-load the mode; if cluster, the term must be there.
        if mode.load(Ordering::Acquire) == MODE_CLUSTER {
            let g = terms.read().unwrap();
            assert_eq!(
                g.get(&7u8),
                Some(&2u64),
                "observed cluster mode but missed the committed term (split-brain window)"
            );
        }

        committer.join().unwrap();
    });
}
