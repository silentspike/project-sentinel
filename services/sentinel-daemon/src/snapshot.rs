//! SnapshotManager: Tiered World Snapshots fuer Time Machine (#250).
//!
//! Erstellt periodisch World Snapshots (bincode), promoted sie durch
//! Tiers (hourly→daily→weekly→monthly) und loescht abgelaufene.

use std::sync::Arc;
use std::time::SystemTime;

use sentinel_common::{SnapshotMeta, SnapshotTier, WorldSnapshot};
use sentinel_limbo::{EventStore, VacuumHandle};
use sentinel_redb::StateStore;
use tracing::{debug, info};

use crate::config::RetentionConfig;

/// Verwaltet World Snapshots mit Tiered Retention.
pub struct SnapshotManager {
    config: RetentionConfig,
    last_snapshot_tick: u64,
}

impl SnapshotManager {
    pub fn new(config: RetentionConfig) -> Self {
        Self {
            config,
            last_snapshot_tick: 0,
        }
    }

    /// Prueft ob ein neuer Snapshot erstellt werden soll.
    pub fn should_create_snapshot(&self, tick: u64) -> bool {
        tick > 0
            && tick.saturating_sub(self.last_snapshot_tick) >= self.config.hourly_interval_ticks
    }

    /// Erstellt einen vollstaendigen World Snapshot und speichert ihn in Limbo.
    pub fn create_and_store(
        &mut self,
        world: &mut bevy_ecs::world::World,
        state_store: &Arc<StateStore>,
        event_store: &Arc<EventStore>,
        tick: u64,
        sim_hour: f32,
    ) -> anyhow::Result<String> {
        let start = std::time::Instant::now();

        // 1. redb Dump (Read-Txn)
        let redb_dump = state_store.dump_all_tables()?;

        // 2. ECS Snapshot
        let ecs_snapshot = sentinel_ecs::snapshot_ecs_state(world);

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
        };

        // 6. bincode serialisieren + in Limbo speichern
        let bytes = bincode::serialize(&snapshot)?;
        let size = bytes.len();
        event_store.save_world_snapshot(
            &snapshot_id,
            &snapshot.tier.to_string(),
            tick,
            sim_hour,
            last_event_id,
            &bytes,
        )?;

        self.last_snapshot_tick = tick;

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
    /// Gibt optional einen VacuumHandle zurueck wenn Auto-Prune VACUUM gestartet wurde.
    /// Der Caller muss den Handle speichern und beim Shutdown canceln.
    pub fn maintain(
        &self,
        event_store: &Arc<EventStore>,
    ) -> anyhow::Result<(MaintenanceReport, Option<VacuumHandle>)> {
        let snapshots = event_store.list_world_snapshots()?;
        if snapshots.is_empty() {
            return Ok((MaintenanceReport::default(), None));
        }

        let now_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let mut promoted = 0u32;
        let mut deleted = 0u32;

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
                        promoted += 1;
                        debug!(id = %snap.id, "Snapshot promoted: hourly → daily");
                    }
                } else if event_store.delete_world_snapshot(&snap.id).unwrap_or(false) {
                    deleted += 1;
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
                        promoted += 1;
                        debug!(id = %snap.id, "Snapshot promoted: daily → weekly");
                    }
                } else if event_store.delete_world_snapshot(&snap.id).unwrap_or(false) {
                    deleted += 1;
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
                        promoted += 1;
                        debug!(id = %snap.id, "Snapshot promoted: weekly → monthly");
                    }
                } else if event_store.delete_world_snapshot(&snap.id).unwrap_or(false) {
                    deleted += 1;
                }
            }
        }

        if promoted > 0 || deleted > 0 {
            info!(promoted, deleted, "Snapshot Maintenance abgeschlossen");
        }

        // Auto-Prune: Events vor zweitaeltestem Snapshot loeschen (1h Puffer)
        let mut vacuum_handle = None;
        if self.config.auto_prune {
            let current_snapshots = event_store.list_world_snapshots().unwrap_or_default();
            if current_snapshots.len() >= 2 {
                let prune_point = current_snapshots[current_snapshots.len() - 2].last_event_id;
                match event_store.prune_events_before(prune_point) {
                    Ok(pruned) if pruned > 0 => {
                        info!(pruned, "Auto-Prune: Events geloescht, starte VACUUM");
                        vacuum_handle =
                            Some(VacuumHandle::spawn(Arc::clone(event_store)));
                    }
                    Ok(_) => {} // Nichts zu prunen
                    Err(e) => {
                        debug!(error = %e, "Auto-Prune uebersprungen (Safety Guard)");
                    }
                }
            }
        }

        Ok((MaintenanceReport { promoted, deleted }, vacuum_handle))
    }

    fn has_snapshot_for_day(
        &self,
        snapshots: &[SnapshotMeta],
        tier: SnapshotTier,
        timestamp_ms: i64,
    ) -> bool {
        let day_start = timestamp_ms - (timestamp_ms % (86400 * 1000));
        let day_end = day_start + 86400 * 1000;
        snapshots
            .iter()
            .any(|s| s.tier == tier && s.created_at_ms >= day_start && s.created_at_ms < day_end)
    }

    fn has_snapshot_for_week(
        &self,
        snapshots: &[SnapshotMeta],
        tier: SnapshotTier,
        timestamp_ms: i64,
    ) -> bool {
        let week_start = timestamp_ms - (timestamp_ms % (7 * 86400 * 1000));
        let week_end = week_start + 7 * 86400 * 1000;
        snapshots
            .iter()
            .any(|s| s.tier == tier && s.created_at_ms >= week_start && s.created_at_ms < week_end)
    }

    fn has_snapshot_for_month(
        &self,
        snapshots: &[SnapshotMeta],
        tier: SnapshotTier,
        timestamp_ms: i64,
    ) -> bool {
        let month_start = timestamp_ms - (timestamp_ms % (30 * 86400 * 1000));
        let month_end = month_start + 30 * 86400 * 1000;
        snapshots.iter().any(|s| {
            s.tier == tier && s.created_at_ms >= month_start && s.created_at_ms < month_end
        })
    }
}

/// Ergebnis einer Maintenance-Operation.
#[derive(Debug, Default)]
pub struct MaintenanceReport {
    pub promoted: u32,
    pub deleted: u32,
}
