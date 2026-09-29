use super::*;
use sentinel_common::AgentId;
use std::sync::{Arc, Barrier};
use tempfile::TempDir;

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn sales_intake_returns_oldest_128_without_changing_ui_inbox_limit() {
    let f = fixture();
    let mut expected = Vec::new();
    for id in 1..=140 {
        expected.push(submit_intake_request(&f, id, 1 + (id % 3) as u64));
    }
    expected.sort_by(|a, b| {
        a.created_at_unix_ms
            .cmp(&b.created_at_unix_ms)
            .then_with(|| a.request_id.cmp(&b.request_id))
    });
    expected.truncate(MAX_AGGREGATE_ITEMS);
    let cursor = f.store.company_event_cursor().unwrap();
    assert_eq!(
        f.store
            .company_sales_intake_requests(&f.customer.tenant_id, "customer-test")
            .unwrap(),
        expected
    );
    assert_eq!(f.store.company_event_cursor().unwrap(), cursor);
    assert_eq!(
        f.store
            .company_customer_requests(&f.customer.tenant_id, "customer-test")
            .unwrap_err()
            .code,
        WorkflowErrorCode::InvalidInput
    );
}

#[test]
fn sales_intake_settled_waiting_and_granted_rows_do_not_starve_later_requests() {
    let f = fixture();
    for id in 1..=260 {
        let request = submit_intake_request(&f, id, 1);
        let (actor, command) = if id <= 130 && id % 2 == 0 {
            (
                &f.sales,
                CompanyWorkflowCommandV1::QualifyCustomerRequest {
                    request_id: request.request_id,
                    expected_version: request.version,
                    reason_ref: "Sales qualified".into(),
                },
            )
        } else if id <= 130 {
            (
                &f.customer,
                CompanyWorkflowCommandV1::CancelCustomerRequest {
                    request_id: request.request_id,
                    expected_version: request.version,
                    reason_ref: "Customer cancelled".into(),
                },
            )
        } else {
            (
                &f.sales,
                CompanyWorkflowCommandV1::SendCustomerRequestMessage {
                    request_id: request.request_id,
                    expected_version: request.version,
                    in_reply_to: None,
                    content: "Which audience?".into(),
                },
            )
        };
        f.store
            .apply_company_command(actor, Uuid::from_u128(1000 + id), &command, 2)
            .unwrap();
    }
    for id in 300..303 {
        let mut g = grant(&f, id, 10, 2);
        g.expires_at_unix_ms = 5;
        let call = f
            .store
            .authorize_request_provider_call(&f.operator, Uuid::from_u128(2000 + id), &g, 2)
            .unwrap();
        if id == 302 {
            f.store
                .claim_request_provider_call(&f.sales, &claim(&call), 3)
                .unwrap();
            f.store
                .abandon_request_provider_call(
                    &f.operator,
                    &call.allowance_id,
                    DIGEST,
                    &Uuid::from_u128(3000 + id).to_string(),
                    4,
                )
                .unwrap();
        }
    }
    let mut expected: Vec<_> = (400..540)
        .map(|id| submit_intake_request(&f, id, 6))
        .collect();
    expected.sort_by(|a, b| a.request_id.cmp(&b.request_id));
    expected.truncate(MAX_AGGREGATE_ITEMS);
    assert_eq!(
        f.store
            .company_sales_intake_requests(&f.customer.tenant_id, "customer-test")
            .unwrap(),
        expected
    );
}

#[test]
fn sales_intake_includes_answered_tail_and_only_excludes_exact_granted_version() {
    let f = fixture();
    let g = grant(&f, 1, 10, 2);
    f.store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &g, 2)
        .unwrap();
    let question = f
        .store
        .apply_company_command(
            &f.sales,
            Uuid::from_u128(101),
            &CompanyWorkflowCommandV1::SendCustomerRequestMessage {
                request_id: g.request_id.clone(),
                expected_version: 1,
                in_reply_to: None,
                content: "Which audience?".into(),
            },
            3,
        )
        .unwrap();
    let CompanyWorkflowResponseV1::CustomerRequest(question) = question.response else {
        panic!()
    };
    assert!(f
        .store
        .company_sales_intake_requests(&f.customer.tenant_id, "customer-test")
        .unwrap()
        .is_empty());
    let answer = f
        .store
        .apply_company_command(
            &f.customer,
            Uuid::from_u128(102),
            &CompanyWorkflowCommandV1::SendCustomerRequestMessage {
                request_id: g.request_id,
                expected_version: question.version,
                in_reply_to: Some(question.consultation.last().unwrap().message_id.clone()),
                content: "Local businesses".into(),
            },
            4,
        )
        .unwrap();
    let CompanyWorkflowResponseV1::CustomerRequest(answer) = answer.response else {
        panic!()
    };
    assert_eq!(
        f.store
            .company_sales_intake_requests(&f.customer.tenant_id, "customer-test")
            .unwrap(),
        vec![answer]
    );
}

