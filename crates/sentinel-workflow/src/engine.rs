use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::store::WorkflowTransaction;
use crate::*;

const ENTITY_REQUEST: &str = "customer_request";
const ENTITY_PROPOSAL: &str = "proposal";
const ENTITY_AGREEMENT: &str = "agreement";
const ENTITY_PROJECT: &str = "project";
const ENTITY_WORK_ITEM: &str = "work_item";
const ENTITY_ASSIGNMENT: &str = "assignment";
const ENTITY_EVIDENCE: &str = "completion_evidence";
const ENTITY_DECISION: &str = "decision";
const ENTITY_HANDOFF: &str = "handoff";
const ENTITY_BLOCKER: &str = "blocker";
const ENTITY_APPROVAL: &str = "approval";
const ENTITY_COST_RESERVATION: &str = "cost_reservation";
const ENTITY_PROJECT_ROOM: &str = "project_room";
const ENTITY_ACTION_ITEM: &str = "action_item";
const ENTITY_QUESTION: &str = "project_question";

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

pub struct WorkflowEngine {
    store: Arc<WorkflowStore>,
    execution_port: Arc<dyn WorkExecutionPort>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for WorkflowEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkflowEngine")
            .finish_non_exhaustive()
    }
}

impl WorkflowEngine {
    pub fn new(store: Arc<WorkflowStore>, execution_port: Arc<dyn WorkExecutionPort>) -> Self {
        Self::with_clock(store, execution_port, Arc::new(SystemClock))
    }

    pub fn with_clock(
        store: Arc<WorkflowStore>,
        execution_port: Arc<dyn WorkExecutionPort>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            execution_port,
            clock,
        }
    }

    pub fn execute(
        &self,
        actor: AuthenticatedActor,
        operation_id: &str,
        command: WorkflowCommand,
    ) -> Result<CommandOutcome, WorkflowError> {
        validate_actor(&actor)?;
        validate_text(operation_id, "operation id")?;
        let operation_digest = canonical_digest(&(actor.clone(), command.clone()))?;
        let digest_for_apply = operation_digest.clone();
        let now_ms = self.clock.now_ms();
        self.store
            .execute(operation_id, &operation_digest, now_ms, move |tx| {
                apply_command(tx, &actor, operation_id, &digest_for_apply, command, now_ms)
            })
    }

    /// Dispatches durable execution requests through the injected port.
    /// A successful external reservation is committed as `InProgress`; a
    /// failure leaves the item `Claimed` and the outbox pending for recovery.
    pub fn dispatch_pending_executions(
        &self,
        limit: usize,
    ) -> Result<Vec<ExecutionReceipt>, WorkflowError> {
        let pending = self.store.pending_executions(limit)?;
        let mut receipts = Vec::new();
        for request in pending {
            let now_ms = self.clock.now_ms();
            let receipt = match self.execution_port.reserve(&request) {
                Ok(receipt)
                    if receipt.accepted && receipt.invocation_id == request.invocation_id =>
                {
                    receipt
                }
                Ok(_) => {
                    self.record_execution_failure(&request, now_ms)?;
                    return Err(WorkflowError::new(
                        WorkflowErrorCode::ExecutionUnavailable,
                        true,
                        "work execution request was not accepted",
                    ));
                }
                Err(_) => {
                    self.record_execution_failure(&request, now_ms)?;
                    return Err(WorkflowError::new(
                        WorkflowErrorCode::ExecutionUnavailable,
                        true,
                        "work execution dependency is unavailable",
                    ));
                }
            };
            let receipt_for_tx = receipt.clone();
            self.store.write(|tx| {
                let mut item: WorkItem =
                    required_entity(tx, ENTITY_WORK_ITEM, &request.work_item_id.0, "work item")?;
                if item.state != WorkItemState::Claimed
                    || item.assignee != Some(request.agent_id)
                    || item.assignment_version != request.assignment_version
                {
                    return Err(WorkflowError::new(
                        WorkflowErrorCode::InvalidTransition,
                        false,
                        "claimed work changed before execution dispatch",
                    ));
                }
                tx.mark_execution_attempt(
                    &request.invocation_id,
                    Some(&receipt_for_tx),
                    None,
                    now_ms,
                )?;
                item.state = WorkItemState::InProgress;
                item.version += 1;
                item.updated_at_ms = now_ms;
                tx.put_entity(ENTITY_WORK_ITEM, &item.spec.id.0, item.version, &item)?;
                let mut event = workflow_event(
                    WorkflowEventType::WorkExecutionStarted,
                    "work_item",
                    &item.spec.id.0,
                    &request.requested_by,
                    request.requested_role,
                    &request.invocation_id,
                    &canonical_digest(&request)?,
                    Some("claimed"),
                    Some("in_progress"),
                    "execution port accepted the durable reservation",
                    &receipt_for_tx,
                    now_ms,
                )?;
                let sequence = tx.append_event(&mut event)?;
                refresh_project_projection(tx, &item.project_id, sequence, now_ms)?;
                Ok(())
            })?;
            receipts.push(receipt);
        }
        Ok(receipts)
    }

    fn record_execution_failure(
        &self,
        request: &PendingExecution,
        now_ms: u64,
    ) -> Result<(), WorkflowError> {
        self.store.write(|tx| {
            let exhausted = tx.mark_execution_attempt(
                &request.invocation_id,
                None,
                Some("execution_dependency_unavailable"),
                now_ms,
            )?;
            if exhausted {
                let mut project: Project =
                    required_entity(tx, ENTITY_PROJECT, &request.project_id.0, "project")?;
                project.state = ProjectState::Blocked;
                project.version += 1;
                project.updated_at_ms = now_ms;
                let blocker = Blocker {
                    schema_version: WORKFLOW_SCHEMA_VERSION,
                    id: BlockerId::new("blocker"),
                    project_id: project.id.clone(),
                    work_item_id: Some(request.work_item_id.clone()),
                    cause_ref: "execution_retry_exhausted".into(),
                    impact: "work execution remained unavailable after three bounded attempts"
                        .into(),
                    owner: project.owner,
                    required_resolution_role: ActorRole::ProjectManager,
                    escalation_target: None,
                    state: BlockerState::Open,
                    resolution_ref: None,
                    created_at_ms: now_ms,
                    updated_at_ms: now_ms,
                };
                tx.put_entity(ENTITY_PROJECT, &project.id.0, project.version, &project)?;
                tx.put_entity(ENTITY_BLOCKER, &blocker.id.0, 1, &blocker)?;
                let mut event = workflow_event(
                    WorkflowEventType::BlockerRaised,
                    "blocker",
                    &blocker.id.0,
                    &request.requested_by,
                    request.requested_role,
                    &request.invocation_id,
                    &canonical_digest(request)?,
                    None,
                    Some("open"),
                    "bounded execution retries were exhausted",
                    &blocker,
                    now_ms,
                )?;
                let sequence = tx.append_event(&mut event)?;
                refresh_project_projection(tx, &project.id, sequence, now_ms)?;
            }
            Ok(())
        })
    }

    pub fn customer_request(
        &self,
        request_id: &CustomerRequestId,
    ) -> Result<Option<CustomerRequest>, WorkflowError> {
        self.store.entity(ENTITY_REQUEST, &request_id.0)
    }

    pub fn project(&self, project_id: &ProjectId) -> Result<Option<Project>, WorkflowError> {
        self.store.entity(ENTITY_PROJECT, &project_id.0)
    }

    pub fn work_item(&self, work_item_id: &WorkItemId) -> Result<Option<WorkItem>, WorkflowError> {
        self.store.entity(ENTITY_WORK_ITEM, &work_item_id.0)
    }

    pub fn project_projection(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<ProjectProjection>, WorkflowError> {
        self.store.project_projection(project_id)
    }

    pub fn events_since(
        &self,
        after_sequence: i64,
        limit: usize,
    ) -> Result<Vec<WorkflowEvent>, WorkflowError> {
        self.store.events_since(after_sequence, limit)
    }
}

