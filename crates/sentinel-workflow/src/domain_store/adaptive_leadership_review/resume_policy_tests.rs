use super::*;
use crate::{AdaptiveModelAdmissionV1, AdaptiveResumePolicyReceiptV1};

fn operator(f: &Fixture) -> AuthenticatedCompanyPrincipalV1 {
    let mut principal = f.leader.clone();
    principal.kind = CompanyPrincipalKindV1::Operator;
    principal.agent_id = None;
    principal
}

fn unknown_context(f: &mut Fixture, now: u64) {
    let source = session(f);
    let AdaptiveCursorV1::ModelUnknown { effect } = &source.cursor else {
        panic!("unknown source");
    };
    f.grant.schema_version = 2;
    f.grant.subject = Some(AdaptiveLeadershipReviewSubjectV2::UnknownModel {
        effect: effect.clone(),
        sealed_unknown_proof_digest: DIGEST.into(),
    });
    f.grant.expected_reason_code.clear();
    f.context.evidence_refs = vec![
        format!("adaptive-model-unknown:{}:{}", effect.id, effect.request_digest),
        format!("sealed-provider-unknown:{DIGEST}"),
    ];
    f.context.source_session = source;
    f.context.source_project = f.store
        .company_project(&f.leader.tenant_id, &f.grant.project_id).unwrap().unwrap();
    f.grant.expected_project_version = f.context.source_project.version;
    f.grant.expected_session_version = f.context.source_session.version;
    f.grant.expires_at_unix_ms = now + 120_000;
    f.grant.recovery_epoch = None;
    f.grant.resume_policy = None;
    rebind_budget_evidence(f);
}

// Reach the reported heads through the journal, without changing any counters.
fn reported_head(unknown: bool) -> (Fixture, u64) {
    let mut f = continuation_fixture_with_calls(unknown, !unknown, 16);
    let first = dispatch_continuation(&f);
    let result = continue_result(&first);
    f.store.complete_adaptive_leadership_review_call(
        &f.leader, &result, CONTINUATION_AT + 2,
    ).unwrap();
    observe_budget_inspection(&f, session(&f), CONTINUATION_AT + 3);
    budget_context(&mut f, CONTINUATION_AT + 7, "second-window");
    let second = dispatch_budget(&f, CONTINUATION_AT + 7);
    let result = budget_result(&second, 1, CONTINUATION_AT + 9);
    f.store.complete_adaptive_leadership_review_call(
        &f.leader, &result, CONTINUATION_AT + 9,
    ).unwrap();
    let mut source = session(&f);
    let now = if unknown {
        let effect = AdaptiveEffectV1 { id: Uuid::new_v4(), request_digest: DIGEST.into() };
        for (offset, command) in [
            AdaptiveTransitionV1::ClaimModel {
                effect: effect.clone(),
                previous_observation_digest: source.last_observation.as_ref()
                    .map(|observation| observation.observation_digest.clone()),
            },
            AdaptiveTransitionV1::MarkUnknown { effect },
        ].iter().enumerate() {
            source = f.store.advance_adaptive_session(
                source.grant.session_id, source.version, Uuid::new_v4(), command,
                &f.grant.assignee_authority, CONTINUATION_AT + 10 + offset as u64,
            ).unwrap().1;
        }
        unknown_context(&mut f, CONTINUATION_AT + 12);
        CONTINUATION_AT + 12
    } else {
        let next = source.active_deadline_ms();
        budget_context(&mut f, next, "third-window");
        let third = dispatch_budget(&f, next);
        let result = budget_result(&third, 1, next + 2);
        f.store.complete_adaptive_leadership_review_call(&f.leader, &result, next + 2)
            .unwrap();
        let next = session(&f).active_deadline_ms();
        budget_context(&mut f, next, "fourth-window-source");
        next
    };
    let source = session(&f);
    assert_eq!((source.version, source.tool_calls, source.grant.max_model_calls), (11, 1, 16));
    assert_eq!(source.model_calls, if unknown { 3 } else { 2 });
    assert_eq!(source.continuation.as_ref().unwrap().authorizations.len(),
        if unknown { 2 } else { 3 });
    (f, now)
}

