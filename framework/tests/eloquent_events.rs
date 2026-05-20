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

// ---- Step 4: Retrieving + Retrieved from Builder -------------------------

static RETRIEVED_COUNT: AtomicUsize = AtomicUsize::new(0);
static RETRIEVING_COUNT: AtomicUsize = AtomicUsize::new(0);

#[suprnova::model(table = "t1_retrieved_users")]
pub struct T1RetrievedUser {
    pub id: i64,
    pub email: String,
}

pub struct CountRetrievedT1;
pub struct CountRetrievingT1;

#[async_trait]
impl Listener<t1_retrieved_user::events::Retrieved> for CountRetrievedT1 {
    async fn handle(
        &self,
        _event: &t1_retrieved_user::events::Retrieved,
    ) -> Result<(), FrameworkError> {
        RETRIEVED_COUNT.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait]
impl Listener<t1_retrieved_user::events::Retrieving> for CountRetrievingT1 {
    async fn handle(
        &self,
        _event: &t1_retrieved_user::events::Retrieving,
    ) -> Result<(), FrameworkError> {
        RETRIEVING_COUNT.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn retrieving_fires_once_and_retrieved_fires_once_per_row_from_get() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_retrieved_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    EventFacade::listen::<t1_retrieved_user::events::Retrieved, _>(std::sync::Arc::new(
        CountRetrievedT1,
    ))
    .await;
    EventFacade::listen::<t1_retrieved_user::events::Retrieving, _>(std::sync::Arc::new(
        CountRetrievingT1,
    ))
    .await;

    // Three rows. Listener counters bumped by create() are reset
    // before the query runs.
    let _ = T1RetrievedUser::create(attrs! { email: "a@a.com" })
        .await
        .unwrap();
    let _ = T1RetrievedUser::create(attrs! { email: "b@b.com" })
        .await
        .unwrap();
    let _ = T1RetrievedUser::create(attrs! { email: "c@c.com" })
        .await
        .unwrap();

    RETRIEVED_COUNT.store(0, Ordering::SeqCst);
    RETRIEVING_COUNT.store(0, Ordering::SeqCst);

    let rows = T1RetrievedUser::query().get().await.unwrap();
    assert_eq!(rows.len(), 3);

    // Retrieving fires once per query, Retrieved fires once per row.
    assert_eq!(
        RETRIEVING_COUNT.load(Ordering::SeqCst),
        1,
        "Retrieving must fire exactly once per query (not per row)"
    );
    assert_eq!(
        RETRIEVED_COUNT.load(Ordering::SeqCst),
        3,
        "Retrieved must fire exactly once per row"
    );
}

#[suprnova::model(table = "t1_first_users")]
pub struct T1FirstUser {
    pub id: i64,
    pub email: String,
}

static FIRST_RETRIEVED: AtomicUsize = AtomicUsize::new(0);

pub struct CountFirstT1;

#[async_trait]
impl Listener<t1_first_user::events::Retrieved> for CountFirstT1 {
    async fn handle(
        &self,
        _event: &t1_first_user::events::Retrieved,
    ) -> Result<(), FrameworkError> {
        FIRST_RETRIEVED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn first_dispatches_retrieved_only_when_a_row_was_hydrated() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_first_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    EventFacade::listen::<t1_first_user::events::Retrieved, _>(std::sync::Arc::new(CountFirstT1))
        .await;

    // Empty table → first() returns None → Retrieved must NOT fire.
    FIRST_RETRIEVED.store(0, Ordering::SeqCst);
    let none = T1FirstUser::query().first().await.unwrap();
    assert!(none.is_none());
    assert_eq!(
        FIRST_RETRIEVED.load(Ordering::SeqCst),
        0,
        "Retrieved must not fire when first() returns None"
    );

    // Insert + query → Retrieved fires exactly once.
    let _ = T1FirstUser::create(attrs! { email: "z@z.com" })
        .await
        .unwrap();
    FIRST_RETRIEVED.store(0, Ordering::SeqCst);
    let one = T1FirstUser::query().first().await.unwrap();
    assert!(one.is_some());
    assert_eq!(FIRST_RETRIEVED.load(Ordering::SeqCst), 1);
}

// ---- Step 5: soft-delete Trashed + Restoring/Restored --------------------

#[suprnova::model(table = "t1_trashed_articles", soft_deletes)]
pub struct T1TrashedArticle {
    pub id: i64,
    pub title: String,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

static TRASHED_FIRED: AtomicUsize = AtomicUsize::new(0);
static DELETED_SOFT_FIRED: AtomicUsize = AtomicUsize::new(0);
static DELETED_SOFT_IS_FORCE_FALSE: AtomicUsize = AtomicUsize::new(0);

pub struct CountTrashedT1;
pub struct CountDeletedSoftT1;

#[async_trait]
impl Listener<t1_trashed_article::events::Trashed> for CountTrashedT1 {
    async fn handle(
        &self,
        _event: &t1_trashed_article::events::Trashed,
    ) -> Result<(), FrameworkError> {
        TRASHED_FIRED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait]
impl Listener<t1_trashed_article::events::Deleted> for CountDeletedSoftT1 {
    async fn handle(
        &self,
        event: &t1_trashed_article::events::Deleted,
    ) -> Result<(), FrameworkError> {
        DELETED_SOFT_FIRED.fetch_add(1, Ordering::SeqCst);
        if !event.is_force {
            DELETED_SOFT_IS_FORCE_FALSE.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

#[tokio::test]
async fn soft_delete_fires_trashed_and_deleted_with_is_force_false() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_trashed_articles (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, deleted_at TEXT)",
    )
    .await
    .unwrap();

    EventFacade::listen::<t1_trashed_article::events::Trashed, _>(std::sync::Arc::new(
        CountTrashedT1,
    ))
    .await;
    EventFacade::listen::<t1_trashed_article::events::Deleted, _>(std::sync::Arc::new(
        CountDeletedSoftT1,
    ))
    .await;

    let a = T1TrashedArticle::create(attrs! { title: "hello" })
        .await
        .unwrap();

    TRASHED_FIRED.store(0, Ordering::SeqCst);
    DELETED_SOFT_FIRED.store(0, Ordering::SeqCst);
    DELETED_SOFT_IS_FORCE_FALSE.store(0, Ordering::SeqCst);

    a.delete().await.unwrap();

    assert_eq!(
        TRASHED_FIRED.load(Ordering::SeqCst),
        1,
        "soft delete must fire Trashed exactly once"
    );
    assert_eq!(
        DELETED_SOFT_FIRED.load(Ordering::SeqCst),
        1,
        "soft delete must also fire Deleted exactly once"
    );
    assert_eq!(
        DELETED_SOFT_IS_FORCE_FALSE.load(Ordering::SeqCst),
        1,
        "soft delete must dispatch Deleted with is_force=false"
    );
}

#[suprnova::model(table = "t1_restore_articles", soft_deletes)]
pub struct T1RestoreArticle {
    pub id: i64,
    pub title: String,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

static RESTORING_FIRED: AtomicUsize = AtomicUsize::new(0);
static RESTORED_FIRED: AtomicUsize = AtomicUsize::new(0);

pub struct CountRestoringT1;
pub struct CountRestoredT1;

#[async_trait]
impl CancellableListener<t1_restore_article::events::Restoring> for CountRestoringT1 {
    async fn handle(&self, _event: &t1_restore_article::events::Restoring) -> EventResult {
        RESTORING_FIRED.fetch_add(1, Ordering::SeqCst);
        EventResult::Ok
    }
}

#[async_trait]
impl Listener<t1_restore_article::events::Restored> for CountRestoredT1 {
    async fn handle(
        &self,
        _event: &t1_restore_article::events::Restored,
    ) -> Result<(), FrameworkError> {
        RESTORED_FIRED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn restore_fires_restoring_then_restored() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_restore_articles (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, deleted_at TEXT)",
    )
    .await
    .unwrap();

