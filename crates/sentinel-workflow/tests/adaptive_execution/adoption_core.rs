use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use sentinel_workflow::{AdaptiveModelAdoptionRead, AdaptiveModelAdoptionReadError};

struct CountedOrganization {
    current: Arc<Mutex<RuntimeAuthoritySnapshotV1>>,
    calls: Arc<AtomicUsize>,
    ready: Arc<AtomicBool>,
}

impl OrganizationRuntimePort for CountedOrganization {
    fn readiness(&self) -> DependencyReadiness {
        if self.ready.load(Ordering::SeqCst) {
            DependencyReadiness::Ready
        } else {
            DependencyReadiness::Unavailable
        }
    }

    fn authority_snapshot(
        &self,
        tenant_id: &TenantId,
        project_id: &ProjectId,
        work_item_id: &WorkItemId,
        agent_id: AgentId,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let current = self.current.lock().unwrap().clone();
        if current.tenant_id != *tenant_id
            || current.project_id != *project_id
            || current.work_item_id != *work_item_id
            || current.agent_id != agent_id
        {
            return Err(WorkflowPortError::AuthorityConflict);
        }
        Ok(current)
    }
}

#[test]
fn adoption_core_reacquires_organization_authority_before_warm_historical_reads() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("workflow.sqlite");
    let auth = authority();
    let grant = grant(auth.clone(), 2);
    let store = Arc::new(WorkflowStore::open(&database).unwrap());
    store.begin_adaptive_session(&grant, &auth, NOW).unwrap();
    let model = effect("01991c34-e03c-70c2-b97e-0591f4be2990", 'a');
    let pending = advance_replay_fixture(
        &store,
        &grant,
        1,
        &AdaptiveTransitionV1::ClaimModel {
            effect: model.clone(),
            previous_observation_digest: None,
        },
    );
    let before = persisted_adaptive_rows(&database);
    let current = Arc::new(Mutex::new(auth.clone()));
    let calls = Arc::new(AtomicUsize::new(0));
    let ready = Arc::new(AtomicBool::new(true));
    let core = WorkflowCore::new(
        Arc::clone(&store),
        CountedOrganization {
            current: Arc::clone(&current),
            calls: Arc::clone(&calls),
            ready: Arc::clone(&ready),
        },
        UnavailableWorkExecutionPort,
        UnavailableCompletionEvidencePort,
        UnavailableGateEvidencePort,
    );
    let decision = AdaptiveModelDecisionV1::Tool {
        tool: inspect_tool(),
        tool_digest: adaptive_tool_digest(&inspect_tool()).unwrap(),
    };
    let read = || core.adaptive_model_adoption_read(&grant, &model, &"9".repeat(64), &decision);
    for expected_calls in 1..=2 {
        match read().unwrap() {
            AdaptiveModelAdoptionRead::Current {
                session,
                already_adopted,
            } => {
                assert_eq!(*session, pending);
                assert!(!already_adopted);
            }
            other => panic!("unexpected adoption read: {other:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
    }
    current.lock().unwrap().assignment_version += 1;
    assert!(
        matches!(read(), Err(AdaptiveModelAdoptionReadError::Session(error))
        if error.code == WorkflowErrorCode::AuthorityConflict)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    *current.lock().unwrap() = auth;
    ready.store(false, Ordering::SeqCst);
    assert!(
        matches!(read(), Err(AdaptiveModelAdoptionReadError::Session(error))
        if error.code == WorkflowErrorCode::OrganizationUnavailable)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(persisted_adaptive_rows(&database), before);
}
