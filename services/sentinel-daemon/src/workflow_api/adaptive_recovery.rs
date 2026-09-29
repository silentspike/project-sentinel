//! A durable leadership decision resolves a blocked journal, never a provider effect.
use super::*;
use sentinel_common::{
    AppendProposalV2, AuthorityKindV1, AuthorityRefV1, CausalContextV1, CausationPolicyV1,
    EventContractError, EventDurability, EventPayloadCodec, EventSchemaDefinition,
    EventSchemaRegistry, ExpectedStreamRevision,
};

const EVENT_TYPE: &str = "adaptive_work_blocked_resolution_authorized";
const PRODUCER: &str = "sentinel-daemon-adaptive-recovery";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveBlockedAdaptiveWorkV1 {
    schema_version: u16,
    operation_id: Uuid,
    project_id: ProjectId,
    work_item_id: WorkItemId,
    session_id: Uuid,
    expected_session_version: u64,
    expected_reason_code: String,
    reason_ref: String,
}

impl ResolveBlockedAdaptiveWorkV1 {
    fn validate(&self) -> Result<(), WorkflowHttpResponse> {
        if self.schema_version != 1
            || self.operation_id.is_nil()
            || self.session_id.is_nil()
            || self.expected_session_version == 0
            || self.expected_session_version.checked_add(1).is_none()
            || self.project_id.validate().is_err()
            || self.work_item_id.validate().is_err()
            || self.expected_reason_code.is_empty()
            || self.expected_reason_code.len() > 64
            || !self
                .expected_reason_code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || self.reason_ref.trim().is_empty()
            || self.reason_ref.len() > 4096
            || self.reason_ref.chars().any(char::is_control)
        {
            return Err(json_error(
                400,
                "invalid_input",
                "invalid adaptive resolution",
                false,
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockedResolutionDecisionV1 {
    schema_version: u16,
    request: ResolveBlockedAdaptiveWorkV1,
    leadership_principal: AuthenticatedCompanyPrincipalV1,
    leadership_authority: PrincipalAuthorityV1,
    assignment_id: String,
    assignee_authority: RuntimeAuthoritySnapshotV1,
}

fn recovery_conflict(message: &'static str) -> WorkflowHttpResponse {
    json_error(409, "adaptive_recovery_conflict", message, false)
}

fn recovery_unavailable() -> WorkflowHttpResponse {
    json_error(
        503,
        "adaptive_recovery_unavailable",
        "adaptive recovery authority unavailable",
        true,
    )
}

impl WorkflowApi {
    pub(super) fn resolve_blocked_adaptive_work(
        &self,
        principal: &BoundPrincipal,
        body: &[u8],
    ) -> WorkflowHttpResponse {
        match self.resolve_blocked_adaptive_work_inner(principal, body) {
            Ok(response) | Err(response) => response,
        }
    }

    fn resolve_blocked_adaptive_work_inner(
        &self,
        principal: &BoundPrincipal,
        body: &[u8],
    ) -> Result<WorkflowHttpResponse, WorkflowHttpResponse> {
        if principal.principal.kind != CompanyPrincipalKindV1::Agent
            || !matches!(
                principal.principal.role,
                CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
            )
            || principal.principal.validate().is_err()
            || !self
                .principals
                .principal(&principal.principal.principal_id)
                .is_some_and(|bound| {
                    bound.principal == principal.principal
                        && bound.execution_authority == principal.execution_authority
                })
        {
            return Err(json_error(
                403,
                "authority_conflict",
                "authenticated project leadership required",
                false,
            ));
        }
        let request: ResolveBlockedAdaptiveWorkV1 = decode_body(body)?;
        request.validate()?;
        let _guard = self
            .mutation_fence
            .write()
            .map_err(|_| recovery_unavailable())?;
        let project = self
            .store
            .company_project(&principal.principal.tenant_id, &request.project_id)
            .map_err(workflow_error)?
            .ok_or_else(|| recovery_conflict("current tenant project unavailable"))?;
        if project.tenant_id != principal.principal.tenant_id
            || project.project_id != request.project_id
            || governed_project_participant(&project, &principal.principal).is_none()
        {
            return Err(json_error(
                403,
                "authority_conflict",
                "current project leadership binding required",
                false,
            ));
        }
        let work = project
            .work_items
            .get(&request.work_item_id)
            .ok_or_else(|| recovery_conflict("current project work item unavailable"))?;
        let mut assignments = work
            .assignments
            .iter()
            .filter(|assignment| assignment.active);
        let assignment = assignments
            .next()
            .ok_or_else(|| recovery_conflict("current work assignment unavailable"))?;
        if assignments.next().is_some() {
            return Err(recovery_conflict("ambiguous current work assignment"));
        }
        if assignment.agent_id != work.spec.owner {
            return Err(recovery_conflict("current assignment owner changed"));
        }
        let current = self
            .authority
            .as_ref()
            .ok_or_else(recovery_unavailable)?
            .snapshot(
                &project.tenant_id,
                &project.project_id,
                &request.work_item_id,
                assignment.agent_id,
            )
            .map_err(|_| recovery_conflict("current assignee runtime authority unavailable"))?;
        let session = self
            .store
            .adaptive_session_for_authority(&current)
            .map_err(workflow_error)?
            .ok_or_else(|| recovery_conflict("current adaptive session unavailable"))?;
        if session.grant.session_id != request.session_id {
            return Err(recovery_conflict(
                "adaptive session is not the current exact head",
            ));
        }
        // A separate productive execution cannot be reset by a journal decision.
        if self
            .store
            .work_item(
                &current.tenant_id,
                &current.project_id,
                &current.work_item_id,
            )
            .map_err(workflow_error)?
            .is_some_and(|execution| {
                !matches!(
                    execution.state,
                    sentinel_workflow::WorkItemState::Done
                        | sentinel_workflow::WorkItemState::Cancelled
                )
            })
        {
            return Err(recovery_conflict(
                "productive execution requires its own reconciliation",
            ));
        }
        let decision = BlockedResolutionDecisionV1 {
            schema_version: 1,
            request: request.clone(),
            leadership_principal: principal.principal.clone(),
            leadership_authority: principal.execution_authority.clone(),
            assignment_id: assignment.assignment_id.clone(),
            assignee_authority: current.clone(),
        };
        let registry = resolution_registry().map_err(|_| recovery_unavailable())?;
        let events = self.event_store.as_ref().ok_or_else(recovery_unavailable)?;
        let event_id = resolution_event_id(request.operation_id);
        let prior = events
            .event_v2_by_id(&event_id.to_string())
            .map_err(|_| recovery_unavailable())?;
        let proposal = if let Some(event) = prior.as_ref() {
            let recorded: BlockedResolutionDecisionV1 = serde_json::from_slice(&event.payload)
                .map_err(|_| recovery_conflict("recorded leadership decision is invalid"))?;
            validate_decision(&event.payload)
                .map_err(|_| recovery_conflict("recorded leadership decision is invalid"))?;
            if !same_replay_decision(&recorded, &decision) {
                return Err(recovery_conflict(
                    "operation is bound to another leadership decision",
                ));
            }
            // Current credentials authorize this retry; the sealed decision
            // retains the credentials that authorized its original creation.
            let historical = resolution_proposal(&recorded)?;
            if event.event_id != event_id.to_string()
                || event.event_type != EVENT_TYPE
                || event.producer != PRODUCER
                || event.schema_version != historical.schema_version
                || event.payload_codec != historical.payload_codec
                || event.payload != historical.payload
                || event.payload_digest != historical.payload_digest
                || event.causal_context != historical.causal_context
                || event.durability != historical.requested_durability
                || event.canonical_request_digest
                    != historical
                        .canonical_request_digest()
                        .map_err(|_| recovery_unavailable())?
            {
                return Err(recovery_conflict(
                    "recorded leadership event binding changed",
                ));
            }
            historical
        } else {
            resolution_proposal(&decision)?
        };
        // Only an exact committed decision may reach the store's atomic replay path.
        // An event without the transition must still match the blocked precondition.
        if prior.is_none() || session.version == request.expected_session_version {
            require_blocked(&session, &request)?;
        } else if request.expected_session_version.checked_add(1) != Some(session.version)
            || !matches!(&session.cursor, AdaptiveCursorV1::BlockedResolved { .. })
        {
            return Err(recovery_conflict(
                "adaptive resolution replay state changed",
            ));
        }
        let caller = sentinel_limbo::AuthenticatedEventCallerV1 {
            service_id: "sentinel-daemon-workflow".to_owned(),
            producer: PRODUCER.to_owned(),
            authority_scope_digest: proposal
                .causal_context
                .authority_scope_digest()
                .map_err(|_| recovery_unavailable())?,
        };
        let event = events
            .append_gateway(&registry)
            .append(&caller, &proposal)
            .map_err(|error| match error {
                sentinel_limbo::EventAppendError::OperationConflict
                | sentinel_limbo::EventAppendError::WrongExpectedRevision { .. } => {
                    recovery_conflict("adaptive resolution decision already claimed")
                }
                _ => recovery_unavailable(),
            })?
            .envelope;
        if event.event_id != event_id.to_string() {
            return Err(recovery_unavailable());
        }
        let (replay, resolved) = self
            .store
            .advance_adaptive_session(
                request.session_id,
                request.expected_session_version,
                stable_operation_id(
                    "sentinel.workflow.resolve-blocked-transition.v1",
                    &format!("{}:{}", request.session_id, request.operation_id),
                    request.expected_session_version,
                ),
                &AdaptiveTransitionV1::ResolveBlocked {
                    expected_reason_code: request.expected_reason_code.clone(),
                    resolution_event_id: event_id.to_string(),
                },
                &current,
                now_unix_ms().max(session.updated_at_ms),
            )
            .map_err(workflow_error)?;
        Ok(json(
            200,
            &serde_json::json!({
                "schema_version": 1,
                "operation_id": request.operation_id,
                "session_id": resolved.grant.session_id,
                "session_version": resolved.version,
                "resolution_event_id": event_id,
                "replay": replay,
            }),
        ))
    }
}

fn same_replay_decision(
    recorded: &BlockedResolutionDecisionV1,
    current: &BlockedResolutionDecisionV1,
) -> bool {
    let old = &recorded.leadership_principal;
    let fresh = &current.leadership_principal;
    recorded.schema_version == current.schema_version
        && recorded.request == current.request
        && recorded.assignment_id == current.assignment_id
        && recorded.assignee_authority == current.assignee_authority
        && old.schema_version == fresh.schema_version
        && old.tenant_id == fresh.tenant_id
        && old.principal_id == fresh.principal_id
        && old.kind == fresh.kind
        && old.role == fresh.role
        && old.agent_id == fresh.agent_id
        && old.customer_id == fresh.customer_id
        && fresh.authority_generation >= old.authority_generation
}

fn require_blocked(
    session: &AdaptiveSessionV1,
    request: &ResolveBlockedAdaptiveWorkV1,
) -> Result<(), WorkflowHttpResponse> {
    if session.version != request.expected_session_version {
        return Err(recovery_conflict("adaptive session version changed"));
    }
    match &session.cursor {
        AdaptiveCursorV1::Blocked { reason_code }
            if *reason_code == request.expected_reason_code =>
        {
            Ok(())
        }
        AdaptiveCursorV1::Blocked { .. } => {
            Err(recovery_conflict("adaptive blocked reason changed"))
        }
        _ => Err(recovery_conflict(
            "adaptive session is not blocked or has unresolved effects",
        )),
    }
}

fn resolution_event_id(operation_id: Uuid) -> Uuid {
    let digest = Sha256::digest(
        format!("sentinel.adaptive-blocked-resolution-event.v1:{operation_id}").as_bytes(),
    );
    // V2 requested IDs require UUIDv7. This deterministic namespace uses epoch
    // zero, not a claimed wall-clock time; the store receipt owns append time.
    let mut bytes = [0_u8; 16];
    bytes[6..].copy_from_slice(&digest[..10]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn validate_decision(payload: &[u8]) -> Result<(), EventContractError> {
    let invalid = || EventContractError::InvalidField {
        field: "adaptive_resolution.payload",
        reason: "invalid typed leadership decision",
    };
    let decision: BlockedResolutionDecisionV1 =
        serde_json::from_slice(payload).map_err(|_| invalid())?;
    let leader = &decision.leadership_principal;
    let current = &decision.assignee_authority;
    if decision.schema_version != 1
        || decision.request.validate().is_err()
        || leader.validate().is_err()
        || current.validate().is_err()
        || decision.leadership_authority.validate().is_err()
        || leader.kind != CompanyPrincipalKindV1::Agent
        || !matches!(
            leader.role,
            CompanyRoleV1::ProjectManager | CompanyRoleV1::TechnicalLead
        )
        || decision.assignment_id.is_empty()
        || decision.assignment_id.len() > 128
        || decision
            .assignment_id
            .bytes()
            .any(|byte| byte.is_ascii_control())
        || leader.tenant_id != current.tenant_id
        || decision.request.project_id != current.project_id
        || decision.request.work_item_id != current.work_item_id
        || decision.leadership_authority.principal_id != leader.principal_id
        || decision.leadership_authority.principal_generation != leader.authority_generation
        || decision.leadership_authority.authority_digest != leader.authority_digest
    {
        return Err(invalid());
    }
    Ok(())
}

fn resolution_registry() -> Result<EventSchemaRegistry, EventContractError> {
    EventSchemaRegistry::new([EventSchemaDefinition {
        event_type: EVENT_TYPE.to_owned(),
        schema_version: 1,
        durability: EventDurability::Authoritative,
        payload_codec: EventPayloadCodec::Json,
        causation_policy: CausationPolicyV1::RootRequired,
        allowed_producers: BTreeSet::from([PRODUCER.to_owned()]),
        deterministic_event_id_producers: BTreeSet::from([PRODUCER.to_owned()]),
        validator_id: "adaptive-blocked-resolution-json-v1".to_owned(),
        validate_payload: validate_decision,
        upcast: None,
    }])
}

fn resolution_proposal(
    decision: &BlockedResolutionDecisionV1,
) -> Result<AppendProposalV2, WorkflowHttpResponse> {
    let payload = sentinel_common::canonical_json(decision).map_err(|_| recovery_unavailable())?;
    validate_decision(&payload).map_err(|_| recovery_unavailable())?;
    let digest = sentinel_common::sha256_hex(&payload);
    let authority = &decision.assignee_authority;
    let runtime_digest = authority.canonical_digest().map_err(workflow_error)?;
    let reference = |kind, id, generation, digest| AuthorityRefV1 {
        kind,
        id,
        authority_generation: generation,
        authority_digest: digest,
    };
    Ok(AppendProposalV2 {
        proposal_version: sentinel_common::EVENT_PROPOSAL_VERSION_V2,
        requested_event_id: Some(resolution_event_id(decision.request.operation_id).to_string()),
        event_type: EVENT_TYPE.to_owned(),
        schema_version: 1,
        payload_codec: EventPayloadCodec::Json,
        payload_digest: digest.clone(),
        payload,
        causal_context: CausalContextV1 {
            schema_version: sentinel_common::CAUSAL_CONTEXT_VERSION_V1,
            tenant: reference(
                AuthorityKindV1::Tenant,
                authority.tenant_id.0.clone(),
                1,
                domain_digest(
                    "sentinel.adaptive-recovery.tenant.v1",
                    &[authority.tenant_id.0.as_bytes()],
                ),
            ),
            company: reference(
                AuthorityKindV1::Company,
                "virtual-company".to_owned(),
                authority.organization_generation,
                authority.organization_digest.clone(),
            ),
            project: reference(
                AuthorityKindV1::Project,
                authority.project_id.0.clone(),
                authority.policy_generation,
                authority.policy_digest.clone(),
            ),
            workflow: Some(reference(
                AuthorityKindV1::Workflow,
                format!("adaptive-recovery:{}", decision.request.session_id),
                1,
                runtime_digest,
            )),
            work_item: Some(reference(
                AuthorityKindV1::WorkItem,
                authority.work_item_id.0.clone(),
                authority.assignment_version,
                authority.assignment_digest.clone(),
            )),
            request_id: decision.request.operation_id.to_string(),
            request_digest: digest.clone(),
            correlation_id: decision.request.session_id.to_string(),
            causation_event_id: None,
            operation_id: decision.request.operation_id.to_string(),
            attempt: 1,
            source_generation: decision.leadership_principal.authority_generation,
            source_digest: digest,
            invocation_id: None,
            agent_id: Some(authority.agent_id.to_string()),
            tick: None,
            artifact_id: None,
            artifact_digest: None,
            qa_run_id: None,
            release_id: None,
            delivery_id: None,
            diagnostic_trace_id: None,
            diagnostic_span_id: None,
        },
        producer: PRODUCER.to_owned(),
        owner_term: None,
        tick: None,
        requested_durability: EventDurability::Authoritative,
        // One leadership decision per exact session authority, including the crash gap.
        expected_stream_revision: ExpectedStreamRevision::NoStream,
        delivery_intents: Vec::new(),
        effect_reservations: Vec::new(),
    })
}

#[cfg(all(test, feature = "llm"))]
mod tests {
    use super::*;
    use sentinel_limbo::rusqlite;
    use sentinel_workflow::{adaptive_tool_digest, AdaptiveModelDecisionV1};

    fn advance(
        api: &WorkflowApi,
        session: &AdaptiveSessionV1,
        command: AdaptiveTransitionV1,
    ) -> AdaptiveSessionV1 {
        api.store
            .advance_adaptive_session(
                session.grant.session_id,
                session.version,
                Uuid::new_v4(),
                &command,
                &session.grant.authority,
                now_unix_ms().max(session.updated_at_ms),
            )
            .unwrap()
            .1
    }

    fn claim(
        api: &WorkflowApi,
        session: &AdaptiveSessionV1,
    ) -> (AdaptiveSessionV1, AdaptiveEffectV1) {
        let effect = AdaptiveEffectV1 {
            id: Uuid::new_v4(),
            request_digest: "a".repeat(64),
        };
        let next = advance(
            api,
            session,
            AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: None,
            },
        );
        (next, effect)
    }

    fn fixture(path: &Path, events: &Path, blocked: bool) -> (WorkflowApi, AdaptiveSessionV1) {
        let (api, binding, _) = model_work::configured_adaptive_test_api(path, events);
        let session = api
            .store
            .adaptive_session(binding.grant.session_id, &binding.grant.authority)
            .unwrap()
            .unwrap();
        if !blocked {
            return (api, session);
        }
        let (pending, effect) = claim(&api, &session);
        let session = advance(
            &api,
            &pending,
            AdaptiveTransitionV1::ResolveModel {
                effect,
                result_digest: "b".repeat(64),
                decision: AdaptiveModelDecisionV1::Blocked {
                    reason_code: "dependency_unavailable".to_owned(),
                },
            },
        );
        (api, session)
    }

    fn request(session: &AdaptiveSessionV1) -> ResolveBlockedAdaptiveWorkV1 {
        ResolveBlockedAdaptiveWorkV1 {
            schema_version: 1,
            operation_id: Uuid::new_v4(),
            project_id: session.grant.authority.project_id.clone(),
            work_item_id: session.grant.authority.work_item_id.clone(),
            session_id: session.grant.session_id,
            expected_session_version: session.version,
            expected_reason_code: "dependency_unavailable".to_owned(),
            reason_ref: "decision:dependency-restored".to_owned(),
        }
    }

    fn call(
        api: &WorkflowApi,
        actor: &str,
        request: &ResolveBlockedAdaptiveWorkV1,
    ) -> WorkflowHttpResponse {
        api.resolve_blocked_adaptive_work(
            &api.principals.principal(actor).unwrap(),
            &serde_json::to_vec(request).unwrap(),
        )
    }

    fn read(api: &WorkflowApi, session: &AdaptiveSessionV1) -> AdaptiveSessionV1 {
        api.store
            .adaptive_session(session.grant.session_id, &session.grant.authority)
            .unwrap()
            .unwrap()
    }

    fn assert_no_decision(api: &WorkflowApi, request: &ResolveBlockedAdaptiveWorkV1) {
        assert!(api
            .event_store
            .as_ref()
            .unwrap()
            .event_v2_by_id(&resolution_event_id(request.operation_id).to_string())
            .unwrap()
            .is_none());
    }

    fn decision(
        api: &WorkflowApi,
        request: &ResolveBlockedAdaptiveWorkV1,
    ) -> BlockedResolutionDecisionV1 {
        let leader = api.principals.principal("pm").unwrap();
        let project = api
            .store
            .company_project(&leader.principal.tenant_id, &request.project_id)
            .unwrap()
            .unwrap();
        let assignment = project.work_items[&request.work_item_id]
            .assignments
            .iter()
            .find(|assignment| assignment.active)
            .unwrap();
        BlockedResolutionDecisionV1 {
            schema_version: 1,
            request: request.clone(),
            leadership_principal: leader.principal,
            leadership_authority: leader.execution_authority,
            assignment_id: assignment.assignment_id.clone(),
            assignee_authority: api
                .authority
                .as_ref()
                .unwrap()
                .snapshot(
                    &project.tenant_id,
                    &project.project_id,
                    &request.work_item_id,
                    assignment.agent_id,
                )
                .unwrap(),
        }
    }

    fn append_decision(
        api: &WorkflowApi,
        decision: &BlockedResolutionDecisionV1,
    ) -> sentinel_common::EventEnvelopeV2 {
        let proposal = resolution_proposal(decision).unwrap();
        let caller = sentinel_limbo::AuthenticatedEventCallerV1 {
            service_id: "sentinel-daemon-workflow".to_owned(),
            producer: PRODUCER.to_owned(),
            authority_scope_digest: proposal.causal_context.authority_scope_digest().unwrap(),
        };
        api.event_store
            .as_ref()
            .unwrap()
            .append_gateway(&resolution_registry().unwrap())
            .append(&caller, &proposal)
            .unwrap()
            .envelope
    }

    fn reopen(path: &Path, events: &Path) -> WorkflowApi {
        let mut api = model_work::configured_test_api(path);
        api.event_store = Some(sentinel_limbo::EventStore::open(events.to_str().unwrap()).unwrap());
        api
    }

    #[test]
    fn caller_operation_reuse_cannot_strand_a_blocked_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let (api, blocked) = fixture(&path, &temp.path().join("events.sqlite"), true);
        let prior: String = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row(
                "SELECT operation_id FROM workflow_operations WHERE operation_namespace=?1 LIMIT 1",
                [format!(
                    "adaptive-session-v1:{}:operations",
                    blocked.grant.session_id
                )],
                |row| row.get(0),
            )
            .unwrap();
        let mut request = request(&blocked);
        request.operation_id = Uuid::parse_str(&prior).unwrap();
        assert_eq!(call(&api, "pm", &request).status, 200);
        assert!(matches!(
            read(&api, &blocked).cursor,
            AdaptiveCursorV1::BlockedResolved { .. }
        ));
        assert_eq!(call(&api, "pm", &request).status, 200);
    }

    #[test]
    fn protected_http_route_requires_credentials_leadership_and_post() {
        let temp = tempfile::tempdir().unwrap();
        let (api, blocked) = fixture(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
            true,
        );
        let request = request(&blocked);
        let body = serde_json::to_vec(&request).unwrap();
        assert_eq!(
            api.handle("POST", ADAPTIVE_RECOVERY_PATH, &HashMap::new(), &body)
                .unwrap()
                .status,
            401
        );
        let customer = HashMap::from([(
            "authorization".to_owned(),
            format!("Bearer test-credential-0-{}", "x".repeat(32)),
        )]);
        assert_eq!(
            api.handle("POST", ADAPTIVE_RECOVERY_PATH, &customer, &body)
                .unwrap()
                .status,
            403
        );
        let pm = HashMap::from([(
            "authorization".to_owned(),
            format!("Bearer test-credential-2-{}", "x".repeat(32)),
        )]);
        assert_eq!(
            api.handle("GET", ADAPTIVE_RECOVERY_PATH, &pm, &body)
                .unwrap()
                .status,
            405
        );
        assert_no_decision(&api, &request);
        assert_eq!(
            api.handle("POST", ADAPTIVE_RECOVERY_PATH, &pm, &body)
                .unwrap()
                .status,
            200
        );
        let disabled = WorkflowApi::disabled().unwrap();
        assert_eq!(
            disabled
                .handle("POST", ADAPTIVE_RECOVERY_PATH, &pm, &body)
                .unwrap()
                .status,
            503
        );
    }

    #[test]
    fn exact_retry_and_restart_preserve_one_event_one_resolution_and_all_effect_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (api, blocked) = fixture(&path, &events, true);
        let request = request(&blocked);
        assert_eq!(
            resolution_event_id(request.operation_id).get_version_num(),
            7
        );
        let project_before = api
            .store
            .company_project(&blocked.grant.authority.tenant_id, &request.project_id)
            .unwrap()
            .unwrap();
        let first = call(&api, "pm", &request);
        assert_eq!(first.status, 200);
        let first_body: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
        assert_eq!(first_body["replay"], false);
        let resolved = read(&api, &blocked);
        assert_eq!(resolved.version, blocked.version + 1);
        assert_eq!(
            serde_json::to_value(&resolved.cursor).unwrap()["kind"],
            "blocked_resolved"
        );
        let mut expected = blocked.clone();
        expected.cursor = resolved.cursor.clone();
        expected.version = resolved.version;
        expected.updated_at_ms = resolved.updated_at_ms;
        assert_eq!(
            resolved, expected,
            "grant, calls, observations, result digest and effect IDs must survive"
        );
        assert_eq!(
            api.store
                .company_project(&blocked.grant.authority.tenant_id, &request.project_id)
                .unwrap()
                .unwrap(),
            project_before,
            "no allowance or business mutation"
        );
        let event_id = resolution_event_id(request.operation_id).to_string();
        let event = api
            .event_store
            .as_ref()
            .unwrap()
            .event_v2_by_id(&event_id)
            .unwrap()
            .unwrap();
        let payload: BlockedResolutionDecisionV1 = serde_json::from_slice(&event.payload).unwrap();
        assert_eq!(payload, decision(&api, &request));
        assert_eq!(event.stream_revision, 1);
        assert_eq!(event.durability, EventDurability::Authoritative);
        assert_eq!(call(&api, "pm", &request).status, 200);
        assert_eq!(read(&api, &blocked), resolved);
        drop(api);
        let api = reopen(&path, &events);
        let replay = call(&api, "pm", &request);
        assert_eq!(replay.status, 200);
        let mut replay_body: serde_json::Value = serde_json::from_slice(&replay.body).unwrap();
        assert_eq!(replay_body["replay"], true);
        replay_body["replay"] = serde_json::json!(false);
        assert_eq!(replay_body, first_body);
        assert_eq!(read(&api, &blocked), resolved);
        assert_eq!(
            api.event_store
                .as_ref()
                .unwrap()
                .event_v2_by_id(&event_id)
                .unwrap()
                .unwrap(),
            event
        );
    }

    #[test]
    fn restart_recovers_event_before_transition_gap_without_a_second_decision() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("company.sqlite");
        let events = temp.path().join("events.sqlite");
        let (api, blocked) = fixture(&path, &events, true);
        let request = request(&blocked);
        let event = append_decision(&api, &decision(&api, &request));
        assert_eq!(read(&api, &blocked), blocked);
        let mut competitor = request.clone();
        competitor.operation_id = Uuid::new_v4();
        assert_eq!(call(&api, "pm", &competitor).status, 409);
        assert_no_decision(&api, &competitor);
        drop(api);
        let api = reopen(&path, &events);
        assert_eq!(call(&api, "pm", &request).status, 200);
        assert_eq!(read(&api, &blocked).version, blocked.version + 1);
        assert_eq!(
            api.event_store
                .as_ref()
                .unwrap()
                .event_v2_by_id(&event.event_id)
                .unwrap()
                .unwrap(),
            event
        );
        assert_eq!(call(&api, "pm", &request).status, 200);
    }

    #[test]
    fn customer_operator_nonleader_and_ungoverned_leader_cannot_resolve() {
        let temp = tempfile::tempdir().unwrap();
        let (api, blocked) = fixture(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
            true,
        );
        let request = request(&blocked);
        for actor in [
            "customer",
            "operator",
            "sales",
            "developer-6",
            "technical-lead",
        ] {
            assert_eq!(call(&api, actor, &request).status, 403, "{actor}");
            assert_no_decision(&api, &request);
            assert_eq!(read(&api, &blocked), blocked);
        }
        let mut claimed = api.principals.principal("developer-6").unwrap();
        claimed.principal.role = CompanyRoleV1::ProjectManager;
        assert_eq!(
            api.resolve_blocked_adaptive_work(&claimed, &serde_json::to_vec(&request).unwrap())
                .status,
            403
        );
    }

    #[test]
    fn registered_cross_tenant_leadership_cannot_name_another_tenants_project() {
        let temp = tempfile::tempdir().unwrap();
        let (mut api, blocked) = fixture(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
            true,
        );
        let request = request(&blocked);
        let foreign = PrincipalAuthenticator::new(vec![(
            "foreign-credential-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".to_owned(),
            PrincipalBinding {
                credential_name: "foreign-pm".to_owned(),
                tenant_id: TenantId::parse("tenant-other").unwrap(),
                principal_id: "foreign-pm".to_owned(),
                kind: CompanyPrincipalKindV1::Agent,
                role: CompanyRoleV1::ProjectManager,
                customer_id: None,
                agent_id: Some(AgentId(5)),
                authority_generation: 1,
            },
        )])
        .unwrap();
        let mut principals = PrincipalAuthenticator {
            by_credential_digest: api.principals.by_credential_digest.clone(),
            by_principal_id: api.principals.by_principal_id.clone(),
        };
        principals
            .by_credential_digest
            .extend(foreign.by_credential_digest);
        principals.by_principal_id.extend(foreign.by_principal_id);
        api.principals = Arc::new(principals);
        assert_eq!(call(&api, "foreign-pm", &request).status, 409);
        assert_no_decision(&api, &request);
        assert_eq!(read(&api, &blocked), blocked);
    }

    #[test]
    fn stale_version_session_work_project_and_reason_reject_before_audit() {
        let temp = tempfile::tempdir().unwrap();
        let (api, blocked) = fixture(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
            true,
        );
        let original = request(&blocked);
        for field in ["version", "session", "work", "project", "reason"] {
            let mut request = original.clone();
            match field {
                "version" => request.expected_session_version -= 1,
                "session" => request.session_id = Uuid::new_v4(),
                "work" => request.work_item_id = WorkItemId::parse("other-work").unwrap(),
                "project" => request.project_id = ProjectId::parse("other-project").unwrap(),
                "reason" => request.expected_reason_code = "another_reason".to_owned(),
                _ => unreachable!(),
            }
            assert_eq!(call(&api, "pm", &request).status, 409, "{field}");
            assert_no_decision(&api, &request);
            assert_eq!(read(&api, &blocked), blocked);
        }
    }

    #[test]
    fn pending_unknown_model_and_tool_effects_are_never_resolved_as_blocked() {
        for (tool, unknown) in [(false, false), (false, true), (true, false), (true, true)] {
            let temp = tempfile::tempdir().unwrap();
            let (api, ready) = fixture(
                &temp.path().join("company.sqlite"),
                &temp.path().join("events.sqlite"),
                false,
            );
            let (mut pending, mut effect) = claim(&api, &ready);
            if tool {
                let tool = WorkbenchTool::InspectFile {
                    path: "README.md".to_owned(),
                    max_bytes: 1024,
                };
                let tool_digest = adaptive_tool_digest(&tool).unwrap();
                pending = advance(
                    &api,
                    &pending,
                    AdaptiveTransitionV1::ResolveModel {
                        effect,
                        result_digest: "b".repeat(64),
                        decision: AdaptiveModelDecisionV1::Tool {
                            tool,
                            tool_digest: tool_digest.clone(),
                        },
                    },
                );
                effect = AdaptiveEffectV1 {
                    id: Uuid::new_v4(),
                    request_digest: "c".repeat(64),
                };
                pending = advance(
                    &api,
                    &pending,
                    AdaptiveTransitionV1::ClaimTool {
                        effect: effect.clone(),
                        tool_digest,
                    },
                );
            }
            if unknown {
                pending = advance(&api, &pending, AdaptiveTransitionV1::MarkUnknown { effect });
            }
            let request = request(&pending);
            assert_eq!(call(&api, "pm", &request).status, 409);
            assert_no_decision(&api, &request);
            assert_eq!(read(&api, &pending), pending);
        }
    }

    #[test]
    fn changed_operation_content_and_new_operation_cannot_rewrite_committed_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let (api, blocked) = fixture(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
            true,
        );
        let original = request(&blocked);
        assert_eq!(call(&api, "pm", &original).status, 200);
        let resolved = read(&api, &blocked);
        for field in ["reason_ref", "reason_code", "version", "operation"] {
            let mut changed = original.clone();
            match field {
                "reason_ref" => changed.reason_ref = "another-decision".to_owned(),
                "reason_code" => changed.expected_reason_code = "another_reason".to_owned(),
                "version" => changed.expected_session_version += 1,
                "operation" => changed.operation_id = Uuid::new_v4(),
                _ => unreachable!(),
            }
            assert_eq!(call(&api, "pm", &changed).status, 409, "{field}");
            assert_eq!(read(&api, &blocked), resolved);
        }
    }

    #[test]
    fn bounded_dto_rejects_raw_identity_unknown_fields_and_invalid_values() {
        let temp = tempfile::tempdir().unwrap();
        let (api, blocked) = fixture(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
            true,
        );
        let request = request(&blocked);
        let pm = api.principals.principal("pm").unwrap();
        for (field, value) in [
            ("schema_version", serde_json::json!(2)),
            ("operation_id", serde_json::json!(Uuid::nil())),
            ("session_id", serde_json::json!(Uuid::nil())),
            ("expected_session_version", serde_json::json!(0)),
            ("expected_session_version", serde_json::json!(u64::MAX)),
            ("expected_reason_code", serde_json::json!("x".repeat(65))),
            ("expected_reason_code", serde_json::json!("raw thought")),
            ("reason_ref", serde_json::json!(" ")),
            ("reason_ref", serde_json::json!("x".repeat(4097))),
            ("reason_ref", serde_json::json!("decision\u{0}")),
            ("agent_id", serde_json::json!(6)),
            ("tenant_id", serde_json::json!("tenant-m0")),
            ("principal_id", serde_json::json!("pm")),
        ] {
            let mut body = serde_json::to_value(&request).unwrap();
            body[field] = value;
            assert_eq!(
                api.resolve_blocked_adaptive_work(&pm, &serde_json::to_vec(&body).unwrap())
                    .status,
                400,
                "{field}"
            );
            assert_no_decision(&api, &request);
        }
        assert_eq!(
            api.resolve_blocked_adaptive_work(&pm, &vec![b' '; MAX_WORKFLOW_BODY_BYTES + 1])
                .status,
            413
        );
        assert_eq!(read(&api, &blocked), blocked);
    }

    #[test]
    fn unavailable_event_store_and_changed_assignee_runtime_authority_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let (mut api, blocked) = fixture(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
            true,
        );
        let request = request(&blocked);
        let events = api.event_store.take().unwrap();
        assert_eq!(call(&api, "pm", &request).status, 503);
        api.event_store = Some(events);
        api.authority
            .as_ref()
            .unwrap()
            .runtime_health
            .write()
            .unwrap()
            .agents
            .iter_mut()
            .find(|agent| agent.agent_id == blocked.grant.authority.agent_id.0)
            .unwrap()
            .expected_active = false;
        assert_eq!(call(&api, "pm", &request).status, 409);
        assert_no_decision(&api, &request);
        assert_eq!(read(&api, &blocked), blocked);
    }

    #[test]
    fn changed_assignment_cannot_recover_a_prior_decision() {
        let temp = tempfile::tempdir().unwrap();
        let (api, blocked) = fixture(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
            true,
        );
        let request = request(&blocked);
        let event = append_decision(&api, &decision(&api, &request));
        let pm = api.principals.principal("pm").unwrap();
        let project = api
            .store
            .company_project(&pm.principal.tenant_id, &request.project_id)
            .unwrap()
            .unwrap();
        api.store
            .apply_company_command(
                &pm.principal,
                Uuid::new_v4(),
                &CompanyWorkflowCommandV1::ReassignWork {
                    project_id: request.project_id.clone(),
                    expected_version: project.version,
                    work_item_id: request.work_item_id.clone(),
                    expected_assignment_version: blocked.grant.authority.assignment_version,
                    agent_id: blocked.grant.authority.agent_id,
                    organization_generation: 2,
                    organization_digest: "d".repeat(64),
                    reason_ref: "new-current-assignment".to_owned(),
                },
                now_unix_ms(),
            )
            .unwrap();
        assert_eq!(call(&api, "pm", &request).status, 409);
        assert_eq!(read(&api, &blocked), blocked);
        assert_eq!(
            api.event_store
                .as_ref()
                .unwrap()
                .event_v2_by_id(&event.event_id)
                .unwrap()
                .unwrap(),
            event
        );
    }

    fn rotate_pm(api: &mut WorkflowApi, generation: u64) -> BoundPrincipal {
        let old = api.principals.principal("pm").unwrap();
        let rotated = PrincipalAuthenticator::new(vec![(
            "rotated-credential-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".to_owned(),
            PrincipalBinding {
                credential_name: "pm".to_owned(),
                tenant_id: old.principal.tenant_id.clone(),
                principal_id: "pm".to_owned(),
                kind: CompanyPrincipalKindV1::Agent,
                role: CompanyRoleV1::ProjectManager,
                customer_id: None,
                agent_id: old.principal.agent_id,
                authority_generation: generation,
            },
        )])
        .unwrap();
        let mut principals = PrincipalAuthenticator {
            by_credential_digest: api.principals.by_credential_digest.clone(),
            by_principal_id: api.principals.by_principal_id.clone(),
        };
        principals
            .by_credential_digest
            .retain(|_, bound| bound.principal.principal_id != "pm");
        principals
            .by_credential_digest
            .extend(rotated.by_credential_digest);
        principals.by_principal_id.extend(rotated.by_principal_id);
        api.principals = Arc::new(principals);
        api.principals.principal("pm").unwrap()
    }

    #[test]
    fn credential_rotation_recovers_the_same_decision_across_the_crash_gap_and_restart() {
        for (completed, generation) in [(false, 1), (false, 2), (true, 2)] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("company.sqlite");
            let events = temp.path().join("events.sqlite");
            let (mut api, blocked) = fixture(&path, &events, true);
            let request = request(&blocked);
            let old = api.principals.principal("pm").unwrap();
            let event = append_decision(&api, &decision(&api, &request));
            if completed {
                assert_eq!(call(&api, "pm", &request).status, 200);
            }
            let fresh = rotate_pm(&mut api, generation);
            let body = serde_json::to_vec(&request).unwrap();
            assert_eq!(api.resolve_blocked_adaptive_work(&old, &body).status, 403);
            let revoked = HashMap::from([(
                "authorization".to_owned(),
                format!("Bearer test-credential-2-{}", "x".repeat(32)),
            )]);
            assert_eq!(
                api.handle("POST", ADAPTIVE_RECOVERY_PATH, &revoked, &body)
                    .unwrap()
                    .status,
                401
            );
            assert_eq!(api.resolve_blocked_adaptive_work(&fresh, &body).status, 200);
            let resolved = read(&api, &blocked);
            assert_eq!(resolved.version, blocked.version + 1);
            assert!(matches!(
                resolved.cursor,
                AdaptiveCursorV1::BlockedResolved { .. }
            ));
            assert_eq!(
                api.event_store
                    .as_ref()
                    .unwrap()
                    .event_v2_by_id(&event.event_id)
                    .unwrap()
                    .unwrap(),
                event
            );
            drop(api);
            let mut api = reopen(&path, &events);
            rotate_pm(&mut api, generation);
            let replay = call(&api, "pm", &request);
            assert_eq!(replay.status, 200);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&replay.body).unwrap()["replay"],
                true
            );
            assert_eq!(read(&api, &blocked), resolved);
            assert_eq!(
                api.event_store
                    .as_ref()
                    .unwrap()
                    .event_v2_by_id(&event.event_id)
                    .unwrap()
                    .unwrap(),
                event
            );
        }
    }

    #[test]
    fn regressed_leadership_generation_cannot_complete_a_recorded_decision() {
        let temp = tempfile::tempdir().unwrap();
        let (mut api, blocked) = fixture(
            &temp.path().join("company.sqlite"),
            &temp.path().join("events.sqlite"),
            true,
        );
        let request = request(&blocked);
        rotate_pm(&mut api, 2);
        let event = append_decision(&api, &decision(&api, &request));
        rotate_pm(&mut api, 1);
        assert_eq!(call(&api, "pm", &request).status, 409);
        assert_eq!(read(&api, &blocked), blocked);
        assert_eq!(
            api.event_store
                .as_ref()
                .unwrap()
                .event_v2_by_id(&event.event_id)
                .unwrap()
                .unwrap(),
            event
        );
    }
}