    listen_cancellable::<t1_restore_article::events::Restoring, _>(std::sync::Arc::new(
        CountRestoringT1,
    ))
    .await;
    EventFacade::listen::<t1_restore_article::events::Restored, _>(std::sync::Arc::new(
        CountRestoredT1,
    ))
    .await;

    let a = T1RestoreArticle::create(attrs! { title: "hi" })
        .await
        .unwrap();
    a.delete().await.unwrap();

    let trashed = T1RestoreArticle::with_trashed()
        .first()
        .await
        .unwrap()
        .unwrap();

    RESTORING_FIRED.store(0, Ordering::SeqCst);
    RESTORED_FIRED.store(0, Ordering::SeqCst);

    trashed.restore().await.unwrap();
    assert_eq!(
        RESTORING_FIRED.load(Ordering::SeqCst),
        1,
        "restore() must fire Restoring before the UPDATE"
    );
    assert_eq!(
        RESTORED_FIRED.load(Ordering::SeqCst),
        1,
        "restore() must fire Restored after the UPDATE"
    );
}

#[suprnova::model(table = "t1_restore_veto_articles", soft_deletes)]
pub struct T1RestoreVetoArticle {
    pub id: i64,
    pub title: String,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct VetoRestoreT1;

#[async_trait]
impl CancellableListener<t1_restore_veto_article::events::Restoring> for VetoRestoreT1 {
    async fn handle(&self, _event: &t1_restore_veto_article::events::Restoring) -> EventResult {
        EventResult::cancel("restore vetoed")
    }
}

#[tokio::test]
async fn restoring_cancel_aborts_restore_row_stays_trashed() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_restore_veto_articles (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, deleted_at TEXT)",
    )
    .await
    .unwrap();