fn amend(f: &Fixture, now: u64, slots: u16) -> AdaptiveResumePolicyReceiptV1 {
    let principal = operator(f);
    let operation = Uuid::new_v4();
    let mut request = if matches!(f.context.source_session.cursor, AdaptiveCursorV1::ModelUnknown { .. }) {
        f.store.adaptive_resume_policy_draft_with_unknown_proof(
            &principal, &f.grant.project_id, f.grant.session_id, operation,
            "operator-approved-resume", now + 3_600_000, now, DIGEST,
        ).unwrap()
    } else {
        f.store.adaptive_resume_policy_draft(
            &principal, &f.grant.project_id, f.grant.session_id, operation,
            "operator-approved-resume", now + 3_600_000, now,
        ).unwrap()
    };
    request.limits.total_review_ceiling = request.source.base_review_count + slots;
    request.limits.total_window_ceiling = request.source.base_window_count + slots;
    let (_, receipt) = f.store.authorize_adaptive_resume_policy_with_unknown_proof(
        &principal, &request, now, |_, _| Ok(DIGEST.into()),
    ).unwrap();
    receipt
}

fn bind(f: &mut Fixture, receipt: &AdaptiveResumePolicyReceiptV1, ordinal: u16) {
    let binding = receipt.binding(ordinal).unwrap();
    f.context.evidence_refs.retain(|reference| !reference.starts_with("adaptive-resume-policy:"));
    f.context.evidence_refs.push(format!(
        "adaptive-resume-policy:{}:{}", binding.receipt_digest, binding.ordinal,
    ));
    f.grant.resume_policy = Some(Box::new(binding));
    rebind_budget_evidence(f);
}

fn issue(f: &Fixture, now: u64) -> AdaptiveLeadershipReviewCallV1 {
    f.store.authorize_resume_leadership_review(
        &f.leader, Uuid::new_v4(), &format!("resume-review-{}", f.grant.review_id),
        &f.grant, &f.context, now,
    ).unwrap()
}

fn result(call: &AdaptiveLeadershipReviewCallV1, now: u64, calls: u16)
    -> CompleteAdaptiveLeadershipReviewCallV1
{
    let window = 300_000;
    let mut result = completion(call, false);
    result.decision = AdaptiveLeadershipReviewDecisionV1 {
        schema_version: call.grant.schema_version,
        decision: AdaptiveLeadershipReviewDecisionKindV1::Continue {
            additional_model_calls: calls, window_ms: window,
            rationale: "Use only the original unspent authority under the declared policy".into(),
            evidence_refs: vec![call.context.evidence_refs[0].clone()],
        },
    };
    let audit = adaptive_leadership_continuation_audit_id(
        call.grant.review_id, &result.request_digest, &result.model_response_digest,
        &result.decision,
    ).unwrap();
    let allowance = call.continuation_allowance(now, now + window, calls).unwrap();
    let (source, abandoned) = match call.grant.subject.as_ref().unwrap() {
        AdaptiveLeadershipReviewSubjectV2::UnknownModel { effect, .. } =>
            (crate::AdaptiveContinuationSourceV1::ModelUnknown, Some(effect.clone())),
        AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget } => (
            crate::AdaptiveContinuationSourceV1::BudgetWindowExhausted {
                active_allowance_digest: budget.active_allowance_digest.clone(),
                continuation_history_digest: budget.continuation_history_digest.clone(),
            }, None,
        ),
        _ => panic!("policy subject"),
    };
    result.resolution_event_id = Some(audit);
    result.continuation = Some(crate::AdaptiveContinuationAuthorizationV1 {
        schema_version: 1, operation_id: call.operation_id, review_id: call.grant.review_id,
        resolution_event_id: audit, session_id: call.grant.session_id,
        source_session_version: call.grant.expected_session_version, source,
        abandoned_model_effect: abandoned, provider_allowance_id: allowance.allowance_id.clone(),
        provider_authority_digest: adaptive_leadership_continuation_provider_authority_digest(
            &allowance, &call.grant.assignee_authority,
        ).unwrap(),
        issued_at_ms: now, deadline_ms: now + window, additional_model_calls: calls,
        local_adoption: None, resume_policy: call.grant.resume_policy.clone(),
        work_funding: None,
    });
    result
}