fn apply_command(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    command: WorkflowCommand,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    match command {
        WorkflowCommand::SubmitCustomerRequest {
            customer_id,
            summary_ref,
            desired_outcome,
            constraints,
        } => {
            require_role(actor, &[ActorRole::Customer])?;
            if actor.customer_id.as_deref() != Some(customer_id.as_str()) {
                return unauthorized("customer identity does not match the authenticated actor");
            }
            validate_text(&summary_ref, "summary reference")?;
            validate_text(&desired_outcome, "desired outcome")?;
            let request = CustomerRequest {
                schema_version: WORKFLOW_SCHEMA_VERSION,
                id: CustomerRequestId::new("request"),
                customer_id,
                summary_ref,
                desired_outcome,
                constraints,
                clarifications: Vec::new(),
                feedback: Vec::new(),
                state: CustomerRequestState::Submitted,
                version: 1,
                proposal_ids: Vec::new(),
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
            };
            tx.put_entity(ENTITY_REQUEST, &request.id.0, request.version, &request)?;
            append_event(
                tx,
                WorkflowEventType::CustomerRequestSubmitted,
                "customer_request",
                &request.id.0,
                actor,
                operation_id,
                operation_digest,
                None,
                Some("submitted"),
                "authenticated customer submitted a bounded request",
                &request,
                now_ms,
            )?;
            Ok(WorkflowResponse::CustomerRequest(request))
        }
        WorkflowCommand::ClarifyCustomerRequest {
            request_id,
            expected_version,
            question_ref,
            answer_ref,
        } => {
            require_role(actor, &[ActorRole::Customer, ActorRole::Sales])?;
            validate_text(&question_ref, "question reference")?;
            validate_text(&answer_ref, "answer reference")?;
            let mut request: CustomerRequest =
                required_entity(tx, ENTITY_REQUEST, &request_id.0, "customer request")?;
            authorize_customer_request(actor, &request)?;
            require_version(request.version, expected_version)?;
            if !matches!(
                request.state,
                CustomerRequestState::Submitted | CustomerRequestState::Clarifying
            ) {
                return invalid_transition("request cannot be clarified in its current state");
            }
            let before = state_name(request.state);
            request.clarifications.push(Clarification {
                question_ref,
                answer_ref,
                recorded_by: actor.actor_id.clone(),
                recorded_at_ms: now_ms,
            });
            request.state = CustomerRequestState::Clarifying;
            request.version += 1;
            request.updated_at_ms = now_ms;
            tx.put_entity(ENTITY_REQUEST, &request.id.0, request.version, &request)?;
            append_event(
                tx,
                WorkflowEventType::CustomerRequestClarified,
                "customer_request",
                &request.id.0,
                actor,
                operation_id,
                operation_digest,
                Some(before),
                Some("clarifying"),
                "bounded clarification was recorded",
                &request.clarifications.last(),
                now_ms,
            )?;
            Ok(WorkflowResponse::CustomerRequest(request))
        }
        WorkflowCommand::QualifyCustomerRequest {
            request_id,
            expected_version,
            reason,
        } => {
            require_role(actor, &[ActorRole::Sales])?;
            validate_text(&reason, "qualification reason")?;
            let mut request: CustomerRequest =
                required_entity(tx, ENTITY_REQUEST, &request_id.0, "customer request")?;
            require_version(request.version, expected_version)?;
            if !matches!(
                request.state,
                CustomerRequestState::Submitted | CustomerRequestState::Clarifying
            ) {
                return invalid_transition("request cannot be qualified in its current state");
            }
            let before = state_name(request.state);
            request.state = CustomerRequestState::Qualified;
            request.version += 1;
            request.updated_at_ms = now_ms;
            tx.put_entity(ENTITY_REQUEST, &request.id.0, request.version, &request)?;
            append_event(
                tx,
                WorkflowEventType::CustomerRequestQualified,
                "customer_request",
                &request.id.0,
                actor,
                operation_id,
                operation_digest,
                Some(before),
                Some("qualified"),
                &reason,
                &serde_json::json!({"reason_ref": reason}),
                now_ms,
            )?;
            Ok(WorkflowResponse::CustomerRequest(request))
        }
        WorkflowCommand::CreateProposal {
            request_id,
            expected_version,
            binding,
        } => {
            require_role(actor, &[ActorRole::Sales])?;
            validate_proposal(&binding, now_ms)?;
            let mut request: CustomerRequest =
                required_entity(tx, ENTITY_REQUEST, &request_id.0, "customer request")?;
            require_version(request.version, expected_version)?;
            if request.state != CustomerRequestState::Qualified {
                return invalid_transition("proposal requires a qualified request");
            }
            let proposal = Proposal {
                schema_version: WORKFLOW_SCHEMA_VERSION,
                id: ProposalId::new("proposal"),
                customer_request_id: request.id.clone(),
                generation: request.proposal_ids.len() as u32 + 1,
                digest: canonical_digest(&binding)?,
                binding,
                created_by: actor.actor_id.clone(),
                created_at_ms: now_ms,
            };
            request.proposal_ids.push(proposal.id.clone());
            request.state = CustomerRequestState::Proposed;
            request.version += 1;
            request.updated_at_ms = now_ms;
            tx.put_entity(ENTITY_PROPOSAL, &proposal.id.0, 1, &proposal)?;
            tx.put_entity(ENTITY_REQUEST, &request.id.0, request.version, &request)?;
            append_event(
                tx,
                WorkflowEventType::ProposalCreated,
                "customer_request",
                &request.id.0,
                actor,
                operation_id,
                operation_digest,
                Some("qualified"),
                Some("proposed"),
                "sales authored a versioned proposal",
                &proposal,
                now_ms,
            )?;
            Ok(WorkflowResponse::Proposal(proposal))
        }
        WorkflowCommand::AcceptProposal {
            request_id,
            expected_version,
            proposal_id,
            proposal_digest,
            profile_id,
            project_owner,
            mut participants,
        } => {
            require_role(actor, &[ActorRole::Customer])?;
            let mut request: CustomerRequest =
                required_entity(tx, ENTITY_REQUEST, &request_id.0, "customer request")?;
            authorize_customer_request(actor, &request)?;
            require_version(request.version, expected_version)?;
            if request.state != CustomerRequestState::Proposed {
                return invalid_transition("customer acceptance requires a proposed request");
            }
            let proposal: Proposal =
                required_entity(tx, ENTITY_PROPOSAL, &proposal_id.0, "proposal")?;
            if proposal.customer_request_id != request.id || proposal.digest != proposal_digest {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::DigestConflict,
                    false,
                    "proposal identity or digest does not match the accepted request",
                ));
            }
            if proposal.binding.expires_at_ms <= now_ms {
                return invalid_transition("proposal is expired");
            }
            validate_text(&profile_id, "profile id")?;
            if !participants.contains(&project_owner) {
                participants.push(project_owner);
            }
            participants.sort_by_key(|agent_id| agent_id.0);
            participants.dedup();
            let agreement = Agreement {
                schema_version: WORKFLOW_SCHEMA_VERSION,
                id: AgreementId::new("agreement"),
                customer_request_id: request.id.clone(),
                proposal_id: proposal.id.clone(),
                proposal_digest: proposal.digest.clone(),
                proposal_binding: proposal.binding.clone(),
                customer_id: request.customer_id.clone(),
                accepted_by: actor.actor_id.clone(),
                accepted_at_ms: now_ms,
            };
            let project = Project {
                schema_version: WORKFLOW_SCHEMA_VERSION,
                id: ProjectId::new("project"),
                agreement_id: agreement.id.clone(),
                agreement_digest: agreement.proposal_digest.clone(),
                profile_id,
                owner: project_owner,
                participants,
                cost_ceiling_micros: proposal.binding.cost_ceiling_micros,
                provider_cost_ceilings_micros: proposal
                    .binding
                    .provider_cost_ceilings_micros
                    .clone(),
                reserved_cost_micros: 0,
                committed_cost_micros: 0,
                state: ProjectState::Planned,
                version: 1,
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
            };
            request.state = CustomerRequestState::Accepted;
            request.version += 1;
            request.updated_at_ms = now_ms;
            tx.put_entity(ENTITY_AGREEMENT, &agreement.id.0, 1, &agreement)?;
            tx.put_entity(ENTITY_PROJECT, &project.id.0, project.version, &project)?;
            tx.put_entity(ENTITY_REQUEST, &request.id.0, request.version, &request)?;
            append_event(
                tx,
                WorkflowEventType::AgreementAccepted,
                "agreement",
                &agreement.id.0,
                actor,
                operation_id,
                operation_digest,
                Some("proposed"),
                Some("accepted"),
                "customer explicitly accepted the exact proposal digest",
                &agreement,
                now_ms,
            )?;
            let sequence = append_event(
                tx,
                WorkflowEventType::ProjectCreated,
                "project",
                &project.id.0,
                actor,
                operation_id,
                operation_digest,
                None,
                Some("planned"),
                "agreement and project were created in one transaction",
                &project,
                now_ms,
            )?;
            refresh_project_projection(tx, &project.id, sequence, now_ms)?;
            Ok(WorkflowResponse::AgreementProject { agreement, project })
        }
        WorkflowCommand::RejectProposal {
            request_id,
            expected_version,
            proposal_id,
            proposal_digest,
            reason_ref,
        } => {
            require_role(actor, &[ActorRole::Customer])?;
            validate_text(&reason_ref, "rejection reason reference")?;
            let mut request: CustomerRequest =
                required_entity(tx, ENTITY_REQUEST, &request_id.0, "customer request")?;
            authorize_customer_request(actor, &request)?;
            require_version(request.version, expected_version)?;
            if request.state != CustomerRequestState::Proposed {
                return invalid_transition("only a proposed request may be rejected");
            }
            let proposal: Proposal =
                required_entity(tx, ENTITY_PROPOSAL, &proposal_id.0, "proposal")?;
            if proposal.customer_request_id != request.id || proposal.digest != proposal_digest {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::DigestConflict,
                    false,
                    "proposal identity or digest does not match the rejected request",
                ));
            }
            request.state = CustomerRequestState::Rejected;
            request.version += 1;
            request.updated_at_ms = now_ms;
            tx.put_entity(ENTITY_REQUEST, &request.id.0, request.version, &request)?;
            append_event(
                tx,
                WorkflowEventType::CustomerRequestRejected,
                "customer_request",
                &request.id.0,
                actor,
                operation_id,
                operation_digest,
                Some("proposed"),
                Some("rejected"),
                "authenticated customer rejected the exact proposal digest",
                &serde_json::json!({
                    "proposal_id": proposal.id,
                    "proposal_digest": proposal.digest,
                    "reason_ref": reason_ref,
                }),
                now_ms,
            )?;
            Ok(WorkflowResponse::CustomerRequest(request))
        }
        WorkflowCommand::CancelCustomerRequest {
            request_id,
            expected_version,
            reason_ref,
        } => {
            require_role(actor, &[ActorRole::Customer])?;
            validate_text(&reason_ref, "cancellation reason reference")?;
            let mut request: CustomerRequest =
                required_entity(tx, ENTITY_REQUEST, &request_id.0, "customer request")?;
            authorize_customer_request(actor, &request)?;
            require_version(request.version, expected_version)?;
            if !matches!(
                request.state,
                CustomerRequestState::Submitted
                    | CustomerRequestState::Clarifying
                    | CustomerRequestState::Qualified
                    | CustomerRequestState::Proposed
            ) {
                return invalid_transition("request can no longer be cancelled");
            }
            let before = state_name(request.state);
            request.state = CustomerRequestState::Cancelled;
            request.version += 1;
            request.updated_at_ms = now_ms;
            tx.put_entity(ENTITY_REQUEST, &request.id.0, request.version, &request)?;
            append_event(
                tx,
                WorkflowEventType::CustomerRequestCancelled,
                "customer_request",
                &request.id.0,
                actor,
                operation_id,
                operation_digest,
                Some(before),
                Some("cancelled"),
                "authenticated customer cancelled before agreement commit",
                &serde_json::json!({"reason_ref": reason_ref}),
                now_ms,
            )?;
            Ok(WorkflowResponse::CustomerRequest(request))
        }
        WorkflowCommand::RecordCustomerFeedback {
            request_id,
            feedback_ref,
        } => {
            require_role(actor, &[ActorRole::Customer])?;
            validate_text(&feedback_ref, "feedback reference")?;
            let mut request: CustomerRequest =
                required_entity(tx, ENTITY_REQUEST, &request_id.0, "customer request")?;
            authorize_customer_request(actor, &request)?;
            if !matches!(
                request.state,
                CustomerRequestState::Accepted
                    | CustomerRequestState::Rejected
                    | CustomerRequestState::Cancelled
            ) {
                return invalid_transition("feedback requires a terminal customer decision");
            }
            let feedback = CustomerFeedback {
                feedback_ref,
                recorded_by: actor.actor_id.clone(),
                recorded_at_ms: now_ms,
            };
            request.feedback.push(feedback.clone());
            request.version += 1;
            request.updated_at_ms = now_ms;
            tx.put_entity(ENTITY_REQUEST, &request.id.0, request.version, &request)?;
            append_event(
                tx,
                WorkflowEventType::CustomerFeedbackRecorded,
                "customer_request",
                &request.id.0,
                actor,
                operation_id,
                operation_digest,
                None,
                None,
                "bounded customer feedback reference was recorded",
                &feedback,
                now_ms,
            )?;
            Ok(WorkflowResponse::CustomerRequest(request))
        }
        WorkflowCommand::PlanWorkGraph {
            project_id,
            expected_version,
            items,
        } => {
            require_role(
                actor,
                &[ActorRole::ProjectManager, ActorRole::TechnicalLead],
            )?;
            validate_work_graph(&items)?;
            let mut project: Project =
                required_entity(tx, ENTITY_PROJECT, &project_id.0, "project")?;
            authorize_project_actor(actor, &project)?;
            require_version(project.version, expected_version)?;
            if items
                .iter()
                .any(|item| !project.participants.contains(&item.owner))
            {
                return unauthorized("work-item owner is not a project participant");
            }
            if project.state != ProjectState::Planned {
                return invalid_transition("work graph can only be planned once");
            }
            let graph_budget = items.iter().try_fold(0u64, |total, item| {
                total.checked_add(item.budget_micros).ok_or_else(|| {
                    WorkflowError::new(
                        WorkflowErrorCode::BudgetExceeded,
                        false,
                        "work graph budget overflow",
                    )
                })
            })?;
            if graph_budget > project.cost_ceiling_micros {
                return budget_exceeded("work graph exceeds the agreed project ceiling");
            }
            let work_items: Vec<WorkItem> = items
                .into_iter()
                .map(|spec| WorkItem {
                    schema_version: WORKFLOW_SCHEMA_VERSION,
                    project_id: project.id.clone(),
                    state: if spec.dependency_ids.is_empty() {
                        WorkItemState::Ready
                    } else {
                        WorkItemState::Proposed
                    },
                    spec,
                    assignee: None,
                    assignment_version: 0,
                    version: 1,
                    output_refs: BTreeMap::new(),
                    completion_evidence_id: None,
                    created_at_ms: now_ms,
                    updated_at_ms: now_ms,
                })
                .collect();
            for item in &work_items {
                tx.put_entity(ENTITY_WORK_ITEM, &item.spec.id.0, item.version, item)?;
            }
            project.state = ProjectState::Active;
            project.version += 1;
            project.updated_at_ms = now_ms;
            tx.put_entity(ENTITY_PROJECT, &project.id.0, project.version, &project)?;
            let sequence = append_event(
                tx,
                WorkflowEventType::WorkGraphPlanned,
                "project",
                &project.id.0,
                actor,
                operation_id,
                operation_digest,
                Some("planned"),
                Some("active"),
                "validated acyclic work graph was committed",
                &work_items,
                now_ms,
            )?;
            refresh_project_projection(tx, &project.id, sequence, now_ms)?;
            Ok(WorkflowResponse::WorkItems(work_items))
        }
        WorkflowCommand::AssignWork {
            work_item_id,
            expected_version,
            assignee,
            reason,
        } => assign_work(
            tx,
            actor,
            operation_id,
            operation_digest,
            work_item_id,
            expected_version,
            assignee,
            reason,
            now_ms,
        ),
        WorkflowCommand::ClaimWork {
            work_item_id,
            expected_version,
            agent_id,
            input_digest,
            deadline_ms,
        } => claim_work(
            tx,
            actor,
            operation_id,
            operation_digest,
            work_item_id,
            expected_version,
            agent_id,
            input_digest,
            deadline_ms,
            now_ms,
        ),
        WorkflowCommand::CompleteWork {
            work_item_id,
            expected_version,
            assignment_version,
            output_refs,
            gate_id,
            gate_passed,
        } => complete_work(
            tx,
            actor,
            operation_id,
            operation_digest,
            work_item_id,
            expected_version,
            assignment_version,
            output_refs,
            gate_id,
            gate_passed,
            now_ms,
        ),
        WorkflowCommand::RecordDecision {
            project_id,
            work_item_id,
            choice,
            alternatives,
            rationale_ref,
            evidence_refs,
        } => {
            require_role(
                actor,
                &[ActorRole::ProjectManager, ActorRole::TechnicalLead],
            )?;
            let project: Project = required_entity(tx, ENTITY_PROJECT, &project_id.0, "project")?;
            authorize_project_actor(actor, &project)?;
            validate_text(&choice, "decision choice")?;
            validate_text(&rationale_ref, "decision rationale reference")?;
            let decision = ProjectDecision {
                schema_version: WORKFLOW_SCHEMA_VERSION,
                id: DecisionId::new("decision"),
                project_id,
                work_item_id,
                choice,
                alternatives,
                rationale_ref,
                evidence_refs,
                decided_by: actor.actor_id.clone(),
                created_at_ms: now_ms,
            };
            tx.put_entity(ENTITY_DECISION, &decision.id.0, 1, &decision)?;
            let sequence = append_event(
                tx,
                WorkflowEventType::DecisionRecorded,
                "project",
                &decision.project_id.0,
                actor,
                operation_id,
                operation_digest,
                None,
                None,
                "structured project decision was recorded",
                &decision,
                now_ms,
            )?;
            refresh_project_projection(tx, &decision.project_id, sequence, now_ms)?;
            Ok(WorkflowResponse::Decision(decision))
        }
        WorkflowCommand::CreateHandoff {
            project_id,
            work_item_id,
            producer,
            consumer,
            artifact_digests,
            reason,
        } => create_handoff(
            tx,
            actor,
            operation_id,
            operation_digest,
            project_id,
            work_item_id,
            producer,
            consumer,
            artifact_digests,
            reason,
            now_ms,
        ),
        WorkflowCommand::AcknowledgeHandoff {
            handoff_id,
            accepted,
            reason,
        } => acknowledge_handoff(
            tx,
            actor,
            operation_id,
            operation_digest,
            handoff_id,
            accepted,
            reason,
            now_ms,
        ),
        WorkflowCommand::RaiseBlocker {
            project_id,
            work_item_id,
            cause_ref,
            impact,
            owner,
            required_resolution_role,
        } => raise_blocker(
            tx,
            actor,
            operation_id,
            operation_digest,
            project_id,
            work_item_id,
            cause_ref,
            impact,
            owner,
            required_resolution_role,
            now_ms,
        ),
        WorkflowCommand::EscalateBlocker {
            blocker_id,
            escalation_target,
            reason,
        } => escalate_blocker(
            tx,
            actor,
            operation_id,
            operation_digest,
            blocker_id,
            escalation_target,
            reason,
            now_ms,
        ),
        WorkflowCommand::ResolveBlocker {
            blocker_id,
            resolution_ref,
        } => resolve_blocker(
            tx,
            actor,
            operation_id,
            operation_digest,
            blocker_id,
            resolution_ref,
            now_ms,
        ),
        WorkflowCommand::RecordApproval {
            project_id,
            work_item_id,
            gate_id,
            subject_digest,
            approved,
            reason,
        } => record_approval(
            tx,
            actor,
            operation_id,
            operation_digest,
            project_id,
            work_item_id,
            gate_id,
            subject_digest,
            approved,
            reason,
            now_ms,
        ),
        WorkflowCommand::ReserveCost {
            project_id,
            work_item_id,
            provider,
            amount_micros,
        } => reserve_cost(
            tx,
            actor,
            operation_id,
            operation_digest,
            project_id,
            work_item_id,
            provider,
            amount_micros,
            now_ms,
        ),
        WorkflowCommand::CommitCost {
            reservation_id,
            actual_micros,
        } => commit_cost(
            tx,
            actor,
            operation_id,
            operation_digest,
            reservation_id,
            actual_micros,
            now_ms,
        ),
        WorkflowCommand::CreateProjectRoom {
            project_id,
            kind,
            team_ref,
            members,
        } => create_project_room(
            tx,
            actor,
            operation_id,
            operation_digest,
            project_id,
            kind,
            team_ref,
            members,
            now_ms,
        ),
        WorkflowCommand::RecordActionItem {
            project_id,
            work_item_id,
            owner,
            action_ref,
            due_at_ms,
        } => record_action_item(
            tx,
            actor,
            operation_id,
            operation_digest,
            project_id,
            work_item_id,
            owner,
            action_ref,
            due_at_ms,
            now_ms,
        ),
        WorkflowCommand::ResolveActionItem {
            action_item_id,
            completed,
            resolution_ref,
        } => resolve_action_item(
            tx,
            actor,
            operation_id,
            operation_digest,
            action_item_id,
            completed,
            resolution_ref,
            now_ms,
        ),
        WorkflowCommand::RecordQuestion {
            project_id,
            work_item_id,
            owner,
            question_ref,
        } => record_question(
            tx,
            actor,
            operation_id,
            operation_digest,
            project_id,
            work_item_id,
            owner,
            question_ref,
            now_ms,
        ),
        WorkflowCommand::ResolveQuestion {
            question_id,
            resolution_ref,
        } => resolve_question(
            tx,
            actor,
            operation_id,
            operation_digest,
            question_id,
            resolution_ref,
            now_ms,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn assign_work(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    work_item_id: WorkItemId,
    expected_version: u64,
    assignee: AgentProfile,
    reason: String,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    require_role(
        actor,
        &[ActorRole::ProjectManager, ActorRole::TechnicalLead],
    )?;
    validate_text(&reason, "assignment reason")?;
    let mut item: WorkItem = required_entity(tx, ENTITY_WORK_ITEM, &work_item_id.0, "work item")?;
    require_version(item.version, expected_version)?;
    if !matches!(item.state, WorkItemState::Ready | WorkItemState::Assigned) {
        return invalid_transition("work item is not assignable");
    }
    let project: Project = required_entity(tx, ENTITY_PROJECT, &item.project_id.0, "project")?;
    authorize_project_actor(actor, &project)?;
    if !project.participants.contains(&assignee.agent_id) {
        return unauthorized("assignee is not a project participant");
    }
    if actor.agent_id == Some(assignee.agent_id) {
        return unauthorized("actors may not authorize their own assignment");
    }
    if !assignee.active || assignee.current_assignments >= assignee.max_assignments {
        return Err(WorkflowError::new(
            WorkflowErrorCode::CapabilityDenied,
            false,
            "assignee is inactive or at its declared workload limit",
        ));
    }
    if assignee.role != item.spec.required_role
        || !item
            .spec
            .required_capabilities
            .is_subset(&assignee.capabilities)
    {
        return Err(WorkflowError::new(
            WorkflowErrorCode::CapabilityDenied,
            false,
            "assignee role or capabilities do not satisfy the work item",
        ));
    }
    if actor.role == ActorRole::TechnicalLead && assignee.reports_to != actor.agent_id {
        return unauthorized("technical lead may assign only its declared reports");
    }
    for mut existing in tx.entities::<Assignment>(ENTITY_ASSIGNMENT)? {
        if existing.work_item_id == item.spec.id && existing.active {
            existing.active = false;
            existing.revoked_at_ms = Some(now_ms);
            tx.put_entity(ENTITY_ASSIGNMENT, &existing.id.0, 2, &existing)?;
        }
    }
    item.assignment_version += 1;
    item.assignee = Some(assignee.agent_id);
    item.state = WorkItemState::Assigned;
    item.version += 1;
    item.updated_at_ms = now_ms;
    let assignment = Assignment {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        id: AssignmentId::new("assignment"),
        project_id: project.id.clone(),
        work_item_id: item.spec.id.clone(),
        assignee: assignee.agent_id,
        assignee_profile: assignee,
        assigned_by: actor.actor_id.clone(),
        assignment_version: item.assignment_version,
        reason,
        active: true,
        created_at_ms: now_ms,
        revoked_at_ms: None,
    };
    tx.put_entity(ENTITY_ASSIGNMENT, &assignment.id.0, 1, &assignment)?;
    tx.put_entity(ENTITY_WORK_ITEM, &item.spec.id.0, item.version, &item)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::WorkAssigned,
        "work_item",
        &item.spec.id.0,
        actor,
        operation_id,
        operation_digest,
        Some("ready"),
        Some("assigned"),
        "authorized hierarchy and capability policy accepted assignment",
        &assignment,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::Assignment(assignment))
}