    listen_cancellable::<t1_restore_veto_article::events::Restoring, _>(std::sync::Arc::new(
        VetoRestoreT1,
    ))
    .await;

    let a = T1RestoreVetoArticle::create(attrs! { title: "stay-trashed" })
        .await
        .unwrap();
    a.delete().await.unwrap();
    let trashed = T1RestoreVetoArticle::with_trashed()
        .first()
        .await
        .unwrap()
        .unwrap();
    let id = trashed.id;

    let err = trashed.restore().await.unwrap_err();
    assert!(format!("{err}").contains("restore vetoed"));

    // Row should still be trashed.
    let still_trashed = T1RestoreVetoArticle::with_trashed()
        .filter("id", id)
        .first()
        .await
        .unwrap()
        .unwrap();
    assert!(
        still_trashed.deleted_at.is_some(),
        "cancelled restore must leave deleted_at intact"
    );
}

#[suprnova::model(table = "t1_force_articles", soft_deletes)]
pub struct T1ForceArticle {
    pub id: i64,
    pub title: String,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

static FORCE_DELETING_FIRED: AtomicUsize = AtomicUsize::new(0);
static FORCE_DELETED_FIRED: AtomicUsize = AtomicUsize::new(0);
static FORCE_TRASHED_FIRED: AtomicUsize = AtomicUsize::new(0);
static FORCE_DELETED_IS_FORCE_TRUE: AtomicUsize = AtomicUsize::new(0);

pub struct CountForceDeletingT1;
pub struct CountForceDeletedT1;
pub struct CountForceTrashedT1;

#[async_trait]
impl Listener<t1_force_article::events::ForceDeleting> for CountForceDeletingT1 {
    async fn handle(
        &self,
        _event: &t1_force_article::events::ForceDeleting,
    ) -> Result<(), FrameworkError> {
        FORCE_DELETING_FIRED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait]
impl Listener<t1_force_article::events::ForceDeleted> for CountForceDeletedT1 {
    async fn handle(
        &self,
        _event: &t1_force_article::events::ForceDeleted,
    ) -> Result<(), FrameworkError> {
        FORCE_DELETED_FIRED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait]
impl Listener<t1_force_article::events::Trashed> for CountForceTrashedT1 {
    async fn handle(
        &self,
        _event: &t1_force_article::events::Trashed,
    ) -> Result<(), FrameworkError> {
        FORCE_TRASHED_FIRED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

pub struct WatchDeletedIsForceT1;

#[async_trait]
impl Listener<t1_force_article::events::Deleted> for WatchDeletedIsForceT1 {
    async fn handle(
        &self,
        event: &t1_force_article::events::Deleted,
    ) -> Result<(), FrameworkError> {
        if event.is_force {
            FORCE_DELETED_IS_FORCE_TRUE.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

// ---- Extra coverage: update events, mutation-through-Arc<Mutex>, ordering -

#[suprnova::model(table = "t1_update_users")]
pub struct T1UpdateUser {
    pub id: i64,
    pub email: String,
}

static UPDATING_FIRED: AtomicUsize = AtomicUsize::new(0);
static UPDATED_PREV_EMAIL: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
static UPDATED_CUR_EMAIL: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();

pub struct CountUpdatingT1;
pub struct RecordUpdatedT1;

#[async_trait]
impl CancellableListener<t1_update_user::events::Updating> for CountUpdatingT1 {
    async fn handle(&self, _event: &t1_update_user::events::Updating) -> EventResult {
        UPDATING_FIRED.fetch_add(1, Ordering::SeqCst);
        EventResult::Ok
    }
}

#[async_trait]
impl Listener<t1_update_user::events::Updated> for RecordUpdatedT1 {
    async fn handle(
        &self,
        event: &t1_update_user::events::Updated,
    ) -> Result<(), FrameworkError> {
        // OnceCell stores the FIRST update only — the static is reset
        // implicitly via the test ordering (one update per test).
        let _ = UPDATED_PREV_EMAIL.set(event.previous.email.clone());
        let _ = UPDATED_CUR_EMAIL.set(event.current.email.clone());
        Ok(())
    }
}

#[tokio::test]
async fn update_fires_updating_and_updated_with_previous_and_current() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_update_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    listen_cancellable::<t1_update_user::events::Updating, _>(std::sync::Arc::new(CountUpdatingT1))
        .await;
    EventFacade::listen::<t1_update_user::events::Updated, _>(std::sync::Arc::new(RecordUpdatedT1))
        .await;

    let u = T1UpdateUser::create(attrs! { email: "before@x.com" })
        .await
        .unwrap();
    UPDATING_FIRED.store(0, Ordering::SeqCst);

    let _ = u.update(attrs! { email: "after@x.com" }).await.unwrap();

    assert_eq!(UPDATING_FIRED.load(Ordering::SeqCst), 1);
    assert_eq!(
        UPDATED_PREV_EMAIL.get().map(|s| s.as_str()),
        Some("before@x.com"),
        "Updated.previous carries the row state from BEFORE the update"
    );
    assert_eq!(
        UPDATED_CUR_EMAIL.get().map(|s| s.as_str()),
        Some("after@x.com"),
        "Updated.current carries the row state AFTER the update lands"
    );
}

#[suprnova::model(table = "t1_mutate_users")]
pub struct T1MutateUser {
    pub id: i64,
    pub email: String,
}

pub struct MutateOnCreateT1;

#[async_trait]
impl CancellableListener<t1_mutate_user::events::Creating> for MutateOnCreateT1 {
    async fn handle(&self, event: &t1_mutate_user::events::Creating) -> EventResult {
        let mut attrs = event.attrs.lock().await;
        // Mutate the in-flight attrs — the resulting INSERT should
        // pick up the new value.
        attrs.insert("email", "mutated@x.com");
        EventResult::Ok
    }
}

#[tokio::test]
async fn creating_listener_can_mutate_attrs_before_insert_lands() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_mutate_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    listen_cancellable::<t1_mutate_user::events::Creating, _>(std::sync::Arc::new(
        MutateOnCreateT1,
    ))
    .await;

    let u = T1MutateUser::create(attrs! { email: "original@x.com" })
        .await
        .unwrap();
    assert_eq!(
        u.email, "mutated@x.com",
        "listener mutation must propagate to the persisted row"
    );

    // Verify it landed in the database, not just the returned struct.
    let from_db = T1MutateUser::find(u.id).await.unwrap().unwrap();
    assert_eq!(from_db.email, "mutated@x.com");
}

#[suprnova::model(table = "t1_order_users")]
pub struct T1OrderUser {
    pub id: i64,
    pub email: String,
}

static ORDER_LOG: tokio::sync::OnceCell<std::sync::Arc<tokio::sync::Mutex<Vec<u8>>>> =
    tokio::sync::OnceCell::const_new();

async fn order_log() -> std::sync::Arc<tokio::sync::Mutex<Vec<u8>>> {
    ORDER_LOG
        .get_or_init(|| async {
            std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()))
        })
        .await
        .clone()
}

pub struct OrderListenerA;
pub struct OrderListenerB;

#[async_trait]
impl Listener<t1_order_user::events::Created> for OrderListenerA {
    async fn handle(
        &self,
        _event: &t1_order_user::events::Created,
    ) -> Result<(), FrameworkError> {
        let log = order_log().await;
        log.lock().await.push(b'A');
        Ok(())
    }
}

#[async_trait]
impl Listener<t1_order_user::events::Created> for OrderListenerB {
    async fn handle(
        &self,
        _event: &t1_order_user::events::Created,
    ) -> Result<(), FrameworkError> {
        let log = order_log().await;
        log.lock().await.push(b'B');
        Ok(())
    }
}

#[tokio::test]
async fn listeners_fire_in_registration_order() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_order_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    EventFacade::listen::<t1_order_user::events::Created, _>(std::sync::Arc::new(OrderListenerA))
        .await;
    EventFacade::listen::<t1_order_user::events::Created, _>(std::sync::Arc::new(OrderListenerB))
        .await;