#[test]
fn sales_intake_isolates_tenant_customer_and_grants() {
    let mut f = fixture();
    let local = submit_intake_request(&f, 1, 1);
    f.customer.customer_id = Some("customer-other".into());
    let other_customer = submit_intake_request(&f, 2, 1);
    let local_tenant = f.customer.tenant_id.clone();
    let foreign_tenant = TenantId::parse("tenant-other").unwrap();
    f.customer.tenant_id = foreign_tenant.clone();
    f.customer.customer_id = Some("customer-test".into());
    f.sales.tenant_id = foreign_tenant.clone();
    f.operator.tenant_id = foreign_tenant.clone();
    let foreign = grant(&f, 1, 10, 2);
    f.store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &foreign, 2)
        .unwrap();
    assert_ne!(foreign.request_id, local.request_id);
    assert_eq!(
        f.store
            .company_sales_intake_requests(&local_tenant, "customer-test")
            .unwrap(),
        vec![local]
    );
    assert_eq!(
        f.store
            .company_sales_intake_requests(&local_tenant, "customer-other")
            .unwrap(),
        vec![other_customer]
    );
    assert!(f
        .store
        .company_sales_intake_requests(&foreign_tenant, "customer-test")
        .unwrap()
        .is_empty());
}

#[test]
fn sales_intake_rejects_selected_request_digest_semantics_and_ownership_corruption() {
    for corruption in ["digest", "semantics", "ownership"] {
        let f = fixture();
        let mut request = submit_intake_request(&f, 1, 1);
        let id = request.request_id.clone();
        if corruption == "semantics" {
            request.created_at_unix_ms = 0;
        } else if corruption == "ownership" {
            request.tenant_id = TenantId::parse("tenant-other").unwrap();
        }
        let mut connection = f.store.connection.lock().unwrap();
        let tx = connection.transaction().unwrap();
        put_entity(
            &tx,
            &f.customer.tenant_id,
            "request",
            &id,
            request.version,
            &request,
        )
        .unwrap();
        if corruption == "digest" {
            tx.execute(
                "UPDATE company_entities SET payload_digest=?1 WHERE entity_kind='request'",
                params!["b".repeat(64)],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        drop(connection);
        assert_eq!(
            f.store
                .company_sales_intake_requests(&f.customer.tenant_id, "customer-test")
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore,
            "{corruption}"
        );
    }
}

fn submit_intake_request(f: &Fixture, id: u128, now_ms: u64) -> CustomerRequestV1 {
    let response = f
        .store
        .apply_company_command(
            &f.customer,
            Uuid::from_u128(id),
            &CompanyWorkflowCommandV1::SubmitCustomerRequest {
                summary_ref: "Customer website".into(),
                desired_outcome: "Accessible pages".into(),
                constraints: vec![],
            },
            now_ms,
        )
        .unwrap();
    let CompanyWorkflowResponseV1::CustomerRequest(request) = response.response else {
        panic!()
    };
    request
}

#[test]
fn autonomous_admission_counts_foreign_reservations_and_unknown_dispatches() {
    for dispatched in [false, true] {
        let mut f = fixture();
        let mut first_grant = grant(&f, 1, 10, 1);
        first_grant.expires_at_unix_ms = 5;
        let first = f
            .store
            .admit_autonomous_request_provider_call(
                &f.operator,
                Uuid::from_u128(100),
                &first_grant,
                2,
            )
            .unwrap()
            .unwrap();
        if dispatched {
            let dispatched = f
                .store
                .claim_request_provider_call(&f.sales, &claim(&first), 3)
                .unwrap();
            let cursor = f.store.company_event_cursor().unwrap();
            assert_eq!(
                f.store
                    .admit_autonomous_request_provider_call(
                        &f.operator,
                        Uuid::from_u128(100),
                        &first_grant,
                        6,
                    )
                    .unwrap(),
                Some(dispatched)
            );
            assert_eq!(f.store.company_event_cursor().unwrap(), cursor);
        }
        let original_tenant = f.operator.tenant_id.clone();
        let foreign = TenantId::parse("tenant-other").unwrap();
        f.operator.tenant_id = foreign.clone();
        f.sales.tenant_id = foreign.clone();
        f.customer.tenant_id = foreign;
        let second_grant = grant(&f, 1, 10, 1);
        let history = all_calls(&f.store.connection.lock().unwrap()).unwrap();
        let cursor = f.store.company_event_cursor().unwrap();
        assert_eq!(
            f.store
                .admit_autonomous_request_provider_call(
                    &f.operator,
                    Uuid::from_u128(101),
                    &second_grant,
                    4,
                )
                .unwrap(),
            None
        );
        assert_eq!(
            all_calls(&f.store.connection.lock().unwrap()).unwrap(),
            history
        );
        assert_eq!(f.store.company_event_cursor().unwrap(), cursor);
        assert!(f
            .store
            .request_provider_calls(&f.operator.tenant_id)
            .unwrap()
            .is_empty());
        assert_eq!(
            f.store.request_provider_calls(&original_tenant).unwrap(),
            history
        );
        let after_expiry = f
            .store
            .admit_autonomous_request_provider_call(
                &f.operator,
                Uuid::from_u128(101),
                &second_grant,
                5,
            )
            .unwrap();
        assert_eq!(after_expiry.is_none(), dispatched);
        if dispatched {
            assert_eq!(f.store.company_event_cursor().unwrap(), cursor);
            // Manual authorization still ignores reservations/concurrency;
            // dispatch retains its existing active-only rejection contract.
            let manual = f
                .store
                .authorize_request_provider_call(
                    &f.operator,
                    Uuid::from_u128(101),
                    &second_grant,
                    5,
                )
                .unwrap();
            assert_eq!(
                f.store
                    .claim_request_provider_call(&f.sales, &claim(&manual), 6)
                    .unwrap_err()
                    .code,
                WorkflowErrorCode::InvalidInput
            );
        }
    }
}

#[test]
fn autonomous_total_exhaustion_and_replay_do_not_mutate_history() {
    let f = fixture();
    let first_grant = grant(&f, 1, 1, 1);
    let second_grant = grant(&f, 2, 1, 1);
    let operation = Uuid::from_u128(100);
    let first = f
        .store
        .admit_autonomous_request_provider_call(&f.operator, operation, &first_grant, 2)
        .unwrap()
        .unwrap();
    let cursor = f.store.company_event_cursor().unwrap();
    // Even expired unsent calls permanently consume the total allowance.
    let mut next = second_grant.clone();
    next.expires_at_unix_ms = 400_000;
    assert_eq!(
        f.store
            .admit_autonomous_request_provider_call(
                &f.operator,
                Uuid::from_u128(101),
                &next,
                300_000,
            )
            .unwrap(),
        None
    );
    assert_eq!(
        f.store
            .authorize_request_provider_call(&f.operator, Uuid::from_u128(101), &next, 300_000,)
            .unwrap_err()
            .code,
        WorkflowErrorCode::InvalidInput
    );
    assert_eq!(
        f.store
            .admit_autonomous_request_provider_call(&f.operator, operation, &first_grant, 400_000,)
            .unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        f.store
            .authorize_request_provider_call(&f.operator, operation, &first_grant, 400_000,)
            .unwrap(),
        first
    );
    let mut changed = first_grant.clone();
    changed.model = "changed".into();
    assert_eq!(
        f.store
            .admit_autonomous_request_provider_call(&f.operator, operation, &changed, 3)
            .unwrap_err()
            .code,
        WorkflowErrorCode::IdempotencyConflict
    );
    assert_eq!(
        all_calls(&f.store.connection.lock().unwrap()).unwrap(),
        vec![first]
    );
    assert_eq!(f.store.company_event_cursor().unwrap(), cursor);
    let missing_id =
        stable_domain_id("subscription", &f.operator.tenant_id, Uuid::from_u128(101)).unwrap();
    assert!(f
        .store
        .request_provider_call(&f.operator.tenant_id, &missing_id)
        .unwrap()
        .is_none());
}

#[test]
fn autonomous_admission_counts_dispatch_and_reservation_together_and_releases_completed_slot() {
    let f = fixture();
    let grants = [
        grant(&f, 1, 10, 2),
        grant(&f, 2, 10, 2),
        grant(&f, 3, 10, 2),
    ];
    let first = f
        .store
        .admit_autonomous_request_provider_call(&f.operator, Uuid::from_u128(100), &grants[0], 2)
        .unwrap()
        .unwrap();
    f.store
        .claim_request_provider_call(&f.sales, &claim(&first), 3)
        .unwrap();
    f.store
        .admit_autonomous_request_provider_call(&f.operator, Uuid::from_u128(101), &grants[1], 4)
        .unwrap()
        .unwrap();
    let cursor = f.store.company_event_cursor().unwrap();
    assert_eq!(
        f.store
            .admit_autonomous_request_provider_call(
                &f.operator,
                Uuid::from_u128(102),
                &grants[2],
                5,
            )
            .unwrap(),
        None
    );
    assert_eq!(f.store.company_event_cursor().unwrap(), cursor);
    f.store
        .adopt_sales_question(&f.sales, &question(&first), 6)
        .unwrap();
    assert!(f
        .store
        .admit_autonomous_request_provider_call(&f.operator, Uuid::from_u128(102), &grants[2], 7,)
        .unwrap()
        .is_some());
}

#[test]
fn concurrent_autonomous_admission_reserves_only_one_slot_or_request() {
    for same_request in [false, true] {
        let f = fixture();
        let first = grant(&f, 1, 10, if same_request { 2 } else { 1 });
        let second = if same_request {
            first.clone()
        } else {
            grant(&f, 2, 10, 1)
        };
        let grants = [first, second];
        let cursor = f.store.company_event_cursor().unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = grants
                .iter()
                .enumerate()
                .map(|(index, grant)| {
                    let store = WorkflowStore::open(f.temp.path().join("workflow.sqlite")).unwrap();
                    let barrier = barrier.clone();
                    let operator = f.operator.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        store.admit_autonomous_request_provider_call(
                            &operator,
                            Uuid::from_u128(100 + index as u128),
                            grant,
                            2,
                        )
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|result| result.is_some()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_none()).count(), 1);
        assert_eq!(
            all_calls(&f.store.connection.lock().unwrap())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(f.store.company_event_cursor().unwrap(), cursor + 1);
    }
}

#[test]
fn autonomous_admission_counts_validated_active_legacy_dispatch_and_total() {
    let (temp, _path, store, _customer, mut project) =
        crate::domain_store::tests::accepted_project_fixture();
    let mut f = fixture();
    f.store = store;
    f.temp = temp;
    let pm = AuthenticatedCompanyPrincipalV1 {
        tenant_id: project.tenant_id.clone(),
        principal_id: "pm-a".into(),
        kind: CompanyPrincipalKindV1::Agent,
        role: CompanyRoleV1::ProjectManager,
        agent_id: Some(AgentId(1)),
        ..f.operator.clone()
    };
    let work_item_id = WorkItemId::parse("build-work").unwrap();
    for step in 0..5 {
        let command = match step {
            0 => CompanyWorkflowCommandV1::PlanWorkGraph {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                items: vec![CompanyWorkItemSpecV1 {
                    work_item_id: work_item_id.clone(),
                    title: "Build".into(),
                    objective: "Build artifact".into(),
                    required_role: CompanyRoleV1::Developer,
                    required_specialties: BTreeSet::from(["rust".into()]),
                    dependency_ids: BTreeSet::new(),
                    owner: AgentId(2),
                    inputs: vec![],
                    outputs: vec![WorkOutputContractV1 {
                        name: "result".into(),
                        media_type: "application/octet-stream".into(),
                        digest_algorithm: "sha256".into(),
                        contract_generation: 1,
                        contract_digest: DIGEST.into(),
                    }],
                    quality_gate: QualityGateBindingV1 {
                        gate_id: "web-work-item-qa-v1".into(),
                        generation: 1,
                        digest: DIGEST.into(),
                    },
                    budget_micros: 100,
                    rework: None,
                }],
            },
            1 => CompanyWorkflowCommandV1::ActivateProject {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                reason_ref: "Approved".into(),
            },
            2 => CompanyWorkflowCommandV1::AssignWork {
                project_id: project.project_id.clone(),
                expected_version: project.version,
                work_item_id: work_item_id.clone(),
                agent_id: AgentId(2),
                organization_generation: 1,
                organization_digest: DIGEST.into(),
                reason_ref: "Assigned".into(),
            },
            3 => {
                let assignment = &project.work_items[&work_item_id].assignments[0];
                CompanyWorkflowCommandV1::GrantSubscriptionCall {
                    project_id: project.project_id.clone(),
                    expected_version: project.version,
                    grant: SubscriptionCallGrantV1 {
                        schema_version: 1,
                        work_item_id: work_item_id.clone(),
                        assignment_id: assignment.assignment_id.clone(),
                        assignment_version: assignment.assignment_version,
                        agent_id: AgentId(2),
                        provider: "codex-cli".into(),
                        model: "model-test".into(),
                        catalog_digest: DIGEST.into(),
                        max_calls: 1,
                        max_concurrent: 1,
                        max_duration_ms: 120_000,
                        token_policy: SubscriptionTokenPolicyV1::MeasuredWithoutGenerationCap,
                        expires_at_unix_ms: 300_000,
                    },
                }
            }
            _ => {
                let allowance = project.subscription_call.as_ref().unwrap();
                CompanyWorkflowCommandV1::ClaimSubscriptionCall {
                    project_id: project.project_id.clone(),
                    expected_version: project.version,
                    allowance_id: allowance.allowance_id.clone(),
                    request_id: format!("company-provider-{}", allowance.allowance_id),
                    request_digest: DIGEST.into(),
                }
            }
        };
        let actor = if step == 4 {
            AuthenticatedCompanyPrincipalV1 {
                principal_id: "developer-a".into(),
                role: CompanyRoleV1::Developer,
                agent_id: Some(AgentId(2)),
                ..pm.clone()
            }
        } else {
            pm.clone()
        };
        let outcome = f
            .store
            .apply_company_command(
                &actor,
                Uuid::from_u128(40 + step),
                &command,
                40 + step as u64,
            )
            .unwrap();
        let CompanyWorkflowResponseV1::Project(updated) = outcome.response else {
            panic!("project");
        };
        project = *updated;
    }
    let g = grant(&f, 1, 10, 1);
    let cursor = f.store.company_event_cursor().unwrap();
    assert_eq!(
        legacy_usage(&f.store.connection.lock().unwrap(), &[]).unwrap(),
        (1, 1)
    );
    assert_eq!(
        f.store
            .admit_autonomous_request_provider_call(&f.operator, Uuid::from_u128(100), &g, 45,)
            .unwrap(),
        None
    );
    let mut total_limited = g.clone();
    total_limited.total_call_limit = 1;
    assert_eq!(
        f.store
            .admit_autonomous_request_provider_call(
                &f.operator,
                Uuid::from_u128(100),
                &total_limited,
                45,
            )
            .unwrap(),
        None
    );
    assert_eq!(f.store.company_event_cursor().unwrap(), cursor);
    assert!(all_calls(&f.store.connection.lock().unwrap())
        .unwrap()
        .is_empty());
    let manual = f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &g, 45)
        .unwrap();
    assert_eq!(
        f.store
            .claim_request_provider_call(&f.sales, &claim(&manual), 46)
            .unwrap_err()
            .code,
        WorkflowErrorCode::InvalidInput
    );
}

#[test]
fn request_provider_admission_preserves_unknown_effect_across_request_versions() {
    let f = fixture();
    let mut g = grant(&f, 1, 10, 2);
    let first = f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(900), &g, 2)
        .unwrap();
    let claimed = f
        .store
        .claim_request_provider_call(&f.sales, &claim(&first), 3)
        .unwrap();
    let question = f
        .store
        .apply_company_command(
            &f.sales,
            Uuid::from_u128(901),
            &CompanyWorkflowCommandV1::SendCustomerRequestMessage {
                request_id: g.request_id.clone(),
                expected_version: 1,
                in_reply_to: None,
                content: "Additional customer constraints".into(),
            },
            4,
        )
        .unwrap();
    let CompanyWorkflowResponseV1::CustomerRequest(question) = question.response else {
        panic!("request");
    };
    let outcome = f
        .store
        .apply_company_command(
            &f.customer,
            Uuid::from_u128(903),
            &CompanyWorkflowCommandV1::SendCustomerRequestMessage {
                request_id: g.request_id.clone(),
                expected_version: question.version,
                in_reply_to: Some(question.consultation[0].message_id.clone()),
                content: "The additional constraints are confirmed".into(),
            },
            5,
        )
        .unwrap();
    let CompanyWorkflowResponseV1::CustomerRequest(updated) = outcome.response else {
        panic!("request");
    };
    g.expected_version = updated.version;
    let cursor = f.store.company_event_cursor().unwrap();
    assert_eq!(
        f.store
            .admit_autonomous_request_provider_call(&f.operator, Uuid::from_u128(902), &g, 6,)
            .unwrap(),
        None
    );
    assert_eq!(f.store.company_event_cursor().unwrap(), cursor);
    assert!(f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(902), &g, 6,)
        .is_err());
    assert_eq!(
        f.store
            .request_provider_calls(&f.operator.tenant_id)
            .unwrap(),
        vec![claimed]
    );
}