#[allow(clippy::too_many_arguments)]
fn claim_work(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    work_item_id: WorkItemId,
    expected_version: u64,
    agent_id: AgentId,
    input_digest: String,
    deadline_ms: u64,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    if actor.agent_id != Some(agent_id) || !actor.role.is_internal() {
        return unauthorized("only the assigned authenticated agent may claim work");
    }
    validate_digest(&input_digest)?;
    if deadline_ms <= now_ms {
        return invalid_input("execution deadline must be in the future");
    }
    let mut item: WorkItem = required_entity(tx, ENTITY_WORK_ITEM, &work_item_id.0, "work item")?;
    require_version(item.version, expected_version)?;
    if item.state != WorkItemState::Assigned || item.assignee != Some(agent_id) {
        return invalid_transition("work item is not assigned to the claiming actor");
    }
    if actor.role != item.spec.required_role {
        return unauthorized("authenticated role does not match the work assignment");
    }
    item.state = WorkItemState::Claimed;
    item.version += 1;
    item.updated_at_ms = now_ms;
    let invocation_id = format!("work-exec-{}-v{}", item.spec.id.0, item.assignment_version);
    let request = PendingExecution {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        invocation_id: invocation_id.clone(),
        project_id: item.project_id.clone(),
        work_item_id: item.spec.id.clone(),
        agent_id,
        requested_by: actor.actor_id.clone(),
        requested_role: actor.role,
        assignment_version: item.assignment_version,
        capabilities: item.spec.required_capabilities.clone(),
        input_digest,
        deadline_ms,
    };
    let request_digest = canonical_digest(&request)?;
    tx.enqueue_execution(&request, &request_digest, now_ms)?;
    tx.put_entity(ENTITY_WORK_ITEM, &item.spec.id.0, item.version, &item)?;
    append_event(
        tx,
        WorkflowEventType::WorkClaimed,
        "work_item",
        &item.spec.id.0,
        actor,
        operation_id,
        operation_digest,
        Some("assigned"),
        Some("claimed"),
        "assigned agent claimed a durable execution request",
        &serde_json::json!({"invocation_id": invocation_id}),
        now_ms,
    )?;
    let sequence = append_event(
        tx,
        WorkflowEventType::WorkExecutionRequested,
        "work_item",
        &item.spec.id.0,
        actor,
        operation_id,
        operation_digest,
        Some("claimed"),
        Some("claimed"),
        "execution request was placed in the durable outbox",
        &request,
        now_ms,
    )?;
    refresh_project_projection(tx, &item.project_id, sequence, now_ms)?;
    Ok(WorkflowResponse::WorkItem(item))
}

