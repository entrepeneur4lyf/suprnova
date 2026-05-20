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