#[test]
fn request_provider_calls_isolates_exact_tenants_without_mutation() {
    let mut f = fixture();
    let tenant = f.operator.tenant_id.clone();
    assert!(f.store.request_provider_calls(&tenant).unwrap().is_empty());
    let first_grant = grant(&f, 1, 10, 2);
    let first = f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &first_grant, 2)
        .unwrap();
    let other = TenantId::parse("tenant-test-other").unwrap();
    f.operator.tenant_id = other.clone();
    f.sales.tenant_id = other.clone();
    f.customer.tenant_id = other.clone();
    let second_grant = grant(&f, 1, 10, 2);
    let second = f
        .store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &second_grant, 2)
        .unwrap();
    let cursor = f.store.company_event_cursor().unwrap();
    assert_eq!(
        f.store.request_provider_calls(&tenant).unwrap(),
        vec![first]
    );
    assert_eq!(
        f.store.request_provider_calls(&other).unwrap(),
        vec![second]
    );
    assert!(f
        .store
        .request_provider_calls(&TenantId::parse("tenant-missing").unwrap())
        .unwrap()
        .is_empty());
    assert_eq!(f.store.company_event_cursor().unwrap(), cursor);
}

#[test]
fn request_provider_calls_preserves_expired_dispatched_answered_and_abandoned_history() {
    let f = fixture();
    let mut expected = Vec::new();
    for index in 0..5_u128 {
        let mut g = grant(&f, index + 1, 10, 2);
        let now = 2 + index as u64 * 4;
        if index == 0 {
            g.expires_at_unix_ms = now + 1;
        }
        let mut call = f
            .store
            .authorize_request_provider_call(&f.operator, Uuid::from_u128(100 + index), &g, now)
            .unwrap();
        if index != 0 {
            call = f
                .store
                .claim_request_provider_call(&f.sales, &claim(&call), now + 1)
                .unwrap();
        }
        match index {
            1 => {
                f.store
                    .adopt_sales_question(&f.sales, &question(&call), now + 2)
                    .unwrap();
            }
            2 => {
                f.store
                    .adopt_sales_proposal(&f.sales, &proposal(&call), now + 2)
                    .unwrap();
            }
            3 => {
                f.store
                    .abandon_request_provider_call(
                        &f.operator,
                        &call.allowance_id,
                        DIGEST,
                        &Uuid::from_u128(200).to_string(),
                        now + 2,
                    )
                    .unwrap();
            }
            _ => {}
        }
        expected.push(
            f.store
                .request_provider_call(&f.operator.tenant_id, &call.allowance_id)
                .unwrap()
                .unwrap(),
        );
    }
    assert!(expected[0].dispatch.is_none());
    assert!(expected[0].grant.expires_at_unix_ms < expected[1].created_at_unix_ms);
    assert!(expected[1].question_response.is_some());
    assert!(expected[2].proposal_response.is_some());
    assert!(expected[3].abandonment_event_id.is_some());
    assert!(expected[4].dispatch.is_some());
    assert!(expected[4].question_response.is_none());
    assert!(expected[4].proposal_response.is_none());
    assert!(expected[4].abandonment_event_id.is_none());
    let reopened = WorkflowStore::open(f.temp.path().join("workflow.sqlite")).unwrap();
    let cursor = reopened.company_event_cursor().unwrap();
    assert_eq!(
        reopened
            .request_provider_calls(&f.operator.tenant_id)
            .unwrap(),
        expected
    );
    assert_eq!(reopened.company_event_cursor().unwrap(), cursor);
}

