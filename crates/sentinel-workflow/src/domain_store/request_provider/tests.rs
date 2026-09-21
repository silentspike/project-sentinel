use super::*;
use sentinel_common::AgentId;
use std::sync::{Arc, Barrier};
use tempfile::TempDir;

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn explicit_abandonment_preserves_consumption_and_allows_only_a_new_bounded_call() {
    let f = fixture();
    let mut g = grant(&f, 1, 1, 1);
    let original = f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &g, 2)
        .unwrap();
    let dispatched = f
        .store
        .claim_request_provider_call(&f.sales, &claim(&original), 3)
        .unwrap();
    let receipt = Uuid::from_u128(200).to_string();
    assert!(f
        .store
        .abandon_request_provider_call(&f.sales, &original.allowance_id, DIGEST, &receipt, 4)
        .is_err());
    assert!(f
        .store
        .abandon_request_provider_call(
            &f.operator,
            &original.allowance_id,
            &"b".repeat(64),
            &receipt,
            4
        )
        .is_err());
    let abandoned = f
        .store
        .abandon_request_provider_call(&f.operator, &original.allowance_id, DIGEST, &receipt, 4)
        .unwrap();
    assert_eq!(abandoned.dispatch, dispatched.dispatch);
    assert_eq!(abandoned.grant, dispatched.grant);
    assert!(abandoned.question_response.is_none());
    assert_eq!(
        f.store
            .abandon_request_provider_call(&f.operator, &original.allowance_id, DIGEST, &receipt, 5)
            .unwrap(),
        abandoned
    );
    assert!(f
        .store
        .abandon_request_provider_call(
            &f.operator,
            &original.allowance_id,
            DIGEST,
            &Uuid::from_u128(201).to_string(),
            5
        )
        .is_err());
    assert!(f
        .store
        .claim_request_provider_call(&f.sales, &claim(&original), 5)
        .is_err());
    assert!(f
        .store
        .adopt_sales_question(&f.sales, &question(&original), 5)
        .is_err());
    assert!(f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(101), &g, 5)
        .is_err());
    let reopened = WorkflowStore::open(f.temp.path().join("workflow.sqlite")).unwrap();
    assert_eq!(
        reopened
            .request_provider_call(&f.operator.tenant_id, &original.allowance_id)
            .unwrap(),
        Some(abandoned)
    );
    g.total_call_limit = 10;
    let next = reopened
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(101), &g, 5)
        .unwrap();
    assert_ne!(next.allowance_id, original.allowance_id);
    reopened
        .claim_request_provider_call(&f.sales, &claim(&next), 6)
        .unwrap();
    assert_eq!(
        all_calls(&reopened.connection.lock().unwrap())
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn expired_unsent_grant_can_be_replaced_but_still_counts_toward_the_ceiling() {
    let f = fixture();
    let mut grant = grant(&f, 1, 1, 1);
    let original = f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &grant, 2)
        .unwrap();
    grant.expires_at_unix_ms = 400_000;
    assert!(f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(101), &grant, 300_000)
        .is_err());
    grant.total_call_limit = 10;
    let replacement = f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(101), &grant, 300_000)
        .unwrap();
    assert_ne!(replacement.allowance_id, original.allowance_id);
    assert_eq!(
        f.store
            .request_provider_call(&f.operator.tenant_id, &original.allowance_id)
            .unwrap(),
        Some(original)
    );
    assert_eq!(
        all_calls(&f.store.connection.lock().unwrap())
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn changing_tenant_does_not_reset_the_store_wide_provider_ceiling() {
    let mut f = fixture();
    let first = grant(&f, 1, 1, 1);
    f.store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &first, 2)
        .unwrap();
    let tenant = TenantId::parse("tenant-other").unwrap();
    f.operator.tenant_id = tenant.clone();
    f.sales.tenant_id = tenant.clone();
    f.customer.tenant_id = tenant;
    let second = grant(&f, 1, 1, 1);
    assert!(f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &second, 2)
        .is_err());
}

struct Fixture {
    temp: TempDir,
    store: WorkflowStore,
    operator: AuthenticatedCompanyPrincipalV1,
    sales: AuthenticatedCompanyPrincipalV1,
    customer: AuthenticatedCompanyPrincipalV1,
}

