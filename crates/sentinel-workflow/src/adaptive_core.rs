//! Authority-fenced orchestration around the durable adaptive journal.

use std::sync::Arc;

use uuid::Uuid;

use crate::{
    AdaptiveContinuationAuthorizationV1, AdaptiveCursorV1, AdaptiveModelObservationV1,
    AdaptiveModelPort, AdaptiveObservationRefV1, AdaptiveSessionV1, AdaptiveToolObservationV1,
    AdaptiveToolPort, AdaptiveTransitionV1, OrganizationRuntimePort, RuntimeAuthoritySnapshotV1,
    WorkflowError, WorkflowErrorCode, WorkflowPortError, WorkflowStore,
};

pub struct AdaptiveWorkflowCore<O, M, T> {
    store: Arc<WorkflowStore>,
    organization: O,
    model: M,
    tool: T,
}

impl<O, M, T> AdaptiveWorkflowCore<O, M, T>
where
    O: OrganizationRuntimePort,
    M: AdaptiveModelPort,
    T: AdaptiveToolPort,
{
    pub fn new(store: impl Into<Arc<WorkflowStore>>, organization: O, model: M, tool: T) -> Self {
        Self {
            store: store.into(),
            organization,
            model,
            tool,
        }
    }

    pub fn reconcile_model(
        &self,
        session_id: Uuid,
        authority: &RuntimeAuthoritySnapshotV1,
        operation_id: Uuid,
        now_ms: u64,
    ) -> Result<AdaptiveSessionV1, WorkflowError> {
        let before = self.current_authority(authority)?;
        let session = self
            .store
            .adaptive_session(session_id, &before)?
            .ok_or_else(not_found)?;
        let effect = match &session.cursor {
            AdaptiveCursorV1::ModelPending { effect } => effect.clone(),
            AdaptiveCursorV1::ModelUnknown { .. } => {
                if self.current_authority(authority)? != before {
                    return Err(authority_conflict());
                }
                return Ok(session);
            }
            _ => return Err(invalid_transition()),
        };
        let observation = self.model.reconcile_model(&session, &effect);
        let after = self.current_authority(authority)?;
        if before != after {
            return Err(authority_conflict());
        }
        let command = match observation {
            Ok(AdaptiveModelObservationV1::Pending) => return Ok(session),
            Ok(AdaptiveModelObservationV1::Completed {
                result_digest,
                decision,
            }) => AdaptiveTransitionV1::ResolveModel {
                effect,
                result_digest,
                decision,
            },
            Ok(AdaptiveModelObservationV1::UnknownOutcome)
            | Err(WorkflowPortError::UnknownOutcome) => {
                AdaptiveTransitionV1::MarkUnknown { effect }
            }
            Err(error) => return Err(map_port_error(error)),
        };
        self.store
            .advance_adaptive_session(
                session_id,
                session.version,
                operation_id,
                &command,
                &after,
                now_ms,
            )
            .map(|(_, value)| value)
    }

    /// Read back an atomic lane-C continuation; never issue a grant separately.
    pub fn continued_adaptive_session(
        &self,
        authorization: &AdaptiveContinuationAuthorizationV1,
        authority: &RuntimeAuthoritySnapshotV1,
    ) -> Result<AdaptiveSessionV1, WorkflowError> {
        authorization.validate()?;
        let before = self.current_authority(authority)?;
        let session = self
            .store
            .adaptive_session(authorization.session_id, &before)?
            .ok_or_else(not_found)?;
        if !session.continuation.as_ref().is_some_and(|state| {
            state
                .authorizations
                .iter()
                .any(|stored| stored == authorization)
        }) {
            return Err(authority_conflict());
        }
        if self.current_authority(authority)? != before {
            return Err(authority_conflict());
        }
        Ok(session)
    }

    pub fn reconcile_tool(
        &self,
        session_id: Uuid,
        authority: &RuntimeAuthoritySnapshotV1,
        operation_id: Uuid,
        now_ms: u64,
    ) -> Result<AdaptiveSessionV1, WorkflowError> {
        let before = self.current_authority(authority)?;
        let session = self
            .store
            .adaptive_session(session_id, &before)?
            .ok_or_else(not_found)?;
        let (effect, tool, tool_digest) = match &session.cursor {
            AdaptiveCursorV1::ToolPending {
                effect,
                tool,
                tool_digest,
            }
            | AdaptiveCursorV1::ToolUnknown {
                effect,
                tool,
                tool_digest,
            } => (effect.clone(), tool.clone(), tool_digest.clone()),
            _ => return Err(invalid_transition()),
        };
        let observation = self
            .tool
            .reconcile_tool(&session, &effect, &tool, &tool_digest);
        let after = self.current_authority(authority)?;
        if before != after {
            return Err(authority_conflict());
        }
        let command = match observation {
            Ok(AdaptiveToolObservationV1::Pending) => return Ok(session),
            Ok(AdaptiveToolObservationV1::Completed { observation_digest }) => {
                AdaptiveTransitionV1::ObserveTool {
                    observation: AdaptiveObservationRefV1 {
                        effect,
                        observation_digest,
                    },
                }
            }
            Ok(AdaptiveToolObservationV1::UnknownOutcome)
            | Err(WorkflowPortError::UnknownOutcome) => {
                if matches!(session.cursor, AdaptiveCursorV1::ToolUnknown { .. }) {
                    return Ok(session);
                }
                AdaptiveTransitionV1::MarkUnknown { effect }
            }
            Err(error) => return Err(map_port_error(error)),
        };
        self.store
            .advance_adaptive_session(
                session_id,
                session.version,
                operation_id,
                &command,
                &after,
                now_ms,
            )
            .map(|(_, value)| value)
    }

    fn current_authority(
        &self,
        expected: &RuntimeAuthoritySnapshotV1,
    ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowError> {
        let current = self
            .organization
            .authority_snapshot(
                &expected.tenant_id,
                &expected.project_id,
                &expected.work_item_id,
                expected.agent_id,
            )
            .map_err(map_organization_error)?;
        if current != *expected {
            return Err(authority_conflict());
        }
        Ok(current)
    }
}

fn map_organization_error(error: WorkflowPortError) -> WorkflowError {
    let code = match error {
        WorkflowPortError::Unavailable | WorkflowPortError::TimedOut => {
            WorkflowErrorCode::OrganizationUnavailable
        }
        WorkflowPortError::UnknownOutcome => WorkflowErrorCode::UnknownOutcome,
        WorkflowPortError::AuthorityConflict | WorkflowPortError::Rejected => {
            WorkflowErrorCode::AuthorityConflict
        }
    };
    WorkflowError::new(
        code,
        matches!(
            error,
            WorkflowPortError::Unavailable | WorkflowPortError::TimedOut
        ),
        "adaptive organization authority is unavailable",
    )
}

fn map_port_error(error: WorkflowPortError) -> WorkflowError {
    let code = match error {
        WorkflowPortError::Unavailable | WorkflowPortError::TimedOut => {
            WorkflowErrorCode::ExecutionUnavailable
        }
        WorkflowPortError::AuthorityConflict => WorkflowErrorCode::AuthorityConflict,
        WorkflowPortError::Rejected => WorkflowErrorCode::InvalidTransition,
        WorkflowPortError::UnknownOutcome => WorkflowErrorCode::UnknownOutcome,
    };
    WorkflowError::new(
        code,
        matches!(
            error,
            WorkflowPortError::Unavailable | WorkflowPortError::TimedOut
        ),
        "adaptive effect reconciliation failed",
    )
}

fn authority_conflict() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::AuthorityConflict,
        false,
        "adaptive authority changed",
    )
}