#[allow(clippy::too_many_arguments)]
fn complete_work(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    work_item_id: WorkItemId,
    expected_version: u64,
    assignment_version: u64,
    output_refs: BTreeMap<String, String>,
    gate_id: String,
    gate_passed: bool,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    let mut item: WorkItem = required_entity(tx, ENTITY_WORK_ITEM, &work_item_id.0, "work item")?;
    require_version(item.version, expected_version)?;
    if actor.agent_id != item.assignee || actor.role != item.spec.required_role {
        return unauthorized("only the current assignee may submit completion evidence");
    }
    if item.state != WorkItemState::InProgress {
        return invalid_transition("work item has no accepted execution in progress");
    }
    if item.assignment_version != assignment_version {
        return Err(WorkflowError::new(
            WorkflowErrorCode::VersionConflict,
            false,
            "completion evidence targets a stale assignment",
        ));
    }
    if gate_id != item.spec.quality_gate || !gate_passed {
        return invalid_transition("required work-item quality gate did not pass");
    }
    let output_kinds: BTreeSet<_> = output_refs.keys().cloned().collect();
    if !item.spec.required_output_kinds.is_subset(&output_kinds)
        || output_refs
            .values()
            .any(|digest| validate_digest(digest).is_err())
    {
        return invalid_input("completion evidence is missing a required digest-bound output");
    }
    let evidence = CompletionEvidence {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        id: EvidenceId::new("evidence"),
        project_id: item.project_id.clone(),
        work_item_id: item.spec.id.clone(),
        assignment_version,
        output_refs: output_refs.clone(),
        gate_id,
        gate_passed,
        recorded_by: actor.actor_id.clone(),
        recorded_at_ms: now_ms,
    };
    item.output_refs = output_refs;
    item.completion_evidence_id = Some(evidence.id.clone());
    item.state = WorkItemState::Done;
    item.version += 1;
    item.updated_at_ms = now_ms;
    tx.put_entity(ENTITY_EVIDENCE, &evidence.id.0, 1, &evidence)?;
    tx.put_entity(ENTITY_WORK_ITEM, &item.spec.id.0, item.version, &item)?;
    unlock_dependents(tx, &item.project_id, &item.spec.id, now_ms)?;
    let mut project: Project = required_entity(tx, ENTITY_PROJECT, &item.project_id.0, "project")?;
    let all_items: Vec<WorkItem> = tx
        .entities::<WorkItem>(ENTITY_WORK_ITEM)?
        .into_iter()
        .filter(|candidate| candidate.project_id == project.id)
        .collect();
    if !all_items.is_empty()
        && all_items
            .iter()
            .all(|candidate| candidate.state == WorkItemState::Done)
    {
        project.state = ProjectState::DeliveryCandidate;
        project.version += 1;
        project.updated_at_ms = now_ms;
        tx.put_entity(ENTITY_PROJECT, &project.id.0, project.version, &project)?;
    }
    let sequence = append_event(
        tx,
        WorkflowEventType::WorkCompleted,
        "work_item",
        &item.spec.id.0,
        actor,
        operation_id,
        operation_digest,
        Some("in_progress"),
        Some("done"),
        "required outputs and completion evidence passed the declared gate",
        &evidence,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::WorkItem(item))
}

#[allow(clippy::too_many_arguments)]
fn create_handoff(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    project_id: ProjectId,
    work_item_id: WorkItemId,
    producer: AgentId,
    consumer: AgentId,
    artifact_digests: BTreeSet<String>,
    reason: String,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    if actor.agent_id != Some(producer) {
        return unauthorized("only the authenticated producer may create a handoff");
    }
    let project: Project = required_entity(tx, ENTITY_PROJECT, &project_id.0, "project")?;
    if !project.participants.contains(&producer) || !project.participants.contains(&consumer) {
        return unauthorized("handoff participants must belong to the project");
    }
    let item: WorkItem = required_entity(tx, ENTITY_WORK_ITEM, &work_item_id.0, "work item")?;
    if item.project_id != project.id || item.assignee != Some(producer) {
        return unauthorized("producer does not own the referenced work item");
    }
    if artifact_digests.is_empty()
        || artifact_digests
            .iter()
            .any(|digest| validate_digest(digest).is_err())
    {
        return invalid_input("handoff requires valid artifact digests");
    }
    validate_text(&reason, "handoff reason")?;
    let handoff = Handoff {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        id: HandoffId::new("handoff"),
        project_id,
        work_item_id,
        producer,
        consumer,
        artifact_digests,
        state: HandoffState::Offered,
        reason,
        created_at_ms: now_ms,
        acknowledged_at_ms: None,
    };
    tx.put_entity(ENTITY_HANDOFF, &handoff.id.0, 1, &handoff)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::HandoffCreated,
        "handoff",
        &handoff.id.0,
        actor,
        operation_id,
        operation_digest,
        None,
        Some("offered"),
        "structured artifact handoff was offered",
        &handoff,
        now_ms,
    )?;
    refresh_project_projection(tx, &handoff.project_id, sequence, now_ms)?;
    Ok(WorkflowResponse::Handoff(handoff))
}