fn journal(f: &Fixture) -> Vec<(String, String, Vec<u8>, i64)> {
    let connection = f.store.connection.lock().unwrap();
    let mut statement = connection.prepare(
        "SELECT operation_id,request_digest,response,created_at_ms FROM workflow_operations WHERE operation_namespace=?1 ORDER BY operation_id",
    ).unwrap();
    statement.query_map([format!("adaptive-session-v1:{}", f.grant.session_id)],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .unwrap().collect::<Result<Vec<_>, _>>().unwrap()
}

#[test]
fn successor_policy_refusal_survives_project_only_changes_for_both_subjects() {
    for unknown in [false, true] {
        let (mut f, now) = reported_head(false);
        let receipt = amend(&f, now, 3);
        let base = receipt.request.source.base_review_count;
        bind(&mut f, &receipt, base + 1);
        let issued = issue(&f, now);
        let call = f.store.claim_adaptive_leadership_review_call(
            &f.leader, &claim(&issued), now + 1,
        ).unwrap();
        f.store.complete_adaptive_leadership_review_call(
            &f.leader, &result(&call, now + 2, 2), now + 2,
        ).unwrap();
        let resumed = session(&f);
        assert!(resumed.requires_fresh_observation());
        let mut observed = observe_budget_inspection(&f, resumed.clone(), now + 3);
        assert!(!observed.requires_fresh_observation());
        assert_eq!(observed.grant, resumed.grant);
        assert_eq!((observed.model_calls, observed.tool_calls),
            (resumed.model_calls + 1, resumed.tool_calls + 1));
        if unknown {
            let effect = AdaptiveEffectV1 { id: Uuid::new_v4(), request_digest: DIGEST.into() };
            let commands = [
                AdaptiveTransitionV1::ClaimModel {
                    effect: effect.clone(),
                    previous_observation_digest: observed.last_observation.as_ref()
                        .map(|observation| observation.observation_digest.clone()),
                },
                AdaptiveTransitionV1::MarkUnknown { effect },
            ];
            for (offset, command) in commands.iter().enumerate() {
                observed = f.store.advance_adaptive_session(
                    observed.grant.session_id, observed.version, Uuid::new_v4(), command,
                    &f.grant.assignee_authority, now + 7 + offset as u64,
                ).unwrap().1;
            }
        }
        let refusal_at = observed.active_deadline_ms();
        if unknown { unknown_context(&mut f, refusal_at); }
        else { budget_context(&mut f, refusal_at, "successor-refusal"); }
        assert!(f.context.source_session.version > receipt.request.source.expected_session_version);
        bind(&mut f, &receipt, base + 2);
        let issued = issue(&f, refusal_at);
        let refused = f.store.claim_adaptive_leadership_review_call(
            &f.leader, &claim(&issued), refusal_at + 1,
        ).unwrap();
        let mut refusal = completion(&refused, false);
        refusal.decision = AdaptiveLeadershipReviewDecisionV1 {
            schema_version: refused.grant.schema_version,
            decision: if unknown {
                AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
                    rationale: "Do not automatically retry this successor head".into(),
                    evidence_refs: vec![refused.context.evidence_refs[0].clone()],
                }
            } else {
                AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
                    rationale: "Do not automatically retry this successor head".into(),
                    evidence_refs: vec![refused.context.evidence_refs[0].clone()],
                }
            },
        };
        f.store.complete_adaptive_leadership_review_call(&f.leader, &refusal, refusal_at + 2)
            .unwrap();
        let head = session(&f);
        change_project(&f, refusal_at + 3);
        if unknown { unknown_context(&mut f, refusal_at + 4); }
        else { budget_context(&mut f, refusal_at + 4, "changed-project-same-refused-head"); }
        bind(&mut f, &receipt, base + 3);
        assert_eq!(f.context.source_session, head);
        assert_ne!(f.context.source_project, refused.context.source_project);
        f.context.validate(&f.grant).unwrap();
        let before = rows(&f.store);
        assert!(f.store.authorize_resume_leadership_review(
            &f.leader, Uuid::new_v4(), "no-project-only-refusal-loop",
            &f.grant, &f.context, refusal_at + 4,
        ).is_err());
        assert_eq!(rows(&f.store), before);
        assert_eq!(session(&f), head);
    }
}

