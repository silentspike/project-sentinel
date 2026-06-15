//! SnapshotManager: Tiered World Snapshots fuer Time Machine (#250).
//!
//! Erstellt periodisch World Snapshots (bincode), promoted sie durch
//! Tiers (hourly→daily→weekly→monthly) und loescht abgelaufene.

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use chrono::{DateTime, Datelike, Utc};
use sentinel_common::{encode_world_snapshot, SnapshotMeta, SnapshotTier, WorldSnapshot};
use sentinel_fs::layer::LayerManager;
use sentinel_limbo::EventStore;
use sentinel_redb::StateStore;
use tracing::{debug, info};

use crate::config::RetentionConfig;

/// Waehlt den Cutoff fuer Pruning aus `list_world_snapshots()`-Ergebnissen.
///
/// Die Liste ist nach `tick DESC` sortiert. Index 1 ist der zweitneueste
/// Restore-Puffer; `len() - 2` waere der zweitaelteste Snapshot und kann nach
/// CTAS-Kompaktion dauerhaft unter `min(events.id)` liegen.
pub(crate) fn prune_cutoff_from_ordered_snapshots(snapshots: &[SnapshotMeta]) -> Option<i64> {
    snapshots.get(1).map(|snapshot| snapshot.last_event_id)
}

/// Verwaltet World Snapshots mit Tiered Retention.
pub struct SnapshotManager {
    config: RetentionConfig,
    last_snapshot_tick: u64,
    /// Aktiver Prune-Cutoff: wenn > 0, loescht prune_tick() 1 Batch pro Tick.
    prune_cutoff: i64,
    prune_total: u64,
    /// #529: nach einem Schichtwechsel wird genau am Shift-Tick ein Anker erzwungen, damit jeder
    /// Restore auf ein Ziel >= Shift-Tick den Post-Shift-Anker waehlt und das Replay-Fenster
    /// `(anker, ziel]` nie ueber eine Schichtgrenze laeuft (#491-Engine ist nur innerhalb einer
    /// Schicht byte-exakt — der Schichtwechsel selbst ist Daemon-Loop-Orchestrierung, nicht im
    /// ECS-Schedule, vgl. docs/spikes/SPIKE-529-cross-nightrun.md). Wird beim naechsten Snapshot
    /// wieder geloescht.
    shift_snapshot_pending: bool,
}

impl SnapshotManager {
    pub fn new(config: RetentionConfig) -> Self {
        Self {
            config,
            last_snapshot_tick: 0,
            prune_cutoff: 0,
            prune_total: 0,
            shift_snapshot_pending: false,
        }
    }

    /// #529: markiert, dass am aktuellen (Shift-)Tick ein Anker-Snapshot erzwungen werden soll.
    /// Aufgerufen vom Tick-Loop direkt nach Abschluss eines Schichtwechsels (Despawn + Respawn),
    /// sodass der naechste `should_create_snapshot` im selben Tick true liefert und den
    /// Post-Shift-Zustand erfasst.
    pub fn mark_shift_snapshot_pending(&mut self) {
        self.shift_snapshot_pending = true;
    }

    /// Loescht einen kleinen Batch alle 10 Ticks. Aufgerufen aus dem Tick-Loop.
    /// Nutzt die shared Connection — kein Lock-Konflikt, kein separater Thread.
    pub fn prune_tick(&mut self, event_store: &EventStore, tick: u64) {
        if self.prune_cutoff <= 0 {
            return;
        }
        // Nur alle 10 Ticks einen Batch — gibt der API genug Fenster
        if !tick.is_multiple_of(10) {
            return;
        }
        match event_store.prune_batch(self.prune_cutoff, 500) {
            Ok(0) => {
                info!(total = self.prune_total, "Prune abgeschlossen");
                self.prune_cutoff = 0;
                self.prune_total = 0;
            }
            Ok(deleted) => {
                self.prune_total += deleted;
            }
            Err(e) => {
                debug!(error = %e, "Prune-Batch fehlgeschlagen");
            }
        }
    }

    /// Startet einen neuen Prune-Lauf (setzt Cutoff, prune_tick() arbeitet ab).
    pub fn start_prune(&mut self, cutoff_event_id: i64) {
        if self.prune_cutoff > 0 {
            info!("Prune laeuft bereits — neuer Cutoff ignoriert");
            return;
        }
        self.prune_cutoff = cutoff_event_id;
        self.prune_total = 0;
        info!(cutoff = cutoff_event_id, "Prune gestartet (1000 Rows/Tick)");
    }