#[test]
fn request_provider_calls_orders_by_creation_then_allowance_id_stably() {
    let f = fixture();
    let mut expected = Vec::new();
    for (index, now) in [4, 2, 2].into_iter().enumerate() {
        let g = grant(&f, index as u128 + 1, 10, 2);
        expected.push(
            f.store
                .authorize_request_provider_call(
                    &f.operator,
                    Uuid::from_u128(100 + index as u128),
                    &g,
                    now,
                )
                .unwrap(),
        );
    }
    expected[1..].sort_by(|a, b| a.allowance_id.cmp(&b.allowance_id));
    expected.rotate_left(1);
    for _ in 0..2 {
        assert_eq!(
            f.store
                .request_provider_calls(&f.operator.tenant_id)
                .unwrap(),
            expected
        );
    }
    let reopened = WorkflowStore::open(f.temp.path().join("workflow.sqlite")).unwrap();
    assert_eq!(
        reopened
            .request_provider_calls(&f.operator.tenant_id)
            .unwrap(),
        expected
    );
}

#[test]
fn request_provider_calls_enforces_store_wide_forty_row_bound_before_filtering() {
    let f = fixture();
    let mut last = None;
    for index in 0..40_u128 {
        let g = grant(&f, index + 1, 40, 2);
        last = Some(
            f.store
                .authorize_request_provider_call(&f.operator, Uuid::from_u128(100 + index), &g, 2)
                .unwrap(),
        );
    }
    assert_eq!(
        f.store
            .request_provider_calls(&f.operator.tenant_id)
            .unwrap()
            .len(),
        40
    );
    let mut extra = last.unwrap();
    extra.operation_id = Uuid::from_u128(200);
    extra.allowance_id =
        stable_domain_id("subscription", &f.operator.tenant_id, extra.operation_id).unwrap();
    extra.validate_entity().unwrap();
    let mut connection = f.store.connection.lock().unwrap();
    let tx = connection.transaction().unwrap();
    put_entity(
        &tx,
        &f.operator.tenant_id,
        KIND,
        &extra.allowance_id,
        extra.version,
        &extra,
    )
    .unwrap();
    tx.commit().unwrap();
    drop(connection);
    for tenant in [
        f.operator.tenant_id.clone(),
        TenantId::parse("tenant-other").unwrap(),
    ] {
        assert_eq!(
            f.store.request_provider_calls(&tenant).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );
        assert_eq!(
            f.store
                .company_sales_intake_requests(&tenant, "customer-test")
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
    }
}

#[test]
fn request_provider_calls_rejects_row_digest_corruption_before_tenant_filtering() {
    let f = fixture();
    let g = grant(&f, 1, 10, 2);
    f.store
        .authorize_request_provider_call(&f.operator, Uuid::from_u128(100), &g, 2)
        .unwrap();
    f.store
        .connection
        .lock()
        .unwrap()
        .execute(
            "UPDATE company_entities SET payload_digest=?1 WHERE entity_kind=?2",
            params!["b".repeat(64), KIND],
        )
        .unwrap();
    for tenant in [
        f.operator.tenant_id.clone(),
        TenantId::parse("tenant-other").unwrap(),
    ] {
        assert_eq!(
            f.store.request_provider_calls(&tenant).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );
        assert_eq!(
            f.store
                .company_sales_intake_requests(&tenant, "customer-test")
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
    }
}

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
    for tenant in [
        f.sales.tenant_id.clone(),
        TenantId::parse("tenant-other").unwrap(),
    ] {
        assert_eq!(
            f.store.request_provider_calls(&tenant).unwrap_err().code,
            WorkflowErrorCode::CorruptStore
        );
        assert_eq!(
            f.store
                .company_sales_intake_requests(&tenant, "customer-test")
                .unwrap_err()
                .code,
            WorkflowErrorCode::CorruptStore
        );
    }
}
