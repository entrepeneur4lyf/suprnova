//! Phase 10C audit-fix AF4 — process-global registries expose a
//! sync `clear()` for opt-in test teardown.
//!
//! The audit (Area 4) flagged that `EventDispatcher`, the cancellable-
//! listener registry, and `ScopeRegistry` are all process-global with
//! no test-teardown hook. Practitioners get away with it by using
//! per-test unique model types, but a test that wants strict isolation
//! had no way to wipe the registries.
//!
//! AF4 ships sync `clear()` / `clear_global()` /
//! `clear_cancellable_listeners()` for the three registries. They are
//! NOT wired into `TestContainerGuard::drop` — that would break
//! parallel test execution (test A's drop clearing test B's still-
//! needed listeners, since cargo runs `#[tokio::test]`s on shared
//! process-global registries). Tests that need a wipe call them
//! explicitly.
//!
//! Each test follows the same shape:
//!
//! 1. Register a listener for a test-local event type.
//! 2. Fire it once, observe the count tick.
//! 3. Call the clear() helper directly.
//! 4. Fire again, observe the count stays where it was — listener
//!    cleared.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use suprnova::eloquent::events::{
    dispatch_cancellable, listen_cancellable, CancellableListener, EventResult,
};
use suprnova::events::Event;
use suprnova::testing::TestDatabase;
use suprnova::FrameworkError;

// ---- Cancellable listener registry --------------------------------------

#[derive(Clone, Debug)]
struct Af4Saving;

impl Event for Af4Saving {
    fn event_name() -> &'static str {
        "Af4Saving"
    }
}

static AF4_FIRES: AtomicUsize = AtomicUsize::new(0);

struct Af4Counter;

#[async_trait::async_trait]
impl CancellableListener<Af4Saving> for Af4Counter {
    async fn handle(&self, _event: &Af4Saving) -> EventResult {
        AF4_FIRES.fetch_add(1, Ordering::SeqCst);
        EventResult::ok()
    }
}

#[tokio::test]
async fn cancellable_listener_registry_clears_on_demand() {
    AF4_FIRES.store(0, Ordering::SeqCst);

    let _db = TestDatabase::sqlite_memory().await.unwrap();
    listen_cancellable::<Af4Saving, _>(Arc::new(Af4Counter)).await;

    dispatch_cancellable::<Af4Saving>(Af4Saving).await.unwrap();
    assert_eq!(
        AF4_FIRES.load(Ordering::SeqCst),
        1,
        "listener should fire after registration"
    );

    // Opt-in clear — the registry is process-global so tests that
    // want strict isolation reach for this directly.
    suprnova::eloquent::events::clear_cancellable_listeners();

    dispatch_cancellable::<Af4Saving>(Af4Saving).await.unwrap();
    assert_eq!(
        AF4_FIRES.load(Ordering::SeqCst),
        1,
        "post-clear dispatch must be a no-op — listener has been wiped"
    );
}

// ---- EventDispatcher (non-cancellable) ----------------------------------

#[derive(Clone, Debug)]
struct Af4Tick;

impl Event for Af4Tick {
    fn event_name() -> &'static str {
        "Af4Tick"
    }
}

static AF4_TICK_FIRES: AtomicUsize = AtomicUsize::new(0);

struct Af4TickListener;

#[async_trait::async_trait]
impl suprnova::events::Listener<Af4Tick> for Af4TickListener {
    async fn handle(&self, _event: &Af4Tick) -> Result<(), FrameworkError> {
        AF4_TICK_FIRES.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn event_dispatcher_clears_on_demand() {
    AF4_TICK_FIRES.store(0, Ordering::SeqCst);

    let _db = TestDatabase::sqlite_memory().await.unwrap();
    suprnova::events::EventFacade::listen::<Af4Tick, _>(Arc::new(Af4TickListener)).await;

    suprnova::events::EventFacade::dispatch(Af4Tick).await.unwrap();
    assert_eq!(
        AF4_TICK_FIRES.load(Ordering::SeqCst),
        1,
        "listener should fire after registration"
    );

    suprnova::events::EventDispatcher::clear_global();

    suprnova::events::EventFacade::dispatch(Af4Tick).await.unwrap();
    assert_eq!(
        AF4_TICK_FIRES.load(Ordering::SeqCst),
        1,
        "post-clear dispatch must be a no-op"
    );
}

// Tag-only model for the scope-registry clear test. We don't query
// through it; we just need a TypeId-distinct slot in the registry to
// register against. The model lives at module scope because the
// `#[suprnova::model]` attribute macro emits items that reference
// `super`, which can't be nested inside a function body.
#[suprnova::model(table = "af4_scoped")]
pub struct Af4Scoped {
    pub id: i64,
}

pub struct Af4NoopScope;

impl suprnova::eloquent::GlobalScope<Af4Scoped> for Af4NoopScope {
    fn apply(&self, b: suprnova::Builder<Af4Scoped>) -> suprnova::Builder<Af4Scoped> {
        b
    }
}

#[tokio::test]
async fn scope_registry_clears_on_demand() {
    // Register a scope, then call the AF4 clear hook. The registry's
    // internals are private — we can't probe it directly — so the
    // signal is: a second `register` after `clear` runs without
    // panicking (the registry is in a fresh state). The first
    // register is the seed; the clear is what we're pinning; the
    // second register exercises the post-clear path.
    suprnova::eloquent::ScopeRegistry::register::<Af4Scoped, _>(Af4NoopScope);
    suprnova::eloquent::ScopeRegistry::clear();
    suprnova::eloquent::ScopeRegistry::register::<Af4Scoped, _>(Af4NoopScope);
}