#[allow(clippy::too_many_arguments)]
fn acknowledge_handoff(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    handoff_id: HandoffId,
    accepted: bool,
    reason: String,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    let mut handoff: Handoff = required_entity(tx, ENTITY_HANDOFF, &handoff_id.0, "handoff")?;
    if actor.agent_id != Some(handoff.consumer) || handoff.state != HandoffState::Offered {
        return unauthorized("only the designated consumer may acknowledge an open handoff");
    }
    validate_text(&reason, "handoff acknowledgement reason")?;
    handoff.state = if accepted {
        HandoffState::Accepted
    } else {
        HandoffState::Rejected
    };
    handoff.reason = reason;
    handoff.acknowledged_at_ms = Some(now_ms);
    tx.put_entity(ENTITY_HANDOFF, &handoff.id.0, 2, &handoff)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::HandoffAcknowledged,
        "handoff",
        &handoff.id.0,
        actor,
        operation_id,
        operation_digest,
        Some("offered"),
        Some(if accepted { "accepted" } else { "rejected" }),
        "designated consumer acknowledged the structured handoff",
        &handoff,
        now_ms,
    )?;
    refresh_project_projection(tx, &handoff.project_id, sequence, now_ms)?;
    Ok(WorkflowResponse::Handoff(handoff))
}

#[allow(clippy::too_many_arguments)]
fn raise_blocker(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    project_id: ProjectId,
    work_item_id: Option<WorkItemId>,
    cause_ref: String,
    impact: String,
    owner: AgentId,
    required_resolution_role: ActorRole,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    if !actor.role.is_internal() || actor.role == ActorRole::Gaia {
        return unauthorized("observer roles cannot create authoritative blockers");
    }
    validate_text(&cause_ref, "blocker cause reference")?;
    validate_text(&impact, "blocker impact")?;
    let mut project: Project = required_entity(tx, ENTITY_PROJECT, &project_id.0, "project")?;
    authorize_project_actor(actor, &project)?;
    if !project.participants.contains(&owner) {
        return unauthorized("blocker owner is not a project participant");
    }
    if let Some(work_item_id) = &work_item_id {
        let mut item: WorkItem =
            required_entity(tx, ENTITY_WORK_ITEM, &work_item_id.0, "work item")?;
        if item.project_id != project.id || item.state == WorkItemState::Done {
            return invalid_transition("referenced work item cannot be blocked");
        }
        item.state = WorkItemState::Blocked;
        item.version += 1;
        item.updated_at_ms = now_ms;
        tx.put_entity(ENTITY_WORK_ITEM, &item.spec.id.0, item.version, &item)?;
    }
    project.state = ProjectState::Blocked;
    project.version += 1;
    project.updated_at_ms = now_ms;
    tx.put_entity(ENTITY_PROJECT, &project.id.0, project.version, &project)?;
    let blocker = Blocker {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        id: BlockerId::new("blocker"),
        project_id: project.id.clone(),
        work_item_id,
        cause_ref,
        impact,
        owner,
        required_resolution_role,
        escalation_target: None,
        state: BlockerState::Open,
        resolution_ref: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    };
    tx.put_entity(ENTITY_BLOCKER, &blocker.id.0, 1, &blocker)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::BlockerRaised,
        "blocker",
        &blocker.id.0,
        actor,
        operation_id,
        operation_digest,
        None,
        Some("open"),
        "authorized actor recorded a durable blocker",
        &blocker,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::Blocker(blocker))
}

#[allow(clippy::too_many_arguments)]
fn escalate_blocker(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    blocker_id: BlockerId,
    escalation_target: AgentId,
    reason: String,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    require_role(
        actor,
        &[ActorRole::ProjectManager, ActorRole::TechnicalLead],
    )?;
    validate_text(&reason, "escalation reason")?;
    let mut blocker: Blocker = required_entity(tx, ENTITY_BLOCKER, &blocker_id.0, "blocker")?;
    if blocker.state == BlockerState::Resolved {
        return invalid_transition("resolved blocker cannot be escalated");
    }
    let project: Project = required_entity(tx, ENTITY_PROJECT, &blocker.project_id.0, "project")?;
    authorize_project_actor(actor, &project)?;
    if !project.participants.contains(&escalation_target) {
        return unauthorized("escalation target is not a project participant");
    }
    blocker.state = BlockerState::Escalated;
    blocker.escalation_target = Some(escalation_target);
    blocker.updated_at_ms = now_ms;
    tx.put_entity(ENTITY_BLOCKER, &blocker.id.0, 2, &blocker)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::BlockerEscalated,
        "blocker",
        &blocker.id.0,
        actor,
        operation_id,
        operation_digest,
        Some("open"),
        Some("escalated"),
        &reason,
        &blocker,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::Blocker(blocker))
}

#[allow(clippy::too_many_arguments)]
fn resolve_blocker(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    blocker_id: BlockerId,
    resolution_ref: String,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    validate_text(&resolution_ref, "blocker resolution reference")?;
    let mut blocker: Blocker = required_entity(tx, ENTITY_BLOCKER, &blocker_id.0, "blocker")?;
    if actor.role != blocker.required_resolution_role {
        return unauthorized("actor lacks the blocker's required resolution role");
    }
    if blocker.state == BlockerState::Resolved {
        return invalid_transition("blocker is already resolved");
    }
    let mut project: Project =
        required_entity(tx, ENTITY_PROJECT, &blocker.project_id.0, "project")?;
    authorize_project_actor(actor, &project)?;
    blocker.state = BlockerState::Resolved;
    blocker.resolution_ref = Some(resolution_ref);
    blocker.updated_at_ms = now_ms;
    tx.put_entity(ENTITY_BLOCKER, &blocker.id.0, 3, &blocker)?;
    if let Some(work_item_id) = &blocker.work_item_id {
        if blocker.cause_ref == "execution_retry_exhausted" {
            tx.reset_failed_execution(&project.id, work_item_id, now_ms)?;
        }
        let mut item: WorkItem =
            required_entity(tx, ENTITY_WORK_ITEM, &work_item_id.0, "work item")?;
        if item.state == WorkItemState::Blocked {
            item.state = if dependencies_done(tx, &item)? {
                WorkItemState::Ready
            } else {
                WorkItemState::Proposed
            };
            item.version += 1;
            item.updated_at_ms = now_ms;
            tx.put_entity(ENTITY_WORK_ITEM, &item.spec.id.0, item.version, &item)?;
        }
    }
    let open_blockers = tx
        .entities::<Blocker>(ENTITY_BLOCKER)?
        .into_iter()
        .any(|candidate| {
            candidate.project_id == project.id && candidate.state != BlockerState::Resolved
        });
    if !open_blockers {
        project.state = ProjectState::Active;
        project.version += 1;
        project.updated_at_ms = now_ms;
        tx.put_entity(ENTITY_PROJECT, &project.id.0, project.version, &project)?;
    }
    let sequence = append_event(
        tx,
        WorkflowEventType::BlockerResolved,
        "blocker",
        &blocker.id.0,
        actor,
        operation_id,
        operation_digest,
        Some("open_or_escalated"),
        Some("resolved"),
        "authorized resolution role supplied durable resolution evidence",
        &blocker,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::Blocker(blocker))
}

#[allow(clippy::too_many_arguments)]
fn record_approval(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    project_id: ProjectId,
    work_item_id: Option<WorkItemId>,
    gate_id: String,
    subject_digest: String,
    approved: bool,
    reason: String,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    require_role(actor, &[ActorRole::Qa, ActorRole::ReleaseManager])?;
    validate_text(&gate_id, "approval gate")?;
    validate_digest(&subject_digest)?;
    validate_text(&reason, "approval reason")?;
    let project: Project = required_entity(tx, ENTITY_PROJECT, &project_id.0, "project")?;
    authorize_project_actor(actor, &project)?;
    if let Some(work_item_id) = &work_item_id {
        let item: WorkItem = required_entity(tx, ENTITY_WORK_ITEM, &work_item_id.0, "work item")?;
        if item.project_id != project.id || item.assignee == actor.agent_id {
            return unauthorized("assignee cannot approve its own work");
        }
    }
    let approval = Approval {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        id: ApprovalId::new("approval"),
        project_id,
        work_item_id,
        gate_id,
        subject_digest,
        state: if approved {
            ApprovalState::Approved
        } else {
            ApprovalState::Rejected
        },
        actor_id: actor.actor_id.clone(),
        actor_role: actor.role,
        reason,
        created_at_ms: now_ms,
    };
    tx.put_entity(ENTITY_APPROVAL, &approval.id.0, 1, &approval)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::ApprovalRecorded,
        "approval",
        &approval.id.0,
        actor,
        operation_id,
        operation_digest,
        None,
        Some(if approved { "approved" } else { "rejected" }),
        "independent digest-bound approval was recorded",
        &approval,
        now_ms,
    )?;
    refresh_project_projection(tx, &approval.project_id, sequence, now_ms)?;
    Ok(WorkflowResponse::Approval(approval))
}

