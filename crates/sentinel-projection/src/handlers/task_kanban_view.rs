//! Handler fuer die `task_kanban` Projektion (#438).
//!
//! Materialisiert den Task-/Auftrags-Lebenszyklus (erstellen/zuweisen/Status/abschliessen) als
//! Read-Model fuer die Kanban-Ueberwachung der Gaia-Konsole.

use sentinel_common::{DomainEvent, DomainEventPayload};
use tracing::debug;

use crate::store::ReadModelTransaction;

use super::ProjectionHandler;

pub struct TaskKanbanHandler;

impl ProjectionHandler for TaskKanbanHandler {
    fn handle(
        &self,
        row_id: i64,
        _event: &DomainEvent,
        payload: &DomainEventPayload,
        txn: &ReadModelTransaction<'_>,
    ) -> anyhow::Result<()> {
        match payload {
            DomainEventPayload::TaskCreated {
                task_id,
                title,
                assigned_to,
                parent_task,
            } => {
                debug!(task_id = task_id.0, "Kanban: Task erstellt");
                txn.upsert_task(
                    task_id.0,
                    title,
                    assigned_to.0,
                    None,
                    parent_task.map(|t| t.0),
                    "pending",
                    row_id,
                )?;
            }
            DomainEventPayload::TaskAssigned {
                task_id,
                assigned_to,
                assigned_by,
            } => {
                debug!(task_id = task_id.0, "Kanban: Task zugewiesen");
                txn.update_task_assignee(
                    task_id.0,
                    assigned_to.0,
                    assigned_by.map(|a| a.0),
                    row_id,
                )?;
            }
            DomainEventPayload::TaskStatusChanged {
                task_id,
                new_status,
                ..
            } => {
                debug!(task_id = task_id.0, status = %new_status, "Kanban: Status");
                txn.update_task_status(task_id.0, new_status, row_id)?;
            }
            DomainEventPayload::TaskCompleted { task_id, result } => {
                debug!(task_id = task_id.0, "Kanban: Task abgeschlossen");
                txn.complete_task(task_id.0, result.as_deref(), row_id)?;
            }
            DomainEventPayload::TaskBlocked { task_id, .. } => {
                debug!(task_id = task_id.0, "Kanban: Task blockiert");
                txn.update_task_status(task_id.0, "blocked", row_id)?;
            }
            _ => {}
        }
        Ok(())
    }
}
