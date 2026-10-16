//! Scripted typed journal/execution evidence, not process or model acceptance.

use super::*;
use crate::*;
use crate::adaptive::continuation_tests::{grant, NOW};
use serde_json::json;

pub(crate) struct Fixture {
    _temp: tempfile::TempDir,
    path: std::path::PathBuf,
    pub(crate) store: WorkflowStore,
    pub(crate) project: ProjectV1,
    pub(crate) work: CompanyWorkItemV1,
    pub(crate) allowance: SubscriptionCallAllowanceV1,
    source: AdaptiveSourceCompletionV1,
    plan: ExecutionPlanV1,
    done: WorkItemExecutionV1,
}

fn advance(store: &WorkflowStore, source: &AdaptiveSessionV1, command: AdaptiveTransitionV1, at: u64) -> AdaptiveSessionV1 {
    store.advance_adaptive_session(source.grant.session_id, source.version, Uuid::new_v4(), &command, &source.grant.authority, at).unwrap().1
}

fn plan(source: &AdaptiveSourceCompletionV1, at: u64, tool: &sentinel_common::WorkbenchTool, output_name: &str) -> ExecutionPlanV1 {
    let sentinel_common::WorkbenchTool::PackageArtifact {artifact_kind,media_type,paths} = tool else {panic!("package fixture required")};
    let authority = &source.provider_source.grant.authority;
    let workspace = format!("{}:{}", authority.project_id, authority.work_item_id);
    let mut value = serde_json::to_value(authority).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("active");
    object.remove("capabilities");
    for (key, field) in json!({
        "plan_id": completion_plan_id(source), "workspace_id": workspace,
        "created_at_unix_ms": at, "deadline_unix_ms": at + 200_000,
        "request_digest": "", "steps": [{
            "step_id": Uuid::new_v4(), "invocation_id": Uuid::new_v4(), "ordinal": 0,
            "workspace_id": workspace, "capabilities": ["artifact.commit"], "inputs": [], "command_policy": [],
            "tool": {"kind":"package_artifact", "artifact_kind":artifact_kind, "media_type":media_type, "paths":paths},
            "outputs":[{"name":output_name, "kind":artifact_kind, "required":true, "digest_algorithm":"sha256"}],
            "artifacts":[{"artifact_kind":artifact_kind, "media_type":media_type, "required_paths":paths}],
            "gate_expectation":{"profile_id": WORK_ITEM_GATE_PROFILE, "profile_generation":1, "profile_digest":"a".repeat(64), "required_checks":["source_integrity"]},
            "resource_bounds":{"wall_time_ms":1000,"cpu_time_ms":1000,"memory_bytes":1048576,"process_count":16,"file_bytes":4096,"stdout_bytes":4096,"stderr_bytes":4096},
            "deadline_unix_ms": at + 200_000,
        }]
    }).as_object().unwrap() {
        object.insert(key.clone(), field.clone());
    }
    serde_json::from_value::<ExecutionPlanV1>(value).unwrap().bind_digest().unwrap()
}