#[allow(clippy::too_many_arguments)]
fn reserve_cost(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    project_id: ProjectId,
    work_item_id: Option<WorkItemId>,
    provider: String,
    amount_micros: u64,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    if !actor.role.is_internal() || actor.role == ActorRole::Gaia {
        return unauthorized("actor may not reserve billable project cost");
    }
    validate_text(&provider, "provider")?;
    let mut project: Project = required_entity(tx, ENTITY_PROJECT, &project_id.0, "project")?;
    authorize_project_actor(actor, &project)?;
    if project.state != ProjectState::Active {
        return invalid_transition("cost may only be reserved for an active project");
    }
    let provider_ceiling_micros = project
        .provider_cost_ceilings_micros
        .get(&provider)
        .copied()
        .unwrap_or(0);
    let provider_total = tx
        .entities::<CostReservation>(ENTITY_COST_RESERVATION)?
        .into_iter()
        .filter(|reservation| {
            reservation.project_id == project.id && reservation.provider == provider
        })
        .try_fold(amount_micros, |total, reservation| {
            total.checked_add(
                reservation
                    .committed_micros
                    .unwrap_or(reservation.amount_micros),
            )
        });
    if amount_micros == 0 || provider_total.is_none_or(|total| total > provider_ceiling_micros) {
        return create_budget_blocker(
            tx,
            actor,
            operation_id,
            operation_digest,
            &mut project,
            work_item_id,
            "provider_ceiling_exhausted",
            format!(
                "provider {provider} admits at most {provider_ceiling_micros} micros; requested {amount_micros}"
            ),
            now_ms,
        );
    }
    if project
        .reserved_cost_micros
        .checked_add(project.committed_cost_micros)
        .and_then(|total| total.checked_add(amount_micros))
        .is_none_or(|total| total > project.cost_ceiling_micros)
    {
        return create_budget_blocker(
            tx,
            actor,
            operation_id,
            operation_digest,
            &mut project,
            work_item_id,
            "project_ceiling_exhausted",
            "requested cost exceeds the immutable project ceiling".into(),
            now_ms,
        );
    }
    if let Some(work_item_id) = &work_item_id {
        let item: WorkItem = required_entity(tx, ENTITY_WORK_ITEM, &work_item_id.0, "work item")?;
        if item.project_id != project.id {
            return unauthorized("cost work item belongs to another project");
        }
        let work_total = tx
            .entities::<CostReservation>(ENTITY_COST_RESERVATION)?
            .into_iter()
            .filter(|reservation| reservation.work_item_id.as_ref() == Some(work_item_id))
            .try_fold(amount_micros, |total, reservation| {
                total.checked_add(
                    reservation
                        .committed_micros
                        .unwrap_or(reservation.amount_micros),
                )
            })
            .ok_or_else(|| {
                WorkflowError::new(
                    WorkflowErrorCode::BudgetExceeded,
                    false,
                    "work item budget overflow",
                )
            })?;
        if work_total > item.spec.budget_micros {
            return create_budget_blocker(
                tx,
                actor,
                operation_id,
                operation_digest,
                &mut project,
                Some(work_item_id.clone()),
                "work_item_ceiling_exhausted",
                "requested cost exceeds the immutable work-item ceiling".into(),
                now_ms,
            );
        }
    }
    project.reserved_cost_micros += amount_micros;
    project.version += 1;
    project.updated_at_ms = now_ms;
    let reservation = CostReservation {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        id: CostReservationId::new("cost"),
        project_id: project.id.clone(),
        work_item_id,
        provider,
        amount_micros,
        committed_micros: None,
        created_by: actor.actor_id.clone(),
        created_at_ms: now_ms,
    };
    tx.put_entity(ENTITY_COST_RESERVATION, &reservation.id.0, 1, &reservation)?;
    tx.put_entity(ENTITY_PROJECT, &project.id.0, project.version, &project)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::CostReserved,
        "project",
        &project.id.0,
        actor,
        operation_id,
        operation_digest,
        None,
        None,
        "project and provider ceilings admitted the billable action",
        &reservation,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::CostReservation(reservation))
}

#[allow(clippy::too_many_arguments)]
fn commit_cost(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    reservation_id: CostReservationId,
    actual_micros: u64,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    if !actor.role.is_internal() || actor.role == ActorRole::Gaia {
        return unauthorized("actor may not commit billable project cost");
    }
    let mut reservation: CostReservation = required_entity(
        tx,
        ENTITY_COST_RESERVATION,
        &reservation_id.0,
        "cost reservation",
    )?;
    if reservation.committed_micros.is_some() {
        return invalid_transition("cost reservation is already committed");
    }
    if actual_micros > reservation.amount_micros {
        let mut project: Project =
            required_entity(tx, ENTITY_PROJECT, &reservation.project_id.0, "project")?;
        authorize_project_actor(actor, &project)?;
        return create_budget_blocker(
            tx,
            actor,
            operation_id,
            operation_digest,
            &mut project,
            reservation.work_item_id.clone(),
            "reservation_ceiling_exhausted",
            "reported actual cost exceeds the admitted reservation".into(),
            now_ms,
        );
    }
    let mut project: Project =
        required_entity(tx, ENTITY_PROJECT, &reservation.project_id.0, "project")?;
    authorize_project_actor(actor, &project)?;
    project.reserved_cost_micros -= reservation.amount_micros;
    project.committed_cost_micros = project
        .committed_cost_micros
        .checked_add(actual_micros)
        .ok_or_else(|| {
            WorkflowError::new(
                WorkflowErrorCode::BudgetExceeded,
                false,
                "project cost overflow",
            )
        })?;
    if project.committed_cost_micros > project.cost_ceiling_micros {
        return budget_exceeded("actual cost exceeds the project ceiling");
    }
    project.version += 1;
    project.updated_at_ms = now_ms;
    reservation.committed_micros = Some(actual_micros);
    tx.put_entity(ENTITY_COST_RESERVATION, &reservation.id.0, 2, &reservation)?;
    tx.put_entity(ENTITY_PROJECT, &project.id.0, project.version, &project)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::CostCommitted,
        "project",
        &project.id.0,
        actor,
        operation_id,
        operation_digest,
        None,
        None,
        "provider cost was committed against its prior reservation",
        &reservation,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::CostReservation(reservation))
}

#[allow(clippy::too_many_arguments)]
fn create_budget_blocker(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    project: &mut Project,
    work_item_id: Option<WorkItemId>,
    cause_ref: &str,
    impact: String,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    project.state = ProjectState::Blocked;
    project.version += 1;
    project.updated_at_ms = now_ms;
    let blocker = Blocker {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        id: BlockerId::new("blocker"),
        project_id: project.id.clone(),
        work_item_id,
        cause_ref: cause_ref.to_owned(),
        impact,
        owner: project.owner,
        required_resolution_role: ActorRole::ProjectManager,
        escalation_target: None,
        state: BlockerState::Open,
        resolution_ref: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    };
    tx.put_entity(ENTITY_PROJECT, &project.id.0, project.version, project)?;
    tx.put_entity(ENTITY_BLOCKER, &blocker.id.0, 1, &blocker)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::BudgetExhausted,
        "blocker",
        &blocker.id.0,
        actor,
        operation_id,
        operation_digest,
        None,
        Some("open"),
        "cost admission failed closed and created an operator-resolvable blocker",
        &blocker,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::Blocker(blocker))
}

#[allow(clippy::too_many_arguments)]
fn create_project_room(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    project_id: ProjectId,
    kind: ProjectRoomKind,
    team_ref: Option<String>,
    mut members: Vec<AgentId>,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    require_role(
        actor,
        &[ActorRole::ProjectManager, ActorRole::TechnicalLead],
    )?;
    let project: Project = required_entity(tx, ENTITY_PROJECT, &project_id.0, "project")?;
    authorize_project_actor(actor, &project)?;
    if matches!(kind, ProjectRoomKind::Team) {
        validate_text(team_ref.as_deref().unwrap_or_default(), "team reference")?;
    } else if team_ref.is_some() {
        return invalid_input("project room cannot carry a team reference");
    }
    members.sort_by_key(|agent_id| agent_id.0);
    members.dedup();
    if members.is_empty()
        || members
            .iter()
            .any(|member| !project.participants.contains(member))
    {
        return unauthorized("room members must be project participants");
    }
    let room = ProjectRoom {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        id: ProjectRoomId::new("room"),
        project_id: project.id.clone(),
        kind,
        team_ref,
        members,
        created_by: actor.actor_id.clone(),
        created_at_ms: now_ms,
    };
    tx.put_entity(ENTITY_PROJECT_ROOM, &room.id.0, 1, &room)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::ProjectRoomCreated,
        "project_room",
        &room.id.0,
        actor,
        operation_id,
        operation_digest,
        None,
        Some("active"),
        "authorized project collaboration room was registered",
        &room,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::ProjectRoom(room))
}

#[allow(clippy::too_many_arguments)]
fn record_action_item(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    project_id: ProjectId,
    work_item_id: Option<WorkItemId>,
    owner: AgentId,
    action_ref: String,
    due_at_ms: Option<u64>,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    require_role(
        actor,
        &[ActorRole::ProjectManager, ActorRole::TechnicalLead],
    )?;
    validate_text(&action_ref, "action reference")?;
    if due_at_ms.is_some_and(|due| due <= now_ms) {
        return invalid_input("action-item due time must be in the future");
    }
    let project: Project = required_entity(tx, ENTITY_PROJECT, &project_id.0, "project")?;
    authorize_project_actor(actor, &project)?;
    if !project.participants.contains(&owner) {
        return unauthorized("action-item owner is not a project participant");
    }
    validate_optional_work_item(tx, &project.id, work_item_id.as_ref())?;
    let action = ActionItem {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        id: ActionItemId::new("action"),
        project_id: project.id.clone(),
        work_item_id,
        owner,
        action_ref,
        due_at_ms,
        state: ActionItemState::Open,
        resolution_ref: None,
        created_by: actor.actor_id.clone(),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    };
    tx.put_entity(ENTITY_ACTION_ITEM, &action.id.0, 1, &action)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::ActionItemRecorded,
        "action_item",
        &action.id.0,
        actor,
        operation_id,
        operation_digest,
        None,
        Some("open"),
        "structured action item was recorded outside free-form chat",
        &action,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::ActionItem(action))
}

