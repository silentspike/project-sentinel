fn diagnose_funded_leadership_source(store: &super::WorkflowStore, now: u64) {
    use crate::{AdaptiveLeadershipReviewSubjectV2, AdaptiveModelAdmissionV1};
    let session_id = std::env::var("SENTINEL_DIAGNOSTIC_SESSION_ID")
        .expect("explicit target session required").parse::<uuid::Uuid>().unwrap();
    let tenant = crate::TenantId::parse(std::env::var("SENTINEL_DIAGNOSTIC_TENANT")
        .expect("explicit tenant required")).unwrap();
    let calls = store.adaptive_leadership_review_calls(&tenant, session_id).unwrap();
    let latest = calls.iter().max_by_key(|call| call.created_at_unix_ms).unwrap();
    let session = store.adaptive_session(session_id, &latest.grant.assignee_authority)
        .unwrap().unwrap();
    let project = store.company_project(&tenant, &latest.grant.project_id).unwrap().unwrap();
    let epoch = store.adaptive_work_funding_for_review(&tenant, session_id, now).unwrap().unwrap();
    let mut grant = latest.grant.clone();
    let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) = &mut grant.subject
    else { panic!("explicit budget review required"); };
    budget.observed_at_ms = now;
    budget.model_calls_exhausted = session.model_calls >= session.active_model_ceiling();
    budget.deadline_expired = now >= session.active_deadline_ms();
    budget.dispatch_slack_insufficient = session.model_admission_at(now) == AdaptiveModelAdmissionV1::InsufficientSlack;
    budget.active_allowance_digest = crate::adaptive_budget_allowance_digest(project.subscription_call.as_ref().unwrap()).unwrap();
    budget.continuation_history_digest = crate::adaptive_budget_history_digest(&session.continuation).unwrap();
    let mut context = latest.context.clone();
    context.source_project = project;
    context.source_session = session;
    context.evidence_refs.retain(|value| ![
        "project:", "adaptive-session:", "adaptive-budget-root:", "adaptive-budget-current:",
        "adaptive-budget-history:", "adaptive-work-funding:", "workbench-observation:",
        "adaptive-model-result:", "leadership-review-retired:", "leadership-review-retirement-history:",
    ].iter().any(|prefix| value.starts_with(prefix)));
    context.evidence_refs.extend([
        format!("project:{}:{}", context.source_project.project_id, context.source_project.version),
        format!("adaptive-session:{}:{}", session_id, context.source_session.version),
        format!("adaptive-budget-root:{}:{}", budget.root_allowance.allowance_id, context.source_session.grant.provider_authority_digest),
        format!("adaptive-budget-current:{}", budget.active_allowance_digest),
        format!("adaptive-budget-history:{}", budget.continuation_history_digest),
        epoch.evidence_ref().unwrap(),
    ]);
    if let Some(observation) = &context.source_session.last_observation {
        context.evidence_refs.push(format!("workbench-observation:{}:{}", observation.effect.id, observation.observation_digest));
    }
    grant.schema_version = 5;
    grant.expected_project_version = context.source_project.version;
    grant.expected_session_version = context.source_session.version;
    grant.work_funding = Some(Box::new(epoch));
    grant.recovery_epoch = None;
    grant.resume_policy = None;
    grant.funded_review_recovery = None;
    grant.expires_at_unix_ms = (now + 300_000).min(grant.work_funding.as_ref().unwrap().binding.limits.expires_at_unix_ms);
    grant.evidence_fingerprint = crate::adaptive_leadership_evidence_fingerprint(&context.tool_catalog, &context.evidence_refs).unwrap();
    grant.review_id = crate::adaptive_leadership_review_id(session_id, grant.expected_session_version, &grant.evidence_fingerprint).unwrap();
    grant.validate(now).unwrap();
    let before = serde_json::to_vec(&context).unwrap();
    let history_bytes = serde_json::to_vec(&context.source_session.continuation.as_ref().unwrap().authorizations).unwrap().len();
    context.validate(&grant).unwrap();
    assert_eq!(serde_json::to_vec(&context).unwrap(), before);
    eprintln!("FUNDED_LEADERSHIP_SOURCE context_bytes={} history_bytes={} head_bytes={} windows={} valid=true legacy_whole_context_pass={} writes=0 not_provider_dispatch=true",
        before.len(), history_bytes, before.len() - history_bytes + 2,
        context.source_session.continuation.as_ref().unwrap().authorizations.len(),
        before.len() <= crate::ADAPTIVE_LEADERSHIP_MAX_CONTEXT_BYTES);
}