fn complete(
    store: &WorkflowStore,
    root: &AdaptiveSessionGrantV1,
    tool: sentinel_common::WorkbenchTool,
    output_name: &str,
    now: u64,
    record_gate: bool,
    revision: Option<&ExecutionRevisionV1>,
) -> (AdaptiveSourceCompletionV1,ExecutionPlanV1,WorkItemExecutionV1,GateEvidenceReadbackV1) {
    let mut session = store.begin_adaptive_session(root, &root.authority, now).unwrap().1;
    let package_effect = AdaptiveEffectV1 {id:Uuid::new_v4(), request_digest:"c".repeat(64)};
    let model = AdaptiveEffectV1 {id:Uuid::new_v4(), request_digest:"d".repeat(64)};
    let tool_digest = adaptive_tool_digest(&tool).unwrap();
    for (offset, command) in [
        AdaptiveTransitionV1::ClaimModel {effect:model.clone(),previous_observation_digest:None},
        AdaptiveTransitionV1::ResolveModel {effect:model,result_digest:"e".repeat(64),decision:AdaptiveModelDecisionV1::Tool {tool:tool.clone(),tool_digest:tool_digest.clone()}},
        AdaptiveTransitionV1::ClaimTool {effect:package_effect.clone(),tool_digest},
        AdaptiveTransitionV1::ObserveTool {observation:AdaptiveObservationRefV1 {effect:package_effect,observation_digest:"f".repeat(64)}},
    ].into_iter().enumerate() {
        session = advance(store, &session, command, now + offset as u64 + 1);
    }
    let source = AdaptiveSourceCompletionV1 {provider_source:session.clone(),effect:AdaptiveEffectV1 {id:Uuid::new_v4(),request_digest:"1".repeat(64)},model_response_digest:"b".repeat(64),artifact_digest:"a".repeat(64)};
    session = advance(store, &session, AdaptiveTransitionV1::ClaimModel {effect:source.effect.clone(),previous_observation_digest:session.last_observation.as_ref().map(|o|o.observation_digest.clone())}, now + 5);
    let plan = plan(&source, now + 6, &tool, output_name);
    if let Some(revision) = revision {
        store.admit_revision_plan(&plan,revision,&root.authority,now+6).unwrap();
    } else {
        store.admit_plan(&plan, &root.authority, now + 6).unwrap();
    }
    advance(store, &session, AdaptiveTransitionV1::ResolveModel {effect:source.effect.clone(),result_digest:source.model_response_digest.clone(),decision:AdaptiveModelDecisionV1::ProposeCompletion {artifact_digest:source.artifact_digest.clone()}}, now + 7);
    let pending = store.pending_executions(10).unwrap().into_iter().find(|pending|pending.plan_id == plan.plan_id).unwrap();
    store.record_execution_observation(&pending, ExecutionReconcileState::Succeeded, &root.authority, now + 8).unwrap();
    let request = store.pending_completion_evidence(10).unwrap().into_iter().find(|pending|pending.plan_id == plan.plan_id).unwrap();
    let sentinel_common::WorkbenchTool::PackageArtifact {artifact_kind,media_type,paths} = tool else {panic!("package fixture required")};
    let outputs = vec![SealedOutputEvidenceV1 {name:output_name.into(),kind:artifact_kind.clone(),digest_algorithm:"sha256".into(),digest:source.artifact_digest.clone()}];
    let artifacts = vec![SealedArtifactEvidenceV1 {artifact_kind,media_type,paths,digest:source.artifact_digest.clone()}];
    let evidence = ExecutionEvidenceReadbackV1::new("package-evidence".into(),request.invocation_id,request.plan_digest.clone(),request.step_digest.clone(),sealed_output_bundle_digest(&outputs,&artifacts).unwrap(),outputs,artifacts,now + 9);
    let in_review = store.record_terminal_evidence(&request,evidence,&root.authority,now + 9).unwrap();
    let request = store.pending_gate_evidence(10).unwrap().into_iter().find(|pending|pending.plan_id == plan.plan_id).unwrap();
    let gate = GateEvidenceReadbackV1::new("independent-gate".into(),request.expectation.profile_generation,request.expectation.profile_digest.clone(),request.subject_digest.clone(),request.required_checks_digest.clone(),now + 10);
    let done = if record_gate {
        store.record_gate_evidence(&request,gate.clone(),&root.authority,now + 10).unwrap()
    } else {in_review};
    (source,plan,done,gate)
}