#[allow(clippy::too_many_arguments)]
fn resolve_action_item(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    action_item_id: ActionItemId,
    completed: bool,
    resolution_ref: String,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    validate_text(&resolution_ref, "action-item resolution reference")?;
    let mut action: ActionItem =
        required_entity(tx, ENTITY_ACTION_ITEM, &action_item_id.0, "action item")?;
    let project: Project = required_entity(tx, ENTITY_PROJECT, &action.project_id.0, "project")?;
    authorize_project_actor(actor, &project)?;
    if action.state != ActionItemState::Open {
        return invalid_transition("action item is already resolved");
    }
    if actor.agent_id != Some(action.owner)
        && !matches!(
            actor.role,
            ActorRole::ProjectManager | ActorRole::TechnicalLead
        )
    {
        return unauthorized("only the owner or project leadership may resolve an action item");
    }
    action.state = if completed {
        ActionItemState::Completed
    } else {
        ActionItemState::Cancelled
    };
    action.resolution_ref = Some(resolution_ref);
    action.updated_at_ms = now_ms;
    tx.put_entity(ENTITY_ACTION_ITEM, &action.id.0, 2, &action)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::ActionItemResolved,
        "action_item",
        &action.id.0,
        actor,
        operation_id,
        operation_digest,
        Some("open"),
        Some(if completed { "completed" } else { "cancelled" }),
        "authorized actor resolved the structured action item",
        &action,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::ActionItem(action))
}

#[allow(clippy::too_many_arguments)]
fn record_question(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    project_id: ProjectId,
    work_item_id: Option<WorkItemId>,
    owner: AgentId,
    question_ref: String,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    validate_text(&question_ref, "question reference")?;
    let project: Project = required_entity(tx, ENTITY_PROJECT, &project_id.0, "project")?;
    authorize_project_actor(actor, &project)?;
    if !project.participants.contains(&owner) {
        return unauthorized("question owner is not a project participant");
    }
    validate_optional_work_item(tx, &project.id, work_item_id.as_ref())?;
    let question = ProjectQuestion {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        id: QuestionId::new("question"),
        project_id: project.id.clone(),
        work_item_id,
        question_ref,
        owner,
        state: ProjectQuestionState::Open,
        resolution_ref: None,
        created_by: actor.actor_id.clone(),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    };
    tx.put_entity(ENTITY_QUESTION, &question.id.0, 1, &question)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::QuestionRecorded,
        "project_question",
        &question.id.0,
        actor,
        operation_id,
        operation_digest,
        None,
        Some("open"),
        "structured unresolved question was recorded",
        &question,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::ProjectQuestion(question))
}

#[allow(clippy::too_many_arguments)]
fn resolve_question(
    tx: &WorkflowTransaction<'_>,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    question_id: QuestionId,
    resolution_ref: String,
    now_ms: u64,
) -> Result<WorkflowResponse, WorkflowError> {
    validate_text(&resolution_ref, "question resolution reference")?;
    let mut question: ProjectQuestion =
        required_entity(tx, ENTITY_QUESTION, &question_id.0, "project question")?;
    let project: Project = required_entity(tx, ENTITY_PROJECT, &question.project_id.0, "project")?;
    authorize_project_actor(actor, &project)?;
    if question.state != ProjectQuestionState::Open {
        return invalid_transition("project question is already resolved");
    }
    if actor.agent_id != Some(question.owner)
        && !matches!(
            actor.role,
            ActorRole::ProjectManager | ActorRole::TechnicalLead
        )
    {
        return unauthorized("only the owner or project leadership may resolve a question");
    }
    question.state = ProjectQuestionState::Resolved;
    question.resolution_ref = Some(resolution_ref);
    question.updated_at_ms = now_ms;
    tx.put_entity(ENTITY_QUESTION, &question.id.0, 2, &question)?;
    let sequence = append_event(
        tx,
        WorkflowEventType::QuestionResolved,
        "project_question",
        &question.id.0,
        actor,
        operation_id,
        operation_digest,
        Some("open"),
        Some("resolved"),
        "authorized actor supplied a durable question resolution",
        &question,
        now_ms,
    )?;
    refresh_project_projection(tx, &project.id, sequence, now_ms)?;
    Ok(WorkflowResponse::ProjectQuestion(question))
}

fn validate_optional_work_item(
    tx: &WorkflowTransaction<'_>,
    project_id: &ProjectId,
    work_item_id: Option<&WorkItemId>,
) -> Result<(), WorkflowError> {
    if let Some(work_item_id) = work_item_id {
        let item: WorkItem = required_entity(tx, ENTITY_WORK_ITEM, &work_item_id.0, "work item")?;
        if item.project_id != *project_id {
            return unauthorized("work item belongs to another project");
        }
    }
    Ok(())
}

fn validate_actor(actor: &AuthenticatedActor) -> Result<(), WorkflowError> {
    validate_text(&actor.actor_id, "actor id")?;
    match actor.role {
        ActorRole::Customer
            if actor
                .customer_id
                .as_deref()
                .is_some_and(|id| !id.is_empty()) =>
        {
            Ok(())
        }
        ActorRole::Customer => invalid_input("customer actor requires a customer identity"),
        _ if actor.agent_id.is_some() => Ok(()),
        _ => invalid_input("internal actor requires an AgentId"),
    }
}

fn require_role(actor: &AuthenticatedActor, allowed: &[ActorRole]) -> Result<(), WorkflowError> {
    if allowed.contains(&actor.role) {
        Ok(())
    } else {
        unauthorized("actor role is not authorized for this command")
    }
}

fn authorize_customer_request(
    actor: &AuthenticatedActor,
    request: &CustomerRequest,
) -> Result<(), WorkflowError> {
    if actor.role == ActorRole::Customer
        && actor.customer_id.as_deref() != Some(request.customer_id.as_str())
    {
        unauthorized("customer cannot access another customer's request")
    } else {
        Ok(())
    }
}

fn authorize_project_actor(
    actor: &AuthenticatedActor,
    project: &Project,
) -> Result<(), WorkflowError> {
    match actor.agent_id {
        Some(agent_id) if project.participants.contains(&agent_id) => Ok(()),
        _ => unauthorized("actor is not an authorized project participant"),
    }
}