fn fixture() -> Fixture {
    let temp = TempDir::new().unwrap();
    let store = WorkflowStore::open(temp.path().join("workflow.sqlite")).unwrap();
    let operator = AuthenticatedCompanyPrincipalV1 {
        schema_version: 1,
        tenant_id: TenantId::parse("tenant-test").unwrap(),
        principal_id: "operator-test".into(),
        kind: CompanyPrincipalKindV1::Operator,
        role: CompanyRoleV1::TechnicalLead,
        customer_id: None,
        agent_id: None,
        authority_generation: 1,
        authority_digest: DIGEST.into(),
    };
    let sales = AuthenticatedCompanyPrincipalV1 {
        principal_id: "sales-test".into(),
        kind: CompanyPrincipalKindV1::Agent,
        role: CompanyRoleV1::Sales,
        agent_id: Some(AgentId(11)),
        ..operator.clone()
    };
    let customer = AuthenticatedCompanyPrincipalV1 {
        principal_id: "customer-test".into(),
        kind: CompanyPrincipalKindV1::Customer,
        role: CompanyRoleV1::Customer,
        customer_id: Some("customer-test".into()),
        ..operator.clone()
    };
    Fixture {
        temp,
        store,
        operator,
        sales,
        customer,
    }
}

fn grant(f: &Fixture, id: u128, limit: u16, concurrency: u16) -> RequestProviderGrantV1 {
    let response = f
        .store
        .apply_company_command(
            &f.customer,
            Uuid::from_u128(id),
            &CompanyWorkflowCommandV1::SubmitCustomerRequest {
                summary_ref: "Studio website".into(),
                desired_outcome: "Three accessible pages".into(),
                constraints: vec![],
            },
            1,
        )
        .unwrap();
    let CompanyWorkflowResponseV1::CustomerRequest(request) = response.response else {
        panic!()
    };
    RequestProviderGrantV1 {
        schema_version: 1,
        request_id: request.request_id,
        expected_version: request.version,
        sales_principal: f.sales.clone(),
        provider: "codex-cli".into(),
        model: "model-test".into(),
        catalog_digest: DIGEST.into(),
        total_call_limit: limit,
        concurrent_call_limit: concurrency,
        max_duration_ms: 120_000,
        token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
        expires_at_unix_ms: 300_000,
    }
}

fn claim(call: &RequestProviderCallV1) -> ClaimRequestProviderCallV1 {
    ClaimRequestProviderCallV1 {
        allowance_id: call.allowance_id.clone(),
        request_id: format!("company-provider-{}", call.allowance_id),
        request_digest: DIGEST.into(),
        context_digest: "b".repeat(64),
    }
}

fn question(call: &RequestProviderCallV1) -> AdoptSalesQuestionV1 {
    AdoptSalesQuestionV1 {
        allowance_id: call.allowance_id.clone(),
        request_digest: DIGEST.into(),
        model_response_digest: "c".repeat(64),
        content: "Which audience should the site address?".into(),
    }
}

fn proposal(call: &RequestProviderCallV1) -> AdoptSalesProposalV1 {
    AdoptSalesProposalV1 {
        allowance_id: call.allowance_id.clone(),
        request_digest: DIGEST.into(),
        model_response_digest: "d".repeat(64),
        binding: ProposalBindingV1 {
            scope: "Design and implement the requested website".into(),
            deliverables: vec!["Validated source tree".into()],
            exclusions: vec!["External hosting".into()],
            acceptance_criteria: vec!["Independent QA passes".into()],
            assumptions: vec!["Customer supplies no external media".into()],
            cost_ceiling_micros: 2_000_000,
            provider_cost_ceilings_micros: BTreeMap::from([("local-loop".into(), 1_000_000)]),
            governance: ProposalGovernanceV1 {
                owner: AgentId(11),
                participants: vec![ParticipantBindingV1 {
                    agent_id: AgentId(11),
                    principal_id: "sales-test".into(),
                    role: CompanyRoleV1::Sales,
                    specialties: BTreeSet::from(["scope_analysis".into()]),
                    reports_to: None,
                    profile: WorkProfileBindingV1 {
                        profile_id: "web-project-v1".into(),
                        generation: 1,
                        digest: DIGEST.into(),
                    },
                }],
                project_profile: WorkProfileBindingV1 {
                    profile_id: "web-project-v1".into(),
                    generation: 1,
                    digest: DIGEST.into(),
                },
            },
            expires_at_unix_ms: 500_000,
        },
    }
}

