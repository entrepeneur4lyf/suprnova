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