#[test]
fn reported_unknown_and_ready_heads_resume_without_rewriting_or_refunding() {
    for unknown in [true, false] {
        let (mut f, now) = reported_head(unknown);
        let before = session(&f);
        let history = journal(&f);
        let receipt = amend(&f, now, 2);
        bind(&mut f, &receipt, receipt.request.source.base_review_count + 1);
        let issued = issue(&f, now);
        let call = f.store.claim_adaptive_leadership_review_call(
            &f.leader, &claim(&issued), now + 1,
        ).unwrap();
        let completion = result(&call, now + 2, 2);
        {
            let connection = f.store.connection.lock().unwrap();
            let mut forged = completion.continuation.as_ref().unwrap().clone();
            forged.operation_id = Uuid::new_v4();
            assert!(crate::domain_store::adaptive_resume_policy::require_resume_authorization_membership(
                &connection, &forged, &call.grant.assignee_authority,
            ).is_err(), "historical leaf membership must bind the exact operation");
        }
        let completed = f.store.complete_adaptive_leadership_review_call(
            &f.leader, &completion, now + 2,
        ).unwrap();
        let resumed = session(&f);
        assert_eq!(resumed.grant, before.grant);
        assert_eq!((resumed.model_calls, resumed.tool_calls), (before.model_calls, before.tool_calls));
        assert_eq!(resumed.effective_call_duration_ms(), 120_000);
        assert!(resumed.requires_fresh_observation());
        assert_eq!(resumed.last_observation, before.last_observation);
        let old = &before.continuation.as_ref().unwrap().authorizations;
        let new = &resumed.continuation.as_ref().unwrap().authorizations;
        assert_eq!(&new[..old.len()], old.as_slice());
        assert_eq!(&journal(&f)[..history.len()], history.as_slice());
        assert_eq!(new.len(), if unknown { 3 } else { 4 });
        if let AdaptiveCursorV1::ModelUnknown { effect } = &before.cursor {
            assert!(resumed.is_abandoned_model_effect(effect));
        }
        assert_eq!(resumed.model_admission_at(now + 3), AdaptiveModelAdmissionV1::Admissible);
        let durable = rows(&f.store);
        assert_eq!(f.store.complete_adaptive_leadership_review_call(
            &f.leader, &completion, receipt.request.limits.expires_at_unix_ms + 1,
        ).unwrap(), completed);
        assert_eq!(rows(&f.store), durable);
    }
}

#[test]
fn forged_binding_or_noncontiguous_ordinal_is_atomic_and_unbound_fresh_issue_is_denied() {
    for target in ["digest", "limits", "ordinal", "legacy"] {
        let (mut f, now) = reported_head(false);
        let receipt = amend(&f, now, 2);
        bind(&mut f, &receipt, receipt.request.source.base_review_count + 1);
        match target {
            "digest" => f.grant.resume_policy.as_mut().unwrap().receipt_digest = "f".repeat(64),
            "limits" => f.grant.resume_policy.as_mut().unwrap().limits.total_review_ceiling += 1,
            "ordinal" => f.grant.resume_policy.as_mut().unwrap().ordinal += 1,
            "legacy" => f.grant.resume_policy = None,
            _ => unreachable!(),
        }
        rebind_budget_evidence(&mut f);
        let before = rows(&f.store);
        assert!(f.store.authorize_adaptive_leadership_review_call(
            &f.leader, Uuid::new_v4(), "forged-review", &f.grant, &f.context, now,
        ).is_err(), "{target}");
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn final_clock_slack_rejection_creates_no_journal_counter_or_effect_and_replay_skips_clock() {
    let (mut f, now) = reported_head(false);
    let receipt = amend(&f, now, 2);
    bind(&mut f, &receipt, receipt.request.source.base_review_count + 1);
    let issued = issue(&f, now);
    let call = f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&issued), now + 1)
        .unwrap();
    let completion = result(&call, now + 2, 2);
    f.store.complete_adaptive_leadership_review_call(&f.leader, &completion, now + 2).unwrap();
    let source = session(&f);
    let threshold = source.active_deadline_ms() - 121_000;
    assert_eq!(source.model_admission_at(threshold), AdaptiveModelAdmissionV1::Admissible);
    assert_eq!(source.model_admission_at(threshold + 1), AdaptiveModelAdmissionV1::InsufficientSlack);
    assert!(source.model_window_exhausted_at(threshold + 1));
    let command = AdaptiveTransitionV1::ClaimModel {
        effect: AdaptiveEffectV1 { id: Uuid::new_v4(), request_digest: DIGEST.into() },
        previous_observation_digest: source.last_observation.as_ref()
            .map(|observation| observation.observation_digest.clone()),
    };
    let operation = Uuid::new_v4();
    let before = rows(&f.store);
    let mut samples = 0;
    assert!(f.store.advance_adaptive_session_with_clock(
        source.grant.session_id, source.version, operation, &command, &f.grant.assignee_authority,
        || { samples += 1; threshold + 1 },
    ).is_err());
    assert_eq!(samples, 1);
    assert_eq!(rows(&f.store), before);
    assert_eq!(session(&f), source);
    let pending = f.store.advance_adaptive_session_with_clock(
        source.grant.session_id, source.version, operation, &command, &f.grant.assignee_authority,
        || threshold,
    ).unwrap();
    assert!(!pending.0);
    assert_eq!(pending.1.model_calls, source.model_calls + 1);
    assert_eq!(pending.1.effective_call_duration_ms(), 120_000);
    assert!(pending.1.active_deadline_ms() >= threshold + 120_000 + 1_000);
    let durable = rows(&f.store);
    let replay = f.store.advance_adaptive_session_with_clock(
        source.grant.session_id, source.version, operation, &command, &f.grant.assignee_authority,
        || panic!("immutable replay must not resample time"),
    ).unwrap();
    assert!(replay.0);
    assert_eq!(replay.1, pending.1);
    assert_eq!(rows(&f.store), durable);
}