    let log = order_log().await;
    log.lock().await.clear();

    let _ = T1OrderUser::create(attrs! { email: "o@o.com" })
        .await
        .unwrap();

    let recorded = log.lock().await.clone();
    assert_eq!(
        recorded,
        b"AB",
        "listeners must fire in the order they were registered"
    );
}

#[suprnova::model(table = "t1_first_cancel_users")]
pub struct T1FirstCancelUser {
    pub id: i64,
    pub email: String,
}

static SECOND_LISTENER_CALLED: AtomicUsize = AtomicUsize::new(0);

pub struct FirstCancels;
pub struct SecondAfterCancel;

#[async_trait]
impl CancellableListener<t1_first_cancel_user::events::Creating> for FirstCancels {
    async fn handle(&self, _event: &t1_first_cancel_user::events::Creating) -> EventResult {
        EventResult::cancel("first vetoed")
    }
}

#[async_trait]
impl CancellableListener<t1_first_cancel_user::events::Creating> for SecondAfterCancel {
    async fn handle(&self, _event: &t1_first_cancel_user::events::Creating) -> EventResult {
        SECOND_LISTENER_CALLED.fetch_add(1, Ordering::SeqCst);
        EventResult::Ok
    }
}

#[tokio::test]
async fn first_cancel_wins_later_cancellable_listeners_are_not_called() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_first_cancel_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    listen_cancellable::<t1_first_cancel_user::events::Creating, _>(std::sync::Arc::new(
        FirstCancels,
    ))
    .await;
    listen_cancellable::<t1_first_cancel_user::events::Creating, _>(std::sync::Arc::new(
        SecondAfterCancel,
    ))
    .await;

