//! Clock and budget presentation only, never a current authority or admission proof.

use sentinel_workflow::{
    AdaptiveLeadershipReviewContextV1, AdaptiveLeadershipReviewGrantV1,
    AdaptiveLeadershipReviewSubjectV2, AdaptiveWorkFundingBindingLimitsV1,
};
use serde_json::{json, Value};

pub(super) const CONTINUATION_CLOCK_NOTICE: &str = "The source execution window, this leadership \
    review grant and a prospective continuation have separate clocks. Source expiry or insufficient \
    source dispatch slack prohibits dispatch under that source window; neither alone decides whether \
    a new bounded window is appropriate. The prospective bounds are evaluated at review issuance, \
    not a guarantee of remaining time at later adoption. If you independently choose Continue, a new \
    window would start at continuation issuance and must be adopted into the same session before \
    employee execution. All authority, expiry, budget and dispatch checks still apply. Fresh private \
    inspection is required AFTER adoption and before other productive tool work, within the new \
    window; it is not native inspection to perform during this review. This requirement grants no \
    inspection capability or tools absent from the validated catalogue. Account for inspection and \
    subsequent work within the finite budget. Defer remains independently admissible; this \
    presentation selects no decision and overrides none, and retained Defers remain valid.";

/// Caller validates the sealed source/grant and gates on presentation_version == Some(2).
/// Legacy rendering must not call this helper.
/// Output has fixed keys and only numeric, boolean and static-string values; no history is copied.
pub(super) fn project(
    source: &AdaptiveLeadershipReviewContextV1,
    grant: &AdaptiveLeadershipReviewGrantV1,
    issued_at_ms: u64,
) -> Result<Value, &'static str> {
    if !matches!(grant.schema_version, 5 | 6) {
        return Err("leadership window projection requires funded grant 5 or 6");
    }
    let Some(AdaptiveLeadershipReviewSubjectV2::BudgetWindowExhausted { budget }) = &grant.subject
    else {
        return Err("leadership window projection requires budget subject");
    };
    let epoch = grant
        .work_funding
        .as_ref()
        .ok_or("leadership window projection requires funding")?;
    let session = &source.source_session;
    let facts = &epoch.receipt.request.source;
    let issued_windows = u64::try_from(
        session
            .continuation
            .as_ref()
            .map_or(0, |state| state.authorizations.len()),
    )
    .map_err(|_| "leadership window count overflow")?;
    project_inputs(ProjectionInputs {
        observed_at_ms: budget.observed_at_ms,
        source_deadline_ms: session.active_deadline_ms(),
        source_model_call_ceiling: session.active_model_ceiling(),
        source_tool_call_ceiling: session.funded_tool_call_ceiling(),
        source_max_call_duration_ms: session.effective_call_duration_ms(),
        source_model_calls_exhausted: budget.model_calls_exhausted,
        source_deadline_expired: budget.deadline_expired,
        source_dispatch_slack_insufficient: budget.dispatch_slack_insufficient,
        source_fresh_observation_required: session.requires_fresh_observation(),
        review_issued_at_ms: issued_at_ms,
        review_expires_at_ms: grant.expires_at_unix_ms,
        review_max_duration_ms: grant.max_duration_ms,
        root_model_call_ceiling: session.grant.max_model_calls,
        root_tool_call_ceiling: session.grant.max_tool_calls,
        model_calls_spent: session.model_calls,
        tool_calls_spent: session.tool_calls,
        previous_model_call_ceiling: facts.current_model_call_ceiling,
        previous_tool_call_ceiling: facts.current_tool_call_ceiling,
        issued_windows,
        review_ordinal: epoch.binding.ordinal,
        already_adopted_epoch: session
            .active_work_funding()
            .is_some_and(|active| active.same_epoch(epoch)),
        limits: &epoch.binding.limits,
    })
}