#[test]
fn insufficient_slack_is_an_exact_budget_subject_before_deadline_or_call_exhaustion() {
    let (mut f, now) = reported_head(false);
    let receipt = amend(&f, now, 2);
    let base = receipt.request.source.base_review_count;
    bind(&mut f, &receipt, base + 1);
    let issued = issue(&f, now);
    let call = f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&issued), now + 1)
        .unwrap();
    f.store.complete_adaptive_leadership_review_call(&f.leader, &result(&call, now + 2, 2), now + 2)
        .unwrap();
    let source = session(&f);
    let late = source.active_deadline_ms() - 120_999;
    budget_context(&mut f, late, "insufficient-dispatch-slack");
    bind(&mut f, &receipt, base + 2);
    let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) = &f.grant.subject
    else { panic!("budget subject"); };
    assert!(budget.dispatch_slack_insufficient);
    assert!(!budget.model_calls_exhausted);
    assert!(!budget.deadline_expired);
    budget.validate(&f.context).unwrap();
    let call = issue(&f, late);
    assert!(call.dispatch.is_none());
    assert_eq!(session(&f), source);
}

#[test]
fn tickets_are_consumed_on_undispatched_expiry_and_stop_at_the_total_ceiling() {
    let (mut f, now) = reported_head(false);
    let receipt = amend(&f, now, 2);
    let base = receipt.request.source.base_review_count;
    bind(&mut f, &receipt, base + 1);
    let first = issue(&f, now);
    bind(&mut f, &receipt, base + 2);
    let before = rows(&f.store);
    assert!(f.store.authorize_resume_leadership_review(
        &f.leader, Uuid::new_v4(), "concurrent-policy-ticket", &f.grant, &f.context, now + 1,
    ).is_err());
    assert_eq!(rows(&f.store), before);
    let expiry = first.grant.expires_at_unix_ms;
    f.store.expire_adaptive_leadership_review_call(
        &f.leader, first.grant.review_id, first.version, expiry,
    ).unwrap();
    budget_context(&mut f, expiry, "second-policy-ticket");
    bind(&mut f, &receipt, base + 2);
    let second = issue(&f, expiry);
    assert!(second.dispatch.is_none());
    let durable = rows(&f.store);
    assert_eq!(f.store.authorize_resume_leadership_review(
        &f.leader, second.operation_id, &second.allowance_id, &second.grant,
        &second.context, receipt.request.limits.expires_at_unix_ms + 1,
    ).unwrap(), second);
    assert_eq!(rows(&f.store), durable);
    assert!(receipt.binding(base + 3).is_err());
    assert_eq!(session(&f).model_calls, 2);
    assert_eq!(session(&f).tool_calls, 1);
}

