//! #250 Retention-Benchmark: misst `SnapshotManager::maintain()` an einer konvergierten
//! Snapshot-Population, wie sie im Produktivbetrieb tatsaechlich vorliegt.
//!
//! Standalone (`harness = false`, eigenes `fn main`): kein Daemon, Temp-SQLite, daher sicher neben
//! dem Produktiv-Daemon auf der Deploy-VM ausfuehrbar (kein #279-Reconcile, keine cgroups, Lehre #529).
//!
//! ```text
//! Build (remote):  cargo remote -c -- build -p sentinel-daemon --release --bench retention_bench
//! Run (Deploy-VM): scp target/release/deps/retention_bench-* ubuntu@10.0.0.240:/tmp/ && ./retention_bench
//!                  parallel mit Sidecars: vmstat 1 / mpstat 1 / iostat -x 1
//! ```
//!
//! Realismus: produktiv laeuft `maintain()` etwa einmal pro Stunde inkrementell. Die in-memory
//! Snapshot-Liste wird innerhalb eines Laufs nicht aktualisiert, d.h. die Dedup wirkt ZWISCHEN den
//! Laeufen (jeder Lauf sieht die im Vorlauf promoteten Keeper). Eine frisch gealterte Population in
//! einem Lauf zu migrieren ist deshalb kein repraesentativer Messpunkt (over-promote, Massen-SQL).
//! Dieser Bench baut die bereits konvergierte Verteilung (je 1 daily/weekly/monthly-Keeper plus
//! redundante junge hourly) und misst den realistischen Pro-Lauf-Aufwand: dominiert von der
//! O(n)-Kalender-Dedup-Skalierung (#250) und der `kept_protected`-Buchhaltung (kein SQL fuer junge
//! redundante), wenige Deletes (Alter ueber 7 Tage). Live-Beleg der Konvergenz: VM daily etwa 9.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use sentinel_daemon::config::RetentionConfig;
use sentinel_daemon::snapshot::SnapshotManager;
use sentinel_limbo::EventStore;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;
const PAYLOAD: &[u8] = b"retention-bench-payload-64-bytes-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Baut einen frischen EventStore mit einer konvergierten, UTC-Kalender-ausgerichteten Verteilung.
/// 12 sehr junge hourly (unter 12h alt, nicht promotion-faehig); fuer `days` vergangene Kalendertage
/// (alle ueber 24h und unter 7d alt) je 1 daily-Keeper plus `redundant_per_day` redundante hourly am
/// selben UTC-Tag wie der Keeper, sodass `has_snapshot_for_day` true liefert und sie als
/// `kept_protected` uebersprungen werden (kein Promote, kein Delete da unter 7 Tagen). Dazu `weeks`
/// weekly- und `months` monthly-Keeper weiter zurueck. Alles auf UTC-Mitternacht verankert, damit die
/// Verteilung deterministisch konvergiert (unabhaengig von der Tageszeit). maintain() leistet dadurch
/// reinen O(n)-Kalender-Dedup-Scan (die #250-Aenderung) statt Massen-SQL. Liefert (TempDir, Store, n).
fn populate_converged(
    days: i64,
    redundant_per_day: i64,
    weeks: i64,
    months: i64,
) -> (tempfile::TempDir, Arc<EventStore>, usize) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(EventStore::open(dir.path().join("bench.db").to_str().unwrap()).unwrap());
    let base = now_ms();
    let today_midnight = (base / DAY_MS) * DAY_MS;
    let mut n = 0usize;
    let mut id = 0u64;
    let mut put = |tier: &str, created: i64, tick: u64| {
        store
            .save_world_snapshot_at(
                &format!("s{id:07}"),
                tier,
                tick,
                0.0,
                id as i64,
                PAYLOAD,
                created,
            )
            .unwrap();
        id += 1;
    };

    // Letzte 12h: junge hourly, noch nicht promotion-faehig (created > daily_cutoff).
    for h in 0..12 {
        put("hourly", base - h * HOUR_MS, h as u64);
        n += 1;
    }
    // Kalendertage d=2..=days+1 (sauber >24h alt, <7d wenn days+1<=7): je 1 daily-Keeper + redundante
    // hourly am SELBEN UTC-Tag (alle innerhalb der ersten ~14h des Tages → garantiert derselbe Tag).
    for d in 2..=(days + 1) {
        let day = today_midnight - d * DAY_MS;
        put("daily", day + 12 * HOUR_MS, (d * 1000) as u64);
        n += 1;
        for r in 0..redundant_per_day {
            put(
                "hourly",
                day + HOUR_MS + r * 5 * 60_000,
                (d * 1000 + r) as u64,
            );
            n += 1;
        }
    }
    // Weekly Keeper (1/Woche) + Monthly Keeper (1/Monat), weiter zurueck.
    for w in 0..weeks {
        put(
            "weekly",
            today_midnight - (10 + w * 7) * DAY_MS,
            10_000 + w as u64,
        );
        n += 1;
    }
    for m in 0..months {
        put(
            "monthly",
            today_midnight - (45 + m * 30) * DAY_MS,
            20_000 + m as u64,
        );
        n += 1;
    }
    (dir, store, n)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

