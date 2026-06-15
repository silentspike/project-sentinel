//! #493 (TM-5) Dead-Branch-GC-Benchmark: misst (a) den Read-Guard-Overhead (`dead_range_exclusion`
//! in jedem Event-Read) p50/p95 MIT vs OHNE `dead_ranges`, und (b) den Prune-Durchsatz, mit dem ein
//! verworfener Zukunfts-Zweig im Retention-Fenster physisch entfernt wird; plus (c) einen Bug-Finder
//! (kein lebendes Event wird faelschlich als tot entfernt) ueber den Sweep der Dead-Intervall-Groesse.
//!
//! Standalone (`harness = false`, eigenes `fn main`): kein Daemon, Temp-SQLite unter einem TempDir,
//! daher sicher neben dem Produktiv-Daemon auf der Deploy-VM ausfuehrbar (Lehre #529).
//!
//! ```text
//! Build (remote):  cargo remote -c -- build -p sentinel-limbo --release --bench dead_branch_gc
//! Run (Deploy-VM): scp target/release/deps/dead_branch_gc-* ubuntu@10.0.0.240:/tmp/ && ./dead_branch_gc-*
//!                  parallel mit Sidecars: vmstat 1 / iostat -x 1 / mpstat 1
//! ```
//!
//! Realismus: `dead_ranges` waechst pro Restore um EIN Intervall und schrumpft, sobald der Pruner das
//! Intervall geleert hat (Auflage 2). Der Read-Guard kostet pro Query genau `read_dead_ranges` +
//! K `AND NOT (...)`-Klauseln. Der Bench haelt das Read-Fenster identisch (die K Intervalle liegen
//! oberhalb des Fensters) und isoliert so den reinen Klausel-Overhead.

use std::time::{Instant, SystemTime};