fn project_authority(project: &ProjectV1, work_id: &WorkItemId) -> RuntimeAuthoritySnapshotV1 {
    let work = &project.work_items[work_id];
    let assignment = work.assignments.iter().find(|assignment|assignment.active).unwrap();
    let participant = project.governance.participants.iter().find(|participant|participant.agent_id == assignment.agent_id).unwrap();
    let mut authority = grant().authority;
    authority.tenant_id = project.tenant_id.clone();
    authority.project_id = project.project_id.clone();
    authority.work_item_id = work_id.clone();
    authority.agent_id = assignment.agent_id;
    authority.assignment_version = assignment.assignment_version;
    authority.assignment_digest = assignment.canonical_digest().unwrap();
    authority.organization_generation = assignment.organization_generation;
    authority.organization_digest = assignment.organization_digest.clone();
    authority.principal = PrincipalAuthorityV1::derive(&participant.principal_id,1,&[0x5a;32]).unwrap();
    authority.profile_id = assignment.profile.profile_id.clone();
    authority.profile_generation = assignment.profile.generation;
    authority.profile_digest = assignment.profile.digest.clone();
    authority
}

pub(crate) fn root_for_project(project: &ProjectV1, work_id: &WorkItemId, now: u64) -> AdaptiveSessionGrantV1 {
    let allowance = project.subscription_call.as_ref().unwrap();
    assert_eq!(allowance.grant.work_item_id,*work_id);
    assert!(allowance.dispatch.is_none());
    assert!(now >= allowance.created_at_unix_ms && now < allowance.grant.expires_at_unix_ms);
    let mut root = grant();
    root.session_id = Uuid::new_v4();
    root.authority = project_authority(project,work_id);
    root.authority.capabilities.insert("artifact.commit".into());
    root.provider_allowance_id = allowance.allowance_id.clone();
    root.provider_authority_digest = adaptive_continuation_provider_digest(allowance,&root.authority).unwrap();
    root.provider = allowance.grant.provider.clone();
    root.model = allowance.grant.model.clone();
    root.catalog_digest = allowance.grant.catalog_digest.clone();
    root.max_model_calls = allowance.grant.max_calls;
    root.max_tool_calls = allowance.grant.max_calls;
    root.max_call_duration_ms = allowance.grant.max_duration_ms;
    root.created_at_ms = allowance.created_at_unix_ms;
    root.deadline_ms = allowance.grant.expires_at_unix_ms;
    root
}

pub(crate) fn complete_for_project(store: &WorkflowStore, project: &ProjectV1, work_id: &WorkItemId, now: u64) -> (AdaptiveSourceCompletionV1,ExecutionPlanV1,WorkItemExecutionV1) {
    let work = &project.work_items[work_id];
    let root = root_for_project(project,work_id,now);
    let kind = if work.spec.required_role == CompanyRoleV1::Designer {"design_specification"} else {"source_tree"};
    let tool = sentinel_common::WorkbenchTool::PackageArtifact {artifact_kind:kind.into(),media_type:work.spec.outputs[0].media_type.clone(),paths:vec!["artifact.json".into()]};
    let revision = project.work_corrections.iter().rev().find(|record|record.previous.spec.work_item_id == *work_id).map(|record|&record.execution_revision);
    let (source,plan,done,_) = complete(store,&root,tool,&work.spec.outputs[0].name,now,true,revision);
    (source,plan,done)
}