    /// Gibt true zurueck wenn gerade ein Prune laeuft.
    pub fn is_pruning(&self) -> bool {
        self.prune_cutoff > 0
    }

    /// Prueft ob ein neuer Snapshot erstellt werden soll.
    ///
    /// #491 (TM-3): in der ERSTEN Stunde (`tick < hourly_interval_ticks`) gilt das feinere
    /// `first_hour_interval_ticks`, damit der Bounded Replay kurz bleibt; danach das grobe
    /// `hourly_interval_ticks` (Tiered Retention). Die EVENT-Aufbewahrung, die das Replay-Fenster
    /// der ersten Stunde tatsaechlich offenhaelt, ist separat (#250); ausserhalb davon faellt der
    /// Restore auf den naechsten Snapshot-Punkt zurueck (AC-3, exact:false).
    pub fn should_create_snapshot(&self, tick: u64) -> bool {
        if tick == 0 {
            return false;
        }
        // #529: Schichtwechsel-Anker hat Vorrang vor dem Intervall (erzwingt den Post-Shift-Anker,
        // damit kein Replay-Fenster eine Schichtgrenze kreuzt).
        if self.shift_snapshot_pending {
            return true;
        }
        let interval = if tick < self.config.hourly_interval_ticks
            && self.config.first_hour_interval_ticks > 0
        {
            self.config.first_hour_interval_ticks
        } else {
            self.config.hourly_interval_ticks
        };
        tick.saturating_sub(self.last_snapshot_tick) >= interval
    }

    /// Erstellt einen vollstaendigen World Snapshot und speichert ihn in Limbo.
    pub fn create_and_store(
        &mut self,
        world: &mut bevy_ecs::world::World,
        state_store: &Arc<StateStore>,
        event_store: &Arc<EventStore>,
        _data_dir: &Path,
        fs_layer: Option<&LayerManager>,
        fs_mount: Option<&str>,
        tick: u64,
        sim_hour: f32,
    ) -> anyhow::Result<String> {
        let start = std::time::Instant::now();

        // 1. redb Dump (Read-Txn)
        let redb_dump = state_store.dump_all_tables()?;

        // 2. ECS Snapshot
        let ecs_snapshot = sentinel_ecs::snapshot_ecs_state(world);

        // 2b. Optionaler sentinel-fs Runtime-Dump fuer echten FUSE-Restore.
        let fs_metadata = if fs_mount.is_some() {
            let layer =
                fs_layer.ok_or_else(|| anyhow::anyhow!("sentinel-fs Layer nicht initialisiert"))?;
            Some(layer.meta().dump_all_tables()?)
        } else {
            None
        };

        // 3. Projection Offsets lesen
        let projection_offsets = event_store.get_all_offsets().unwrap_or_default();

        // 4. Last Event ID
        let last_event_id = event_store.get_latest_event_id().unwrap_or(0);

        // 5. WorldSnapshot zusammenbauen
        let snapshot_id = uuid::Uuid::now_v7().to_string();
        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let snapshot = WorldSnapshot {
            snapshot_id: snapshot_id.clone(),
            schema_version: WorldSnapshot::SCHEMA_VERSION,
            tick,
            sim_hour,
            timestamp_ms: now_ms,
            tier: SnapshotTier::Hourly,
            last_event_id,
            redb: redb_dump,
            ecs: ecs_snapshot,
            projection_offsets,
            fs_metadata,
        };

        // 6. Snapshot serialisieren + in Limbo speichern
        let bytes = encode_world_snapshot(&snapshot)?;
        let size = bytes.len();
        event_store.save_world_snapshot(
            &snapshot_id,
            &snapshot.tier.to_string(),
            tick,
            sim_hour,
            last_event_id,
            &bytes,
        )?;

        // #492: pin the CAS blobs this snapshot's FS metadata references, so Trash GC cannot delete
        // them while the snapshot is retained (pointer manifest from inode hashes, not a blob copy).
        if let (Some(layer), Some(dump)) = (fs_layer, snapshot.fs_metadata.as_ref()) {
            let hashes = sentinel_fs::metadata::referenced_blob_hashes(dump);
            layer.meta().pin_snapshot_blobs(&snapshot_id, &hashes)?;
        }

        self.last_snapshot_tick = tick;
        // #529: erzwungener Shift-Anker erledigt — Flag zuruecksetzen (Intervall ab hier ab tick).
        self.shift_snapshot_pending = false;

        let duration_ms = start.elapsed().as_millis();
        info!(
            snapshot_id = %snapshot_id,
            tick,
            sim_hour,
            size_kb = size / 1024,
            duration_ms,
            "World Snapshot erstellt"
        );

        Ok(snapshot_id)
    }

