//! Selector descriptors only; these altered DTOs are never persisted or granted.

use super::*;

fn assigned_project() -> (
    tempfile::TempDir,
    WorkflowApi,
    sentinel_workflow::ProjectV1,
) {
    let temp = tempfile::tempdir().unwrap();
    let api = super::super::model_work::configured_test_api(&temp.path().join("company.sqlite"));
    let binding = super::super::model_work::assign_test_work_from(&api, Some(4), 0);
    let project = api
        .store
        .company_project(
            &TenantId::parse(&binding.tenant_id).unwrap(),
            &ProjectId::parse(&binding.project_id).unwrap(),
        )
        .unwrap()
        .unwrap();
    assert!(project.subscription_call.as_ref().unwrap().dispatch.is_none());
    (temp, api, project)
}

fn sequential_descriptor(project: &sentinel_workflow::ProjectV1) -> sentinel_workflow::ProjectV1 {
    let mut next = project.clone();
    let current_id = next.subscription_call.as_ref().unwrap().grant.work_item_id.clone();
    let current = next.work_items.get_mut(&current_id).unwrap();
    current.state = CompanyWorkStateV1::Done;
    current.spec.required_role = CompanyRoleV1::Designer;
    let mut successor = current.clone();
    successor.spec.work_item_id = WorkItemId::parse("next-developer-work").unwrap();
    successor.spec.required_role = CompanyRoleV1::Developer;
    successor.state = CompanyWorkStateV1::Assigned;
    assert_eq!(successor.assignments.iter().filter(|assignment| assignment.active).count(), 1);
    next.work_items.insert(successor.spec.work_item_id.clone(), successor);
    next
}

#[test]
fn completed_adaptive_descriptor_advances_before_expiry_without_mutating_store() {
    let (_temp, api, project) = assigned_project();
    let descriptor = sequential_descriptor(&project);
    let allowance = descriptor.subscription_call.as_ref().unwrap();
    let now = allowance.created_at_unix_ms;
    assert!(now < allowance.grant.expires_at_unix_ms);
    assert!(WorkflowApi::model_work_grant_due(&descriptor, now));
    assert_eq!(allowance.grant.max_calls, 4);
    let retained = api.store.company_project(&project.tenant_id, &project.project_id)
        .unwrap().unwrap();
    assert_eq!(retained, project);
}

#[test]
fn adaptive_completion_descriptor_preserves_successor_and_source_review_guards() {
    let (_temp, _api, project) = assigned_project();
    let descriptor = sequential_descriptor(&project);
    let allowance = descriptor.subscription_call.as_ref().unwrap().clone();
    let now = allowance.created_at_unix_ms;
    let next_id = WorkItemId::parse("next-developer-work").unwrap();
    for case in ["current-assigned", "missing-current", "no-next", "next-ready", "next-qa", "no-assignment", "two-assignments", "source-review"] {
        let mut changed = descriptor.clone();
        match case {
            "current-assigned" => changed.work_items.get_mut(&allowance.grant.work_item_id).unwrap().state = CompanyWorkStateV1::Assigned,
            "missing-current" => { changed.work_items.remove(&allowance.grant.work_item_id); }
            "no-next" => { changed.work_items.remove(&next_id); }
            "next-ready" => changed.work_items.get_mut(&next_id).unwrap().state = CompanyWorkStateV1::Ready,
            "next-qa" => changed.work_items.get_mut(&next_id).unwrap().spec.required_role = CompanyRoleV1::Qa,
            "no-assignment" => changed.work_items.get_mut(&next_id).unwrap().assignments.clear(),
            "two-assignments" => {
                let next = changed.work_items.get_mut(&next_id).unwrap();
                let active = next.assignments.iter().find(|assignment| assignment.active).unwrap().clone();
                next.assignments.push(active);
            }
            "source-review" => changed.source_review_previous_call = Some(allowance.clone()),
            _ => unreachable!(),
        }
        assert!(!WorkflowApi::model_work_grant_due(&changed, now), "{case}");
    }
    let mut designer = descriptor;
    designer.work_items.get_mut(&next_id).unwrap().spec.required_role = CompanyRoleV1::Designer;
    assert!(WorkflowApi::model_work_grant_due(&designer, now));
}

#[test]
fn grant_due_preserves_undispatched_renewal_and_legacy_dispatch_behavior() {
    let (_temp, _api, project) = assigned_project();
    let allowance = project.subscription_call.as_ref().unwrap().clone();
    let expiry = allowance.grant.expires_at_unix_ms;
    assert!(!WorkflowApi::model_work_grant_due(&project, expiry - 1));
    assert!(WorkflowApi::model_work_grant_due(&project, expiry));
    assert!(WorkflowApi::model_work_grant_due(&project, expiry + 1));

    let dispatch = sentinel_workflow::SubscriptionCallDispatchV1 {
        request_id: format!("company-provider-{}", allowance.allowance_id),
        request_digest: "a".repeat(64),
        dispatched_at_unix_ms: allowance.created_at_unix_ms,
    };
    let mut legacy = project.clone();
    legacy.subscription_call.as_mut().unwrap().dispatch = Some(dispatch.clone());
    assert!(!WorkflowApi::model_work_grant_due(&legacy, expiry - 1));
    assert!(!WorkflowApi::model_work_grant_due(&legacy, expiry + 1));
    legacy = sequential_descriptor(&project);
    legacy.subscription_call.as_mut().unwrap().dispatch = Some(dispatch);
    assert!(WorkflowApi::model_work_grant_due(&legacy, allowance.created_at_unix_ms));
    assert!(WorkflowApi::model_work_grant_due(&legacy, expiry + 1));
    legacy.source_review_previous_call = Some(allowance);
    assert!(!WorkflowApi::model_work_grant_due(&legacy, expiry + 1));
    let mut no_allowance = project;
    no_allowance.subscription_call = None;
    assert!(WorkflowApi::model_work_grant_due(&no_allowance, expiry - 1));
}
