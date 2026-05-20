//! Phase 10C T1 — Model lifecycle events.
//!
//! Covers the cross-model shared types (`EventResult`,
//! `CancellableListener`) plus the macro-emitted per-model
//! `events::*` submodule with 16 lifecycle event structs.
//!
//! ## Test isolation
//!
//! The dispatcher is process-global (`EventFacade` + the
//! `CANCELLABLE_REGISTRY` static). Listeners registered in one test
//! survive into the next, so we shard each scenario across a
//! distinct model type (`T1CreatedUser`, `T1CancelUser`, ...) — same
//! convention every other 10A/10B test follows for inventory
//! collisions. The `Created` listener for `T1CreatedUser` only sees
//! `T1CreatedUser` events; `T1CancelUser::create` is unaffected.

use suprnova::eloquent::events::EventResult;
use suprnova::Event;

#[test]
fn event_result_ok_is_not_cancelled() {
    let r = EventResult::ok();
    assert!(!r.is_cancelled());
}

#[test]
fn event_result_cancel_carries_reason() {
    let r = EventResult::cancel("policy denied");
    assert!(r.is_cancelled());
    match r {
        EventResult::Cancel(reason) => assert_eq!(reason, "policy denied"),
        _ => panic!("expected Cancel"),
    }
}

// ---- Model used only for asserting event-struct emission shape -----------
//
// We declare a thin model whose only job is to provide
// `t1_user::events::*` for the event-name assertions below. No
// migrations / runtime persistence needed for this test — the macro's
// emission is what we're verifying.