pub(crate) fn complete_negative_review(store: &WorkflowStore, project: &ProjectV1, review_id: &WorkItemId, now: u64) -> WorkItemExecutionV1 {
    let work = &project.work_items[review_id];
    assert_eq!(work.spec.required_role,CompanyRoleV1::Qa);
    assert!(project.subscription_call.as_ref().unwrap().dispatch.is_some());
    let mut authority = project_authority(project,review_id);
    authority.capabilities = BTreeSet::from(["file.write".into()]);
    // Scripted negative report sealed through the ordinary execution/gate APIs;
    // this does not claim a live model review or evaluator invocation.
    let report = json!({"schema_version":1,"source_files":[{"path":"artifact.json","sha256":"a".repeat(64)}],
        "verdict":"changes_requested","findings":[{"path":"artifact.json","line":1,"reason":"Contracted behavior missing"}]}).to_string();
    let digest = format!("{:x}",Sha256::digest(report.as_bytes()));
    let workspace = format!("{}:{}",project.project_id,review_id);
    let mut value = serde_json::to_value(&authority).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("active");
    object.remove("capabilities");
    for (key,field) in json!({"plan_id":Uuid::new_v4(),"workspace_id":workspace,
        "created_at_unix_ms":now,"deadline_unix_ms":now+200_000,"request_digest":"",
        "steps":[{"step_id":Uuid::new_v4(),"invocation_id":Uuid::new_v4(),"ordinal":0,
            "workspace_id":workspace,"capabilities":["file.write"],"inputs":[],"command_policy":[],
            "tool":{"kind":"write_file","path":"review.json","content":report,"expected_sha256":null},
            "outputs":[{"name":work.spec.outputs[0].name,"kind":"qa_report","required":true,"digest_algorithm":"sha256"}],"artifacts":[],
            "gate_expectation":{"profile_id":work.spec.quality_gate.gate_id,"profile_generation":work.spec.quality_gate.generation,"profile_digest":work.spec.quality_gate.digest,"required_checks":["review_schema"]},
            "resource_bounds":{"wall_time_ms":1000,"cpu_time_ms":1000,"memory_bytes":1048576,"process_count":1,"file_bytes":4096,"stdout_bytes":4096,"stderr_bytes":4096},"deadline_unix_ms":now+200_000}]
    }).as_object().unwrap() {object.insert(key.clone(),field.clone());}
    let plan = serde_json::from_value::<ExecutionPlanV1>(value).unwrap().bind_digest().unwrap();
    store.admit_plan(&plan,&authority,now).unwrap();
    let pending = store.pending_executions(10).unwrap().into_iter().find(|pending|pending.plan_id == plan.plan_id).unwrap();
    store.record_execution_observation(&pending,ExecutionReconcileState::Succeeded,&authority,now+1).unwrap();
    let request = store.pending_completion_evidence(10).unwrap().into_iter().find(|pending|pending.plan_id == plan.plan_id).unwrap();
    let outputs = vec![SealedOutputEvidenceV1 {name:work.spec.outputs[0].name.clone(),kind:"qa_report".into(),digest_algorithm:"sha256".into(),digest}];
    let evidence = ExecutionEvidenceReadbackV1::new("review-evidence".into(),request.invocation_id,request.plan_digest.clone(),request.step_digest.clone(),sealed_output_bundle_digest(&outputs,&[]).unwrap(),outputs,vec![],now+2);
    store.record_terminal_evidence(&request,evidence,&authority,now+2).unwrap();
    let request = store.pending_gate_evidence(10).unwrap().into_iter().find(|pending|pending.plan_id == plan.plan_id).unwrap();
    let gate = GateEvidenceReadbackV1::new("review-gate".into(),request.expectation.profile_generation,request.expectation.profile_digest.clone(),request.subject_digest.clone(),request.required_checks_digest.clone(),now+3);
    store.record_gate_evidence(&request,gate,&authority,now+3).unwrap()
}

pub(crate) fn journal_rows(store: &WorkflowStore, id: Uuid) -> Vec<(String,String,String,Vec<u8>,i64)> {
    let connection = store.lock().unwrap();
    let ns = format!("adaptive-session-v1:{id}");
    let mut statement = connection.prepare("SELECT operation_namespace,operation_id,request_digest,response,created_at_ms FROM workflow_operations WHERE operation_namespace IN (?1,?2,?3) ORDER BY operation_namespace,operation_id").unwrap();
    let rows = statement.query_map(params![ns,format!("{ns}:operations"),format!("{ns}:rejected-model-dispositions")],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).unwrap();
    rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
}

impl Fixture {
    pub(crate) fn new() -> Self {
        Self::with_recorded_gate(true)
    }

    pub(crate) fn with_recorded_gate(record_gate: bool) -> Self {
        Self::with_legacy_rollover(record_gate,false)
    }

