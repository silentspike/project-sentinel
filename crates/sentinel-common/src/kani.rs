use crate::{decode_snapshot_cursor, encode_snapshot_cursor, SnapshotCursor, WorldSnapshot};

#[kani::proof]
fn snapshot_cursor_roundtrip_preserves_cursor_fields() {
    let tick_u8: u8 = kani::any();
    let last_event_u8: u8 = kani::any();
    let tick = tick_u8 as u64;
    let last_event_id = last_event_u8 as i64;

    let cursor = SnapshotCursor {
        schema_version: WorldSnapshot::SCHEMA_VERSION,
        tick,
        ecs_tick: tick,
        last_event_id,
    };

    let encoded = encode_snapshot_cursor(cursor).expect("snapshot cursor should encode");
    let decoded = decode_snapshot_cursor(&encoded).expect("snapshot cursor should decode");

    assert_eq!(decoded, cursor);
}