#[test]
fn request_grant_replay_preserves_source_and_never_resets_allowance() {
    let f = fixture();
    let grant = grant(&f, 1, 1, 1);
    let op = Uuid::from_u128(100);
    let call = f
        .store
        .authorize_request_provider_call(&f.operator, op, &grant, 2)
        .unwrap();
    assert_eq!(
        f.store
            .company_customer_request(&f.customer.tenant_id, &grant.request_id)
            .unwrap()
            .unwrap(),
        call.source_request
    );
    let reopened = WorkflowStore::open(f.temp.path().join("workflow.sqlite")).unwrap();
    assert_eq!(
        reopened
            .authorize_request_provider_call(&f.operator, op, &grant, 400_000)
            .unwrap(),
        call
    );
    assert!(reopened
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(101), &grant, 3)
        .is_err());
    let mut changed = grant.clone();
    changed.model = "changed".into();
    assert_eq!(
        reopened
            .authorize_request_provider_call(&f.operator, op, &changed, 3)
            .unwrap_err()
            .code,
        WorkflowErrorCode::IdempotencyConflict
    );
    let second = self::grant(&f, 2, 1, 1);
    assert!(reopened
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(102), &second, 3)
        .is_err());
    assert_eq!(
        all_calls(&reopened.connection.lock().unwrap())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn request_grant_rejects_agent_escalation_foreign_tenant_and_invalid_limits() {
    let f = fixture();
    let original = grant(&f, 1, 10, 2);
    assert!(f
        .store
        .authorize_request_provider_call(&f.sales, Uuid::from_u128(100), &original, 2)
        .is_err());
    assert!(f
        .store
        .authorize_request_provider_call(&f.customer, Uuid::from_u128(100), &original, 2)
        .is_err());
    let mut invalid = vec![];
    let mut candidate = original.clone();
    candidate.sales_principal.tenant_id = TenantId::parse("foreign").unwrap();
    invalid.push(candidate);
    let mut candidate = original.clone();
    candidate.sales_principal.role = CompanyRoleV1::Developer;
    invalid.push(candidate);
    let mut candidate = original.clone();
    candidate.total_call_limit = 41;
    invalid.push(candidate);
    let mut candidate = original.clone();
    candidate.concurrent_call_limit = 3;
    invalid.push(candidate);
    let mut candidate = original.clone();
    candidate.max_duration_ms = 120_001;
    invalid.push(candidate);
    let mut candidate = original.clone();
    candidate.expires_at_unix_ms = 2;
    invalid.push(candidate);
    let mut candidate = original.clone();
    candidate.expected_version = 2;
    invalid.push(candidate);
    for grant in invalid {
        assert!(f
            .store
            .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &grant, 2)
            .is_err());
    }
    assert!(all_calls(&f.store.connection.lock().unwrap())
        .unwrap()
        .is_empty());
}

#[test]
fn request_dispatch_is_once_and_binds_fresh_request_and_exact_principal() {
    let f = fixture();
    let grant = grant(&f, 1, 10, 2);
    let call = f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &grant, 2)
        .unwrap();
    let mut stale = f.sales.clone();
    stale.authority_generation += 1;
    assert!(f
        .store
        .claim_request_provider_call(&stale, &claim(&call), 3)
        .is_err());
    assert!(f
        .store
        .claim_request_provider_call(&f.operator, &claim(&call), 3)
        .is_err());
    assert!(f
        .store
        .claim_request_provider_call(&f.sales, &claim(&call), 1)
        .is_err());
    assert!(f
        .store
        .claim_request_provider_call(&f.sales, &claim(&call), 300_000)
        .is_err());
    let dispatched = f
        .store
        .claim_request_provider_call(&f.sales, &claim(&call), 3)
        .unwrap();
    assert!(dispatched.dispatch.is_some());
    let reopened = WorkflowStore::open(f.temp.path().join("workflow.sqlite")).unwrap();
    assert!(reopened
        .claim_request_provider_call(&f.sales, &claim(&call), 4)
        .is_err());
    assert_eq!(
        reopened
            .request_provider_call(&f.sales.tenant_id, &call.allowance_id)
            .unwrap(),
        Some(dispatched)
    );
}

