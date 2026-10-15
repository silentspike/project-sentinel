use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Hash, PartialEq, Eq)]
struct RecoveryKey {
    store: usize,
    store_type: &'static str,
    request_id: String,
}

static ACTIVE: OnceLock<Mutex<HashSet<RecoveryKey>>> = OnceLock::new();

fn active() -> &'static Mutex<HashSet<RecoveryKey>> {
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

// This only coalesces concurrent work on one store instance. Durable store
// claims remain authoritative across restarts and separate database handles.
pub(super) struct CompletionRecovery<'a, S> {
    key: RecoveryKey,
    _store: PhantomData<&'a S>,
}

impl<'a, S> CompletionRecovery<'a, S> {
    pub(super) fn try_acquire(store: &'a S, request_id: &str) -> Option<Self> {
        let key = RecoveryKey {
            store: std::ptr::from_ref(store).addr(),
            store_type: std::any::type_name::<S>(),
            request_id: request_id.to_owned(),
        };
        let mut entries = active().lock().unwrap_or_else(|error| error.into_inner());
        if !entries.insert(key.clone()) {
            return None;
        }
        Some(Self {
            key,
            _store: PhantomData,
        })
    }
}

impl<S> Drop for CompletionRecovery<'_, S> {
    fn drop(&mut self) {
        active()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_recovery_only_coalesces_same_store_and_request() {
        let first = 1_u64;
        let second = 2_u64;
        let guard = CompletionRecovery::try_acquire(&first, "same").unwrap();
        assert!(CompletionRecovery::try_acquire(&first, "same").is_none());
        assert!(CompletionRecovery::try_acquire(&first, "other").is_some());
        assert!(CompletionRecovery::try_acquire(&second, "same").is_some());
        drop(guard);
        assert!(CompletionRecovery::try_acquire(&first, "same").is_some());
    }

    #[test]
    fn completion_recovery_unwind_releases_the_request() {
        let store = 3_u64;
        assert!(std::panic::catch_unwind(|| {
            let _guard = CompletionRecovery::try_acquire(&store, "panic").unwrap();
            panic!("injected admission panic");
        })
        .is_err());
        assert!(CompletionRecovery::try_acquire(&store, "panic").is_some());
    }
}
