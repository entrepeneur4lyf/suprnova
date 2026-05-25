//! Regression: HIGH audit finding `eloquent` #2 — `save()` and
//! `save_with_tx()` ignored listener mutations to the shared
//! `Updating` / `Saving` attrs payload. The earlier code serialized
//! `self`, handed listeners an `Arc<Mutex<Attrs>>` view of that
//! payload, then built the ActiveModel from `self.clone()` and ran
//! the UPDATE — discarding any mutations the listeners had made.
//!
//! Result in production: observers that normalize, redact, audit-tag,
//! or enforce values on update appeared to succeed but persisted the
//! unmodified `self` instead. Mirrors a bug class Laravel has
//! documented for years on its own Saving hooks; we ship the
//! corrected behaviour from day one.
//!
//! Fix: mirror `update()`'s pattern — after the Updating + Saving
//! listeners run, read the (possibly mutated) `Attrs` back from the
//! shared mutex and apply via `apply_attrs_to_active_model` before
//! the UPDATE fires.
//!
//! These tests prove the listener-mutated value lands in the
//! database for both `save()` (default executor) and `save_with_tx()`
//! (explicit transaction).

use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use suprnova::eloquent::events::{listen_cancellable, CancellableListener, EventResult};
use suprnova::testing::TestDatabase;
use suprnova::{attrs, Model, DB};

#[suprnova::model(table = "t338_save_users")]
pub struct T338SaveUser {
    pub id: i64,
    pub email: String,
}

// Listener flips `email` to a sentinel value every time `Updating`
// fires. Test asserts the database row picks up the sentinel.
pub struct RedactOnUpdating;

#[async_trait]
impl CancellableListener<t338_save_user::events::Updating> for RedactOnUpdating {
    async fn handle(&self, event: &t338_save_user::events::Updating) -> EventResult {
        let mut attrs = event.attrs.lock().await;
        attrs.insert("email", "from-listener@x.com");
        EventResult::Ok
    }
}

// Once-shot guard so the global registry only carries the listener
// for the duration of one test invocation. Atomics on the listener
// itself capture "did this run?" — used in the second test.
static SAVE_LISTENER_FIRED: AtomicBool = AtomicBool::new(false);

pub struct RecordSavingFired;

#[async_trait]
impl CancellableListener<t338_save_user::events::Saving> for RecordSavingFired {
    async fn handle(&self, _event: &t338_save_user::events::Saving) -> EventResult {
        SAVE_LISTENER_FIRED.store(true, Ordering::SeqCst);
        EventResult::Ok
    }
}

async fn setup_table(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE t338_save_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn save_persists_updating_listener_mutation_not_self_clone() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    setup_table(&db).await;

    listen_cancellable::<t338_save_user::events::Updating, _>(std::sync::Arc::new(
        RedactOnUpdating,
    ))
    .await;

    // Create the row so we have something to save() against.
    let mut user = T338SaveUser::create(attrs! { email: "original@x.com" })
        .await
        .unwrap();

    // Mutate the in-memory model — but the listener will REPLACE
    // this with the sentinel. The audit-flagged bug used to persist
    // this in-memory value instead.
    user.email = "from-self@x.com".to_string();
    user.save().await.unwrap();

    // The DB row must reflect the LISTENER's mutation, not the
    // in-memory `self.email` value.
    let reloaded = T338SaveUser::find(user.id).await.unwrap().unwrap();
    assert_eq!(
        reloaded.email, "from-listener@x.com",
        "save() must read the listener-mutated attrs and apply them — \
         not silently use self.clone(). Got `{}` instead of \
         `from-listener@x.com`; if you see `from-self@x.com` the audit \
         regression is back.",
        reloaded.email,
    );
}

#[tokio::test]
async fn save_with_tx_also_persists_listener_mutation() {
    // `save_with_tx` has the same lifecycle but pins the SQL to a
    // user-supplied transaction. The audit's concern applied to both
    // arms; this proves the fix is mirrored.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    setup_table(&_db).await;

    listen_cancellable::<t338_save_user::events::Updating, _>(std::sync::Arc::new(
        RedactOnUpdating,
    ))
    .await;
    listen_cancellable::<t338_save_user::events::Saving, _>(std::sync::Arc::new(
        RecordSavingFired,
    ))
    .await;
    SAVE_LISTENER_FIRED.store(false, Ordering::SeqCst);

    // Bootstrap row outside the tx so we have a target.
    let mut user = T338SaveUser::create(attrs! { email: "boot@x.com" })
        .await
        .unwrap();

    let tx = DB::begin_transaction().await.unwrap();
    user.email = "tx-self@x.com".to_string();
    user.save_with_tx(&tx).await.unwrap();
    tx.commit().await.unwrap();

    assert!(
        SAVE_LISTENER_FIRED.load(Ordering::SeqCst),
        "Saving listener must fire on save_with_tx too"
    );

    let reloaded = T338SaveUser::find(user.id).await.unwrap().unwrap();
    assert_eq!(
        reloaded.email, "from-listener@x.com",
        "save_with_tx() must also apply listener mutations — got `{}`",
        reloaded.email,
    );
}