#[suprnova::model(table = "t1_users")]
pub struct T1User {
    pub id: i64,
    pub email: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[test]
fn macro_emits_sixteen_event_structs() {
    // Each event struct names itself with the model's module path so
    // listeners and log lines disambiguate between per-model events
    // sharing the same Laravel name (`User::Created` vs
    // `Post::Created`).
    assert_eq!(
        t1_user::events::Retrieving::event_name(),
        "t1_user::events::Retrieving"
    );
    assert_eq!(
        t1_user::events::Retrieved::event_name(),
        "t1_user::events::Retrieved"
    );
    assert_eq!(
        t1_user::events::Saving::event_name(),
        "t1_user::events::Saving"
    );
    assert_eq!(
        t1_user::events::Creating::event_name(),
        "t1_user::events::Creating"
    );
    assert_eq!(
        t1_user::events::Created::event_name(),
        "t1_user::events::Created"
    );
    assert_eq!(
        t1_user::events::Updating::event_name(),
        "t1_user::events::Updating"
    );
    assert_eq!(
        t1_user::events::Updated::event_name(),
        "t1_user::events::Updated"
    );
    assert_eq!(
        t1_user::events::Saved::event_name(),
        "t1_user::events::Saved"
    );
    assert_eq!(
        t1_user::events::Deleting::event_name(),
        "t1_user::events::Deleting"
    );
    assert_eq!(
        t1_user::events::Deleted::event_name(),
        "t1_user::events::Deleted"
    );
    assert_eq!(
        t1_user::events::Trashed::event_name(),
        "t1_user::events::Trashed"
    );
    assert_eq!(
        t1_user::events::Restoring::event_name(),
        "t1_user::events::Restoring"
    );
    assert_eq!(
        t1_user::events::Restored::event_name(),
        "t1_user::events::Restored"
    );
    assert_eq!(
        t1_user::events::Replicating::event_name(),
        "t1_user::events::Replicating"
    );
    assert_eq!(
        t1_user::events::ForceDeleting::event_name(),
        "t1_user::events::ForceDeleting"
    );
    assert_eq!(
        t1_user::events::ForceDeleted::event_name(),
        "t1_user::events::ForceDeleted"
    );
}

// ---- Step 3: CRUD-dispatched events --------------------------------------
//
// Each test owns its own model so listener state on the process-global
// dispatcher never leaks between tests in the same binary. See the
// header note on test isolation.

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use suprnova::eloquent::events::{listen_cancellable, CancellableListener};
use suprnova::events::{EventFacade, Listener};
use suprnova::testing::TestDatabase;
use suprnova::{attrs, FrameworkError, Model};

// `Created` listener counter for the create-dispatch test.
static CREATED_FIRED: AtomicUsize = AtomicUsize::new(0);
static SAVED_FIRED: AtomicUsize = AtomicUsize::new(0);

#[suprnova::model(table = "t1_created_users")]
pub struct T1CreatedUser {
    pub id: i64,
    pub email: String,
}

pub struct CountCreatedT1;

#[async_trait]
impl Listener<t1_created_user::events::Created> for CountCreatedT1 {
    async fn handle(&self, _event: &t1_created_user::events::Created) -> Result<(), FrameworkError> {
        CREATED_FIRED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

pub struct CountSavedT1;

#[async_trait]
impl Listener<t1_created_user::events::Saved> for CountSavedT1 {
    async fn handle(&self, _event: &t1_created_user::events::Saved) -> Result<(), FrameworkError> {
        SAVED_FIRED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn created_and_saved_events_fire_on_model_create() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_created_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    EventFacade::listen::<t1_created_user::events::Created, _>(std::sync::Arc::new(CountCreatedT1))
        .await;
    EventFacade::listen::<t1_created_user::events::Saved, _>(std::sync::Arc::new(CountSavedT1))
        .await;

    CREATED_FIRED.store(0, Ordering::SeqCst);
    SAVED_FIRED.store(0, Ordering::SeqCst);

    let _ = T1CreatedUser::create(attrs! { email: "alice@example.com" })
        .await
        .unwrap();

    assert_eq!(CREATED_FIRED.load(Ordering::SeqCst), 1);
    assert_eq!(SAVED_FIRED.load(Ordering::SeqCst), 1);
}

#[suprnova::model(table = "t1_cancel_users")]
pub struct T1CancelUser {
    pub id: i64,
    pub email: String,
}

pub struct CancelCreateT1;

#[async_trait]
impl CancellableListener<t1_cancel_user::events::Creating> for CancelCreateT1 {
    async fn handle(&self, _event: &t1_cancel_user::events::Creating) -> EventResult {
        EventResult::cancel("policy denied")
    }
}

#[tokio::test]
async fn creating_event_cancel_aborts_create_and_does_not_persist() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_cancel_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    listen_cancellable::<t1_cancel_user::events::Creating, _>(std::sync::Arc::new(CancelCreateT1))
        .await;

    let result = T1CancelUser::create(attrs! { email: "bob@example.com" }).await;
    assert!(result.is_err(), "cancelled listener must surface as Err");
    let err = result.err().unwrap();
    let msg = format!("{err}");
    assert!(
        msg.contains("policy denied"),
        "expected bad_request with cancel reason, got: {msg}"
    );
    assert_eq!(
        err.status_code(),
        400,
        "Cancel must surface as HTTP 400, got {}",
        err.status_code()
    );

    // Row should NOT have been inserted.
    let rows = T1CancelUser::all().await.unwrap();
    assert!(rows.is_empty(), "cancelled create must not persist");
}

#[suprnova::model(table = "t1_saving_flag_users")]
pub struct T1SavingFlagUser {
    pub id: i64,
    pub email: String,
}

#[derive(Default)]
pub struct ObserveSavingT1 {
    is_creating_at_create: std::sync::Arc<tokio::sync::Mutex<Vec<bool>>>,
}

#[async_trait]
impl CancellableListener<t1_saving_flag_user::events::Saving> for ObserveSavingT1 {
    async fn handle(&self, event: &t1_saving_flag_user::events::Saving) -> EventResult {
        let mut log = self.is_creating_at_create.lock().await;
        log.push(event.is_creating);
        EventResult::Ok
    }
}

#[tokio::test]
async fn saving_event_is_creating_flag_disambiguates_insert_from_update() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_saving_flag_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let listener = std::sync::Arc::new(ObserveSavingT1::default());
    let log = listener.is_creating_at_create.clone();
    listen_cancellable::<t1_saving_flag_user::events::Saving, _>(listener).await;

    let user = T1SavingFlagUser::create(attrs! { email: "c@c.com" })
        .await
        .unwrap();
    let _ = user.update(attrs! { email: "c2@c.com" }).await.unwrap();

    let recorded = log.lock().await.clone();
    assert_eq!(
        recorded,
        vec![true, false],
        "Saving must carry is_creating=true on create then is_creating=false on update"
    );
}

#[suprnova::model(table = "t1_deleting_users")]
pub struct T1DeletingUser {
    pub id: i64,
    pub email: String,
}

static DELETING_FIRED: AtomicUsize = AtomicUsize::new(0);
static DELETED_FIRED: AtomicUsize = AtomicUsize::new(0);

pub struct CountDeletingT1;
pub struct CountDeletedT1;

#[async_trait]
impl Listener<t1_deleting_user::events::Deleted> for CountDeletedT1 {
    async fn handle(&self, event: &t1_deleting_user::events::Deleted) -> Result<(), FrameworkError> {
        assert!(!event.is_force, "non-force delete must report is_force=false");
        DELETED_FIRED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait]
impl CancellableListener<t1_deleting_user::events::Deleting> for CountDeletingT1 {
    async fn handle(&self, event: &t1_deleting_user::events::Deleting) -> EventResult {
        assert!(!event.is_force, "non-force delete must report is_force=false");
        DELETING_FIRED.fetch_add(1, Ordering::SeqCst);
        EventResult::Ok
    }
}

#[tokio::test]
async fn deleting_and_deleted_events_fire_on_hard_delete() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_deleting_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    listen_cancellable::<t1_deleting_user::events::Deleting, _>(std::sync::Arc::new(
        CountDeletingT1,
    ))
    .await;
    EventFacade::listen::<t1_deleting_user::events::Deleted, _>(std::sync::Arc::new(
        CountDeletedT1,
    ))
    .await;

