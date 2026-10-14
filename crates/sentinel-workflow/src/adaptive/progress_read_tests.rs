use super::continuation_tests::{effect, grant, NOW};
use super::work_funding_tests::{epoch_for, funded_authorization};
use super::*;

// Independent reference for the pre-projection admission contract.
fn reference(session: &AdaptiveSessionV1, now: u64) -> AdaptiveModelAdmissionV1 {
    if now < session.updated_at_ms {
        return AdaptiveModelAdmissionV1::DeadlineExpired;
    }
    if session.model_calls >= session.active_model_ceiling() {
        return AdaptiveModelAdmissionV1::CallsExhausted;
    }
    if now >= session.active_deadline_ms() {
        return AdaptiveModelAdmissionV1::DeadlineExpired;
    }
    let margin = session
        .active_work_funding()
        .map(|epoch| epoch.binding.limits.dispatch_margin_ms)
        .or_else(|| {
            session
                .continuation
                .as_ref()
                .and_then(|state| state.authorizations.last())
                .and_then(|auth| auth.resume_policy.as_ref())
                .map(|binding| binding.limits.dispatch_margin_ms)
        });
    if margin.is_some_and(|margin| {
        session
            .effective_call_duration_ms()
            .checked_add(margin)
            .is_none_or(|minimum| session.active_deadline_ms() - now < minimum)
    }) {
        return AdaptiveModelAdmissionV1::InsufficientSlack;
    }
    AdaptiveModelAdmissionV1::Admissible
}

#[test]
fn compact_progress_matches_legacy_and_funded_admission_at_every_boundary() {
    let root = AdaptiveSessionV1::initial(grant()).unwrap();
    let epoch = epoch_for(&root, 500);
    let auth = funded_authorization(&root, epoch, root.active_deadline_ms());
    let funded = root
        .transition(
            &AdaptiveTransitionV1::ContinueGoverned {
                authorization: auth.clone(),
            },
            auth.issued_at_ms,
        )
        .unwrap();
    let mut legacy = root.clone();
    let mut legacy_auth = super::continuation_tests::authorization(&legacy);
    legacy_auth.issued_at_ms = legacy.updated_at_ms;
    legacy_auth.deadline_ms = legacy.updated_at_ms + 4_000;
    legacy.continuation = Some(AdaptiveContinuationStateV1 {
        model_ceiling: 4,
        observation_required: false,
        authorizations: vec![legacy_auth],
    });
    for session in [root, legacy, funded] {
        for calls in [
            0,
            session.active_model_ceiling().saturating_sub(1),
            session.active_model_ceiling(),
        ] {
            let mut session = session.clone();
            session.model_calls = calls;
            let facts = session.progress_read_facts();
            let deadline = session.active_deadline_ms();
            let minimum = session.effective_call_duration_ms().saturating_add(
                session
                    .active_work_funding()
                    .map_or(0, |epoch| epoch.binding.limits.dispatch_margin_ms),
            );
            for now in [
                session.updated_at_ms - 1,
                session.updated_at_ms,
                deadline.saturating_sub(minimum).saturating_sub(1),
                deadline.saturating_sub(minimum),
                deadline.saturating_sub(minimum).saturating_add(1),
                deadline - 1,
                deadline,
                deadline + 1,
            ] {
                let admission = reference(&session, now);
                assert_eq!(session.model_admission_at(now), admission);
                assert_eq!(
                    facts.model_window_exhausted_at(now),
                    now >= session.updated_at_ms
                        && admission != AdaptiveModelAdmissionV1::Admissible
                );
            }
        }
    }
}

#[test]
fn compact_progress_preserves_kind_privacy_and_checked_slack_overflow() {
    let mut session = AdaptiveSessionV1::initial(grant()).unwrap();
    let tool = WorkbenchTool::InspectFile {
        path: "private-path".into(),
        max_bytes: 1,
    };
    for (cursor, kind) in [
        (
            AdaptiveCursorV1::Blocked {
                reason_code: "no_private_observation".into(),
            },
            AdaptiveProgressReadKind::NoPrivateObservation,
        ),
        (
            AdaptiveCursorV1::Blocked {
                reason_code: "PRIVATE_REASON".into(),
            },
            AdaptiveProgressReadKind::Other,
        ),
        (
            AdaptiveCursorV1::ModelUnknown { effect: effect(55) },
            AdaptiveProgressReadKind::ModelUnknown,
        ),
        (
            AdaptiveCursorV1::ToolUnknown {
                effect: effect(56),
                tool,
                tool_digest: "a".repeat(64),
            },
            AdaptiveProgressReadKind::ToolUnknown,
        ),
        (AdaptiveCursorV1::Cancelled, AdaptiveProgressReadKind::Other),
    ] {
        session.cursor = cursor;
        let facts = session.progress_read_facts();
        assert_eq!(facts.kind(), kind);
        assert!(!facts.model_window_exhausted_at(session.active_deadline_ms() + 1));
        let encoded = serde_json::to_string(&facts).unwrap();
        assert!(!encoded.contains("PRIVATE_REASON") && !encoded.contains("private-path"));
    }
    let facts = ModelAdmissionReadFacts {
        updated_at_ms: NOW,
        model_calls: 0,
        model_ceiling: 1,
        deadline_ms: NOW + 1_000,
        call_duration_ms: u64::MAX,
        dispatch_margin_ms: Some(1),
    };
    assert_eq!(
        facts.admission_at(NOW),
        AdaptiveModelAdmissionV1::InsufficientSlack
    );
}