    SECOND_LISTENER_CALLED.store(0, Ordering::SeqCst);

    let err = T1FirstCancelUser::create(attrs! { email: "x@x.com" })
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("first vetoed"));
    assert_eq!(
        SECOND_LISTENER_CALLED.load(Ordering::SeqCst),
        0,
        "second cancellable listener must not be called when the first cancelled"
    );
}

#[suprnova::model(table = "t1_no_listeners_users")]
pub struct T1NoListenersUser {
    pub id: i64,
    pub email: String,
}

#[suprnova::model(table = "t1_saving_cancel_users")]
pub struct T1SavingCancelUser {
    pub id: i64,
    pub email: String,
}

pub struct CancelSavingT1;

#[async_trait]
impl CancellableListener<t1_saving_cancel_user::events::Saving> for CancelSavingT1 {
    async fn handle(&self, _event: &t1_saving_cancel_user::events::Saving) -> EventResult {
        EventResult::cancel("saving vetoed")
    }
}

#[tokio::test]
async fn saving_cancel_aborts_both_create_and_update() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_saving_cancel_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    listen_cancellable::<t1_saving_cancel_user::events::Saving, _>(std::sync::Arc::new(
        CancelSavingT1,
    ))
    .await;

    // Saving cancels create (fires after Creating-which-passes).
    let err = T1SavingCancelUser::create(attrs! { email: "x@x.com" })
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("saving vetoed"));