#[test]
fn request_change_rejects_dispatch_and_adoption_without_losing_reservation() {
    for dispatched in [false, true] {
        let f = fixture();
        let grant = grant(&f, 1, 10, 2);
        let call = f
            .store
            .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &grant, 2)
            .unwrap();
        if dispatched {
            f.store
                .claim_request_provider_call(&f.sales, &claim(&call), 3)
                .unwrap();
        }
        f.store
            .apply_company_command(
                &f.customer,
                Uuid::from_u128(200),
                &CompanyWorkflowCommandV1::CancelCustomerRequest {
                    request_id: grant.request_id.clone(),
                    expected_version: 1,
                    reason_ref: "Cancelled".into(),
                },
                4,
            )
            .unwrap();
        assert!(f
            .store
            .claim_request_provider_call(&f.sales, &claim(&call), 5)
            .is_err());
        assert!(f
            .store
            .adopt_sales_question(&f.sales, &question(&call), 5)
            .is_err());
        let saved = f
            .store
            .request_provider_call(&f.sales.tenant_id, &call.allowance_id)
            .unwrap()
            .unwrap();
        assert_eq!(saved.dispatch.is_some(), dispatched);
        assert!(saved.question_response.is_none());
    }
}

#[test]
fn question_and_receipt_are_atomic_and_replay_survives_new_customer_reply() {
    let f = fixture();
    let grant = grant(&f, 1, 10, 2);
    let call = f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &grant, 2)
        .unwrap();
    f.store
        .claim_request_provider_call(&f.sales, &claim(&call), 3)
        .unwrap();
    let cursor = f.store.company_event_cursor().unwrap();
    assert!(f
        .store
        .adopt_sales_question_inner(&f.sales, &question(&call), 4, true)
        .is_err());
    assert_eq!(f.store.company_event_cursor().unwrap(), cursor);
    assert_eq!(
        f.store
            .company_customer_request(&f.sales.tenant_id, &grant.request_id)
            .unwrap()
            .unwrap(),
        call.source_request
    );
    assert!(f
        .store
        .request_provider_call(&f.sales.tenant_id, &call.allowance_id)
        .unwrap()
        .unwrap()
        .question_response
        .is_none());
    let reopened = WorkflowStore::open(f.temp.path().join("workflow.sqlite")).unwrap();
    // A verified durable result remains adoptable after the provider deadline;
    // this is a local retry, not a renewed dispatch permission.
    let response = reopened
        .adopt_sales_question(&f.sales, &question(&call), 400_000)
        .unwrap();
    assert_eq!(response.consultation.len(), 1);
    assert_eq!(response.consultation[0].recorded_by, f.sales.principal_id);
    let after = reopened.company_event_cursor().unwrap();
    assert_eq!(
        reopened
            .adopt_sales_question(&f.sales, &question(&call), 400_001)
            .unwrap(),
        response
    );
    assert_eq!(reopened.company_event_cursor().unwrap(), after);
    reopened
        .apply_company_command(
            &f.customer,
            Uuid::from_u128(200),
            &CompanyWorkflowCommandV1::SendCustomerRequestMessage {
                request_id: grant.request_id,
                expected_version: response.version,
                in_reply_to: Some(response.consultation[0].message_id.clone()),
                content: "Local businesses".into(),
            },
            400_002,
        )
        .unwrap();
    assert_eq!(
        reopened
            .adopt_sales_question(&f.sales, &question(&call), 400_003)
            .unwrap(),
        response
    );
    let mut changed = question(&call);
    changed.content = "Different question".into();
    assert_eq!(
        reopened
            .adopt_sales_question(&f.sales, &changed, 400_004)
            .unwrap_err()
            .code,
        WorkflowErrorCode::IdempotencyConflict
    );
    assert!(reopened
        .claim_request_provider_call(&f.sales, &claim(&call), 400_004)
        .is_err());
}

