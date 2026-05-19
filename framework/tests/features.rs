//! Phase 13 — feature flags integration tests.
//!
//! Covers [`DatabaseEvaluator`]'s SeaORM snapshot path:
//!
//! * explicit global enable
//! * user-scoped flag that overrides the global default
//! * absent flag falling through to `None`
//!
//! Each test wires its own in-memory SQLite via
//! [`DatabaseEvaluator::new_in_memory`] so they stay hermetic and don't
//! fight over the framework's [`TestContainer`] singleton (which a
//! parallel test might be using for a different schema).
//!
//! # Why `with_default`
//!
//! The featureflag `context!` macro calls
//! [`Evaluator::on_new_context`] **only when an evaluator is in
//! scope** (`set_global_default` / `set_thread_default` /
//! `with_default`). Our `DatabaseEvaluator` is the thing that
//! translates raw context fields (`user_id = 1i64`) into the typed
//! `UserIdField` extension that `is_enabled` reads. We therefore
//! wrap each test body in `with_default(Arc::clone(&flagger), || { ... })`
//! before constructing any context. The pattern mirrors
//! `featureflag/tests/context.rs`.
//!
//! `set_global_default` would also work but is set-once-per-process,
//! so it would fight cross-test setup. `with_default` is task-scoped
//! and replaces cleanly between tests.

use std::sync::Arc;

use suprnova::features::{Context, DatabaseEvaluator, Evaluator};

/// Drive a closure with `flagger` installed as the active default,
/// mirroring the production wiring without committing to a
/// process-global.
fn with_flagger<F, R>(flagger: Arc<DatabaseEvaluator>, f: F) -> R
where
    F: FnOnce() -> R,
{
    featureflag::evaluator::with_default(flagger, f)
}

#[tokio::test]
async fn database_evaluator_returns_explicit_enabled() {
    let flagger = Arc::new(DatabaseEvaluator::new_in_memory().await.unwrap());
    flagger.set_flag("checkout-v2", "", true).await.unwrap();

    let result = with_flagger(Arc::clone(&flagger), || {
        // Root context — no scope fields. Should hit the global
        // `""` scope and return Some(true).
        let ctx = Context::root();
        flagger.is_enabled("checkout-v2", &ctx)
    });

    assert_eq!(result, Some(true));
}

#[tokio::test]
async fn database_evaluator_user_scope_overrides_global() {
    let flagger = Arc::new(DatabaseEvaluator::new_in_memory().await.unwrap());
    flagger.set_flag("internal-tools", "", false).await.unwrap();
    flagger
        .set_flag("internal-tools", "user:1", true)
        .await
        .unwrap();

    let (for_user_1, for_user_99) = with_flagger(Arc::clone(&flagger), || {
        // Each context!() invocation routes through
        // `Evaluator::on_new_context` because `with_flagger`
        // installed the evaluator above. That hook stashes a
        // `UserIdField` in the context's extensions; `is_enabled`
        // then reads it via the `context.iter()` walk.
        let ctx_user_1 = featureflag::context! { user_id = 1i64 };
        let r1 = flagger.is_enabled("internal-tools", &ctx_user_1);

        let ctx_user_99 = featureflag::context! { user_id = 99i64 };
        let r99 = flagger.is_enabled("internal-tools", &ctx_user_99);

        (r1, r99)
    });

    // user:1 has its own override → true wins.
    assert_eq!(for_user_1, Some(true));
    // user:99 has no override → falls through to the global "" =
    // false.
    assert_eq!(for_user_99, Some(false));
}

#[tokio::test]
async fn database_evaluator_unknown_returns_none() {
    let flagger = Arc::new(DatabaseEvaluator::new_in_memory().await.unwrap());

    let result = with_flagger(Arc::clone(&flagger), || {
        let ctx = Context::root();
        flagger.is_enabled("never-defined", &ctx)
    });

    assert_eq!(result, None);
}