fn invalid_transition() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::InvalidTransition,
        false,
        "adaptive effect is not pending",
    )
}

fn not_found() -> WorkflowError {
    WorkflowError::new(
        WorkflowErrorCode::NotFound,
        false,
        "adaptive session was not found",
    )
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    use crate::adaptive::continuation_tests::{effect, grant, NOW};

    struct Organization(RuntimeAuthoritySnapshotV1);
    impl OrganizationRuntimePort for Organization {
        fn readiness(&self) -> crate::DependencyReadiness {
            crate::DependencyReadiness::Ready
        }
        fn authority_snapshot(
            &self,
            _: &crate::TenantId,
            _: &crate::ProjectId,
            _: &crate::WorkItemId,
            _: crate::AgentId,
        ) -> Result<RuntimeAuthoritySnapshotV1, WorkflowPortError> {
            Ok(self.0.clone())
        }
    }
    struct NeverPoll;
    impl AdaptiveModelPort for NeverPoll {
        fn reconcile_model(
            &self,
            _: &AdaptiveSessionV1,
            _: &crate::AdaptiveEffectV1,
        ) -> Result<AdaptiveModelObservationV1, WorkflowPortError> {
            panic!("a sealed unknown request must never be polled or adopted");
        }
    }
    impl AdaptiveToolPort for NeverPoll {
        fn reconcile_tool(
            &self,
            _: &AdaptiveSessionV1,
            _: &crate::AdaptiveEffectV1,
            _: &sentinel_common::WorkbenchTool,
            _: &str,
        ) -> Result<AdaptiveToolObservationV1, WorkflowPortError> {
            panic!("no tool may run while reading a sealed model request");
        }
    }

    #[test]
    fn sealed_unknown_read_never_polls_provider_or_tools() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap());
        let root = grant();
        store
            .begin_adaptive_session(&root, &root.authority, NOW)
            .unwrap();
        store
            .advance_adaptive_session(
                root.session_id,
                1,
                Uuid::from_u128(10),
                &AdaptiveTransitionV1::ClaimModel {
                    effect: effect(102),
                    previous_observation_digest: None,
                },
                &root.authority,
                NOW + 1,
            )
            .unwrap();
        let unknown = store
            .advance_adaptive_session(
                root.session_id,
                2,
                Uuid::from_u128(11),
                &AdaptiveTransitionV1::MarkUnknown {
                    effect: effect(102),
                },
                &root.authority,
                NOW + 2,
            )
            .unwrap()
            .1;
        let core = AdaptiveWorkflowCore::new(
            store,
            Organization(root.authority.clone()),
            NeverPoll,
            NeverPoll,
        );
        assert_eq!(
            core.reconcile_model(
                root.session_id,
                &root.authority,
                Uuid::from_u128(12),
                NOW + 100_000
            )
            .unwrap(),
            unknown
        );
    }
}