    /// Promoted Snapshots durch die Tiers und loescht abgelaufene.
    ///
    /// Promoted Snapshots durch die Tiers und loescht abgelaufene.
    /// Auto-Prune loescht Events vor dem zweitneuesten Snapshot.
    pub fn maintain(
        &mut self,
        event_store: &Arc<EventStore>,
        fs_layer: Option<&LayerManager>,
    ) -> anyhow::Result<MaintenanceReport> {
        let snapshots = event_store.list_world_snapshots()?;
        if snapshots.is_empty() {
            return Ok(MaintenanceReport::default());
        }

        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let mut report = MaintenanceReport::default();

        // Promotion: hourly → daily (aelter als daily_keep_hours)
        let daily_cutoff_ms = now_ms - (self.config.daily_keep_hours as i64 * 3600 * 1000);
        for snap in &snapshots {
            if snap.tier == SnapshotTier::Hourly && snap.created_at_ms < daily_cutoff_ms {
                // Promoten statt loeschen — es sei denn es gibt schon einen Daily fuer diesen Tag
                if !self.has_snapshot_for_day(&snapshots, SnapshotTier::Daily, snap.created_at_ms) {
                    if event_store
                        .promote_world_snapshot(&snap.id, "daily")
                        .unwrap_or(false)
                    {
                        report.promoted += 1;
                        debug!(id = %snap.id, "Snapshot promoted: hourly → daily");
                    }
                } else {
                    delete_redundant(event_store, fs_layer, snap, now_ms, &mut report)?;
                }
            }
        }

        // Promotion: daily → weekly (aelter als weekly_keep_days)
        let weekly_cutoff_ms = now_ms - (self.config.weekly_keep_days as i64 * 86400 * 1000);
        for snap in &snapshots {
            if snap.tier == SnapshotTier::Daily && snap.created_at_ms < weekly_cutoff_ms {
                if !self.has_snapshot_for_week(&snapshots, SnapshotTier::Weekly, snap.created_at_ms)
                {
                    if event_store
                        .promote_world_snapshot(&snap.id, "weekly")
                        .unwrap_or(false)
                    {
                        report.promoted += 1;
                        debug!(id = %snap.id, "Snapshot promoted: daily → weekly");
                    }
                } else {
                    delete_redundant(event_store, fs_layer, snap, now_ms, &mut report)?;
                }
            }
        }

        // Promotion: weekly → monthly (aelter als monthly_keep_weeks)
        let monthly_cutoff_ms = now_ms - (self.config.monthly_keep_weeks as i64 * 7 * 86400 * 1000);
        for snap in &snapshots {
            if snap.tier == SnapshotTier::Weekly && snap.created_at_ms < monthly_cutoff_ms {
                if !self.has_snapshot_for_month(
                    &snapshots,
                    SnapshotTier::Monthly,
                    snap.created_at_ms,
                ) {
                    if event_store
                        .promote_world_snapshot(&snap.id, "monthly")
                        .unwrap_or(false)
                    {
                        report.promoted += 1;
                        debug!(id = %snap.id, "Snapshot promoted: weekly → monthly");
                    }
                } else {
                    delete_redundant(event_store, fs_layer, snap, now_ms, &mut report)?;
                }
            }
        }

        if report.promoted > 0 || report.deleted > 0 || report.delete_blocked_young > 0 {
            info!(
                promoted = report.promoted,
                deleted = report.deleted,
                kept_protected = report.kept_protected,
                delete_blocked_young = report.delete_blocked_young,
                "Snapshot Maintenance abgeschlossen"
            );
        }

        // Auto-Prune: Cutoff setzen, prune_tick() arbeitet 1 Batch/Tick ab
        if self.config.auto_prune && !self.is_pruning() {
            let current_snapshots = event_store.list_world_snapshots().unwrap_or_default();
            if let Some(prune_point) = prune_cutoff_from_ordered_snapshots(&current_snapshots) {
                if event_store.can_prune(prune_point).unwrap_or(false) {
                    self.start_prune(prune_point);
                }
            }
        }

        Ok(report)
    }

