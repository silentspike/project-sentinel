use super::*;
use crate::{CompanyRoleV1, CompanyWorkStateV1, ProjectV1, TenantId, WorkItemId};

/// One pinned SQL discovery boundary, never cached runtime or dispatch authority.
#[derive(Debug, Clone, Serialize)]
pub struct AdaptiveLeadershipDiscoveryRead {
    projects: Vec<AdaptiveLeadershipProjectRead>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdaptiveLeadershipProjectRead {
    project: ProjectV1,
    scopes: Vec<AdaptiveLeadershipScopeRead>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdaptiveLeadershipScopeRead {
    tenant_id: TenantId,
    project_id: crate::ProjectId,
    work_item_id: WorkItemId,
    agent_id: crate::AgentId,
    heads: Result<Vec<AdaptiveSessionV1>, DeferredHeadReadError>,
    reviews: Vec<SessionReviews>,
}

#[derive(Debug, Clone, Serialize)]
struct SessionReviews {
    session_id: Uuid,
    policy_present: Result<bool, DeferredHeadReadError>,
    calls: Result<Vec<AdaptiveLeadershipReviewCallV1>, DeferredHeadReadError>,
}

impl AdaptiveLeadershipDiscoveryRead {
    pub fn projects(&self) -> &[AdaptiveLeadershipProjectRead] {
        &self.projects
    }

    fn retainable(&self) -> bool {
        self.projects.iter().all(|project| {
            project.scopes.iter().all(|scope| {
                scope.heads.is_ok()
                    && scope
                        .reviews
                        .iter()
                        .all(|review| review.policy_present.is_ok() && review.calls.is_ok())
            })
        })
    }
}

impl AdaptiveLeadershipProjectRead {
    pub fn project(&self) -> &ProjectV1 {
        &self.project
    }

    pub fn scopes(&self) -> &[AdaptiveLeadershipScopeRead] {
        &self.scopes
    }
}

impl AdaptiveLeadershipScopeRead {
    pub fn work_item_id(&self) -> &WorkItemId {
        &self.work_item_id
    }

    pub fn agent_id(&self) -> crate::AgentId {
        self.agent_id
    }

    pub fn session_for_authority(
        &self,
        current: &RuntimeAuthoritySnapshotV1,
    ) -> Result<Option<AdaptiveSessionV1>, WorkflowError> {
        current.validate()?;
        if current.tenant_id != self.tenant_id
            || current.project_id != self.project_id
            || current.work_item_id != self.work_item_id
            || current.agent_id != self.agent_id
        {
            return Err(authority_conflict());
        }
        select_scoped_adaptive_head(
            self.heads
                .as_ref()
                .map_err(DeferredHeadReadError::restore)?,
            current,
        )
    }

    pub fn policy_present(&self, session_id: Uuid) -> Result<bool, WorkflowError> {
        self.reviews(session_id)?
            .policy_present
            .as_ref()
            .copied()
            .map_err(DeferredHeadReadError::restore)
    }

    pub fn review_calls(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<AdaptiveLeadershipReviewCallV1>, WorkflowError> {
        self.reviews(session_id)?
            .calls
            .as_ref()
            .cloned()
            .map_err(DeferredHeadReadError::restore)
    }

    fn reviews(&self, session_id: Uuid) -> Result<&SessionReviews, WorkflowError> {
        self.reviews
            .iter()
            .find(|review| review.session_id == session_id)
            .ok_or_else(corrupt_store)
    }
}

impl WorkflowStore {
    /// Validate the full inventory, then collect this leader's finite SQL scopes.
    /// Every project/head/policy/history keeps its existing independent replay budget.
    /// Errors in unselected heads are deferred; runtime checks still precede them.
    pub fn adaptive_leadership_discovery_read(
        &self,
        leader: crate::AgentId,
    ) -> Result<std::sync::Arc<AdaptiveLeadershipDiscoveryRead>, WorkflowError> {
        let key = encode(&leader)?;
        let mut connection = self.lock()?;
        self.validated_read_snapshot_shared_if(
            &mut connection,
            "adaptive-leadership-discovery",
            &key,
            |tx| leadership_discovery_on_connection(tx, leader),
            AdaptiveLeadershipDiscoveryRead::retainable,
        )
    }
}

fn leadership_discovery_on_connection(
    tx: &Connection,
    leader: crate::AgentId,
) -> Result<AdaptiveLeadershipDiscoveryRead, WorkflowError> {
    // Do not put a shared validation arena around independent projects/scopes.
    let projects = crate::domain_store::validated_company_projects_in_snapshot(tx)?;
    let mut result = Vec::new();
    for project in projects {
        if !project.governance.participants.iter().any(|participant| {
            participant.agent_id == leader
                && matches!(
                    participant.role,
                    CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
                )
        }) {
            continue;
        }
        let mut scopes = Vec::new();
        for work in project.work_items.values().filter(|work| {
            work.state == CompanyWorkStateV1::Assigned
                && matches!(
                    work.spec.required_role,
                    CompanyRoleV1::Developer | CompanyRoleV1::Designer
                )
        }) {
            let mut assignments = work
                .assignments
                .iter()
                .filter(|assignment| assignment.active);
            let Some(assignment) = assignments.next() else {
                continue;
            };
            if assignments.next().is_some() {
                continue;
            }
            let heads = scoped_adaptive_heads(
                tx,
                &project.tenant_id,
                &project.project_id,
                &work.spec.work_item_id,
                assignment.agent_id,
            )
            .map_err(DeferredHeadReadError::capture);
            let mut reviews = Vec::new();
            if let Ok(heads) = &heads {
                for session in heads {
                    let session_id = session.grant.session_id;
                    let policy_present =
                        crate::domain_store::validation_scope::with_scope(tx, || {
                            read_resume_policy_leaf(tx, &project.tenant_id, session_id)
                        })
                        .map(|policy| policy.is_some())
                        .map_err(DeferredHeadReadError::capture);
                    let calls = crate::domain_store::adaptive_leadership_review::calls_for_session(
                        tx,
                        &project.tenant_id,
                        session_id,
                    )
                    .map_err(DeferredHeadReadError::capture);
                    reviews.push(SessionReviews {
                        session_id,
                        policy_present,
                        calls,
                    });
                }
            }
            scopes.push(AdaptiveLeadershipScopeRead {
                tenant_id: project.tenant_id.clone(),
                project_id: project.project_id.clone(),
                work_item_id: work.spec.work_item_id.clone(),
                agent_id: assignment.agent_id,
                heads,
                reviews,
            });
        }
        result.push(AdaptiveLeadershipProjectRead { project, scopes });
    }
    Ok(AdaptiveLeadershipDiscoveryRead { projects: result })
}
