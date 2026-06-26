//! #497 V27 — the per-container snapshot's cross-store consistency cut binds the boundary.

use sentinel_common::SnapshotCut;

#[test]
fn ecs_native_cut_pins_nothing_and_excludes_inbound() {
    // ECS-native default (no bwrap home, no CAS, bounded class): empty pin set, no inbound cursor.
    let d = SnapshotCut::default();
    assert!(d.cas_pin_set.is_empty(), "ECS-native cut pins no CAS blobs");
    assert!(
        d.inbound_cursor.is_none(),
        "Track-A bounded class excludes active inbound (no cursor)"
    );
}

#[test]
fn cut_round_trips_losslessly() {
    // A fenced cut with a CAS pin (the bwrap/K2 case) survives serialization unchanged — the cut
    // is the reconcile boundary, so it must not lose any of its fields.
    let cut = SnapshotCut {
        owner_epoch: 7,
        event_cursor: 4242,
        cas_pin_set: vec!["cas-blob:v1:sha256:deadbeef".into()],
        inbound_cursor: Some(99),
    };
    let json = serde_json::to_string(&cut).unwrap();
    let back: SnapshotCut = serde_json::from_str(&json).unwrap();

    assert_eq!(
        back.owner_epoch, 7,
        "owner-epoch fence is bound to the cut (#496)"
    );
    assert_eq!(
        back.event_cursor, 4242,
        "event cursor bounds the restore replay"
    );
    assert_eq!(
        back.cas_pin_set,
        vec!["cas-blob:v1:sha256:deadbeef".to_string()]
    );
    assert_eq!(back.inbound_cursor, Some(99));
}