/// Misst `maintain()` auf einer konvergierten Population (realistischer Pro-Lauf-Aufwand).
fn bench_maintain(label: &str, days: i64, redundant: i64, trials: usize) {
    let cfg = RetentionConfig::default();
    let mut durations_ms = Vec::with_capacity(trials);
    let (mut total, mut promoted, mut deleted, mut protected) = (0usize, 0u32, 0u32, 0u32);
    for _ in 0..trials {
        let (_dir, store, n) = populate_converged(days, redundant, 4, 4);
        total = n;
        let mut mgr = SnapshotManager::new(cfg.clone());
        let t0 = Instant::now();
        let report = mgr.maintain(&store, None).unwrap();
        durations_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        promoted = report.promoted;
        deleted = report.deleted;
        protected = report.kept_protected;
    }
    durations_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "maintain {label:<18} N={total:5}  p50={:7.3}ms  p95={:7.3}ms  max={:7.3}ms  | promoted={promoted} deleted={deleted} kept_protected={protected}",
        percentile(&durations_ms, 0.50),
        percentile(&durations_ms, 0.95),
        durations_ms.last().copied().unwrap_or(0.0),
    );
}

fn bench_queries(label: &str, days: i64, redundant: i64) {
    let (_dir, store, n) = populate_converged(days, redundant, 4, 4);
    let mut mgr = SnapshotManager::new(RetentionConfig::default());
    mgr.maintain(&store, None).unwrap();

    let t0 = Instant::now();
    let metas = store.list_world_snapshots().unwrap();
    let list_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let t1 = Instant::now();
    let rows = store.count_world_snapshots_by_tier().unwrap();
    let dist_ms = t1.elapsed().as_secs_f64() * 1000.0;

    // Restore-Anker: Blob-Fetch des neuesten Snapshots (Proxy; voller Restore = #491-Pfad).
    let newest = &metas[0];
    let t2 = Instant::now();
    let _blob = store.load_world_snapshot(&newest.id).unwrap().unwrap();
    let load_ms = t2.elapsed().as_secs_f64() * 1000.0;

    println!(
        "queries {label:<18} N={n:5}  list={list_ms:.3}ms ({} rows)  tier_dist_sql={dist_ms:.3}ms  load_snapshot={load_ms:.3}ms  tiers={rows:?}",
        metas.len()
    );
}

