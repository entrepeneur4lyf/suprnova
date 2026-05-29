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
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

#[derive(Default)]
struct FakeStore {
    /// Events recorded keyed by `TypeId` so the typed assertion helpers can
    /// downcast.
    recorded: HashMap<TypeId, Vec<Box<dyn Any + Send + Sync>>>,
    /// Also keyed by the user-visible `event_name()` so [`dispatched_events`]
    /// can report the count without iterating every `TypeId`.
    recorded_by_name: HashMap<&'static str, usize>,
    /// Mode controlling which events the fake intercepts:
    /// - `All` — every event is faked (default `Event::fake()`)
    /// - `Only(names)` — only the listed names are faked; everything else
    ///   passes through to the real dispatcher (`Event::fake_only`)
    /// - `Except(names)` — every event is faked EXCEPT the listed names
    ///   (`Event::fake_except` / Laravel's `EventFake::except`)
    mode: FakeMode,
    /// Registered listener types observed while the fake is active. Powers
    /// [`assert_listening`]: tests register their listener inside `Event::fake`,
    /// then call `assert_listening::<E, L>()` to confirm the registration
    /// happened.
    listening: HashSet<(TypeId, &'static str)>,
}

#[derive(Default)]
enum FakeMode {
    #[default]
    All,
    Only(HashSet<&'static str>),
    Except(HashSet<&'static str>),
}

/// Process-wide serializer: only one test may hold the event fake at a time.
static FAKE_SERIAL: Mutex<()> = Mutex::new(());
static FAKE: Mutex<Option<FakeStore>> = Mutex::new(None);
// Per-task flag for [`muted`]: when set, every dispatch through the global
// `Event` facade is discarded without recording. Mirrors Laravel's
// `NullDispatcher` semantic when run inside a guarded scope.
tokio::task_local! {
    static MUTE_FLAG: ();
}

/// Poison-safe access to the fake store (never aborts the process on a
/// poisoned mutex — a panicking test must not take the whole suite down).
fn lock_fake() -> MutexGuard<'static, Option<FakeStore>> {
    FAKE.lock().unwrap_or_else(|e| e.into_inner())
}

/// True when an active fake should INTERCEPT the named event (record it and
/// suppress listener invocation). Returns `false` when no fake is installed
/// AND the task is not muted; in that case the caller falls through to the
/// real dispatcher.
///
/// For `fake_only` / `fake_except`, events outside the configured filter pass
/// through to the real dispatcher — that's the parity match for Laravel's
/// `EventFake::shouldFakeEvent`.
pub(crate) fn is_active<E: Event>() -> bool {
    if MUTE_FLAG.try_with(|_| ()).is_ok() {
        return true;
    }
    let guard = lock_fake();
    let Some(store) = guard.as_ref() else {
        return false;
    };
    match &store.mode {
        FakeMode::All => true,
        FakeMode::Only(set) => set.contains(E::event_name()),
        FakeMode::Except(set) => !set.contains(E::event_name()),
    }
}

/// True iff a fake is currently installed on the global dispatcher (regardless
/// of `fake_only` / `fake_except` filtering). Used by `Event::push` to skip
/// recording — Laravel's `EventFake::push` is a deliberate no-op.
pub(crate) fn fake_installed() -> bool {
    lock_fake().is_some()
}

pub(crate) fn record<E: Event>(event: E) {
    // Muted dispatches discard the event without recording.
    if MUTE_FLAG.try_with(|_| ()).is_ok() {
        return;
    }
    if let Some(store) = lock_fake().as_mut() {
        store
            .recorded
            .entry(TypeId::of::<E>())
            .or_default()
            .push(Box::new(event));
        *store.recorded_by_name.entry(E::event_name()).or_insert(0) += 1;
    }
}

pub(crate) fn record_listener<E: Event, L: 'static>() {
    if let Some(store) = lock_fake().as_mut() {
        store.listening.insert((TypeId::of::<L>(), E::event_name()));
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

/// Install a fake that intercepts only the listed event names; every other
/// event passes through to the real dispatcher. Mirrors Laravel's
/// `Event::fake([UserRegistered::class, ...])`.
pub fn install_fake_only(names: &[&'static str]) -> EventFakeGuard {
    let serial = FAKE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let store = FakeStore {
        mode: FakeMode::Only(names.iter().copied().collect()),
        ..FakeStore::default()
    };
    *lock_fake() = Some(store);
    EventFakeGuard { _serial: serial }
}

/// Install a fake that intercepts every event EXCEPT the listed names; the
/// excepted events pass through to the real dispatcher. Mirrors Laravel's
/// `EventFake::except($events)`.
pub fn install_fake_except(names: &[&'static str]) -> EventFakeGuard {
    let serial = FAKE_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let store = FakeStore {
        mode: FakeMode::Except(names.iter().copied().collect()),
        ..FakeStore::default()
    };
    *lock_fake() = Some(store);
    EventFakeGuard { _serial: serial }
}

/// Run `callback` with every dispatched event silently discarded (no
/// recording, no listener invocation). Mirrors Laravel's `NullDispatcher`
/// scoped to a callback.
///
/// Unlike [`install_fake`], `muted` does NOT acquire the process-wide
/// serializer — the mute flag is task-local, so two tasks can be muted in
/// parallel without contention. Inside `muted` the global fake (if any) is
/// also bypassed for recording; assertions made on the fake after `muted`
/// returns will see no dispatches captured from inside `muted`.
pub async fn muted<F, T>(callback: F) -> T
where
    F: std::future::Future<Output = T> + Send,
{
    MUTE_FLAG.scope((), callback).await
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

/// Assert that exactly one event of type `E` was dispatched (no predicate;
/// every recorded `E` counts). Mirrors Laravel's `assertDispatchedOnce`.
pub fn assert_dispatched_once<E: Event>() {
    assert_dispatched_times::<E>(1);
}

/// Assert that exactly `times` events of type `E` were dispatched. Mirrors
/// Laravel's `assertDispatchedTimes`.
pub fn assert_dispatched_times<E: Event>(times: usize) {
    let count = dispatched_count::<E>(|_| true);
    assert_eq!(
        count,
        times,
        "expected {} to be dispatched {} times, found {}",
        E::event_name(),
        times,
        count
    );
}

/// Assert that NO event of any type was dispatched while the fake was active.
/// Mirrors Laravel's `assertNothingDispatched`.
pub fn assert_nothing_dispatched() {
    let names = dispatched_events();
    assert!(
        names.is_empty(),
        "expected no events to be dispatched, found: {names:?}"
    );
}

/// True when at least one event of type `E` has been recorded by the fake.
/// Mirrors Laravel's `EventFake::hasDispatched`.
pub fn has_dispatched<E: Event>() -> bool {
    let guard = lock_fake();
    let Some(store) = guard.as_ref() else {
        return false;
    };
    store
        .recorded
        .get(&TypeId::of::<E>())
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Return clones of every dispatched event of type `E` matching `pred`.
/// Mirrors Laravel's `EventFake::dispatched($event, $callback)`. Useful when
/// you need to inspect multiple fields on a captured event, not just count
/// matches.
pub fn dispatched<E: Event>(pred: impl Fn(&E) -> bool) -> Vec<E> {
    let guard = lock_fake();
    let Some(store) = guard.as_ref() else {
        return Vec::new();
    };
    store
        .recorded
        .get(&TypeId::of::<E>())
        .map(|bucket| {
            bucket
                .iter()
                .filter_map(|b| b.downcast_ref::<E>())
                .filter(|e| pred(e))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Return a map of every dispatched event's name to its dispatch count.
/// Mirrors Laravel's `EventFake::dispatchedEvents()` (Laravel returns the
/// payload arrays; we return counts, which is the shape Suprnova tests
/// typically need — use [`dispatched`] for the typed-payload form).
pub fn dispatched_events() -> HashMap<&'static str, usize> {
    let guard = lock_fake();
    guard
        .as_ref()
        .map(|store| store.recorded_by_name.clone())
        .unwrap_or_default()
}

/// Assert that a listener of type `L` has been registered for event type `E`
/// while the fake was active. Mirrors Laravel's `assertListening`.
///
/// The fake observes registrations via [`record_listener`], which the
/// dispatcher's `listen` method calls when a fake is active. This means the
/// listener registration must happen INSIDE the `Event::fake()` scope; tests
/// that register at module load (before the fake) will not be observed.
pub fn assert_listening<E: Event, L: 'static>() {
    let guard = lock_fake();
    let store = guard
        .as_ref()
        .expect("Event::fake() must be active to call assert_listening");
    assert!(
        store
            .listening
            .contains(&(TypeId::of::<L>(), E::event_name())),
        "expected a listener of type {} for event {} to be registered",
        std::any::type_name::<L>(),
        E::event_name()
    );
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

    // ----- Laravel-13 parity assertions ------------------------------------

    #[derive(Debug, Clone)]
    struct OneShot;
    impl crate::events::Event for OneShot {
        fn event_name() -> &'static str {
            "OneShot"
        }
    }

    #[tokio::test]
    async fn assert_dispatched_once_passes_when_dispatched_exactly_once() {
        let _guard = EventFacade::fake();
        EventFacade::dispatch(OneShot).await.unwrap();
        assert_dispatched_once::<OneShot>();
    }

    #[tokio::test]
    #[should_panic(expected = "expected OneShot to be dispatched 1 times")]
    async fn assert_dispatched_once_fails_when_more_than_one() {
        let _guard = EventFacade::fake();
        EventFacade::dispatch(OneShot).await.unwrap();
        EventFacade::dispatch(OneShot).await.unwrap();
        // assert_dispatched_once delegates to assert_dispatched_times(1)
        assert_dispatched_once::<OneShot>();
    }

    #[tokio::test]
    async fn assert_dispatched_times_counts_exact_dispatches() {
        let _guard = EventFacade::fake();
        EventFacade::dispatch(OneShot).await.unwrap();
        EventFacade::dispatch(OneShot).await.unwrap();
        EventFacade::dispatch(OneShot).await.unwrap();
        assert_dispatched_times::<OneShot>(3);
    }

    #[tokio::test]
    async fn assert_nothing_dispatched_passes_when_no_events_fired() {
        let _guard = EventFacade::fake();
        assert_nothing_dispatched();
    }

    #[tokio::test]
    #[should_panic(expected = "expected no events to be dispatched")]
    async fn assert_nothing_dispatched_fails_when_an_event_fired() {
        let _guard = EventFacade::fake();
        EventFacade::dispatch(OneShot).await.unwrap();
        assert_nothing_dispatched();
    }

    #[tokio::test]
    async fn has_dispatched_reports_recorded_event_presence() {
        let _guard = EventFacade::fake();
        assert!(!has_dispatched::<OneShot>());
        EventFacade::dispatch(OneShot).await.unwrap();
        assert!(has_dispatched::<OneShot>());
    }

    #[tokio::test]
    async fn dispatched_returns_clones_of_matching_events() {
        let _guard = EventFacade::fake();
        EventFacade::dispatch(Noted { note: "x".into() })
            .await
            .unwrap();
        EventFacade::dispatch(Noted { note: "y".into() })
            .await
            .unwrap();
        EventFacade::dispatch(Noted { note: "x".into() })
            .await
            .unwrap();
        let xs = dispatched::<Noted>(|e| e.note == "x");
        assert_eq!(xs.len(), 2);
        assert!(xs.iter().all(|e| e.note == "x"));
    }

    #[tokio::test]
    async fn dispatched_events_returns_name_to_count_map() {
        let _guard = EventFacade::fake();
        EventFacade::dispatch(OneShot).await.unwrap();
        EventFacade::dispatch(OneShot).await.unwrap();
        EventFacade::dispatch(Noted { note: "n".into() })
            .await
            .unwrap();
        let names = dispatched_events();
        assert_eq!(names.get("OneShot").copied(), Some(2));
        assert_eq!(names.get("Noted").copied(), Some(1));
    }

    // ----- fake_only / fake_except / muted ---------------------------------

    #[tokio::test]
    async fn fake_only_intercepts_named_events_and_passes_others_through() {
        // Pre-condition: dispatch the un-faked event to verify it goes
        // through to the global dispatcher without panicking. (No listener
        // is registered for OneShot on the global dispatcher in this test.)
        let _guard = EventFacade::fake_only(&["Noted"]);
        EventFacade::dispatch(Noted {
            note: "captured".into(),
        })
        .await
        .unwrap();
        // Non-faked event passes through silently (no listeners → Ok(())).
        EventFacade::dispatch(OneShot).await.unwrap();
        assert_dispatched::<Noted>(|e| e.note == "captured");
        assert!(
            !has_dispatched::<OneShot>(),
            "OneShot is not in fake_only's list — must not be recorded"
        );
    }

    #[tokio::test]
    async fn fake_except_passes_through_named_events_and_intercepts_others() {
        let _guard = EventFacade::fake_except(&["OneShot"]);
        EventFacade::dispatch(Noted {
            note: "captured".into(),
        })
        .await
        .unwrap();
        EventFacade::dispatch(OneShot).await.unwrap();
        assert_dispatched::<Noted>(|e| e.note == "captured");
        assert!(
            !has_dispatched::<OneShot>(),
            "OneShot is in the except list — must pass through, not record"
        );
    }

    #[tokio::test]
    async fn muted_discards_events_silently() {
        // muted does NOT acquire the FAKE_SERIAL lock — it's task-local.
        EventFacade::muted(async {
            EventFacade::dispatch(OneShot).await.unwrap();
            EventFacade::dispatch(Noted {
                note: "ignored".into(),
            })
            .await
            .unwrap();
        })
        .await;
        // After muted returns, the global fake is not installed (we never
        // installed one), so verify state by ensuring no panic and no
        // tracking. Nothing else to assert — silence is the contract.
    }

    // ----- assert_listening ------------------------------------------------

    struct Marker;
    use crate::FrameworkError;
    use async_trait::async_trait;
    #[async_trait]
    impl crate::events::Listener<OneShot> for Marker {
        async fn handle(&self, _e: &OneShot) -> Result<(), FrameworkError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn assert_listening_sees_listener_registered_inside_fake_scope() {
        let _guard = EventFacade::fake();
        EventFacade::listen::<OneShot, Marker>(std::sync::Arc::new(Marker)).await;
        assert_listening::<OneShot, Marker>();
    }
}
