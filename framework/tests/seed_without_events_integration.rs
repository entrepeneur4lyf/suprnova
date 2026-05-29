//! `seed::without_events` model-path integration test.
//!
//! The unit tests in `seeders.rs` prove `dispatch_after` /
//! `dispatch_cancellable` short-circuit when the `EVENTS_MUTED`
//! task-local is set. This file proves the same thing through a
//! `Model::create` call site — the user-facing surface a real
//! seeder would invoke.
//!
//! The model + observer here are local to this test binary so the
//! observer's process-global registration doesn't bleed into other
//! tests' models. Each test resets the per-test atomic before
//! creating, then asserts the observer was/was not invoked.

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use suprnova::FrameworkError;
use suprnova::eloquent::events::EventResult;
use suprnova::eloquent::observers::Observer;
use suprnova::testing::TestDatabase;
use suprnova::{Model as _, attrs, model, seed};

#[model(table = "swe_users", observers = [WithoutEventsObserver])]
pub struct SweUser {
    pub id: i64,
    pub email: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

static CREATED_FIRES: AtomicUsize = AtomicUsize::new(0);
static CREATING_FIRES: AtomicUsize = AtomicUsize::new(0);

pub struct WithoutEventsObserver;

#[suprnova::observer(SweUser)]
#[async_trait]
impl Observer<SweUser> for WithoutEventsObserver {
    async fn creating(&self, _attrs: &mut suprnova::eloquent::attrs::Attrs) -> EventResult {
        CREATING_FIRES.fetch_add(1, Ordering::SeqCst);
        EventResult::Ok
    }

    async fn created(&self, _user: &SweUser) -> Result<(), FrameworkError> {
        CREATED_FIRES.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Sequential tests can race the global observer registry, so combine
/// both assertions into one `#[tokio::test]` — same approach
/// `eloquent_observers.rs` takes for the same reason. The full
/// chokepoint coverage (`dispatch_after` vs `dispatch_cancellable`
/// independently) is already proven by the unit tests in
/// `seeders.rs::without_events::*`; this test exists to prove the
/// integration with the real `Model::create` path.
#[tokio::test]
async fn model_create_fires_events_normally_and_is_muted_inside_without_events_scope() {
    // The `db` handle MUST stay alive for the duration of the test —
    // dropping `TestDatabase` removes the singleton binding that
    // `Model::create` resolves via `DB::connection()`. Inline the
    // setup so the binding is kept in scope.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE IF NOT EXISTS swe_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            email TEXT NOT NULL,\
            created_at TEXT NOT NULL,\
            updated_at TEXT NOT NULL\
        )",
    )
    .await
    .unwrap();

    suprnova::eloquent::observers::bootstrap_observers()
        .await
        .unwrap();

    // Baseline: outside `without_events`, Model::create fires both
    // `creating` (cancellable) and `created` (after).
    CREATING_FIRES.store(0, Ordering::SeqCst);
    CREATED_FIRES.store(0, Ordering::SeqCst);

    let _ = SweUser::create(attrs! { email: "alice@example.com" })
        .await
        .unwrap();

    assert_eq!(
        CREATING_FIRES.load(Ordering::SeqCst),
        1,
        "Model::create fires `creating` (cancellable) outside seed::without_events",
    );
    assert_eq!(
        CREATED_FIRES.load(Ordering::SeqCst),
        1,
        "Model::create fires `created` (after) outside seed::without_events",
    );

    // Now under without_events: neither hook fires for the same call.
    CREATING_FIRES.store(0, Ordering::SeqCst);
    CREATED_FIRES.store(0, Ordering::SeqCst);

    seed::without_events(async {
        let _ = SweUser::create(attrs! { email: "bob@example.com" })
            .await
            .unwrap();
    })
    .await;

    assert_eq!(
        CREATING_FIRES.load(Ordering::SeqCst),
        0,
        "seed::without_events mutes the cancellable `creating` event on Model::create",
    );
    assert_eq!(
        CREATED_FIRES.load(Ordering::SeqCst),
        0,
        "seed::without_events mutes the after `created` event on Model::create",
    );

    // After leaving the scope, events fire again — task-locals are
    // strictly scoped to the future, not process-globally toggled.
    CREATING_FIRES.store(0, Ordering::SeqCst);
    CREATED_FIRES.store(0, Ordering::SeqCst);

    let _ = SweUser::create(attrs! { email: "carol@example.com" })
        .await
        .unwrap();

    assert_eq!(
        CREATING_FIRES.load(Ordering::SeqCst),
        1,
        "after the seed::without_events scope ends, `creating` fires again on Model::create",
    );
    assert_eq!(
        CREATED_FIRES.load(Ordering::SeqCst),
        1,
        "after the seed::without_events scope ends, `created` fires again on Model::create",
    );
}