#[test]
fn refusal_prevents_automatic_exact_source_reissue_and_changed_source_cannot_dispatch() {
    for (unknown, stale) in [(false, false), (true, false), (false, true)] {
        let (mut f, now) = reported_head(unknown);
        let receipt = amend(&f, now, 2);
        bind(&mut f, &receipt, receipt.request.source.base_review_count + 1);
        let issued = issue(&f, now);
        if stale {
            change_project(&f, now + 1);
            let before = rows(&f.store);
            assert!(f.store.claim_adaptive_leadership_review_call(
                &f.leader, &claim(&issued), now + 2,
            ).is_err());
            assert_eq!(rows(&f.store), before);
        } else {
            let call = f.store.claim_adaptive_leadership_review_call(
                &f.leader, &claim(&issued), now + 1,
            ).unwrap();
            let mut refusal = completion(&call, false);
            refusal.decision = AdaptiveLeadershipReviewDecisionV1 {
                schema_version: call.grant.schema_version,
                decision: if unknown {
                    AdaptiveLeadershipReviewDecisionKindV1::KeepUnknown {
                        rationale: "Retain this exact unknown without another automatic review".into(),
                        evidence_refs: vec![call.context.evidence_refs[0].clone()],
                    }
                } else {
                    AdaptiveLeadershipReviewDecisionKindV1::DeferBudget {
                        rationale: "No further work on this exact source".into(),
                        evidence_refs: vec![call.context.evidence_refs[0].clone()],
                    }
                },
            };
            f.store.complete_adaptive_leadership_review_call(&f.leader, &refusal, now + 2).unwrap();
            bind(&mut f, &receipt, receipt.request.source.base_review_count + 2);
            let before = rows(&f.store);
            assert!(f.store.authorize_resume_leadership_review(
                &f.leader, Uuid::new_v4(), "refused-source", &f.grant, &f.context, now + 3,
            ).is_err());
            assert_eq!(rows(&f.store), before);
        }
    }
}

#[test]
fn persisted_policy_and_membership_corruption_fail_closed_without_review_recursion() {
    for (kind, reseal_row) in [
        ("adaptive_resume_policy", false), ("adaptive_resume_review_membership", false),
        ("adaptive_resume_policy", true), ("adaptive_resume_review_membership", true),
    ] {
        let (mut f, now) = reported_head(false);
        let receipt = amend(&f, now, 2);
        bind(&mut f, &receipt, receipt.request.source.base_review_count + 1);
        let issued = issue(&f, now);
        {
            let connection = f.store.connection.lock().unwrap();
            if reseal_row {
                let payload: Vec<u8> = connection.query_row(
                    "SELECT payload FROM company_entities WHERE tenant_id=?1 AND entity_kind=?2",
                    params![f.leader.tenant_id.0, kind], |row| row.get(0),
                ).unwrap();
                let mut value: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                if kind == "adaptive_resume_policy" {
                    value["request"]["reason_ref"] = serde_json::json!("changed-reason");
                } else {
                    value["membership"]["context_digest"] = serde_json::json!("f".repeat(64));
                }
                let payload = serde_json::to_vec(&value).unwrap();
                let digest = bytes_digest("sentinel.workflow.company-entity-row.v1", &payload).unwrap();
                assert_eq!(connection.execute(
                    "UPDATE company_entities SET payload=?1,payload_digest=?2 WHERE tenant_id=?3 AND entity_kind=?4",
                    params![payload, digest, f.leader.tenant_id.0, kind],
                ).unwrap(), 1);
            } else {
                assert_eq!(connection.execute(
                    "UPDATE company_entities SET payload=X'00' WHERE tenant_id=?1 AND entity_kind=?2",
                    params![f.leader.tenant_id.0, kind],
                ).unwrap(), 1);
            }
        }
        let before = rows(&f.store);
        assert!(f.store.claim_adaptive_leadership_review_call(
            &f.leader, &claim(&issued), now + 1,
        ).is_err(), "{kind}");
        assert_eq!(rows(&f.store), before);
    }
}

#[test]
fn journal_replay_needs_sealed_membership_not_completed_review_bodies() {
    let (mut f, now) = reported_head(false);
    let receipt = amend(&f, now, 2);
    bind(&mut f, &receipt, receipt.request.source.base_review_count + 1);
    let issued = issue(&f, now);
    let call = f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&issued), now + 1)
        .unwrap();
    f.store.complete_adaptive_leadership_review_call(&f.leader, &result(&call, now + 2, 2), now + 2)
        .unwrap();
    let expected = session(&f);
    {
        let connection = f.store.connection.lock().unwrap();
        assert_eq!(connection.execute(
            "UPDATE company_entities SET payload=X'00' WHERE tenant_id=?1 AND entity_kind=?2 AND entity_id=?3",
            params![f.leader.tenant_id.0, KIND, call.review_key],
        ).unwrap(), 1);
    }
    assert_eq!(session(&f), expected);
    {
        let connection = f.store.connection.lock().unwrap();
        assert_eq!(connection.execute(
            "UPDATE company_entities SET payload=X'00' WHERE tenant_id=?1 AND entity_kind='adaptive_resume_review_membership'",
            [f.leader.tenant_id.0.as_str()],
        ).unwrap(), 1);
    }
    assert!(f.store.adaptive_session(f.grant.session_id, &f.grant.assignee_authority).is_err());
}