#[derive(Clone, Copy)]
struct ProjectionInputs<'a> {
    observed_at_ms: u64,
    source_deadline_ms: u64,
    source_model_call_ceiling: u16,
    source_tool_call_ceiling: u16,
    source_max_call_duration_ms: u64,
    source_model_calls_exhausted: bool,
    source_deadline_expired: bool,
    source_dispatch_slack_insufficient: bool,
    source_fresh_observation_required: bool,
    review_issued_at_ms: u64,
    review_expires_at_ms: u64,
    review_max_duration_ms: u64,
    root_model_call_ceiling: u16,
    root_tool_call_ceiling: u16,
    model_calls_spent: u16,
    tool_calls_spent: u16,
    previous_model_call_ceiling: u16,
    previous_tool_call_ceiling: u16,
    issued_windows: u64,
    review_ordinal: u16,
    already_adopted_epoch: bool,
    limits: &'a AdaptiveWorkFundingBindingLimitsV1,
}

fn project_inputs(input: ProjectionInputs<'_>) -> Result<Value, &'static str> {
    let limits = input.limits;
    if input.observed_at_ms == 0
        || input.review_issued_at_ms < input.observed_at_ms
        || input.review_expires_at_ms <= input.review_issued_at_ms
        || input.review_max_duration_ms == 0
        || input.review_ordinal == 0
    {
        return Err("leadership window projection clocks or ordinal invalid");
    }
    let model_remaining = limits
        .total_model_call_ceiling
        .checked_sub(input.model_calls_spent)
        .ok_or("leadership window projection model accounting invalid")?;
    let tool_remaining = limits
        .total_tool_call_ceiling
        .checked_sub(input.tool_calls_spent)
        .ok_or("leadership window projection tool accounting invalid")?;
    let windows_remaining = u64::from(limits.total_window_ceiling)
        .checked_sub(input.issued_windows)
        .ok_or("leadership window projection window accounting invalid")?;
    let reviews_remaining = limits
        .total_review_ceiling
        .checked_sub(input.review_ordinal)
        .ok_or("leadership window projection review accounting invalid")?;
    let minimum_window_ms = limits
        .max_call_duration_ms
        .checked_add(limits.dispatch_margin_ms)
        .filter(|minimum| *minimum > 0)
        .ok_or("leadership window projection dispatch margin overflow")?;
    let epoch_time_remaining_ms = limits
        .expires_at_unix_ms
        .saturating_sub(input.review_issued_at_ms);
    let maximum_window_ms = limits.max_window_ms.min(epoch_time_remaining_ms);
    // This boolean describes numeric bounds at a sealed clock, not admissibility now.
    let numeric_bounds_allow_proposal = model_remaining > 0
        && tool_remaining > 0
        && windows_remaining > 0
        && maximum_window_ms >= minimum_window_ms;
    let additional_model_calls_max = if numeric_bounds_allow_proposal {
        model_remaining
    } else {
        0
    };
    let funding_status = if input.already_adopted_epoch {
        "already_adopted_current_epoch"
    } else {
        "proposed_awaiting_adoption_epoch"
    };
    Ok(json!({
        "schema_version": 2,
        "presentation_only": true,
        "source_execution_window": {
            "scope": "source_employee_execution_only",
            "observed_at_ms": input.observed_at_ms,
            "deadline_ms": input.source_deadline_ms,
            "remaining_ms_at_observation": input.source_deadline_ms.saturating_sub(input.observed_at_ms),
            "model_calls_exhausted_at_observation": input.source_model_calls_exhausted,
            "deadline_expired_at_observation": input.source_deadline_expired,
            "dispatch_slack_insufficient_at_observation": input.source_dispatch_slack_insufficient,
            "deadline_expired_at_review_issuance": input.review_issued_at_ms >= input.source_deadline_ms,
            "model_call_ceiling": input.source_model_call_ceiling,
            "tool_call_ceiling": input.source_tool_call_ceiling,
            "max_call_duration_ms": input.source_max_call_duration_ms,
            "fresh_observation_required_in_source": input.source_fresh_observation_required,
        },
        "leadership_review_grant": {
            "scope": "leadership_decision_only_not_employee_execution",
            "issued_at_ms": input.review_issued_at_ms,
            "expires_at_ms": input.review_expires_at_ms,
            "max_call_duration_ms": input.review_max_duration_ms,
            "current_liveness_not_asserted": true,
        },
        "prospective_continuation": {
            "status": "conditional_proposal_not_issued_or_adopted",
            "as_of_review_issuance_ms": input.review_issued_at_ms,
            "starts_at": "continuation_issuance",
            "adoption_required_before_employee_execution": true,
            "numeric_bounds_allow_proposal_at_review_issuance": numeric_bounds_allow_proposal,
            "additional_model_calls_max": additional_model_calls_max,
            "window_ms_min": minimum_window_ms,
            "window_ms_max_at_review_issuance": maximum_window_ms,
            "funding_expires_at_ms": limits.expires_at_unix_ms,
            "funding_time_remaining_ms_at_review_issuance": epoch_time_remaining_ms,
            "dispatch_margin_ms": limits.dispatch_margin_ms,
            "max_employee_call_duration_ms": limits.max_call_duration_ms,
            "future_deadline_or_dispatch_not_guaranteed": true,
            "fresh_private_inspection_required_after_adoption": true,
            "new_tool_or_inspection_permission_granted": false,
            "tool_permissions": "only_the_validated_catalogue_and_current_authority",
        },
        "accounting": {
            "model_calls_spent": input.model_calls_spent,
            "tool_calls_spent": input.tool_calls_spent,
            "root_initial_historical_nonoperative": {
                "model_call_ceiling": input.root_model_call_ceiling,
                "tool_call_ceiling": input.root_tool_call_ceiling,
                "model_calls_remaining": input.root_model_call_ceiling.saturating_sub(input.model_calls_spent),
                "tool_calls_remaining": input.root_tool_call_ceiling.saturating_sub(input.tool_calls_spent),
            },
            "funded_limits_for_this_review": {
                "status": funding_status,
                "model_call_ceiling": limits.total_model_call_ceiling,
                "tool_call_ceiling": limits.total_tool_call_ceiling,
                "model_calls_remaining": model_remaining,
                "tool_calls_remaining": tool_remaining,
                "previous_model_call_ceiling_at_funding_issuance": input.previous_model_call_ceiling,
                "previous_tool_call_ceiling_at_funding_issuance": input.previous_tool_call_ceiling,
            },
            "continuation_windows_issued": input.issued_windows,
            "continuation_window_ceiling": limits.total_window_ceiling,
            "continuation_windows_remaining": windows_remaining,
            "review_ordinal": input.review_ordinal,
            "review_ceiling": limits.total_review_ceiling,
            "reviews_remaining_after_this_issuance": reviews_remaining,
            "leadership_reviews_are_not_employee_calls_or_windows": true,
            "spent_counts_never_reset_or_refund": true,
        },
        "observation_coverage": "latest_private_observation_only_not_complete_work_history",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> AdaptiveWorkFundingBindingLimitsV1 {
        AdaptiveWorkFundingBindingLimitsV1 {
            total_model_call_ceiling: 64,
            total_tool_call_ceiling: 64,
            total_review_ceiling: 60,
            total_window_ceiling: 60,
            max_window_ms: 300_000,
            max_call_duration_ms: 120_000,
            dispatch_margin_ms: 10_000,
            expires_at_unix_ms: 2_000_000,
        }
    }

    fn inputs(limits: &AdaptiveWorkFundingBindingLimitsV1) -> ProjectionInputs<'_> {
        ProjectionInputs {
            observed_at_ms: 1_000_000,
            source_deadline_ms: 900_000,
            source_model_call_ceiling: 39,
            source_tool_call_ceiling: 64,
            source_max_call_duration_ms: 120_000,
            source_model_calls_exhausted: false,
            source_deadline_expired: true,
            source_dispatch_slack_insufficient: false,
            source_fresh_observation_required: false,
            review_issued_at_ms: 1_000_000,
            review_expires_at_ms: 1_300_000,
            review_max_duration_ms: 120_000,
            root_model_call_ceiling: 16,
            root_tool_call_ceiling: 16,
            model_calls_spent: 32,
            tool_calls_spent: 29,
            previous_model_call_ceiling: 48,
            previous_tool_call_ceiling: 48,
            issued_windows: 32,
            review_ordinal: 53,
            already_adopted_epoch: true,
            limits,
        }
    }

    #[test]
    fn expired_source_is_separate_from_review_and_conditional_new_window() {
        let limits = limits();
        let value = project_inputs(inputs(&limits)).unwrap();
        let source = &value["source_execution_window"];
        assert_eq!(source["deadline_expired_at_observation"], true);
        assert_eq!(source["remaining_ms_at_observation"], 0);
        assert_eq!(value["leadership_review_grant"]["expires_at_ms"], 1_300_000);
        let prospective = &value["prospective_continuation"];
        assert_eq!(
            prospective["numeric_bounds_allow_proposal_at_review_issuance"],
            true
        );
        assert_eq!(prospective["additional_model_calls_max"], 32);
        assert_eq!(prospective["window_ms_min"], 130_000);
        assert_eq!(prospective["window_ms_max_at_review_issuance"], 300_000);
        assert_eq!(prospective["starts_at"], "continuation_issuance");
        assert!(prospective.get("deadline_ms").is_none());
        assert_eq!(
            prospective["future_deadline_or_dispatch_not_guaranteed"],
            true
        );
        assert_eq!(
            prospective["fresh_private_inspection_required_after_adoption"],
            true
        );
        assert_eq!(source["fresh_observation_required_in_source"], false);
    }

    #[test]
    fn insufficient_source_slack_does_not_consume_prospective_time() {
        let limits = limits();
        let value = project_inputs(ProjectionInputs {
            source_deadline_ms: 1_085_684,
            source_deadline_expired: false,
            source_dispatch_slack_insufficient: true,
            ..inputs(&limits)
        })
        .unwrap();
        let source = &value["source_execution_window"];
        let prospective = &value["prospective_continuation"];
        assert_eq!(source["remaining_ms_at_observation"], 85_684);
        assert_eq!(source["dispatch_slack_insufficient_at_observation"], true);
        assert_eq!(prospective["additional_model_calls_max"], 32);
        assert_eq!(prospective["window_ms_max_at_review_issuance"], 300_000);
    }

    #[test]
    fn epoch_tail_bounds_the_proposal_without_promising_a_future_deadline() {
        for (tail, available) in [
            (130_001, true),
            (130_000, true),
            (129_999, false),
            (0, false),
        ] {
            let mut limits = limits();
            limits.expires_at_unix_ms = 1_000_000 + tail;
            let value = project_inputs(inputs(&limits)).unwrap();
            let prospective = &value["prospective_continuation"];
            assert_eq!(prospective["window_ms_max_at_review_issuance"], tail);
            assert_eq!(
                prospective["numeric_bounds_allow_proposal_at_review_issuance"],
                available
            );
            assert_eq!(
                prospective["additional_model_calls_max"],
                if available { 32 } else { 0 }
            );
            assert!(prospective.get("deadline_ms").is_none());
        }
    }

    #[test]
    fn exhausted_calls_tools_or_windows_leave_no_numeric_proposal() {
        let limits = limits();
        for input in [
            ProjectionInputs {
                model_calls_spent: 64,
                ..inputs(&limits)
            },
            ProjectionInputs {
                tool_calls_spent: 64,
                ..inputs(&limits)
            },
            ProjectionInputs {
                issued_windows: 60,
                ..inputs(&limits)
            },
        ] {
            let value = project_inputs(input).unwrap();
            let prospective = &value["prospective_continuation"];
            assert_eq!(prospective["additional_model_calls_max"], 0);
            assert_eq!(
                prospective["numeric_bounds_allow_proposal_at_review_issuance"],
                false
            );
        }
        let value = project_inputs(ProjectionInputs {
            review_ordinal: 60,
            ..inputs(&limits)
        })
        .unwrap();
        assert_eq!(
            value["accounting"]["reviews_remaining_after_this_issuance"],
            0
        );
        assert_eq!(
            value["prospective_continuation"]["additional_model_calls_max"],
            32
        );
    }

    #[test]
    fn overflow_and_inconsistent_accounting_fail_instead_of_wrapping() {
        let mut overflow = limits();
        overflow.max_call_duration_ms = u64::MAX;
        assert_eq!(
            project_inputs(inputs(&overflow)).unwrap_err(),
            "leadership window projection dispatch margin overflow"
        );
        let limits = limits();
        for input in [
            ProjectionInputs {
                model_calls_spent: 65,
                ..inputs(&limits)
            },
            ProjectionInputs {
                tool_calls_spent: 65,
                ..inputs(&limits)
            },
            ProjectionInputs {
                issued_windows: 61,
                ..inputs(&limits)
            },
            ProjectionInputs {
                review_ordinal: 61,
                ..inputs(&limits)
            },
            ProjectionInputs {
                review_expires_at_ms: 999_999,
                ..inputs(&limits)
            },
            ProjectionInputs {
                review_issued_at_ms: 999_999,
                ..inputs(&limits)
            },
        ] {
            assert!(project_inputs(input).is_err());
        }
    }

    #[test]
    fn largest_clock_values_use_epoch_subtraction_without_deadline_overflow() {
        let mut limits = limits();
        limits.expires_at_unix_ms = u64::MAX;
        let issued_at_ms = u64::MAX - 130_000;
        let value = project_inputs(ProjectionInputs {
            observed_at_ms: issued_at_ms,
            source_deadline_ms: issued_at_ms - 1,
            review_issued_at_ms: issued_at_ms,
            review_expires_at_ms: u64::MAX,
            ..inputs(&limits)
        })
        .unwrap();
        let prospective = &value["prospective_continuation"];
        assert_eq!(prospective["window_ms_max_at_review_issuance"], 130_000);
        assert_eq!(prospective["additional_model_calls_max"], 32);
        assert!(prospective.get("deadline_ms").is_none());
        assert!(serde_json::to_vec(&value).unwrap().len() < 8192);
    }

    #[test]
    fn spent_root_budget_is_historical_not_the_funded_remainder() {
        let limits = limits();
        let value = project_inputs(inputs(&limits)).unwrap();
        let accounting = &value["accounting"];
        assert_eq!(accounting["model_calls_spent"], 32);
        assert_eq!(
            accounting["root_initial_historical_nonoperative"]["model_calls_remaining"],
            0
        );
        let funded = &accounting["funded_limits_for_this_review"];
        assert_eq!(funded["model_calls_remaining"], 32);
        assert_eq!(funded["tool_calls_remaining"], 35);
        assert_eq!(accounting["continuation_windows_remaining"], 28);
    }

    #[test]
    fn projection_is_bounded_and_never_grants_inspection_or_overrides_defer() {
        let limits = limits();
        let value = project_inputs(inputs(&limits)).unwrap();
        assert!(serde_json::to_vec(&value).unwrap().len() < 8192);
        assert_eq!(
            value["prospective_continuation"]["new_tool_or_inspection_permission_granted"],
            false
        );
        assert_eq!(
            value["leadership_review_grant"]["current_liveness_not_asserted"],
            true
        );
        assert!(CONTINUATION_CLOCK_NOTICE.contains("Defer remains independently admissible"));
        assert!(CONTINUATION_CLOCK_NOTICE.contains("retained Defers remain valid"));
        assert!(CONTINUATION_CLOCK_NOTICE.contains("AFTER adoption"));
        assert!(CONTINUATION_CLOCK_NOTICE.contains("grants no inspection capability"));
    }

    #[test]
    fn public_projection_uses_borrowed_sealed_facts_without_catalogue_capability_claims() {
        let (mut context, _, _) =
            super::super::adaptive_leadership_review::tests::funded_prompt_fixture();
        context.source.tool_catalog = json!({"schema_version": 1, "tools": []});
        let before = serde_json::to_vec(&context).unwrap();
        let value = project(
            &context.source,
            &context.binding.grant,
            context.binding.issued_at_ms,
        )
        .unwrap();
        assert_eq!(
            value["accounting"]["funded_limits_for_this_review"]["status"],
            "proposed_awaiting_adoption_epoch"
        );
        assert_eq!(
            value["prospective_continuation"]["new_tool_or_inspection_permission_granted"],
            false
        );
        assert!(value.get("tools").is_none());
        assert_eq!(serde_json::to_vec(&context).unwrap(), before);
        let mut invalid_grant = context.binding.grant.clone();
        invalid_grant.schema_version = 3;
        assert!(project(
            &context.source,
            &invalid_grant,
            context.binding.issued_at_ms
        )
        .is_err());
        invalid_grant = context.binding.grant.clone();
        invalid_grant.subject = None;
        assert!(project(
            &context.source,
            &invalid_grant,
            context.binding.issued_at_ms
        )
        .is_err());
        invalid_grant = context.binding.grant.clone();
        invalid_grant.work_funding = None;
        assert!(project(
            &context.source,
            &invalid_grant,
            context.binding.issued_at_ms
        )
        .is_err());
    }
}
