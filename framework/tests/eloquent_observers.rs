//! Phase 10C T2a — `Observer<M>` trait default no-op smoke test.
//!
//! T2a only ships the trait + inventory entry + `bootstrap_observers()`.
//! The `#[suprnova::observer(M)]` attribute that walks the impl block
//! and emits adapter listeners is T2b. The `#[model(observers = [...])]`
//! shorthand plus `Model::observe()` manual entry land in T2c.
//!
//! What this test verifies:
//! - `impl Observer<M> for X {}` with NO method bodies compiles. Every
//!   one of the 16 trait methods has a default no-op.
//! - Cancellable defaults (`creating`/`saving`/`updating`/`deleting`/
//!   `restoring`) return `EventResult::Ok`.
//! - Non-cancellable defaults (`created`/`saved`/`updated`/`deleted`/
//!   `trashed`/`restored`/`replicating`/`force_deleting`/
//!   `force_deleted`/`retrieving`/`retrieved`) return
//!   `Ok(())`.
//!
//! T2a does NOT wire the trait to the event registry. That's T2b's job
//! via the attribute macro. The trait stands on its own as the contract
//! between the user's observer impl and the framework's dispatch path.

use async_trait::async_trait;
use suprnova::eloquent::attrs::Attrs;
use suprnova::eloquent::events::EventResult;
use suprnova::eloquent::observers::Observer;
use suprnova::{model, FrameworkError, Model as _};

// ---- Model under test ---------------------------------------------------

