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
use suprnova::{model, FrameworkError};

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