    fn with_legacy_rollover(record_gate: bool, legacy_rollover: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("completion.sqlite");
        let store = WorkflowStore::open(&path).unwrap();
        let mut root = grant();
        root.deadline_ms = NOW + 300_000;
        root.authority.capabilities.insert("artifact.commit".into());
        let assignment: AssignmentV1 = serde_json::from_value(json!({
            "assignment_id":"assignment-source", "agent_id":root.authority.agent_id, "role":"developer",
            "specialties":["rust"], "profile":{"profile_id":root.authority.profile_id,"generation":root.authority.profile_generation,"digest":root.authority.profile_digest},
            "organization_generation":root.authority.organization_generation, "organization_digest":root.authority.organization_digest,
            "assignment_version":root.authority.assignment_version,"delegated_by":null,"reason_ref":"assigned","active":true,
            "assigned_by":"pm", "created_at_unix_ms":NOW,"ended_at_unix_ms":null,
        })).unwrap();
        root.authority.assignment_digest = assignment.canonical_digest().unwrap();
        let allowance = SubscriptionCallAllowanceV1 {
            allowance_id: root.provider_allowance_id.clone(),
            grant: SubscriptionCallGrantV1 {
                schema_version:1, work_item_id:root.authority.work_item_id.clone(), assignment_id:assignment.assignment_id.clone(),
                assignment_version:root.authority.assignment_version, agent_id:root.authority.agent_id,
                provider:root.provider.clone(),model:root.model.clone(),catalog_digest:root.catalog_digest.clone(),
                max_calls:root.max_model_calls,max_concurrent:1,max_duration_ms:root.max_call_duration_ms,
                token_policy:SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,expires_at_unix_ms:root.deadline_ms,
            },
            created_by:"pm".into(),created_at_unix_ms:NOW,dispatch:None,
        };
        root.provider_authority_digest = adaptive_continuation_provider_digest(&allowance, &root.authority).unwrap();
        if legacy_rollover {
            let mut expired = root.clone();
            expired.session_id = Uuid::new_v4();
            expired.provider_allowance_id = "expired-idle".into();
            expired.created_at_ms = NOW-2_000;
            expired.deadline_ms = NOW;
            let mut previous_allowance = allowance.clone();
            previous_allowance.allowance_id = expired.provider_allowance_id.clone();
            previous_allowance.created_at_unix_ms = expired.created_at_ms;
            previous_allowance.grant.expires_at_unix_ms = expired.deadline_ms;
            expired.provider_authority_digest = adaptive_continuation_provider_digest(&previous_allowance,&expired.authority).unwrap();
            store.begin_adaptive_session(&expired,&expired.authority,NOW-2_000).unwrap();
        }
        let tool = sentinel_common::WorkbenchTool::PackageArtifact {artifact_kind:"source_tree".into(),media_type:"text/x-python".into(),paths:vec!["app.py".into()]};
        let (source,plan,done,gate) = complete(&store,&root,tool,"source",NOW,record_gate,None);
        let work: CompanyWorkItemV1 = serde_json::from_value(json!({
            "spec":{"work_item_id":root.authority.work_item_id,"title":"Source","objective":"Produce source","required_role":"developer","required_specialties":["rust"],"dependency_ids":[],"owner":root.authority.agent_id,"inputs":[],
                "outputs":[{"name":"source","media_type":"text/x-python","digest_algorithm":"sha256","contract_generation":1,"contract_digest":"a".repeat(64)}],
                "quality_gate":{"gate_id":gate.profile_id,"generation":gate.profile_generation,"digest":gate.profile_digest},"budget_micros":100},
            "state":"done","version":4,"assignments":[assignment],
            "output_receipts":[{"name":"source","contract_generation":1,"contract_digest":"a".repeat(64),"content_digest":source.artifact_digest}],
            "gate_receipt":{"gate_id":gate.profile_id,"generation":gate.profile_generation,"gate_digest":gate.profile_digest,"subject_digest":gate.subject_digest,"passed":true},
            "transition_history":[],
        })).unwrap();
        let profile = json!({"profile_id":root.authority.profile_id,"generation":root.authority.profile_generation,"digest":root.authority.profile_digest});
        let project = serde_json::from_value(json!({
            "schema_version":1,"tenant_id":root.authority.tenant_id,"project_id":root.authority.project_id,"agreement_id":"agreement","agreement_digest":"a".repeat(64),
            "governance":{"owner":1,"project_profile":profile,"participants":[{"agent_id":1,"principal_id":"pm","role":"project_manager","specialties":["coordination"],"reports_to":null,"profile":profile}]},
            "cost_ceiling_micros":100,"provider_cost_ceilings_micros":{},"lifecycle_state":"active","reserved_cost_micros":0,"committed_cost_micros":0,
            "work_items":{"work-01":work},"decisions":[],"handoffs":[],"blockers":[],"approvals":[],"reservations":[],"rooms":[],"questions":[],"actions":[],
            "version":1,"created_at_unix_ms":NOW,"updated_at_unix_ms":NOW+11,"subscription_call":allowance,
        })).unwrap();
        Self {_temp:temp,path,store,project,work,allowance,source,plan,done}
    }