#[model(table = "t2_users")]
pub struct T2User {
    pub id: i64,
    pub email: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

// ---- Observer with NO methods overridden --------------------------------

pub struct EmptyObserver;

#[async_trait]
impl Observer<T2User> for EmptyObserver {}

fn sample_user() -> T2User {
    // `#[suprnova::model]` emits private `__eager` / `__pivot` fields
    // for the 10B relation cache + pivot accessor surfaces. `..Default::default()`
    // fills them; the macro derives `Default` automatically.
    T2User {
        id: 1,
        email: "x@example.com".into(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        ..Default::default()
    }
}

// ---- Cancellable defaults: all return EventResult::Ok -------------------

#[tokio::test]
async fn trait_defaults_compile_with_no_methods() {
    let obs = EmptyObserver;
    // The Cancellable-five default to EventResult::Ok.
    assert!(matches!(
        obs.creating(&mut Attrs::new()).await,
        EventResult::Ok
    ));
    assert!(matches!(
        obs.saving(&mut Attrs::new(), true).await,
        EventResult::Ok
    ));
    let prev = sample_user();
    assert!(matches!(
        obs.updating(&prev, &mut Attrs::new()).await,
        EventResult::Ok
    ));
    let m = sample_user();
    assert!(matches!(obs.deleting(&m, false).await, EventResult::Ok));
    assert!(matches!(obs.restoring(&m).await, EventResult::Ok));

    // The Non-cancellable-eleven default to Ok(()).
    assert!(obs.retrieving().await.is_ok());
    assert!(obs.retrieved(&m).await.is_ok());
    assert!(obs.created(&m).await.is_ok());
    assert!(obs.saved(&m).await.is_ok());
    let prev = sample_user();
    let cur = sample_user();
    assert!(obs.updated(&prev, &cur).await.is_ok());
    assert!(obs.deleted(&m, false).await.is_ok());
    assert!(obs.trashed(&m).await.is_ok());
    assert!(obs.restored(&m).await.is_ok());
    let mut replica = sample_user();
    assert!(obs.replicating(&m, &mut replica).await.is_ok());
    assert!(obs.force_deleting(&m).await.is_ok());
    assert!(obs.force_deleted(&m).await.is_ok());
}

// ---- Observers can override individual methods --------------------------

pub struct CancellingObserver;

#[async_trait]
impl Observer<T2User> for CancellingObserver {
    async fn creating(&self, _attrs: &mut Attrs) -> EventResult {
        EventResult::cancel("policy refused")
    }

    async fn created(&self, _model: &T2User) -> Result<(), FrameworkError> {
        Err(FrameworkError::bad_request("after-create checked failed"))
    }
}

#[tokio::test]
async fn observer_can_override_individual_methods() {
    let obs = CancellingObserver;

    let mut attrs = Attrs::new();
    match obs.creating(&mut attrs).await {
        EventResult::Cancel(reason) => assert_eq!(reason, "policy refused"),
        EventResult::Ok => panic!("expected cancel"),
    }

    let m = sample_user();
    let err = obs.created(&m).await.expect_err("expected error");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("after-create checked failed"),
        "error message did not surface verbatim: {msg}"
    );

    // Non-overridden defaults still no-op.
    assert!(matches!(
        obs.saving(&mut Attrs::new(), false).await,
        EventResult::Ok
    ));
    assert!(obs.saved(&m).await.is_ok());
}

// ---- bootstrap_observers() is callable on an empty inventory ------------

#[tokio::test]
async fn bootstrap_observers_drains_inventory_cleanly() {
    // T2a does not ship the `#[suprnova::observer(M)]` attribute macro;
    // therefore no `ObserverEntry` is submitted to the inventory at
    // T2a. `bootstrap_observers` should return `Ok(())` cleanly with
    // an empty inventory.
    //
    // T2b WILL ship the macro that calls `inventory::submit!`. When
    // T2b's tests land, this test stays valid (the trivially-empty
    // inventory path remains exercised) but it no longer covers all
    // observed behaviour — T2b adds its own end-to-end install test.
    suprnova::eloquent::observers::bootstrap_observers()
        .await
        .expect("bootstrap_observers should succeed with no entries");

    // T2a does NOT promise idempotency in the presence of real
    // entries — that's T2b's contract. But the empty-inventory path
    // IS idempotent because it's just an empty loop.
    suprnova::eloquent::observers::bootstrap_observers()
        .await
        .expect("empty-inventory bootstrap should be safely repeatable");
}

// ---- Re-export surface ---------------------------------------------------

#[tokio::test]
async fn observer_types_reexport_at_crate_root() {
    // These four items must be reachable from `suprnova::`. The use
    // statements compile-fail if any name is missing.
    use suprnova::{bootstrap_observers, Observer as ObserverAlias, ObserverEntry, ObserverInstallFuture};

    let _ = bootstrap_observers; // fn pointer is Send
    let _: Option<&ObserverEntry> = None;
    fn _accepts<T: ObserverAlias<T2User>>(_: T) {}

    fn _produces_future() -> Option<ObserverInstallFuture> {
        None
    }
}

// =========================================================================
// T2b — `#[suprnova::observer(M)]` attribute macro
// =========================================================================
//
// T2b ships the attribute macro that walks an `impl Observer<M>` block,
// identifies which methods the user actually overrode, and emits one
// adapter listener per overridden method. The adapter listeners are
// registered through the same `EventFacade::listen` / `listen_cancellable`
// paths users call by hand, so the macro is a DX layer on top of T1's
// dispatch surface — nothing about the model's lifecycle path changes.
//
// The two observers below are the smallest fixture that exercises the
// "registers what's overridden, ignores the rest" contract:
//
// - `CountingObserver` overrides `created` only → exactly one listener
//   on `T2User::events::Created`.
// - `OnlyUpdatesObserver` overrides `updated` only → exactly one
//   listener on `T2User::events::Updated`. Creating a row must NOT
//   fire it.
//
// Both tests live in a single function so they share one bootstrap
// invocation. `bootstrap_observers()` is idempotent (the macro emits
// an `AtomicBool` gate per-observer), but the EventDispatcher's
// `OnceLock<EventDispatcher>` and the cancellable registry's
// `OnceLock<RwLock<...>>` are process-wide, so listeners installed by
// one test are visible to every later test in this binary. Combining
// the two checks into a single test eliminates the cross-test race on
// `T2User::create()` firing the `CountingObserver` adapter.

use std::sync::atomic::{AtomicUsize, Ordering};
use suprnova::testing::TestDatabase;

static CREATED_OBSERVER_FIRES: AtomicUsize = AtomicUsize::new(0);

pub struct CountingObserver;

#[suprnova::observer(T2User)]
#[async_trait]
impl Observer<T2User> for CountingObserver {
    async fn created(&self, _user: &T2User) -> Result<(), FrameworkError> {
        CREATED_OBSERVER_FIRES.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

static UPDATED_OBSERVER_FIRES: AtomicUsize = AtomicUsize::new(0);

pub struct OnlyUpdatesObserver;

#[suprnova::observer(T2User)]
#[async_trait]
impl Observer<T2User> for OnlyUpdatesObserver {
    async fn updated(
        &self,
        _previous: &T2User,
        _current: &T2User,
    ) -> Result<(), FrameworkError> {
        UPDATED_OBSERVER_FIRES.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// All assertions touching `CREATED_OBSERVER_FIRES` /
// `UPDATED_OBSERVER_FIRES` live in a single test because the binary's
// `OnceLock<EventDispatcher>` and the cancellable registry are
// process-global — once listeners install, ANY `T2User::create` call
// anywhere in this test binary increments the counters. Two separate
// `#[tokio::test]`s would race each other on the reset → create →
// assert sequence. Combining them eliminates that race without
// sacrificing coverage.

#[tokio::test]
async fn observer_macro_emits_only_overridden_method_adapters() {
    // Set up the database for `T2User::create` to write to. The schema
    // is the bare-minimum the `#[model(table = "t2_users")]` macro
    // expects. `TestDatabase::sqlite_memory()` registers the connection
    // with `TestContainer::singleton` so `T2User::create` resolves it
    // via `DB::connection()` without us threading the handle through.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t2_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            email TEXT NOT NULL,\
            created_at TEXT NOT NULL,\
            updated_at TEXT NOT NULL\
        )",
    )
    .await
    .unwrap();

    // Drain the observer inventory twice. The `AtomicBool` gate in
    // each generated install fn means this is safe to call repeatedly;
    // T2a's docs explicitly delegate this contract to T2b. If the gate
    // were missing, the second bootstrap would double-register the
    // adapter listener and the counter check below would see `2`.
    suprnova::eloquent::observers::bootstrap_observers()
        .await
        .unwrap();
    suprnova::eloquent::observers::bootstrap_observers()
        .await
        .unwrap();

    // Reset counters AFTER bootstrap so listeners that installed at
    // boot time (potentially from earlier tests in this binary) don't
    // poison the assertions. Counters were never incremented by
    // `bootstrap_observers` itself — only by `T2User::create` calls.
    CREATED_OBSERVER_FIRES.store(0, Ordering::SeqCst);
    UPDATED_OBSERVER_FIRES.store(0, Ordering::SeqCst);

    // Creating a row fires `created` exactly once. The macro emits an
    // adapter only for methods present in the impl block, so:
    //
    //   - `CountingObserver` (overrides `created` only) → 1 fire.
    //   - `OnlyUpdatesObserver` (overrides `updated` only) → 0 fires
    //     on a `create` path. This is the load-bearing negative case
    //     that proves the macro filters by name match instead of
    //     blindly registering all 16 default-no-op methods.
    let _ = T2User::create(suprnova::attrs! { email: "alice@example.com" })
        .await
        .unwrap();

    assert_eq!(
        CREATED_OBSERVER_FIRES.load(Ordering::SeqCst),
        1,
        "CountingObserver::created should fire exactly once per create; \
         a count of 2 would mean the AtomicBool idempotency gate did \
         not hold across the double `bootstrap_observers` call"
    );
    assert_eq!(
        UPDATED_OBSERVER_FIRES.load(Ordering::SeqCst),
        0,
        "OnlyUpdatesObserver::updated must NOT fire on create — the \
         macro should only register adapters for methods the user \
         actually overrode"
    );
}

// =========================================================================
// T2c — `#[model(observers = [...])]` compile-time validation
// =========================================================================
//
// The model-side attribute serves two purposes:
//   1. Compile-time validation that each listed observer type exists
//      (catches typos at the model declaration site).
//   2. Documentation marker: readers of the model declaration can see
//      which observers attach to it.
//
// The actual listener registration is via T2b's inventory pathway — the
// `#[observer(T2Article)]` attribute on `ModelAttrObserver` is what
// registers the listener at bootstrap. The `observers = [...]` attribute
// is independent of the listener install.

static MODEL_ATTR_OBS_FIRES: AtomicUsize = AtomicUsize::new(0);

pub struct ModelAttrObserver;

#[suprnova::observer(T2Article)]
#[async_trait]
impl Observer<T2Article> for ModelAttrObserver {
    async fn created(&self, _a: &T2Article) -> Result<(), FrameworkError> {
        MODEL_ATTR_OBS_FIRES.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[suprnova::model(table = "t2_articles", observers = [ModelAttrObserver])]
pub struct T2Article {
    pub id: i64,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[tokio::test]
async fn model_attribute_observers_auto_register() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t2_articles (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            title TEXT NOT NULL,\
            created_at TEXT NOT NULL,\
            updated_at TEXT NOT NULL\
        )",
    )
    .await
    .unwrap();

    suprnova::eloquent::observers::bootstrap_observers()
        .await
        .unwrap();
    MODEL_ATTR_OBS_FIRES.store(0, Ordering::SeqCst);

    let _ = T2Article::create(suprnova::attrs! { title: "hello" })
        .await
        .unwrap();
    assert_eq!(MODEL_ATTR_OBS_FIRES.load(Ordering::SeqCst), 1);
}

// =========================================================================
// T2c — `Model::observe()` manual registration shim
// =========================================================================
//
// Every `#[suprnova::model]` struct gets a per-model `observe<O>(...)`
// shim that registers all 16 lifecycle listener adapters at runtime.
// This is the analogue of Laravel's `User::observe(MyObserver::class)`
// for users who can't or don't want the `#[observer(M)]` inventory
// pathway (e.g. dynamic registration based on config).
//
// The shim is independent of the `observers = [...]` attribute — every
// model has it whether or not it declared any observers. Idempotency
// is the caller's concern: calling `User::observe(MyObserver)` twice
// registers two adapter sets, matching Laravel's manual semantics.
//
// We use a dedicated `T2ManualUser` model so the manual `observe()`
// call doesn't interfere with T2b's `T2User`-scoped assertions. Process-
// global listener registries make cross-test bleed real; per-scenario
// model types are the established isolation pattern (see T2User /
// T2Comment / T2Subscription elsewhere in this file).

static MANUAL_OBS_FIRES: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone)]
pub struct ManualRegisterObs;

#[async_trait]
impl Observer<T2ManualUser> for ManualRegisterObs {
    async fn created(&self, _u: &T2ManualUser) -> Result<(), FrameworkError> {
        MANUAL_OBS_FIRES.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[suprnova::model(table = "t2_manual_users")]
pub struct T2ManualUser {
    pub id: i64,
    pub email: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[tokio::test]
async fn manual_observe_registration_works_without_attribute_macro() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t2_manual_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            email TEXT NOT NULL,\
            created_at TEXT NOT NULL,\
            updated_at TEXT NOT NULL\
        )",
    )
    .await
    .unwrap();

    // Manual registration — no `#[suprnova::observer]` on
    // `ManualRegisterObs`, so this is the ONLY path by which it can
    // fire. If the shim didn't exist, this line would fail to compile.
    T2ManualUser::observe(ManualRegisterObs).await;

    // Reset AFTER registration so any earlier `T2ManualUser` work
    // (none expected, but defensive) doesn't pre-poison the counter.
    MANUAL_OBS_FIRES.store(0, Ordering::SeqCst);

    let _ = T2ManualUser::create(suprnova::attrs! { email: "manual@example.com" })
        .await
        .unwrap();

    assert_eq!(
        MANUAL_OBS_FIRES.load(Ordering::SeqCst),
        1,
        "manual `T2ManualUser::observe(ManualRegisterObs)` should install a \
         created-listener that fires exactly once per row created"
    );
}
