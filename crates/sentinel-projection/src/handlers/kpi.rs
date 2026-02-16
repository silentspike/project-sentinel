//! Handler fuer die `kpi_1m` Projektion.
//!
//! Minutenbasierte operative KPIs. Bucket-Start = timestamp_ms / 60_000 * 60_000.
//! Verarbeitet 11 Event-Varianten (alle ausser BioActionPerformed, AgentStatusChanged,
//! TransitCompleted).

use sentinel_common::{DomainEvent, DomainEventPayload};
use tracing::debug;

use crate::store::{KpiField, ReadModelTransaction};

use super::ProjectionHandler;

pub struct KpiHandler;

impl ProjectionHandler for KpiHandler {
    fn handle(
        &self,
        row_id: i64,
        event: &DomainEvent,
        payload: &DomainEventPayload,
        txn: &ReadModelTransaction<'_>,
    ) -> anyhow::Result<()> {
        let ts = event.timestamp_ms;

        match payload {
            DomainEventPayload::AgentSpawned { .. } => {
                debug!("KPI: active_agents++");
                txn.increment_kpi(ts, KpiField::ActiveAgents(1), row_id)?;
            }

            DomainEventPayload::AgentDespawned { .. } => {
                debug!("KPI: active_agents--");
                txn.increment_kpi(ts, KpiField::ActiveAgents(-1), row_id)?;
            }

            DomainEventPayload::TransitStarted { .. } => {
                debug!("KPI: total_transits++");
                txn.increment_kpi(ts, KpiField::TotalTransits, row_id)?;
            }

            DomainEventPayload::AgentActionReceived { .. } => {
                debug!("KPI: total_actions++");
                txn.increment_kpi(ts, KpiField::TotalActions, row_id)?;
            }

            DomainEventPayload::ChaosTriggered { .. } => {
                debug!("KPI: chaos_events++");
                txn.increment_kpi(ts, KpiField::ChaosEvents, row_id)?;
            }

            DomainEventPayload::TickSnapshot { .. } => {
                debug!("KPI: tick_count++");
                txn.increment_kpi(ts, KpiField::TickCount, row_id)?;
            }

            DomainEventPayload::ShiftTransitionCompleted { removed_count, .. } => {
                debug!(
                    removed = removed_count,
                    "KPI: shift_changes++ & active_agents -= N"
                );
                txn.increment_kpi(ts, KpiField::ShiftChanges, row_id)?;
                txn.increment_kpi(ts, KpiField::ActiveAgents(-(*removed_count as i64)), row_id)?;
            }

            DomainEventPayload::NightRunStarted { .. }
            | DomainEventPayload::NightRunCompleted { .. }
            | DomainEventPayload::AgentConsolidated { .. }
            | DomainEventPayload::AgentConsolidationFailed { .. } => {
                debug!("KPI: nightrun_events++");
                txn.increment_kpi(ts, KpiField::NightrunEvents, row_id)?;
            }

            // TransitCompleted, BioActionPerformed, AgentStatusChanged: kein KPI-Impact
            _ => {}
        }
        Ok(())
    }
}
