//! Integration tests for `SupervisorRegistry::shutdown`.
//!
//! These tests verify that `shutdown(timeout)` cancels the supervisor token
//! and drains the task JoinSet cleanly within the grace window.
//!
//! The framework integration-test binary has no `inventory::submit!`
//! registrations, so we exercise the shutdown path by:
//!   1. Calling `start_all()` to initialize the process-global statics.
//!   2. Manually spawning a long-running cancellable task into the JoinSet via
//!      the public `supervisor_tasks()` accessor.
//!   3. Calling `shutdown(timeout)` and asserting clean termination.
//!
//! ## Note on process-global statics
//!
//! `SUPERVISOR_TASKS` and `SUPERVISOR_CANCEL` are process-level `OnceLock`s.
//! Multiple tests in the same binary would share these statics and race on
//! them. This file intentionally contains a single test to avoid that hazard.
//! The token-already-cancelled and empty-JoinSet paths are both covered within
//! the single test by running `shutdown` twice.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use suprnova::supervisor::{supervisor_cancel_token, supervisor_tasks, SupervisorRegistry};

/// Full shutdown lifecycle:
///
/// 1. `start_all()` initializes the process-global statics.
/// 2. We manually inject a cancel-aware sentinel task into the JoinSet.
/// 3. `shutdown(1s)` fires the cancel token, the task exits, JoinSet drains.
/// 4. A second `shutdown(1s)` on the already-empty/already-cancelled statics
///    returns immediately (no hang, no panic) — verifies the idempotent path.
#[tokio::test]
async fn shutdown_cancels_token_drains_tasks_and_is_idempotent() {
    // ── Phase 1: initialize statics ──────────────────────────────────────────
    SupervisorRegistry::start_all().await;

    let exited = Arc::new(AtomicBool::new(false));

    // ── Phase 2: inject a sentinel cancel-aware task ─────────────────────────
    {
        let sv_tasks = supervisor_tasks()
            .expect("start_all must have initialised SUPERVISOR_TASKS");
        let cancel = supervisor_cancel_token()
            .expect("start_all must have initialised SUPERVISOR_CANCEL")
            .clone();
        // Only inject when the token hasn't already fired (e.g., from a prior
        // test binary run that somehow shared state — extremely unlikely but
        // defensive).
        if !cancel.is_cancelled() {
            let exited_clone = Arc::clone(&exited);
            let mut guard = sv_tasks.lock().await;
            guard.spawn(async move {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        exited_clone.store(true, Ordering::SeqCst);
                    }
                    _ = tokio::time::sleep(Duration::from_secs(3600)) => {
                        // Would never reach here in this test.
                    }
                }
            });
        }
    }

    // ── Phase 3: shutdown — must cancel token and drain ──────────────────────
    let before = std::time::Instant::now();
    SupervisorRegistry::shutdown(Duration::from_secs(1)).await;
    let elapsed = before.elapsed();

    // Should complete well within the deadline (task exits as soon as token fires).
    assert!(
        elapsed < Duration::from_millis(800),
        "shutdown took too long ({elapsed:?}); cancel-aware task should exit near-instantly"
    );

    // Token must be fired.
    let token = supervisor_cancel_token().expect("token must still exist");
    assert!(token.is_cancelled(), "cancel token should be fired after shutdown");

    // Sentinel task must have observed the cancellation.
    assert!(
        exited.load(Ordering::SeqCst),
        "sentinel task should have set exited=true when cancel fired"
    );

    // JoinSet must be empty.
    {
        let sv_tasks = supervisor_tasks().unwrap();
        let guard = sv_tasks.lock().await;
        assert!(guard.is_empty(), "JoinSet should be empty after shutdown drained all tasks");
    }

    // ── Phase 4: second shutdown — idempotent, no hang ───────────────────────
    // Token already cancelled, JoinSet already empty. Shutdown must return
    // immediately without hanging or panicking.
    let before2 = std::time::Instant::now();
    SupervisorRegistry::shutdown(Duration::from_millis(200)).await;
    let elapsed2 = before2.elapsed();

    assert!(
        elapsed2 < Duration::from_millis(150),
        "second shutdown (idempotent) should return near-instantly; took {elapsed2:?}"
    );
}