#[test]
fn legacy_grants_authorizations_and_budget_flags_keep_the_same_serialized_bytes() {
    let f = budget_fixture(16);
    let grant = serde_json::to_value(&f.grant).unwrap();
    assert!(grant.get("resume_policy").is_none());
    let subject = grant.get("subject").unwrap().get("budget").unwrap();
    assert!(subject.get("dispatch_slack_insufficient").is_none());
    let encoded = serde_json::to_vec(&f.grant).unwrap();
    let decoded: AdaptiveLeadershipReviewGrantV1 = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), encoded);
    let authorization = f.context.source_session.continuation.as_ref().unwrap()
        .authorizations.first().unwrap();
    let value = serde_json::to_value(authorization).unwrap();
    assert!(value.get("resume_policy").is_none());
    let bytes = serde_json::to_vec(authorization).unwrap();
    let decoded: crate::AdaptiveContinuationAuthorizationV1 = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(serde_json::to_vec(&decoded).unwrap(), bytes);
    assert_eq!(session(&f).model_admission_at(CONTINUATION_AT + 7),
        AdaptiveModelAdmissionV1::CallsExhausted);
}

fn legacy_epoch_source() -> (Fixture, crate::AdaptiveLeadershipRecoveryRequestV1, u64) {
    let mut f = continuation_fixture_with_calls(true, false, 16);
    let mut retired = Vec::new();
    let mut now = CONTINUATION_AT;
    for index in 0..ADAPTIVE_LEADERSHIP_MAX_REVIEWS {
        f.grant.expires_at_unix_ms = now + 1_000;
        f.context.evidence_refs.push(format!("legacy-review:{index}"));
        rebind_budget_evidence(&mut f);
        let issued = f.store.authorize_adaptive_leadership_review_call(
            &f.leader, Uuid::new_v4(), &format!("legacy-slot-{index}"),
            &f.grant, &f.context, now,
        ).unwrap();
        let dispatched = f.store.claim_adaptive_leadership_review_call(
            &f.leader, &claim(&issued), now + 1,
        ).unwrap();
        retired.push(f.store.expire_adaptive_leadership_review_call(
            &f.leader, issued.grant.review_id, dispatched.version, now + 1_000,
        ).unwrap());
        now += 1_001;
    }
    f.grant.expires_at_unix_ms = now + 120_000;
    let source = &f.context.source_session;
    let AdaptiveCursorV1::ModelUnknown { effect } = &source.cursor else {
        panic!("unknown source");
    };
    let head = crate::store::adaptive::load(&f.store.connection.lock().unwrap(), f.grant.session_id)
        .unwrap().unwrap().1;
    let request = crate::AdaptiveLeadershipRecoveryRequestV1 {
        schema_version: 1, operation_id: Uuid::new_v4(), tenant_id: f.leader.tenant_id.clone(),
        project_id: f.grant.project_id.clone(), work_item_id: f.grant.work_item_id.clone(),
        session_id: f.grant.session_id, expected_project_version: f.grant.expected_project_version,
        expected_session_version: source.version, session_head_digest: head,
        session_digest: crate::adaptive_leadership_recovery_session_digest(source).unwrap(),
        project_digest: crate::adaptive_leadership_recovery_project_digest(&f.context.source_project).unwrap(),
        unknown_effect: Some(effect.clone()), sealed_unknown_proof_digest: Some(DIGEST.into()),
        blocked_subject: None, admission_repair: None,
        prior_review_history_digest: crate::adaptive_leadership_recovery_history_digest(&retired).unwrap(),
        repair_digest: "b".repeat(64),
        release: crate::AdaptiveRecoveryReleaseV1 {
            schema_version: 1, source_git_sha: "c".repeat(40),
            release_manifest_digest: "d".repeat(64), gateway_binary_digest: "e".repeat(64),
        },
        reason_ref: "declared-legacy-repair".into(), expires_at_unix_ms: now + 120_000,
        max_additional_model_calls: 1, max_window_ms: 120_000,
    };
    f.context.evidence_refs.push(format!("recovery-request:{}", request.canonical_digest().unwrap()));
    rebind_budget_evidence(&mut f);
    (f, request, now)
}