    fn read(&self) -> Result<Option<AdaptiveSourceCompletionV1>, WorkflowError> {
        self.store.adaptive_source_completion(&self.project.tenant_id,&self.project.project_id,&self.allowance,self.plan.plan_id)
    }

    fn require(&self, work: &CompanyWorkItemV1, cutoff: u64) -> Result<(), WorkflowError> {
        let connection = self.store.lock().unwrap();
        require_source_review_producer(&connection,&self.project,work,&self.allowance,cutoff)
    }
}

#[test]
fn completion_plan_identity_is_the_existing_daemon_request_identity() {
    let mut source = AdaptiveSourceCompletionV1 {
        provider_source:AdaptiveSessionV1::initial(grant()).unwrap(),
        effect:AdaptiveEffectV1 {id:Uuid::from_u128(202),request_digest:"1".repeat(64)},
        model_response_digest:"2".repeat(64),artifact_digest:"3".repeat(64),
    };
    // SHA-256 of the daemon's domain:company-adaptive-session-effect:digest:1 framing.
    let expected = Uuid::parse_str("c5d9b683-0327-5ebf-a01a-5f12d5a360b4").unwrap();
    assert_eq!(completion_plan_id(&source),expected);
    source.effect.request_digest = "4".repeat(64);
    assert_ne!(completion_plan_id(&source),expected);
}

#[test]
fn exact_completion_read_reopen_and_source_proof_never_write_or_need_current_project() {
    let mut f = Fixture::new();
    for _ in 0..2 {
        let before: i64 = f.store.lock().unwrap().query_row("SELECT total_changes()",[],|row|row.get(0)).unwrap();
        assert_eq!(f.read().unwrap(),Some(f.source.clone()));
        f.require(&f.work,NOW+11).unwrap();
        assert_eq!(f.store.lock().unwrap().query_row("SELECT total_changes()",[],|row|row.get::<_,i64>(0)).unwrap(),before);
        f.store = WorkflowStore::open(&f.path).unwrap();
    }
}

#[test]
fn qa_allowance_does_not_retarget_producer_but_deleted_head_is_corruption() {
    let mut f = Fixture::new();
    let old = f.project.clone();
    let mut qa = f.allowance.clone();
    qa.allowance_id = "qa-allowance".into();
    qa.grant.agent_id = AgentId(8);
    qa.grant.work_item_id = WorkItemId::parse("review-work").unwrap();
    f.project.source_review_previous_call = Some(f.allowance.clone());
    f.project.subscription_call = Some(qa);
    f.project.updated_at_unix_ms = NOW + 100;
    assert_eq!(f.read().unwrap(),Some(f.source.clone()));
    f.require(&f.work,NOW+11).unwrap();
    let connection = f.store.lock().unwrap();
    require_source_review_producer(&connection,&old,&f.work,&f.allowance,NOW+11).unwrap();
    assert_eq!(connection.execute("DELETE FROM workflow_adaptive_heads",[]).unwrap(),1);
    drop(connection);
    assert!(f.read().is_err());
    assert!(f.require(&f.work,NOW+11).is_err());
}