    let rows = T1SavingCancelUser::all().await.unwrap();
    assert!(
        rows.is_empty(),
        "Saving cancel must abort the INSERT — no row persisted"
    );
}

#[suprnova::model(table = "t1_updating_cancel_users")]
pub struct T1UpdatingCancelUser {
    pub id: i64,
    pub email: String,
}

pub struct CancelUpdatingT1;

#[async_trait]
impl CancellableListener<t1_updating_cancel_user::events::Updating> for CancelUpdatingT1 {
    async fn handle(&self, _event: &t1_updating_cancel_user::events::Updating) -> EventResult {
        EventResult::cancel("updating vetoed")
    }
}

#[tokio::test]
async fn updating_cancel_aborts_update_row_unchanged() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_updating_cancel_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    listen_cancellable::<t1_updating_cancel_user::events::Updating, _>(std::sync::Arc::new(
        CancelUpdatingT1,
    ))
    .await;

    let u = T1UpdatingCancelUser::create(attrs! { email: "before@x.com" })
        .await
        .unwrap();
    let id = u.id;

    let err = u
        .update(attrs! { email: "after@x.com" })
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("updating vetoed"));

    let unchanged = T1UpdatingCancelUser::find(id).await.unwrap().unwrap();
    assert_eq!(
        unchanged.email, "before@x.com",
        "Updating cancel must leave the row at its pre-update value"
    );
}

#[tokio::test]
async fn no_listener_fast_path_succeeds_silently() {
    // Models without any registered listeners must complete their
    // CRUD operations cleanly — dispatch_cancellable's empty-list
    // fast path returns Ok immediately.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_no_listeners_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let u = T1NoListenersUser::create(attrs! { email: "quiet@x.com" })
        .await
        .unwrap();
    assert_eq!(u.email, "quiet@x.com");

    let u = u.update(attrs! { email: "still-quiet@x.com" }).await.unwrap();
    assert_eq!(u.email, "still-quiet@x.com");

    u.delete().await.unwrap();
    assert!(T1NoListenersUser::all().await.unwrap().is_empty());
}

#[tokio::test]
async fn force_delete_fires_force_deleted_and_deleted_with_is_force_true_but_not_trashed() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t1_force_articles (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, deleted_at TEXT)",
    )
    .await
    .unwrap();

    EventFacade::listen::<t1_force_article::events::ForceDeleting, _>(std::sync::Arc::new(
        CountForceDeletingT1,
    ))
    .await;
    EventFacade::listen::<t1_force_article::events::ForceDeleted, _>(std::sync::Arc::new(
        CountForceDeletedT1,
    ))
    .await;
    EventFacade::listen::<t1_force_article::events::Trashed, _>(std::sync::Arc::new(
        CountForceTrashedT1,
    ))
    .await;
    EventFacade::listen::<t1_force_article::events::Deleted, _>(std::sync::Arc::new(
        WatchDeletedIsForceT1,
    ))
    .await;

    let a = T1ForceArticle::create(attrs! { title: "go-away" })
        .await
        .unwrap();

    FORCE_DELETING_FIRED.store(0, Ordering::SeqCst);
    FORCE_DELETED_FIRED.store(0, Ordering::SeqCst);
    FORCE_TRASHED_FIRED.store(0, Ordering::SeqCst);
    FORCE_DELETED_IS_FORCE_TRUE.store(0, Ordering::SeqCst);

    a.force_delete().await.unwrap();

    assert_eq!(
        FORCE_DELETING_FIRED.load(Ordering::SeqCst),
        1,
        "force_delete must fire ForceDeleting"
    );
    assert_eq!(
        FORCE_DELETED_FIRED.load(Ordering::SeqCst),
        1,
        "force_delete must fire ForceDeleted"
    );
    assert_eq!(
        FORCE_DELETED_IS_FORCE_TRUE.load(Ordering::SeqCst),
        1,
        "force_delete must also fire Deleted with is_force=true"
    );
    assert_eq!(
        FORCE_TRASHED_FIRED.load(Ordering::SeqCst),
        0,
        "force_delete must NOT fire Trashed (the row is gone, not tombstoned)"
    );
}