    // #250: Promotion-Dedup ist KALENDER-aligned (nicht Epoch-Modulo). Genau EIN Keeper pro
    // Kalender-Periode (UTC). Die Bucket-Schluessel hier MUESSEN exakt zu den Verify-SQL-Ausdruecken
    // passen, mit denen die ACs geprueft werden (SSOT — driften sie auseinander, entstehen
    // Phantom-Duplikate im Test ODER im Live-Betrieb):
    //   AC-2 Tag:   SELECT date(created_at/1000,'unixepoch'), count(*) ... GROUP BY 1
    //   AC-3 Woche: SELECT date(created_at/1000,'unixepoch',
    //                      '-'||((strftime('%w',created_at/1000,'unixepoch')+6)%7)||' days') ...  -- Montag der Woche
    //   AC-3 Monat: SELECT strftime('%Y-%m',created_at/1000,'unixepoch'), count(*) ... GROUP BY 1
    // 1970-01-01 war ein Donnerstag → die alte `% (7*86400_000)`-Woche brach Do/Mi statt Mo/So und
    // `% (30*86400_000)` driftet gegen echte Kalendermonate; beides erzeugte Doppel-Keeper an den
    // Periodengrenzen (Live `daily=13`/`weekly` zu hoch). Repro-Tests: siehe unten.

    fn has_snapshot_for_day(
        &self,
        snapshots: &[SnapshotMeta],
        tier: SnapshotTier,
        timestamp_ms: i64,
    ) -> bool {
        let key = calendar_day_key(timestamp_ms);
        snapshots
            .iter()
            .any(|s| s.tier == tier && calendar_day_key(s.created_at_ms) == key)
    }

    fn has_snapshot_for_week(
        &self,
        snapshots: &[SnapshotMeta],
        tier: SnapshotTier,
        timestamp_ms: i64,
    ) -> bool {
        let key = calendar_week_key(timestamp_ms);
        snapshots
            .iter()
            .any(|s| s.tier == tier && calendar_week_key(s.created_at_ms) == key)
    }

    fn has_snapshot_for_month(
        &self,
        snapshots: &[SnapshotMeta],
        tier: SnapshotTier,
        timestamp_ms: i64,
    ) -> bool {
        let key = calendar_month_key(timestamp_ms);
        snapshots
            .iter()
            .any(|s| s.tier == tier && calendar_month_key(s.created_at_ms) == key)
    }
}

/// Konvertiert Epoch-Millisekunden in eine UTC-`DateTime`. Out-of-range-Werte fallen auf die
/// Epoch zurueck (deterministisch, kein Panic).
fn utc_dt(created_at_ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(created_at_ms)
        .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("epoch is valid"))
}

/// Kalendertag-Bucket (UTC) — entspricht `date(created_at/1000,'unixepoch')`.
fn calendar_day_key(created_at_ms: i64) -> i32 {
    utc_dt(created_at_ms).date_naive().num_days_from_ce()
}

/// Kalenderwochen-Bucket (UTC, Montag-verankert) — entspricht dem Montag der Woche
/// (`date(...,'-'||((strftime('%w',...)+6)%7)||' days')`). Schluessel = Tagesnummer dieses Montags.
fn calendar_week_key(created_at_ms: i64) -> i32 {
    let date = utc_dt(created_at_ms).date_naive();
    let monday = date - chrono::Duration::days(date.weekday().num_days_from_monday() as i64);
    monday.num_days_from_ce()
}

/// Kalendermonat-Bucket (UTC) — entspricht `strftime('%Y-%m',created_at/1000,'unixepoch')`.
fn calendar_month_key(created_at_ms: i64) -> (i32, u32) {
    let dt = utc_dt(created_at_ms);
    (dt.year(), dt.month())
}