#[test]
fn amendment_denies_direct_legacy_epoch_and_repair_issuance_before_verifier_or_writes() {
    let (f, request, now) = legacy_epoch_source();
    amend(&f, now, 2);
    let before = rows(&f.store);
    let error = f.store.authorize_adaptive_leadership_recovery_epoch(
        &operator(&f), &request, &f.grant, &f.context, now,
    ).unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
    let mut repair = request.clone();
    repair.schema_version = 3;
    repair.admission_repair = Some(crate::AdaptiveLeadershipAdmissionRepairV1 {
        schema_version: 1, source_digest: "b".repeat(64), disposition_digest: "c".repeat(64),
        failed_release: request.release.clone(),
    });
    let error = f.store.authorize_adaptive_leadership_admission_repair(
        &operator(&f), &repair, &f.grant, &f.context, now,
        |_, _| panic!("policy denial must precede legacy repair verification"),
    ).unwrap_err();
    assert_eq!(error.code, WorkflowErrorCode::AuthorityConflict);
    assert_eq!(rows(&f.store), before);
}

#[test]
fn amendment_preserves_exact_historical_epoch_and_review_replay_but_denies_new_dispatch() {
    let (f, request, now) = legacy_epoch_source();
    let (_, epoch) = f.store.authorize_adaptive_leadership_recovery_epoch(
        &operator(&f), &request, &f.grant, &f.context, now,
    ).unwrap();
    let call = f.store.adaptive_leadership_review_call(&f.leader.tenant_id, epoch.review_id)
        .unwrap().unwrap();
    let now = call.grant.expires_at_unix_ms;
    let call = f.store.expire_adaptive_leadership_review_call(
        &f.leader, call.grant.review_id, call.version, now,
    ).unwrap();
    amend(&f, now, 2);
    let before = rows(&f.store);
    assert_eq!(f.store.authorize_adaptive_leadership_recovery_epoch(
        &operator(&f), &request, &f.grant, &f.context, now + 1,
    ).unwrap(), (true, epoch));
    assert_eq!(f.store.adaptive_leadership_review_call(&f.leader.tenant_id, call.grant.review_id)
        .unwrap(), Some(call.clone()));
    assert!(f.store.claim_adaptive_leadership_review_call(&f.leader, &claim(&call), now + 1)
        .is_err());
    assert_eq!(rows(&f.store), before);
}

#[test]
fn successive_bound_windows_reach_the_finite_total_and_cannot_grant_past_root_calls() {
    let (mut f, mut now) = reported_head(false);
    let receipt = amend(&f, now, 2);
    let base = receipt.request.source.base_review_count;
    for index in 1..=2 {
        bind(&mut f, &receipt, base + index);
        let issued = issue(&f, now);
        let call = f.store.claim_adaptive_leadership_review_call(
            &f.leader, &claim(&issued), now + 1,
        ).unwrap();
        let remaining = 16 - session(&f).model_calls;
        assert!(call.continuation_allowance(now + 2, now + 300_002, remaining + 1).is_err());
        assert!(call.continuation_allowance(now + 2, now + 120_002, 1).is_err());
        let completion = result(&call, now + 2, remaining);
        f.store.complete_adaptive_leadership_review_call(&f.leader, &completion, now + 2).unwrap();
        let resumed = session(&f);
        assert_eq!(resumed.active_model_ceiling(), 16);
        assert_eq!((resumed.model_calls, resumed.tool_calls), (2, 1));
        now = resumed.active_deadline_ms();
        budget_context(&mut f, now, &format!("next-bound-window:{index}"));
    }
    assert_eq!(session(&f).continuation.as_ref().unwrap().authorizations.len(), 5);
    assert!(receipt.binding(base + 3).is_err());
    let before = rows(&f.store);
    assert!(f.store.authorize_resume_leadership_review(
        &f.leader, Uuid::new_v4(), "exhausted-policy", &f.grant, &f.context, now,
    ).is_err());
    assert_eq!(rows(&f.store), before);
}
