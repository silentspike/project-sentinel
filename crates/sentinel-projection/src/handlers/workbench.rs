//! Projection handler for safe workbench invocation telemetry (#694).

use sentinel_common::{DomainEvent, DomainEventPayload};

use crate::store::ReadModelTransaction;

use super::ProjectionHandler;

pub struct WorkbenchHandler;

impl ProjectionHandler for WorkbenchHandler {
    fn handle(
        &self,
        row_id: i64,
        _event: &DomainEvent,
        payload: &DomainEventPayload,
        txn: &ReadModelTransaction<'_>,
    ) -> anyhow::Result<()> {
        if let DomainEventPayload::WorkbenchInvocationUpdated {
            invocation_id,
            agent_id,
            project_id,
            work_item_id,
            tool_class,
            runtime_key,
            state,
            resources,
            artifact_ids,
            error_code,
        } = payload
        {
            txn.upsert_workbench_invocation(
                invocation_id,
                agent_id.0,
                project_id,
                work_item_id,
                tool_class,
                runtime_key,
                state,
                resources,
                artifact_ids,
                error_code.as_deref(),
                row_id,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sentinel_common::{AgentId, WorkbenchResourceUsage};

    use crate::store::ReadModelStore;

    use super::*;

    #[test]
    fn projects_safe_invocation_state_idempotently() {
        let store = ReadModelStore::open(":memory:").unwrap();
        let payload = DomainEventPayload::WorkbenchInvocationUpdated {
            invocation_id: "018f3f32-4f01-7f2c-a6c1-f6f4a81b2901".to_string(),
            agent_id: AgentId(7),
            project_id: "project-01".to_string(),
            work_item_id: "work-04".to_string(),
            tool_class: "artifact.commit".to_string(),
            runtime_key: "bwrap-landlock".to_string(),
            state: "succeeded".to_string(),
            resources: WorkbenchResourceUsage {
                duration_ms: 42,
                artifact_bytes: 128,
                ..WorkbenchResourceUsage::default()
            },
            artifact_ids: vec![format!("sha256:{}", "a".repeat(64))],
            error_code: None,
        };
        let event = DomainEvent::new(
            payload.event_type_str(),
            "AGENT-07",
            &payload.to_json(),
            "workbench-test",
            1,
        );
        let transaction = store.begin_transaction().unwrap();
        transaction.begin().unwrap();
        WorkbenchHandler
            .handle(10, &event, &payload, &transaction)
            .unwrap();
        WorkbenchHandler
            .handle(9, &event, &payload, &transaction)
            .unwrap();
        transaction.commit().unwrap();
        drop(transaction);

        let view = store
            .get_workbench_invocation("018f3f32-4f01-7f2c-a6c1-f6f4a81b2901")
            .unwrap()
            .unwrap();
        assert_eq!(view.agent_id, 7);
        assert_eq!(view.state, "succeeded");
        assert_eq!(view.duration_ms, 42);
        assert_eq!(view.artifact_bytes, 128);
        assert_eq!(view.last_event_id, 10);
    }
}