/// #250/#264: Ein redundanter Snapshot darf erst geloescht werden, wenn er das Immutability-Fenster
/// verlassen hat. DIESELBE Schwelle (`IMMUTABLE_SNAPSHOT_MS`) blockt der DB-Trigger
/// `protect_recent_snapshots`. Daemon-Skip-Alter == Trigger-Block-Schwelle ist ein GETESTETES
/// Boundary-Invariant (siehe tests): driftet eine Seite, schlaegt der Test an.
fn is_past_immutability_window(now_ms: i64, created_at_ms: i64) -> bool {
    now_ms - created_at_ms >= sentinel_limbo::IMMUTABLE_SNAPSHOT_MS
}

/// Erkennt den #264-Trigger-Abbruch ('Cannot delete snapshot younger than 7 days') anhand der
/// RAISE(ABORT)-Message. Andere DB-Fehler (Lock/IO) werden NICHT als Block klassifiziert und vom
/// Aufrufer propagiert.
fn is_immutability_block(err: &anyhow::Error) -> bool {
    err.to_string()
        .contains("Cannot delete snapshot younger than")
}

/// Behandelt einen redundanten Snapshot (es existiert bereits ein Keeper fuer seine Kalenderperiode
/// im hoeheren Tier) #264-konform (Variante B): loescht ihn NUR wenn er das Immutability-Fenster
/// verlassen hat. Junge Snapshots werden bewusst uebersprungen (`kept_protected`) — Retention
/// loescht NIE junge Snapshots, sie altern aus dem Schutzfenster und werden dann geloescht. Ein
/// dennoch auftretender Trigger-Block wird gezaehlt+geloggt statt geschluckt (`delete_blocked_young`
/// = #264-Drift-Alarm, bei korrektem Boundary-Invariant immer 0). Echte DB-Fehler propagieren.
fn delete_redundant(
    event_store: &Arc<EventStore>,
    fs_layer: Option<&LayerManager>,
    snap: &SnapshotMeta,
    now_ms: i64,
    report: &mut MaintenanceReport,
) -> anyhow::Result<()> {
    if !is_past_immutability_window(now_ms, snap.created_at_ms) {
        report.kept_protected += 1;
        debug!(
            id = %snap.id,
            "Redundanter Snapshot bleibt geschuetzt (juenger als Immutability-Fenster)"
        );
        return Ok(());
    }
    match event_store.delete_world_snapshot(&snap.id) {
        Ok(true) => {
            report.deleted += 1;
            // #492: the snapshot is gone → release its blob pins so the referenced blobs become
            // Trash-GC-eligible once no live refcount / other snapshot pin holds them. Best-effort:
            // a pin leak only delays GC, never corrupts.
            if let Some(layer) = fs_layer {
                if let Err(e) = layer.meta().unpin_snapshot_blobs(&snap.id) {
                    debug!(id = %snap.id, error = %e, "unpin_snapshot_blobs fehlgeschlagen");
                }
            }
        }
        Ok(false) => {} // Zeile bereits weg (Race) — kein Fehler.
        Err(e) if is_immutability_block(&e) => {
            // Drift-Alarm: bei korrektem Boundary-Invariant unerreichbar (Daemon haette skippt).
            report.delete_blocked_young += 1;
            debug!(id = %snap.id, "DELETE vom #264-Trigger geblockt (Drift-Alarm) — uebersprungen");
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Ergebnis einer Maintenance-Operation.
#[derive(Debug, Default)]
pub struct MaintenanceReport {
    pub promoted: u32,
    pub deleted: u32,
    /// #250/#264: redundante Snapshots die NICHT geloescht wurden, weil sie noch im
    /// Immutability-Fenster (`IMMUTABLE_SNAPSHOT_MS`, 7 Tage) liegen — bewusst uebersprungen
    /// (Variante B: Retention loescht NIE junge Snapshots, nur promoten/post-7d-loeschen).
    pub kept_protected: u32,
    /// #264-Drift-Alarm: DELETE-Versuche die der Trigger geblockt hat, OBWOHL der Daemon sie nicht
    /// uebersprungen hat. Bei korrekt synchronisierter Schwelle (Boundary-Invariant) immer 0;
    /// `> 0` bedeutet Daemon-Skip-Alter und Trigger-Block-Schwelle sind auseinandergedriftet.
    pub delete_blocked_young: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(tick: u64, last_event_id: i64) -> SnapshotMeta {
        SnapshotMeta {
            id: format!("snap-{tick}"),
            tier: SnapshotTier::Hourly,
            tick,
            sim_hour: 0.0,
            last_event_id,
            payload_size_bytes: 0,
            created_at_ms: 0,
        }
    }

    #[test]
    fn prune_cutoff_uses_second_newest_snapshot() {
        let snapshots = vec![meta(300, 30), meta(200, 20), meta(100, 10)];
        assert_eq!(prune_cutoff_from_ordered_snapshots(&snapshots), Some(20));
    }

    #[test]
    fn prune_cutoff_requires_two_snapshots() {
        assert_eq!(prune_cutoff_from_ordered_snapshots(&[]), None);
        assert_eq!(prune_cutoff_from_ordered_snapshots(&[meta(100, 10)]), None);
    }

    #[test]
    fn should_create_snapshot_uses_fine_interval_in_first_hour() {
        // #491 (TM-3): erste Stunde feines Intervall (300), danach grob (3600).
        let cfg = RetentionConfig {
            hourly_interval_ticks: 3600,
            first_hour_interval_ticks: 300,
            ..RetentionConfig::default()
        };
        let mut mgr = SnapshotManager::new(cfg);
        // Erste Stunde, last=0: feines Intervall 300.
        assert!(!mgr.should_create_snapshot(0), "tick 0 nie");
        assert!(!mgr.should_create_snapshot(299), "vor erstem 5-min-Anchor");
        assert!(
            mgr.should_create_snapshot(300),
            "erster 5-min-Anchor in 1. Stunde"
        );
        // Nach einem Anchor bei 300: naechster feiner Anchor erst bei 600 (in der 1. Stunde).
        mgr.last_snapshot_tick = 300;
        assert!(
            !mgr.should_create_snapshot(599),
            "vor naechstem 5-min-Anchor"
        );
        assert!(mgr.should_create_snapshot(600), "naechster 5-min-Anchor");
        // Nach der ersten Stunde (tick >= 3600) gilt das GROBE Intervall: eine 300-Tick-Luecke
        // loest dann KEINEN Snapshot mehr aus (Tiered Retention).
        mgr.last_snapshot_tick = 3600;
        assert!(
            !mgr.should_create_snapshot(3900),
            "300-Tick-Luecke triggert nach 1. Stunde NICHT (grob)"
        );
        assert!(
            !mgr.should_create_snapshot(3601),
            "in 2. Stunde: erst < hourly seit last"
        );
        assert!(
            mgr.should_create_snapshot(7200),
            "hourly-Anchor (3600 seit last) ausserhalb 1. Stunde"
        );
    }

    #[test]
    fn shift_pending_forces_snapshot_regardless_of_interval() {
        // #529: Ein Schichtwechsel erzwingt einen Anker am Shift-Tick, unabhaengig vom Intervall —
        // damit das Replay-Fenster jedes Post-Shift-Ziels innerhalb einer Schicht bleibt.
        let cfg = RetentionConfig {
            hourly_interval_ticks: 3600,
            first_hour_interval_ticks: 300,
            ..RetentionConfig::default()
        };
        let mut mgr = SnapshotManager::new(cfg);
        mgr.last_snapshot_tick = 10_000; // gerade gesnapshottet -> Intervall NICHT faellig
        assert!(
            !mgr.should_create_snapshot(10_050),
            "ohne Shift: Intervall nicht faellig"
        );
        mgr.mark_shift_snapshot_pending();
        assert!(
            mgr.should_create_snapshot(10_050),
            "Schichtwechsel erzwingt Snapshot trotz frischem Intervall"
        );
        assert!(
            !mgr.should_create_snapshot(0),
            "tick 0 nie ein Snapshot, auch mit pending"
        );
    }

    #[test]
    fn should_create_snapshot_fine_interval_zero_falls_back_to_hourly() {
        // first_hour_interval_ticks = 0 -> deaktiviert, hourly gilt auch in der 1. Stunde.
        let cfg = RetentionConfig {
            hourly_interval_ticks: 3600,
            first_hour_interval_ticks: 0,
            ..RetentionConfig::default()
        };
        let mgr = SnapshotManager::new(cfg);
        assert!(
            !mgr.should_create_snapshot(300),
            "kein 5-min-Anchor wenn deaktiviert"
        );
        assert!(mgr.should_create_snapshot(3600), "hourly greift");
    }

    // ── #250: Promotion-Dedup (Kalender-aligned) + #264-Konformitaet ───────────────────────────

    fn utc_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s)
            .unwrap()
            .timestamp_millis()
    }

    fn snap_at(id: &str, tier: SnapshotTier, created_at_ms: i64) -> SnapshotMeta {
        SnapshotMeta {
            id: id.to_string(),
            tier,
            tick: 0,
            sim_hour: 0.0,
            last_event_id: 0,
            payload_size_bytes: 0,
            created_at_ms,
        }
    }

    /// #264/#250: Daemon-Skip-Alter == Trigger-Block-Schwelle, exakt am Boundary.
    /// `is_past_immutability_window(now, created)` ist true gdw. `now - created >= const`; der
    /// Trigger blockt gdw. `now - created < const` → bei Alter `const-1` skippen BEIDE, bei `const`
    /// loeschen BEIDE. Das behaviorale Gegenstueck (Trigger blockt jung / erlaubt alt) liegt in
    /// `sentinel-limbo` (`test_snapshot_delete_blocked_within_7_days` / `_allowed_after_7_days`).
    #[test]
    fn immutability_window_boundary_is_exact() {
        let now = 10_000_000_000_000i64;
        let c = sentinel_limbo::IMMUTABLE_SNAPSHOT_MS;
        assert!(
            !is_past_immutability_window(now, now - (c - 1)),
            "Alter const-1ms: geschuetzt (Daemon skippt, Trigger blockt)"
        );
        assert!(
            is_past_immutability_window(now, now - c),
            "Alter == const: loeschbar (Trigger erlaubt: now-created < const ist false)"
        );
        assert!(
            is_past_immutability_window(now, now - (c + 1)),
            "Alter const+1ms: loeschbar"
        );
    }

    #[test]
    fn immutability_block_classifies_trigger_error_only() {
        let block = anyhow::anyhow!("Cannot delete snapshot younger than 7 days");
        assert!(
            is_immutability_block(&block),
            "Trigger-Abbruch wird erkannt"
        );
        let other = anyhow::anyhow!("database is locked");
        assert!(
            !is_immutability_block(&other),
            "echter DB-Fehler ist KEIN Immutability-Block → muss propagieren"
        );
    }

    #[test]
    fn week_dedup_uses_calendar_week_across_epoch_thursday_boundary() {
        // 1970-01-01 war ein Donnerstag → die alte `% (7*86400_000)`-Logik brach Wochen Do/Mi.
        // Mi 2024-01-03 und Do 2024-01-04 liegen in DERSELBEN Kalenderwoche (Mo 2024-01-01..So
        // 2024-01-07), aber in ZWEI verschiedenen Epoch-Modulo-Wochen → alte Logik haette zwei
        // Weeklies fuer eine Kalenderwoche behalten (Doppel-Keeper). Repro-Test + Fix-Nachweis.
        let wed = utc_ms(2024, 1, 3, 12, 0, 0);
        let thu = utc_ms(2024, 1, 4, 12, 0, 0);
        let mgr = SnapshotManager::new(RetentionConfig::default());
        let snaps = vec![snap_at("w", SnapshotTier::Weekly, wed)];
        assert!(
            mgr.has_snapshot_for_week(&snaps, SnapshotTier::Weekly, thu),
            "Mi und Do derselben Kalenderwoche teilen denselben Wochen-Bucket"
        );
        let prev_sun = utc_ms(2023, 12, 31, 12, 0, 0); // Sonntag der Vorwoche
        assert!(
            !mgr.has_snapshot_for_week(&snaps, SnapshotTier::Weekly, prev_sun),
            "andere Kalenderwoche → anderer Bucket"
        );
    }

    #[test]
    fn month_dedup_uses_calendar_month_not_30day_block() {
        // `% (30*86400_000)` driftet gegen echte Monate: der 1. und der 31. desselben Monats lagen
        // in verschiedenen 30-Tage-Bloecken → alte Logik haette zwei Monthlies behalten. Repro + Fix.
        let jan1 = utc_ms(2024, 1, 1, 12, 0, 0);
        let jan31 = utc_ms(2024, 1, 31, 12, 0, 0);
        let mgr = SnapshotManager::new(RetentionConfig::default());
        let snaps = vec![snap_at("m", SnapshotTier::Monthly, jan1)];
        assert!(
            mgr.has_snapshot_for_month(&snaps, SnapshotTier::Monthly, jan31),
            "1. und 31. desselben Kalendermonats teilen denselben Monats-Bucket"
        );
        let feb1 = utc_ms(2024, 2, 1, 12, 0, 0);
        assert!(
            !mgr.has_snapshot_for_month(&snaps, SnapshotTier::Monthly, feb1),
            "anderer Kalendermonat → anderer Bucket"
        );
    }

    #[test]
    fn day_dedup_uses_calendar_utc_day() {
        let morning = utc_ms(2024, 1, 5, 1, 0, 0);
        let evening = utc_ms(2024, 1, 5, 23, 30, 0);
        let next_day = utc_ms(2024, 1, 6, 0, 30, 0);
        let mgr = SnapshotManager::new(RetentionConfig::default());
        let snaps = vec![snap_at("d", SnapshotTier::Daily, morning)];
        assert!(
            mgr.has_snapshot_for_day(&snaps, SnapshotTier::Daily, evening),
            "gleicher UTC-Tag (1:00 und 23:30) → gleicher Bucket"
        );
        assert!(
            !mgr.has_snapshot_for_day(&snaps, SnapshotTier::Daily, next_day),
            "naechster UTC-Tag → anderer Bucket"
        );
    }

    #[test]
    fn delete_redundant_skips_young_snapshot_as_protected() {
        // Variante B: ein redundanter junger Snapshot wird NICHT geloescht (Daemon versucht es gar
        // nicht erst), sondern als kept_protected gezaehlt — er altert spaeter aus dem Schutzfenster.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(EventStore::open(dir.path().join("t.db").to_str().unwrap()).unwrap());
        store
            .save_world_snapshot("young", "hourly", 1, 0.0, 0, b"x")
            .unwrap();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let snap = store
            .list_world_snapshots()
            .unwrap()
            .into_iter()
            .find(|s| s.id == "young")
            .unwrap();
        let mut report = MaintenanceReport::default();
        delete_redundant(&store, None, &snap, now, &mut report).unwrap();
        assert_eq!(
            report.kept_protected, 1,
            "junger Snapshot bleibt geschuetzt"
        );
        assert_eq!(report.deleted, 0);
        assert_eq!(
            report.delete_blocked_young, 0,
            "Daemon versucht gar nicht erst zu loeschen (kein Trigger-Block-Pfad)"
        );
        assert_eq!(
            store.list_world_snapshots().unwrap().len(),
            1,
            "Snapshot noch vorhanden"
        );
    }

    #[test]
    fn delete_redundant_counts_trigger_block_on_clock_drift() {
        // Simulierte Uhr-Drift: Daemon-Uhr 7d+ VOR der DB-Uhr → Daemon haelt den Snapshot fuer
        // loeschbar, der #264-Trigger (echte Uhr) blockt ihn aber. Erwartung: gezaehlt+geloggt als
        // delete_blocked_young (Drift-Alarm), KEIN Crash, KEIN stiller Swallow, Snapshot bleibt.
        // Beweist zugleich, dass `is_immutability_block` die ECHTE rusqlite-RAISE-Message matcht.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(EventStore::open(dir.path().join("t.db").to_str().unwrap()).unwrap());
        store
            .save_world_snapshot("young", "hourly", 1, 0.0, 0, b"x")
            .unwrap();
        let snap = store
            .list_world_snapshots()
            .unwrap()
            .into_iter()
            .find(|s| s.id == "young")
            .unwrap();
        let future_now = snap.created_at_ms + sentinel_limbo::IMMUTABLE_SNAPSHOT_MS + 1000;
        let mut report = MaintenanceReport::default();
        delete_redundant(&store, None, &snap, future_now, &mut report).unwrap();
        assert_eq!(
            report.delete_blocked_young, 1,
            "Trigger-Block wird gezaehlt statt geschluckt"
        );
        assert_eq!(report.deleted, 0);
        assert_eq!(report.kept_protected, 0);
        assert_eq!(
            store.list_world_snapshots().unwrap().len(),
            1,
            "Snapshot bleibt — Trigger schuetzt ihn"
        );
    }
}