/// Korrektheit: 5 aufeinanderfolgende maintain()-Zyklen muessen KONVERGIEREN (Total nicht-monoton-
/// wachsend) und duerfen keinen #264-Drift-Alarm ausloesen (`delete_blocked_young == 0`). Die
/// rigorose, deterministische Verifikation der Kalender-Dedup (UTC-Tag/Woche/Monat) liegt in den
/// Unit-Tests (snapshot.rs `week_dedup_*`/`month_dedup_*`/`day_dedup_*`); hier wird die Anzahl
/// gleicher Kalenderperioden pro Tier nur BERICHTET (nicht asserted), weil maintain() pre-existierende
/// Gleich-Tier-Snapshots NICHT dedupliziert — die Dedup wirkt ausschliesslich beim Promoten, und der
/// stale-in-memory-Snapshot innerhalb EINES Laufs kann bei Backlog (zwei Geschwister-Perioden kreuzen
/// dieselbe Schwelle im selben Lauf) einen Doppel-Keeper erzeugen. Das ist eine PRE-EXISTIERENDE
/// stale-list-Eigenschaft (von #250 unveraendert: #250 tauscht nur die Bucket-Funktion) und im
/// inkrementellen Stundenbetrieb selten — als Follow-up-Kandidat notiert, NICHT im #250-Scope.
fn correctness_cycles(days: i64, redundant: i64) {
    let (_dir, store, n0) = populate_converged(days, redundant, 4, 4);
    let mut mgr = SnapshotManager::new(RetentionConfig::default());
    let mut prev_total = usize::MAX;
    println!("correctness (Start N={n0}, 5 Zyklen):");
    for cycle in 1..=5 {
        let report = mgr.maintain(&store, None).unwrap();
        let metas = store.list_world_snapshots().unwrap();
        let total = metas.len();
        let (mut h, mut d, mut w, mut m) = (0, 0, 0, 0);
        for s in &metas {
            match s.tier {
                sentinel_common::SnapshotTier::Hourly => h += 1,
                sentinel_common::SnapshotTier::Daily => d += 1,
                sentinel_common::SnapshotTier::Weekly => w += 1,
                sentinel_common::SnapshotTier::Monthly => m += 1,
            }
        }
        assert!(
            total <= prev_total || cycle == 1,
            "Zyklus {cycle}: Total {total} > vorher {prev_total} — Retention waechst (Konvergenz verletzt)"
        );
        prev_total = total;
        assert_eq!(
            report.delete_blocked_young, 0,
            "Drift-Alarm: Trigger blockte einen Delete, den der Daemon nicht uebersprungen hat"
        );
        let dups = calendar_duplicate_count(&metas);
        println!(
            "  Zyklus {cycle}: total={total:5} hourly={h} daily={d} weekly={w} monthly={m} | promoted={} deleted={} kept_protected={} blocked_young={} kalender_dups={dups}",
            report.promoted, report.deleted, report.kept_protected, report.delete_blocked_young
        );
    }
    println!("  OK: konvergiert (Total nicht-wachsend), kein #264-Drift-Alarm");
}

/// Zaehlt Daily/Weekly/Monthly-Snapshots, die dieselbe Kalenderperiode (UTC-Tag / Montag-Woche /
/// Kalendermonat) teilen — informativ (siehe `correctness_cycles`-Doku zur stale-list-Eigenschaft).
fn calendar_duplicate_count(metas: &[sentinel_common::SnapshotMeta]) -> usize {
    use chrono::{DateTime, Datelike, Utc};
    let key = |ms: i64, tier: sentinel_common::SnapshotTier| -> String {
        let dt: DateTime<Utc> = DateTime::from_timestamp_millis(ms)
            .unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap());
        let d = dt.date_naive();
        match tier {
            sentinel_common::SnapshotTier::Daily => format!("D:{d}"),
            sentinel_common::SnapshotTier::Weekly => {
                let monday = d - chrono::Duration::days(d.weekday().num_days_from_monday() as i64);
                format!("W:{monday}")
            }
            sentinel_common::SnapshotTier::Monthly => format!("M:{}-{}", dt.year(), dt.month()),
            sentinel_common::SnapshotTier::Hourly => String::new(),
        }
    };
    let mut seen = std::collections::HashSet::new();
    let mut dups = 0;
    for s in metas {
        if matches!(s.tier, sentinel_common::SnapshotTier::Hourly) {
            continue;
        }
        if !seen.insert(key(s.created_at_ms, s.tier)) {
            dups += 1;
        }
    }
    dups
}

fn main() {
    println!(
        "=== #250 Retention-Benchmark (SnapshotManager::maintain, konvergierte Population) ==="
    );
    println!(
        "config: hourly_interval=3600 daily_keep_hours=24 weekly_keep_days=7 monthly_keep_weeks=4"
    );
    println!("Produktiv-Bound world_snapshots ≈ 200 (7d×24 hourly + daily/weekly/monthly).\n");

    // days=6 (d=2..7); realistic ≈ 200 (28 redundante/Tag), Headroom ≈ 1000 (160/Tag).
    // Trials bewusst klein: maintain() ist ~ms-schnell, der Per-Trial-Kostentreiber ist
    // EventStore::open() (Schema-DDL + WAL + 256MB-mmap) auf der 2011-VM-virtio-Disk.
    bench_maintain("realistic(~200)", 6, 28, 10);
    bench_maintain("headroom(~1000)", 6, 160, 6);
    println!();
    bench_queries("realistic(~200)", 6, 28);
    bench_queries("headroom(~1000)", 6, 160);
    println!();
    correctness_cycles(6, 28);
}
