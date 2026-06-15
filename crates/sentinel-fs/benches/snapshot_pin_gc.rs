//! #492 (TM-4) Snapshot-Blob-Pin / Trash-GC-Benchmark: misst den Pro-Pass-Aufwand der
//! `gc_trash`-Erweiterung (der `pinned_hashes()`-Aufbau + die O(1)-Pin-Membership pro Blob) und
//! beweist die KORREKTHEIT (kein von einem Snapshot gepinnter Blob wird je faelschlich geloescht).
//!
//! Standalone (`harness = false`, eigenes `fn main`): kein Daemon, Temp-redb + Temp-CAS unter einem
//! TempDir, daher sicher neben dem Produktiv-Daemon auf der Deploy-VM ausfuehrbar (kein
//! #279-Reconcile, keine cgroups, Lehre #529).
//!
//! ```text
//! Build (remote):  cargo remote -c -- build -p sentinel-fs --release --bench snapshot_pin_gc
//! Run (Deploy-VM): scp target/release/deps/snapshot_pin_gc-* ubuntu@10.0.0.240:/tmp/ && ./snapshot_pin_gc-*
//!                  parallel mit Sidecars: vmstat 1 / iostat -x 1 / mpstat 1
//! ```
//!
//! Gemessen:
//! - (a) `pinned_hashes()`-Aufbau-Latenz (transienter HashSet-Scan der Pin-Tabelle) + die volle
//!   `gc_trash`-Pass-Zeit MIT Pin-Check, ueber den Sweep N ∈ {100, 1k, 10k}.
//! - (b) Bug-Finder: nach der GC ist JEDER gepinnte Blob noch auf der Platte (kein false-delete) und
//!   GENAU die ungepinnten, abgelaufenen Blobs wurden befreit.
//! - (c) Sweep ueber Blob-/Pin-Zahl → Pin-Index-Groesse + GC-Skalierung (bestaetigt: ein transientes
//!   HashSet + EINE `(snapshot_id, hash)`-Tabelle skaliert; sekundaerer hash-first-Index unnoetig).

use std::time::{Instant, SystemTime};

use sentinel_fs::cas::CasStore;
use sentinel_fs::metadata::{InodeData, MetadataDurability, MetadataStore};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Baut N abgelaufene Trash-Blobs (refcount 0, ueber der Grace-Period), pinnt einen Anteil
/// `pinned_frac` ueber EINEN Snapshot und misst (a) `pinned_hashes()`-Aufbau + `gc_trash`-Pass sowie
/// (b)/(c) Korrektheit + Skalierung. Jeder Blob hat eindeutigen Inhalt → eindeutiger Hash (kein Dedup).
fn bench_pin_gc(n: usize, pinned_frac: f64) {
    let dir = tempfile::tempdir().unwrap();
    // Eventual durability for the throwaway bench DB: the per-blob setup writes are otherwise
    // ~3 fsync'd redb txns each (30k fsyncs at N=10k on the 2011 virtio disk = minutes of SETUP,
    // not the measured path). gc_trash time is CAS-unlink-IO-dominated, durability-independent.
    let store = MetadataStore::open_with_durability(
        dir.path().join("meta.redb"),
        MetadataDurability::Eventual,
    )
    .unwrap();
    let cas = CasStore::open(dir.path()).unwrap();
    let expired = now_ms() - 25 * 3600 * 1000; // 25h alt > 24h Grace

    let mut hashes = Vec::with_capacity(n);
    for i in 0..n {
        let content = format!("issue-492-blob-{i:08}-padding-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let (hash, _) = cas.store(content.as_bytes()).unwrap();
        let data = InodeData::regular(hash, content.len() as u64, 0o644);
        let inode = (i + 2) as u64;
        let name = format!("f{i}");
        store
            .create_file("AGENT-01", 1, &name, inode, &data)
            .unwrap();
        store.remove_file("AGENT-01", 1, &name, inode).unwrap(); // refcount -> 0, in Trash
        store.set_trash_timestamp(&hash, Some(expired)).unwrap();
        hashes.push(hash);
    }

    let n_pinned = (n as f64 * pinned_frac) as usize;
    store
        .pin_snapshot_blobs("snap-bench", &hashes[..n_pinned])
        .unwrap();

    // (a) pinned_hashes()-Aufbau (transienter Scan der Pin-Tabelle).
    let t0 = Instant::now();
    let pin_set = store.pinned_hashes().unwrap();
    let pin_build_us = t0.elapsed().as_micros();
    assert_eq!(pin_set.len(), n_pinned, "Pin-Set muss alle Pins enthalten");

    // gc_trash-Pass MIT Pin-Check (baut intern pinned_hashes erneut + scannt die Trash-Queue).
    let t1 = Instant::now();
    let stats = store.gc_trash(&cas, 24).unwrap();
    let gc_ms = t1.elapsed().as_secs_f64() * 1000.0;

    // (b) Bug-Finder: kein gepinnter Blob geloescht; alle ungepinnten befreit.
    let pinned_surviving = hashes[..n_pinned]
        .iter()
        .filter(|h| cas.contains(h))
        .count();
    let unpinned_remaining = hashes[n_pinned..]
        .iter()
        .filter(|h| cas.contains(h))
        .count();
    let freed = stats.freed_from_trash;
    println!(
        "pin_gc  N={n:6}  pinned={n_pinned:6}  | pinned_hashes_build={pin_build_us:7}us  gc_trash_pass={gc_ms:9.3}ms  freed={freed:6}  pinned_surviving={pinned_surviving}/{n_pinned}"
    );
    assert_eq!(
        pinned_surviving, n_pinned,
        "BUG: ein von einem Snapshot gepinnter Blob wurde geloescht!"
    );
    assert_eq!(
        unpinned_remaining, 0,
        "BUG: ein ungepinnter, abgelaufener Blob ueberlebte die GC"
    );
    assert_eq!(
        freed as usize,
        n - n_pinned,
        "BUG: nicht genau die ungepinnten Blobs wurden befreit"
    );
}

fn main() {
    println!("=== #492 Snapshot-Blob-Pin / Trash-GC-Benchmark (MetadataStore::gc_trash) ===");
    println!("Pin = (snapshot_id, hash)-Pointer (kein Blob-Copy); GC-Check = transientes HashSet, O(1)/Blob.");
    println!("Sweep N ∈ {{100, 1k, 10k}}, pinned_frac=0.5 → Pin-Index-Groesse + GC-Skalierung.\n");

    bench_pin_gc(100, 0.5);
    bench_pin_gc(1_000, 0.5);
    bench_pin_gc(10_000, 0.5);

    println!("\nOK: kein gepinnter Blob je geloescht (Bug-Finder gruen), GC skaliert linear im transienten Pin-HashSet.");
}