fn validate_work_graph(items: &[WorkItemSpec]) -> Result<(), WorkflowError> {
    if items.is_empty() {
        return Err(WorkflowError::new(
            WorkflowErrorCode::DagInvalid,
            false,
            "work graph must contain at least one item",
        ));
    }
    let ids: HashSet<_> = items.iter().map(|item| item.id.clone()).collect();
    if ids.len() != items.len() {
        return dag_invalid("work graph contains duplicate work-item ids");
    }
    for item in items {
        validate_text(&item.title, "work item title")?;
        validate_text(&item.objective, "work item objective")?;
        validate_text(&item.quality_gate, "work item quality gate")?;
        if item.required_output_kinds.is_empty()
            || item.required_capabilities.is_empty()
            || item.input_refs.is_empty()
            || item.budget_micros == 0
            || item.dependency_ids.contains(&item.id)
            || item
                .dependency_ids
                .iter()
                .any(|dependency| !ids.contains(dependency))
        {
            return dag_invalid("work graph contains an invalid dependency or output contract");
        }
        for input in &item.input_refs {
            validate_text(input, "work item input reference")?;
        }
        for output in &item.required_output_kinds {
            validate_text(output, "work item output kind")?;
        }
    }
    let dependencies: HashMap<_, _> = items
        .iter()
        .map(|item| (item.id.clone(), item.dependency_ids.clone()))
        .collect();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for id in &ids {
        visit_dag(id, &dependencies, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit_dag(
    id: &WorkItemId,
    dependencies: &HashMap<WorkItemId, BTreeSet<WorkItemId>>,
    visiting: &mut HashSet<WorkItemId>,
    visited: &mut HashSet<WorkItemId>,
) -> Result<(), WorkflowError> {
    if visited.contains(id) {
        return Ok(());
    }
    if !visiting.insert(id.clone()) {
        return dag_invalid("work graph contains a dependency cycle");
    }
    if let Some(edges) = dependencies.get(id) {
        for dependency in edges {
            visit_dag(dependency, dependencies, visiting, visited)?;
        }
    }
    visiting.remove(id);
    visited.insert(id.clone());
    Ok(())
}

fn validate_proposal(binding: &ProposalBinding, now_ms: u64) -> Result<(), WorkflowError> {
    validate_text(&binding.scope, "proposal scope")?;
    if binding.deliverables.is_empty()
        || binding.acceptance_criteria.is_empty()
        || binding.cost_ceiling_micros == 0
        || binding.provider_cost_ceilings_micros.is_empty()
        || binding.expires_at_ms <= now_ms
    {
        return invalid_input("proposal binding is incomplete or already expired");
    }
    for (provider, ceiling) in &binding.provider_cost_ceilings_micros {
        validate_text(provider, "proposal provider")?;
        if *ceiling == 0 || *ceiling > binding.cost_ceiling_micros {
            return invalid_input("provider ceiling must be positive and within project budget");
        }
    }
    Ok(())
}

fn unlock_dependents(
    tx: &WorkflowTransaction<'_>,
    project_id: &ProjectId,
    completed_id: &WorkItemId,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    for mut candidate in tx.entities::<WorkItem>(ENTITY_WORK_ITEM)? {
        if candidate.project_id == *project_id
            && candidate.state == WorkItemState::Proposed
            && candidate.spec.dependency_ids.contains(completed_id)
            && dependencies_done(tx, &candidate)?
        {
            candidate.state = WorkItemState::Ready;
            candidate.version += 1;
            candidate.updated_at_ms = now_ms;
            tx.put_entity(
                ENTITY_WORK_ITEM,
                &candidate.spec.id.0,
                candidate.version,
                &candidate,
            )?;
        }
    }
    Ok(())
}

fn dependencies_done(tx: &WorkflowTransaction<'_>, item: &WorkItem) -> Result<bool, WorkflowError> {
    for dependency_id in &item.spec.dependency_ids {
        let dependency: WorkItem =
            required_entity(tx, ENTITY_WORK_ITEM, &dependency_id.0, "dependency")?;
        if dependency.project_id != item.project_id || dependency.state != WorkItemState::Done {
            return Ok(false);
        }
    }
    Ok(true)
}

fn refresh_project_projection(
    tx: &WorkflowTransaction<'_>,
    project_id: &ProjectId,
    last_event_sequence: i64,
    now_ms: u64,
) -> Result<(), WorkflowError> {
    let project: Project = required_entity(tx, ENTITY_PROJECT, &project_id.0, "project")?;
    let agreement: Agreement =
        required_entity(tx, ENTITY_AGREEMENT, &project.agreement_id.0, "agreement")?;
    let mut work_items: Vec<WorkItem> = tx
        .entities::<WorkItem>(ENTITY_WORK_ITEM)?
        .into_iter()
        .filter(|item| item.project_id == *project_id)
        .collect();
    work_items.sort_by(|left, right| left.spec.id.cmp(&right.spec.id));
    let mut work_items_by_state = BTreeMap::new();
    for item in &work_items {
        *work_items_by_state
            .entry(work_state_name(item.state).to_string())
            .or_insert(0) += 1;
    }
    let work_item_ids: HashSet<WorkItemId> =
        work_items.iter().map(|item| item.spec.id.clone()).collect();
    let mut assignments: Vec<Assignment> = tx
        .entities::<Assignment>(ENTITY_ASSIGNMENT)?
        .into_iter()
        .filter(|assignment| assignment.project_id == *project_id)
        .collect();
    assignments.sort_by(|left, right| left.id.cmp(&right.id));
    let mut completion_evidence: Vec<CompletionEvidence> = tx
        .entities::<CompletionEvidence>(ENTITY_EVIDENCE)?
        .into_iter()
        .filter(|evidence| work_item_ids.contains(&evidence.work_item_id))
        .collect();
    completion_evidence.sort_by(|left, right| left.id.cmp(&right.id));
    let mut blockers: Vec<Blocker> = tx
        .entities::<Blocker>(ENTITY_BLOCKER)?
        .into_iter()
        .filter(|blocker| blocker.project_id == *project_id)
        .collect();
    blockers.sort_by(|left, right| left.id.cmp(&right.id));
    let open_blockers = blockers
        .iter()
        .filter(|blocker| blocker.state != BlockerState::Resolved)
        .count() as u32;
    let mut decisions: Vec<ProjectDecision> = tx
        .entities::<ProjectDecision>(ENTITY_DECISION)?
        .into_iter()
        .filter(|decision| decision.project_id == *project_id)
        .collect();
    decisions.sort_by(|left, right| left.id.cmp(&right.id));
    let mut handoffs: Vec<Handoff> = tx
        .entities::<Handoff>(ENTITY_HANDOFF)?
        .into_iter()
        .filter(|handoff| handoff.project_id == *project_id)
        .collect();
    handoffs.sort_by(|left, right| left.id.cmp(&right.id));
    let mut approvals: Vec<Approval> = tx
        .entities::<Approval>(ENTITY_APPROVAL)?
        .into_iter()
        .filter(|approval| approval.project_id == *project_id)
        .collect();
    approvals.sort_by(|left, right| left.id.cmp(&right.id));
    let mut rooms: Vec<ProjectRoom> = tx
        .entities::<ProjectRoom>(ENTITY_PROJECT_ROOM)?
        .into_iter()
        .filter(|room| room.project_id == *project_id)
        .collect();
    rooms.sort_by(|left, right| left.id.cmp(&right.id));
    let mut action_items: Vec<ActionItem> = tx
        .entities::<ActionItem>(ENTITY_ACTION_ITEM)?
        .into_iter()
        .filter(|item| item.project_id == *project_id)
        .collect();
    action_items.sort_by(|left, right| left.id.cmp(&right.id));
    let open_action_items = action_items
        .iter()
        .filter(|item| item.state == ActionItemState::Open)
        .count() as u32;
    let mut unresolved_questions: Vec<ProjectQuestion> = tx
        .entities::<ProjectQuestion>(ENTITY_QUESTION)?
        .into_iter()
        .filter(|question| {
            question.project_id == *project_id && question.state == ProjectQuestionState::Open
        })
        .collect();
    unresolved_questions.sort_by(|left, right| left.id.cmp(&right.id));
    let open_questions = unresolved_questions.len() as u32;
    tx.put_projection(&ProjectProjection {
        schema_version: WORKFLOW_SCHEMA_VERSION,
        project_id: project.id,
        agreement,
        state: project.state,
        work_items_total: work_items.len() as u32,
        work_items_by_state,
        participants: project.participants.clone(),
        work_items,
        assignments,
        completion_evidence,
        decisions,
        handoffs,
        blockers,
        approvals,
        rooms,
        action_items,
        unresolved_questions,
        open_blockers,
        open_action_items,
        open_questions,
        cost_ceiling_micros: project.cost_ceiling_micros,
        reserved_cost_micros: project.reserved_cost_micros,
        committed_cost_micros: project.committed_cost_micros,
        last_event_sequence,
        updated_at_ms: now_ms,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_event<T: Serialize>(
    tx: &WorkflowTransaction<'_>,
    event_type: WorkflowEventType,
    aggregate_type: &str,
    aggregate_id: &str,
    actor: &AuthenticatedActor,
    operation_id: &str,
    operation_digest: &str,
    before_state: Option<&str>,
    after_state: Option<&str>,
    reason: &str,
    payload: &T,
    now_ms: u64,
) -> Result<i64, WorkflowError> {
    let mut event = workflow_event(
        event_type,
        aggregate_type,
        aggregate_id,
        &actor.actor_id,
        actor.role,
        operation_id,
        operation_digest,
        before_state,
        after_state,
        reason,
        payload,
        now_ms,
    )?;
    tx.append_event(&mut event)
}

#[allow(clippy::too_many_arguments)]
fn workflow_event<T: Serialize>(
    event_type: WorkflowEventType,
    aggregate_type: &str,
    aggregate_id: &str,
    actor_id: &str,
    actor_role: ActorRole,
    operation_id: &str,
    operation_digest: &str,
    before_state: Option<&str>,
    after_state: Option<&str>,
    reason: &str,
    payload: &T,
    now_ms: u64,
) -> Result<WorkflowEvent, WorkflowError> {
    Ok(WorkflowEvent {
        sequence: 0,
        schema_version: WORKFLOW_SCHEMA_VERSION,
        event_id: uuid::Uuid::now_v7().to_string(),
        event_type,
        aggregate_type: aggregate_type.to_string(),
        aggregate_id: aggregate_id.to_string(),
        actor_id: actor_id.to_string(),
        actor_role,
        operation_id: operation_id.to_string(),
        operation_digest: operation_digest.to_string(),
        before_state: before_state.map(str::to_string),
        after_state: after_state.map(str::to_string),
        reason: reason.to_string(),
        payload: serde_json::to_value(payload)?,
        timestamp_ms: now_ms,
    })
}

fn required_entity<T: serde::de::DeserializeOwned>(
    tx: &WorkflowTransaction<'_>,
    entity_type: &str,
    entity_id: &str,
    label: &str,
) -> Result<T, WorkflowError> {
    tx.entity(entity_type, entity_id)?.ok_or_else(|| {
        WorkflowError::new(
            WorkflowErrorCode::NotFound,
            false,
            format!("{label} not found"),
        )
    })
}

fn require_version(actual: u64, expected: u64) -> Result<(), WorkflowError> {
    if actual == expected {
        Ok(())
    } else {
        Err(WorkflowError::new(
            WorkflowErrorCode::VersionConflict,
            false,
            "aggregate version does not match expected version",
        ))
    }
}

fn canonical_digest<T: Serialize>(value: &T) -> Result<String, WorkflowError> {
    use std::fmt::Write as _;

    let bytes = serde_json::to_vec(value)?;
    let hash = Sha256::digest(bytes);
    let mut digest = String::with_capacity(64);
    for byte in hash {
        write!(&mut digest, "{byte:02x}").map_err(|_| WorkflowError::persistence())?;
    }
    Ok(digest)
}

fn validate_digest(value: &str) -> Result<(), WorkflowError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        invalid_input("digest must be a 64-character hexadecimal SHA-256 value")
    }
}

fn validate_text(value: &str, label: &str) -> Result<(), WorkflowError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 4_096 {
        invalid_input(&format!("{label} is empty or exceeds its bound"))
    } else {
        Ok(())
    }
}

fn state_name(state: CustomerRequestState) -> &'static str {
    match state {
        CustomerRequestState::Submitted => "submitted",
        CustomerRequestState::Clarifying => "clarifying",
        CustomerRequestState::Qualified => "qualified",
        CustomerRequestState::Proposed => "proposed",
        CustomerRequestState::Accepted => "accepted",
        CustomerRequestState::Rejected => "rejected",
        CustomerRequestState::Expired => "expired",
        CustomerRequestState::Cancelled => "cancelled",
    }
}

fn work_state_name(state: WorkItemState) -> &'static str {
    match state {
        WorkItemState::Proposed => "proposed",
        WorkItemState::Ready => "ready",
        WorkItemState::Assigned => "assigned",
        WorkItemState::Claimed => "claimed",
        WorkItemState::InProgress => "in_progress",
        WorkItemState::InReview => "in_review",
        WorkItemState::Done => "done",
        WorkItemState::Blocked => "blocked",
        WorkItemState::Cancelled => "cancelled",
    }
}

fn invalid_input<T>(message: &str) -> Result<T, WorkflowError> {
    Err(WorkflowError::new(
        WorkflowErrorCode::InvalidInput,
        false,
        message,
    ))
}

fn invalid_transition<T>(message: &str) -> Result<T, WorkflowError> {
    Err(WorkflowError::new(
        WorkflowErrorCode::InvalidTransition,
        false,
        message,
    ))
}

fn unauthorized<T>(message: &str) -> Result<T, WorkflowError> {
    Err(WorkflowError::new(
        WorkflowErrorCode::Unauthorized,
        false,
        message,
    ))
}

fn budget_exceeded<T>(message: &str) -> Result<T, WorkflowError> {
    Err(WorkflowError::new(
        WorkflowErrorCode::BudgetExceeded,
        false,
        message,
    ))
}

fn dag_invalid<T>(message: &str) -> Result<T, WorkflowError> {
    Err(WorkflowError::new(
        WorkflowErrorCode::DagInvalid,
        false,
        message,
    ))
}