use sentinel_limbo::EventStore;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Bulk-seedet `n` Events (ids 1..=n) ueber EINE Transaktion direkt in die `events`-Tabelle (schnell;
/// der Seed-Aufwand ist NICHT das Messobjekt). Eindeutige `event_id`/`operation_id` pro Zeile.
fn seed_events(db_path: &str, n: i64) {
    let mut conn = rusqlite::Connection::open(db_path).unwrap();
    let base_ts = now_ms();
    let tx = conn.transaction().unwrap();
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO events (event_id, event_type, aggregate_id, payload, correlation_id, causation_id, operation_id, tick, timestamp_ms, schema_version, compensation_type) \
                 VALUES (?1, 'agent_action_received', 'AGENT-01', '{}', 'corr-1', NULL, ?2, ?3, ?4, 1, 'none')",
            )
            .unwrap();
        for i in 1..=n {
            stmt.execute(rusqlite::params![
                format!("evt-{i}"),
                format!("op-{i}"),
                i,
                base_ts + i
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

/// p50/p95 (us) von `get_events_since(0, limit)` ueber `iters` Wiederholungen.
fn time_reads(store: &EventStore, limit: usize, iters: usize) -> (f64, f64) {
    let mut us = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        let rows = store.get_events_since(0, limit).unwrap();
        std::hint::black_box(rows.len());
        us.push(t.elapsed().as_micros() as f64);
    }
    us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (percentile(&us, 0.50), percentile(&us, 0.95))
}

/// (a) Read-Guard-Overhead: dasselbe Read-Fenster (erste `limit` Events) OHNE und MIT `k_ranges`
/// toten Intervallen (oberhalb des Fensters → Ergebnis unveraendert, nur K Ausschluss-Klauseln mehr).
fn bench_read_guard(n: i64, limit: usize, k_ranges: i64) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bench.db");
    let db = db_path.to_str().unwrap();
    let store = EventStore::open(db).unwrap();
    seed_events(db, n);

    let (base_p50, base_p95) = time_reads(&store, limit, 300);

    // K winzige tote Intervalle weit OBERHALB des Read-Fensters (Resultat bleibt identisch).
    let top = n - 2 * k_ranges - 2;
    for j in 0..k_ranges {
        let from = top + 2 * j;
        store.push_dead_range(from, from + 1).unwrap();
    }
    let (dead_p50, dead_p95) = time_reads(&store, limit, 300);

    // Sanity: das Read-Fenster ist unveraendert (Intervalle liegen oberhalb).
    let got = store.get_events_since(0, limit).unwrap().len();
    assert_eq!(
        got,
        limit.min(n as usize),
        "Read-Fenster darf sich nicht aendern"
    );

    println!(
        "read_guard N={n:7} limit={limit:5} k_ranges={k_ranges:4} | p50: base={base_p50:7.1}us with={dead_p50:7.1}us (+{:+.1}us)  p95: base={base_p95:7.1}us with={dead_p95:7.1}us",
        dead_p50 - base_p50
    );
}

/// (b)/(c) Prune-Durchsatz + Bug-Finder: `dead_size` tote Events oben, `live_below` lebende unten.
/// Der Pruner muss GENAU die toten Events entfernen (auch oberhalb des retention-Cutoffs) und das
/// `dead_ranges`-Intervall leeren — kein lebendes Event darf verschwinden.
fn bench_prune(live_below: i64, dead_size: i64) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("bench.db");
    let db = db_path.to_str().unwrap();
    let store = EventStore::open(db).unwrap();
    let n = live_below + dead_size;
    seed_events(db, n);

    // Restore verwirft die "Zukunft" (live_below, n] = die obersten dead_size Events.
    store.increment_restore_generation().unwrap();
    store.push_dead_range(live_below, n).unwrap();

    // Prune mit cutoff=1 → `id < 1` entfernt NICHTS unten; nur das tote Intervall wird abgeraeumt
    // (aggressiv, oberhalb des Cutoffs). Loop bis 0 (wie der Tick-Loop, 1000 Rows/Batch).
    let t0 = Instant::now();
    let mut total = 0u64;
    loop {
        let d = store.prune_batch(1, 1000).unwrap();
        total += d;
        if d == 0 {
            break;
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    let per_s = (total as f64 / secs) as u64;

    // Bug-Finder: GENAU die toten Events weg, alle lebenden da, dead_ranges-Eintrag geleert.
    let remaining = store.event_count().unwrap();
    let dead_left = store.dead_ranges().unwrap().len();
    println!(
        "prune      live_below={live_below:6} dead_size={dead_size:7} | removed={total:7} in {:8.3}ms  ({per_s:>9}/s)  remaining={remaining}  dead_ranges_left={dead_left}",
        secs * 1000.0
    );
    assert_eq!(
        total, dead_size as u64,
        "BUG: nicht genau die toten Events entfernt"
    );
    assert_eq!(
        remaining, live_below,
        "BUG: ein lebendes Event wurde geloescht (oder ein totes ueberlebte)"
    );
    assert_eq!(
        dead_left, 0,
        "BUG: geleertes dead_ranges-Intervall nicht abgeraeumt"
    );
}

fn main() {
    println!("=== #493 Dead-Branch-GC-Benchmark (Read-Guard-Overhead + Prune-Durchsatz) ===");
    println!(
        "dead_ranges = id-Intervall-Pointer in sim_metadata; events bleibt append-only SSOT.\n"
    );

    println!("(a) Read-Guard-Overhead (get_events_since, identisches Fenster, k tote Intervalle):");
    bench_read_guard(100_000, 500, 0);
    bench_read_guard(100_000, 500, 1);
    bench_read_guard(100_000, 500, 16);
    bench_read_guard(100_000, 500, 64);

    println!("\n(b)/(c) Prune-Durchsatz + Bug-Finder (Sweep Dead-Intervall-Groesse):");
    bench_prune(1_000, 1_000);
    bench_prune(1_000, 10_000);
    bench_prune(1_000, 100_000);

    println!("\nOK: kein lebendes Event als tot entfernt (Bug-Finder gruen); dead_ranges-Eintrag nach Prune geleert.");
}