#[test]
fn canonical_idle_rollover_without_cancel_companion_preserves_completed_producer() {
    let f = Fixture::with_legacy_rollover(true,true);
    let heads = f.store.adaptive_sessions_for_health().unwrap();
    assert_eq!(heads.len(),1);
    assert!(matches!(heads[0].cursor,AdaptiveCursorV1::CompletionProposed { .. }));
    assert_eq!(f.read().unwrap(),Some(f.source.clone()));
    f.require(&f.work,NOW+11).unwrap();
    let connection = f.store.lock().unwrap();
    // Removing an ordinary model/tool companion is still corruption, despite
    // the separately valid legacy rollover cancellation in this lineage.
    assert_eq!(connection.execute("DELETE FROM workflow_operations WHERE operation_namespace=?1 AND operation_id=(SELECT operation_id FROM workflow_operations WHERE operation_namespace=?1 LIMIT 1)",[format!("adaptive-session-v1:{}:operations",f.source.provider_source.grant.session_id)]).unwrap(),1);
    drop(connection);
    assert!(f.read().is_err());
    assert!(f.store.adaptive_sessions_for_health().is_err());
}

#[test]
fn missing_cancel_companion_on_a_head_is_not_a_rollover_exception() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(temp.path().join("cancel.sqlite")).unwrap();
    let root = grant();
    let initial = store.begin_adaptive_session(&root,&root.authority,NOW).unwrap().1;
    advance(&store,&initial,AdaptiveTransitionV1::Cancel,NOW+1);
    assert_eq!(store.adaptive_sessions_for_health().unwrap().len(),1);
    let connection = store.lock().unwrap();
    assert_eq!(connection.execute("DELETE FROM workflow_operations WHERE operation_namespace=?1",[format!("adaptive-session-v1:{}:operations",root.session_id)]).unwrap(),1);
    drop(connection);
    assert!(store.adaptive_sessions_for_health().is_err());
}

#[test]
fn future_or_mismatched_company_outputs_gate_and_assignment_never_authorize_handoff() {
    let f = Fixture::new();
    f.require(&f.work,NOW+11).unwrap();
    assert!(f.require(&f.work,NOW+9).is_err());
    for change in 0..7 {
        let mut work = f.work.clone();
        match change {
            0 => work.state = CompanyWorkStateV1::InReview,
            1 => work.output_receipts[0].content_digest = "2".repeat(64),
            2 => work.output_receipts[0].contract_generation += 1,
            3 => work.gate_receipt.as_mut().unwrap().subject_digest = "2".repeat(64),
            4 => work.gate_receipt.as_mut().unwrap().passed = false,
            5 => work.assignments[0].profile.generation += 1,
            _ => work.assignments[0].assignment_version += 1,
        }
        assert!(f.require(&work,NOW+11).is_err(),"change={change}");
    }
}

#[test]
fn wrong_plan_tenant_project_or_full_allowance_is_not_a_producer() {
    let f = Fixture::new();
    assert_eq!(f.store.adaptive_source_completion(&f.project.tenant_id,&f.project.project_id,&f.allowance,Uuid::new_v4()).unwrap(),None);
    assert_eq!(f.store.adaptive_source_completion(&TenantId::parse("foreign").unwrap(),&f.project.project_id,&f.allowance,f.plan.plan_id).unwrap(),None);
    assert_eq!(f.store.adaptive_source_completion(&f.project.tenant_id,&ProjectId::parse("foreign").unwrap(),&f.allowance,f.plan.plan_id).unwrap(),None);
    for change in 0..6 {
        let mut allowance = f.allowance.clone();
        match change {
            0 => allowance.allowance_id = "other".into(),
            1 => allowance.created_by = "other".into(),
            2 => allowance.grant.assignment_version += 1,
            3 => allowance.grant.max_calls += 1,
            4 => allowance.grant.catalog_digest = "2".repeat(64),
            _ => allowance.created_at_unix_ms += 1,
        }
        assert_eq!(f.store.adaptive_source_completion(&f.project.tenant_id,&f.project.project_id,&allowance,f.plan.plan_id).unwrap(),None,"change={change}");
    }
}

