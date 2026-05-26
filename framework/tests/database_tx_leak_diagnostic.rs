//! Regression: HIGH audit finding `database` #3 — closure-form
//! transactions where the user leaks a `TxHandle` clone past the
//! closure boundary used to be silently "best effort" on the Err
//! path: the original closure error surfaced, but explicit rollback
//! was skipped (because `Arc::try_unwrap` failed) and SeaORM's
//! `DatabaseTransaction::drop` rollback only fired when the LAST
//! leaked handle eventually dropped.
//!
//! In the meantime the transaction sat in zombie state — queries
//! through the leaked handle continued to run against the still-open
//! tx. That's a real data-integrity hazard, and the previous
//! `tracing::warn!` only fired when explicit rollback *failed* (which
//! it doesn't in the leak case — we never even called rollback).
//!
//! The fix escalates the leak case to `tracing::error!` with the
//! leak count and the original closure error, so operators can
//! observe and alert on the condition. The fix doesn't (and can't,
//! without runtime cooperation from SeaORM) force-rollback through a
//! shared `Arc<DatabaseTransaction>` — the close is still deferred
//! to the last drop, but now it's deferred LOUDLY.
//!
//! This test forces the leak and asserts the ERROR log fires + the
//! closure's original error is the one returned.

use std::sync::{Arc, Mutex};

use suprnova::error::FrameworkError;
use suprnova::testing::TestDatabase;
use suprnova::{DB, TxHandle};
use tracing_test::traced_test;

#[tokio::test]
#[traced_test]
async fn leaked_txhandle_on_err_path_logs_error_and_surfaces_closure_err() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();

    // Share a slot with the closure via Arc<Mutex<...>> — the
    // closure's HRTB signature requires `'static` captures, so a
    // bare `&mut` to a local won't compile.
    let leaked: Arc<Mutex<Option<TxHandle>>> = Arc::new(Mutex::new(None));
    let leaked_for_closure = leaked.clone();

    let result: Result<(), FrameworkError> = DB::transaction(move |tx| {
        let slot = leaked_for_closure.clone();
        let handle = tx.handle();
        Box::pin(async move {
            // Stash the handle outside the closure — the audit-flagged
            // misuse this regression test exercises.
            *slot.lock().unwrap() = Some(handle);
            Err(FrameworkError::database(
                "intentional failure to exercise the Err path",
            ))
        })
    })
    .await;

    // 1. The closure's Err must surface untouched.
    let err = result.expect_err("closure returned Err, so transaction must return Err");
    let msg = format!("{err}");
    assert!(
        msg.contains("intentional failure to exercise the Err path"),
        "the original closure error must be surfaced, not masked by the leak diagnostic; \
         got: {msg}"
    );

    // 2. The leaked-handle ERROR log must fire. tracing_test captures
    //    every log emitted during the `#[traced_test]` body.
    logs_assert(|lines| {
        let hit = lines
            .iter()
            .any(|line| line.contains("ZOMBIE STATE") && line.contains("leaked_handles"));
        if hit {
            Ok(())
        } else {
            Err(format!(
                "expected the leaked-handle ERROR log; captured lines:\n{}",
                lines.join("\n")
            ))
        }
    });

    // 3. Drop the leaked clone so the transaction can finally close —
    //    SeaORM's Drop rollback runs when the last Arc reference
    //    falls. Without this drop the tx would linger until end of
    //    test process; we explicitly close to keep the test clean.
    leaked.lock().unwrap().take();
}

#[tokio::test]
#[traced_test]
async fn no_leak_means_no_zombie_error_log() {
    // The negative control: a normal Err-returning closure with no
    // leaked clones must NOT fire the leak diagnostic — only the
    // expected rollback path runs.
    let _db = TestDatabase::sqlite_memory().await.unwrap();

    let result: Result<(), FrameworkError> = DB::transaction(|_tx| {
        Box::pin(async move { Err(FrameworkError::database("intentional failure with no leak")) })
    })
    .await;

    assert!(result.is_err());

    logs_assert(|lines| {
        let saw_zombie = lines.iter().any(|line| line.contains("ZOMBIE STATE"));
        if saw_zombie {
            Err(format!(
                "no leak means no zombie log; but got: {}",
                lines.join("\n")
            ))
        } else {
            Ok(())
        }
    });
}
