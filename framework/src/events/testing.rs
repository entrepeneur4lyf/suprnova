//! `Event::fake()` — replaces the global dispatcher with one that
//! records dispatched events instead of invoking listeners.
//!
//! `install_fake()` acquires a process-wide serialization mutex for the
//! lifetime of the returned `EventFakeGuard`, so parallel `#[tokio::test]`s
//! that fake events run one at a time and cannot clobber each other's
//! recorded-events store. This mirrors [`crate::queue::testing`]. The
//! single shared `FAKE` store means nested `Event::fake()` on one task is
//! unsupported (it would deadlock on the serializer) — fake exactly once
//! per test.

use super::Event;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

#[derive(Default)]
struct FakeStore {
    recorded: HashMap<TypeId, Vec<Box<dyn Any + Send + Sync>>>,
}

/// Process-wide serializer: only one test may hold the event fake at a time.
static FAKE_SERIAL: Mutex<()> = Mutex::new(());
static FAKE: Mutex<Option<FakeStore>> = Mutex::new(None);

/// Poison-safe access to the fake store (never aborts the process on a
/// poisoned mutex — a panicking test must not take the whole suite down).
fn lock_fake() -> MutexGuard<'static, Option<FakeStore>> {
    FAKE.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn is_active() -> bool {
    lock_fake().is_some()
}

pub(crate) fn record<E: Event>(event: E) {
    if let Some(store) = lock_fake().as_mut() {
        store
            .recorded
            .entry(TypeId::of::<E>())
            .or_default()
            .push(Box::new(event));
    }
}

/// Replace the global dispatcher with a fake. Returns a guard that removes
/// the fake on drop, restoring real listener invocation.
///
/// The guard holds a process-wide serialization lock so parallel
/// `#[tokio::test]`s using the fake run one at a time; it releases on drop.
/// (Tests therefore no longer need their own serializing mutex around
/// `Event::fake()`.)
pub fn install_fake() -> EventFakeGuard {
    let serial = FAKE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    *lock_fake() = Some(FakeStore::default());
    EventFakeGuard { _serial: serial }
}

pub struct EventFakeGuard {
    _serial: MutexGuard<'static, ()>,
}

impl Drop for EventFakeGuard {
    fn drop(&mut self) {
        *lock_fake() = None;
    }
}

/// Assert that at least one event of type `E` matching `pred` was
/// dispatched while the fake was active.
pub fn assert_dispatched<E: Event>(pred: impl Fn(&E) -> bool) {
    let count = dispatched_count::<E>(pred);
    assert!(
        count > 0,
        "expected at least one matching {} to be dispatched",
        E::event_name()
    );
}

/// Assert that no event of type `E` matching `pred` was dispatched.
pub fn assert_not_dispatched<E: Event>(pred: impl Fn(&E) -> bool) {
    let count = dispatched_count::<E>(pred);
    assert_eq!(
        count,
        0,
        "expected no matching {} to be dispatched, found {}",
        E::event_name(),
        count
    );
}

/// Count dispatched events of type `E` matching `pred`.
pub fn dispatched_count<E: Event>(pred: impl Fn(&E) -> bool) -> usize {
    let guard = lock_fake();
    let store = guard
        .as_ref()
        .expect("Event::fake() must be active to read dispatched_count");
    store
        .recorded
        .get(&TypeId::of::<E>())
        .map(|bucket| {
            bucket
                .iter()
                .filter_map(|b| b.downcast_ref::<E>())
                .filter(|e| pred(e))
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventFacade;

    // No manual serialization needed: `EventFacade::fake()` holds the
    // process-wide `FAKE_SERIAL` lock for the duration of each test.

    #[derive(Debug, Clone)]
    struct Noted {
        pub note: String,
    }
    impl crate::events::Event for Noted {
        fn event_name() -> &'static str {
            "Noted"
        }
    }

    #[tokio::test]
    async fn fake_records_dispatched_events_and_does_not_call_listeners() {
        let _guard = EventFacade::fake();
        EventFacade::dispatch(Noted { note: "hi".into() })
            .await
            .unwrap();
        EventFacade::dispatch(Noted { note: "bye".into() })
            .await
            .unwrap();
        assert_dispatched::<Noted>(|e| e.note == "hi");
        assert_dispatched::<Noted>(|e| e.note == "bye");
        assert_not_dispatched::<Noted>(|e| e.note == "nope");
    }

    #[tokio::test]
    async fn dispatched_count_works() {
        let _guard = EventFacade::fake();
        EventFacade::dispatch(Noted { note: "a".into() })
            .await
            .unwrap();
        EventFacade::dispatch(Noted { note: "a".into() })
            .await
            .unwrap();
        EventFacade::dispatch(Noted { note: "b".into() })
            .await
            .unwrap();
        assert_eq!(dispatched_count::<Noted>(|e| e.note == "a"), 2);
        assert_eq!(dispatched_count::<Noted>(|e| e.note == "b"), 1);
    }
}