#[test]
fn missing_and_corrupt_journal_completion_gate_or_execution_never_become_authority() {
    for sql in [
        "DELETE FROM workflow_operations WHERE operation_namespace GLOB 'adaptive-session-v1:*:operations' AND operation_id=(SELECT operation_id FROM workflow_operations WHERE operation_namespace GLOB 'adaptive-session-v1:*:operations' LIMIT 1)",
        "UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace GLOB 'adaptive-session-v1:*' AND operation_id='00000000000000000001'",
        "DELETE FROM workflow_operations WHERE operation_namespace GLOB 'adaptive-session-v1:*' AND operation_id='00000000000000000001'",
        "UPDATE workflow_completion_outbox SET evidence=X'00'",
        "DELETE FROM workflow_gate_outbox",
        "UPDATE workflow_gate_outbox SET evidence=X'00'",
        "UPDATE workflow_execution_outbox SET request=X'00'",
        "UPDATE workflow_work_items SET payload=X'00'",
    ] {
        let f = Fixture::new();
        assert!(f.read().unwrap().is_some());
        let connection = Connection::open(&f.path).unwrap();
        assert!(connection.execute(sql,[]).unwrap() > 0);
        assert!(f.read().is_err(),"{sql}");
        assert!(f.require(&f.work,NOW+11).is_err(),"{sql}");
    }
}

#[test]
fn archived_execution_stays_provable_while_a_new_revision_is_pending_and_after_reopen() {
    let mut f = Fixture::new();
    let revision = ExecutionRevisionV1::from_completed_work(&f.done,"2".repeat(64)).unwrap();
    let mut next = f.plan.clone();
    next.plan_id = Uuid::new_v4();
    next.created_at_unix_ms = NOW + 20;
    next.steps[0].step_id = Uuid::new_v4();
    next.steps[0].invocation_id = Uuid::new_v4();
    let next = next.bind_digest().unwrap();
    f.store.admit_revision_plan(&next,&revision,&f.source.provider_source.grant.authority,NOW+20).unwrap();
    for _ in 0..2 {
        assert_eq!(f.read().unwrap(),Some(f.source.clone()));
        f.require(&f.work,NOW+11).unwrap();
        f.store = WorkflowStore::open(&f.path).unwrap();
    }
    let connection = Connection::open(&f.path).unwrap();
    assert_eq!(connection.execute("UPDATE workflow_operations SET request_digest='invalid' WHERE operation_namespace GLOB 'execution-history:*'",[]).unwrap(),1);
    assert!(f.read().is_err());
}

#[test]
fn missing_execution_with_retained_outbox_is_corrupt_not_authority_absence() {
    let f = Fixture::new();
    let connection = Connection::open(&f.path).unwrap();
    assert_eq!(connection.execute("DELETE FROM workflow_work_items",[]).unwrap(),1);
    assert!(f.read().is_err());
    assert!(f.require(&f.work,NOW+11).is_err());
}

#[test]
fn proposed_completion_without_recorded_independent_gate_is_not_a_producer() {
    let f = Fixture::with_recorded_gate(false);
    assert_eq!(f.done.state,WorkItemState::InReview);
    assert_eq!(f.store.pending_gate_evidence(10).unwrap().len(),1);
    assert_eq!(f.read().unwrap(),None);
    // A caller-supplied Done DTO with the expected digests is not gate authority.
    assert_eq!(f.work.state,CompanyWorkStateV1::Done);
    assert!(f.require(&f.work,NOW+11).is_err());
}
