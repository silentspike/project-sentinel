use std::sync::Arc;

use super::{corrupt, persistence, validated_company_project_in_snapshot};
use crate::store::{DeferredWorkflowDiagnostic, WorkflowDiagnosticSpan, WorkflowDiagnosticStage};
use crate::{ProjectId, ProjectV1, TenantId, WorkflowError, WorkflowStore};

struct ProjectReadDiagnostics {
    active: Option<WorkflowDiagnosticSpan>,
    completed: [Option<DeferredWorkflowDiagnostic>; 3],
    next: usize,
}

impl ProjectReadDiagnostics {
    fn new() -> Self {
        Self {
            active: None,
            completed: std::array::from_fn(|_| None),
            next: 0,
        }
    }

    fn start(&mut self, store: &WorkflowStore, stage: WorkflowDiagnosticStage) {
        self.active = Some(store.diagnostic_span(stage));
    }

    fn finish(&mut self, succeeded: bool) {
        self.completed[self.next] = self
            .active
            .take()
            .map(|span| span.finish_deferred(succeeded));
        self.next += 1;
    }
}

impl Drop for ProjectReadDiagnostics {
    fn drop(&mut self) {
        for record in &mut self.completed {
            if let Some(record) = record.take() {
                record.publish();
            }
        }
        // An unfinished span records failure on drop, also after the read guards.
    }
}

pub(super) fn read(
    store: &WorkflowStore,
    tenant: &TenantId,
    project: &ProjectId,
) -> Result<Option<ProjectV1>, WorkflowError> {
    read_with(store, tenant, project, |owner| owner.as_ref().clone())
}

fn read_with<T>(
    store: &WorkflowStore,
    tenant: &TenantId,
    project: &ProjectId,
    materialize: impl FnOnce(&Arc<Option<ProjectV1>>) -> T,
) -> Result<T, WorkflowError> {
    tenant.validate()?;
    project.validate()?;
    // This owner outlives the nested guard scope, including error and unwind paths.
    let mut diagnostics = ProjectReadDiagnostics::new();
    let owner = {
        diagnostics.start(store, WorkflowDiagnosticStage::CompanyProjectConnectionWait);
        let connection = store.connection.lock().map_err(|_| persistence());
        diagnostics.finish(connection.is_ok());
        let mut connection = connection?;
        diagnostics.start(store, WorkflowDiagnosticStage::CompanyProjectRead);
        let result = (|| {
            let key = serde_json::to_vec(&(tenant, project)).map_err(|_| corrupt())?;
            store.validated_read_snapshot_shared_if(
                &mut connection,
                "company-project",
                &key,
                |snapshot| validated_company_project_in_snapshot(snapshot, tenant, project),
                |_| true,
            )
        })();
        diagnostics.finish(result.is_ok());
        result?
    };
    diagnostics.start(store, WorkflowDiagnosticStage::CompanyProjectClone);
    let result = materialize(&owner);
    diagnostics.finish(true);
    Ok(result)
}

#[cfg(test)]
mod tests;