#[test]
fn qualification_proposal_and_receipt_commit_atomically_and_replay_once() {
    let f = fixture();
    let grant = grant(&f, 1, 10, 2);
    let call = f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &grant, 2)
        .unwrap();
    f.store
        .claim_request_provider_call(&f.sales, &claim(&call), 3)
        .unwrap();
    let cursor = f.store.company_event_cursor().unwrap();
    assert!(f
        .store
        .adopt_sales_proposal_inner(&f.sales, &proposal(&call), 4, true)
        .is_err());
    assert_eq!(f.store.company_event_cursor().unwrap(), cursor);
    assert_eq!(
        f.store
            .company_customer_request(&f.sales.tenant_id, &grant.request_id)
            .unwrap(),
        Some(call.source_request.clone())
    );
    assert!(f.store.company_projects().unwrap().is_empty());
    let reopened = WorkflowStore::open(f.temp.path().join("workflow.sqlite")).unwrap();
    let response = reopened
        .adopt_sales_proposal(&f.sales, &proposal(&call), 400_000)
        .unwrap();
    assert_eq!(response.request.state, CustomerRequestStateV1::Proposed);
    assert_eq!(response.request.version, call.source_request.version + 2);
    assert_eq!(
        response.request.proposal_ids,
        vec![response.proposal.proposal_id.clone()]
    );
    assert_eq!(response.proposal.created_by, f.sales.principal_id);
    assert!(reopened.company_projects().unwrap().is_empty());
    let after = reopened.company_event_cursor().unwrap();
    assert_eq!(
        reopened
            .adopt_sales_proposal(&f.sales, &proposal(&call), 500_001)
            .unwrap(),
        response
    );
    assert_eq!(reopened.company_event_cursor().unwrap(), after);
    let mut changed = proposal(&call);
    changed.binding.scope = "Changed scope".into();
    assert_eq!(
        reopened
            .adopt_sales_proposal(&f.sales, &changed, 500_002)
            .unwrap_err()
            .code,
        WorkflowErrorCode::IdempotencyConflict
    );
}

#[test]
fn concurrent_connections_cannot_exceed_dispatch_capacity_or_reuse_unknown_slot() {
    let f = fixture();
    let grants = [grant(&f, 1, 10, 1), grant(&f, 2, 10, 1)];
    let calls: Vec<_> = grants
        .iter()
        .enumerate()
        .map(|(i, grant)| {
            f.store
                .authorize_request_provider_call(
                    &f.operator,
                    Uuid::from_u128(100 + i as u128),
                    grant,
                    2,
                )
                .unwrap()
        })
        .collect();
    let barrier = Arc::new(Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = calls
            .iter()
            .map(|call| {
                let store = WorkflowStore::open(f.temp.path().join("workflow.sqlite")).unwrap();
                let barrier = barrier.clone();
                let sales = f.sales.clone();
                scope.spawn(move || {
                    barrier.wait();
                    store.claim_request_provider_call(&sales, &claim(call), 3)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let waiting = calls
        .iter()
        .zip(&results)
        .find(|(_, result)| result.is_err())
        .unwrap()
        .0;
    // Expiry does not prove a provider stopped. The uncertain slot is retained.
    assert!(f
        .store
        .claim_request_provider_call(&f.sales, &claim(waiting), 299_999)
        .is_err());
    let completed = results.iter().find_map(|r| r.as_ref().ok()).unwrap();
    f.store
        .adopt_sales_question(&f.sales, &question(completed), 4)
        .unwrap();
    assert!(f
        .store
        .claim_request_provider_call(&f.sales, &claim(waiting), 5)
        .is_ok());
}

#[test]
fn semantic_call_corruption_is_rejected_even_with_recomputed_row_digest() {
    let f = fixture();
    let grant = grant(&f, 1, 10, 2);
    let mut call = f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &grant, 2)
        .unwrap();
    call.grant.sales_principal.role = CompanyRoleV1::Developer;
    let mut connection = f.store.connection.lock().unwrap();
    let tx = connection.transaction().unwrap();
    put_entity(
        &tx,
        &f.sales.tenant_id,
        KIND,
        &call.allowance_id,
        call.version,
        &call,
    )
    .unwrap();
    tx.commit().unwrap();
    drop(connection);
    assert_eq!(
        f.store
            .request_provider_call(&f.sales.tenant_id, &call.allowance_id)
            .unwrap_err()
            .code,
        WorkflowErrorCode::CorruptStore
    );
}
