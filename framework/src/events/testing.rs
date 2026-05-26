//! `Event::fake()` — replaces the global dispatcher with one that
//! records dispatched events instead of invoking listeners. The
//! returned guard restores listener invocation on drop.

use super::Event;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct FakeStore {
    recorded: HashMap<TypeId, Vec<Box<dyn Any + Send + Sync>>>,
}

static FAKE: Mutex<Option<FakeStore>> = Mutex::new(None);

pub(crate) fn is_active() -> bool {
    FAKE.lock().unwrap().is_some()
}

pub(crate) fn record<E: Event>(event: E) {
    if let Some(store) = FAKE.lock().unwrap().as_mut() {
        store
            .recorded
            .entry(TypeId::of::<E>())
            .or_default()
            .push(Box::new(event));
    }
}

/// Replace the global dispatcher with a fake. Returns a guard that
/// removes the fake on drop, restoring real listener invocation.
pub fn install_fake() -> EventFakeGuard {
    *FAKE.lock().unwrap() = Some(FakeStore::default());
    EventFakeGuard
}

pub struct EventFakeGuard;

impl Drop for EventFakeGuard {
    fn drop(&mut self) {
        *FAKE.lock().unwrap() = None;
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
    let guard = FAKE.lock().unwrap();
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
    use tokio::sync::Mutex;

    // Tests in this module share the global `FAKE` store, so they
    // need to run serially to avoid cross-test contamination. Use
    // `tokio::sync::Mutex` so the guard can be safely held across
    // `.await` points.
    static TEST_LOCK: Mutex<()> = Mutex::const_new(());

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
        let _serial = TEST_LOCK.lock().await;
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
        let _serial = TEST_LOCK.lock().await;
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