    let u = T1DeletingUser::create(attrs! { email: "d@d.com" }).await.unwrap();
    DELETING_FIRED.store(0, Ordering::SeqCst);
    DELETED_FIRED.store(0, Ordering::SeqCst);

    u.delete().await.unwrap();
    assert_eq!(DELETING_FIRED.load(Ordering::SeqCst), 1);
    assert_eq!(DELETED_FIRED.load(Ordering::SeqCst), 1);

    // Row gone.
    let rows = T1DeletingUser::all().await.unwrap();
    assert!(rows.is_empty());
}

#[suprnova::model(table = "t1_delete_veto_users")]
pub struct T1DeleteVetoUser {
    pub id: i64,
    pub email: String,
}

pub struct VetoDeleteT1;

#[async_trait]
impl CancellableListener<t1_delete_veto_user::events::Deleting> for VetoDeleteT1 {
    async fn handle(&self, _event: &t1_delete_veto_user::events::Deleting) -> EventResult {
        EventResult::cancel("veto")
    }
}

#[tokio::test]
async fn deleting_cancel_aborts_delete_and_row_stays() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_delete_veto_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    listen_cancellable::<t1_delete_veto_user::events::Deleting, _>(std::sync::Arc::new(
        VetoDeleteT1,
    ))
    .await;

    let u = T1DeleteVetoUser::create(attrs! { email: "veto@v.com" })
        .await
        .unwrap();
    let id = u.id;

    let err = u.delete().await.unwrap_err();
    assert!(format!("{err}").contains("veto"));

    // Row stays — cancelled delete must not have run.
    let still = T1DeleteVetoUser::find(id).await.unwrap();
    assert!(still.is_some(), "cancelled delete must leave the row");
}
