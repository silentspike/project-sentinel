use super::*;
use crate::adaptive::continuation_tests::{grant, NOW};
use crate::adaptive::work_funding_tests::{epoch_for, funded_authorization};

fn anchored_epoch(
    connection: &Connection,
    source: &AdaptiveSessionV1,
) -> crate::AdaptiveWorkFundingEpochV1 {
    let mut epoch = epoch_for(source, 10);
    let ns = namespace(source.grant.session_id);
    let (root_digest, _) = evidence_entry(connection, &ns, 1).unwrap();
    let (head_digest, _) = evidence_entry(connection, &ns, source.version).unwrap();
    epoch.receipt.request.source.resume_source.root_entry_digest = root_digest;
    epoch.receipt.request.source.resume_source.head_entry_digest = head_digest;
    epoch.binding = epoch.receipt.binding(1).unwrap();
    epoch
}

#[test]
fn descriptive_epoch_cannot_insert_membership_and_transaction_rolls_back() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("funding.sqlite");
    let store = WorkflowStore::open(&path).unwrap();
    let root = grant();
    let source = store
        .begin_adaptive_session(&root, &root.authority, NOW)
        .unwrap()
        .1;
    {
        let mut connection = store.lock().unwrap();
        let epoch = anchored_epoch(&connection, &source);
        let auth = funded_authorization(&source, epoch, source.grant.deadline_ms);
        let tx = immediate(&mut connection).unwrap();
        assert!(insert_funding_adoption_membership(&tx, &auth, &root.authority).is_err());
        tx.rollback().unwrap();
        assert_eq!(
            load(&connection, root.session_id).unwrap().unwrap().0,
            source
        );
    }
    drop(store);
    let reopened = WorkflowStore::open(&path).unwrap();
    let connection = reopened.lock().unwrap();
    assert_eq!(
        load(&connection, root.session_id).unwrap().unwrap().0,
        source
    );
}

#[test]
fn forged_funded_journal_without_authoritative_membership_fails_replay_and_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("funding.sqlite");
    let store = WorkflowStore::open(&path).unwrap();
    let root = grant();
    let source = store
        .begin_adaptive_session(&root, &root.authority, NOW)
        .unwrap()
        .1;
    {
        let mut connection = store.lock().unwrap();
        let epoch = anchored_epoch(&connection, &source);
        require_work_funding_anchor(&connection, &epoch, &source).unwrap();
        let auth = funded_authorization(&source, epoch, source.grant.deadline_ms);
        assert!(
            require_funding_authorization_membership(&connection, &auth, &root.authority).is_err()
        );
        let command = AdaptiveTransitionV1::ContinueGoverned {
            authorization: auth.clone(),
        };
        let next = source.transition(&command, auth.issued_at_ms).unwrap();
        let tx = immediate(&mut connection).unwrap();
        let (_, prior_digest) = load(&tx, root.session_id).unwrap().unwrap();
        append(
            &tx,
            &namespace(root.session_id),
            &Entry {
                previous_digest: Some(prior_digest),
                command: Some(command),
                session: next.clone(),
                recovery_feedback: None,
            },
        )
        .unwrap();
        update_head(&tx, &source, &next).unwrap();
        tx.commit().unwrap();
        assert!(load(&connection, root.session_id).is_err());
    }
    drop(store);
    let reopened = WorkflowStore::open(&path).unwrap();
    let connection = reopened.lock().unwrap();
    assert!(load(&connection, root.session_id).is_err());
}

#[test]
fn funded_anchor_checks_exact_root_and_historical_head_not_current_clock() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(temp.path().join("funding.sqlite")).unwrap();
    let root = grant();
    let source = store
        .begin_adaptive_session(&root, &root.authority, NOW)
        .unwrap()
        .1;
    let connection = store.lock().unwrap();
    let epoch = anchored_epoch(&connection, &source);
    require_work_funding_anchor(&connection, &epoch, &source).unwrap();
    for root_digest in [true, false] {
        let mut changed = epoch.clone();
        if root_digest {
            changed
                .receipt
                .request
                .source
                .resume_source
                .root_entry_digest = "0".repeat(64);
        } else {
            changed
                .receipt
                .request
                .source
                .resume_source
                .head_entry_digest = "0".repeat(64);
        }
        changed.binding = changed.receipt.binding(1).unwrap();
        assert!(require_work_funding_anchor(&connection, &changed, &source).is_err());
    }
    let command = AdaptiveTransitionV1::ContinueGoverned {
        authorization: funded_authorization(&source, epoch.clone(), source.grant.deadline_ms),
    };
    let next = source
        .transition(&command, source.grant.deadline_ms)
        .unwrap();
    require_work_funding_anchor(&connection, &epoch, &next).unwrap();
}
