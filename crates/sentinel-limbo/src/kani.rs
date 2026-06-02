use crate::event_store::{classify_offset_update, OffsetUpdateDecision};

#[derive(Clone, Copy)]
struct OperationDedupModel {
    seen: [Option<u8>; 2],
    len: usize,
}

impl OperationDedupModel {
    fn new() -> Self {
        Self {
            seen: [None, None],
            len: 0,
        }
    }

    fn append(&mut self, operation_id: u8) -> bool {
        let mut index = 0;
        while index < self.len {
            if self.seen[index] == Some(operation_id) {
                return false;
            }
            index += 1;
        }

        self.seen[self.len] = Some(operation_id);
        self.len += 1;
        true
    }
}

#[kani::proof]
fn operation_dedup_model_is_idempotent_for_same_operation() {
    let operation_id: u8 = kani::any();
    let mut model = OperationDedupModel::new();

    assert!(model.append(operation_id));
    assert!(!model.append(operation_id));
    assert_eq!(model.len, 1);
}

#[kani::proof]
fn operation_dedup_model_accepts_distinct_operations() {
    let first: u8 = kani::any();
    let second: u8 = kani::any();
    kani::assume(first != second);

    let mut model = OperationDedupModel::new();

    assert!(model.append(first));
    assert!(model.append(second));
    assert_eq!(model.len, 2);
}

#[kani::proof]
fn projection_offset_decision_is_monotonic() {
    let has_current: bool = kani::any();
    let current_value: i64 = kani::any();
    let attempted: i64 = kani::any();
    let current = if has_current {
        Some(current_value)
    } else {
        None
    };

    let decision = classify_offset_update(current, attempted);

    if let Some(current) = current {
        if attempted < current {
            assert_eq!(decision, OffsetUpdateDecision::Reject);
        } else if attempted == current {
            assert_eq!(decision, OffsetUpdateDecision::Noop);
        } else {
            assert_eq!(decision, OffsetUpdateDecision::InsertOrAdvance);
        }
    } else {
        assert_eq!(decision, OffsetUpdateDecision::InsertOrAdvance);
    }
}
