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

// =============================================================================
// T6 — Admin CRUD tests
//
// The admin module operates against the global `DB::connection()`
// (not the standalone connection `DatabaseEvaluator::new_in_memory`
// holds), so these tests use `TestDatabase::fresh::<TestMigrator>` to
// install a connection in the container plus apply the framework's
// `features` migration.
// =============================================================================

#[tokio::test]
async fn admin_upsert_inserts_then_updates_returning_canonical_row() {
    use sea_orm_migration::MigratorTrait;
    use suprnova::features::admin;
    use suprnova::features::migrations::CreateFeaturesTable;
    use suprnova::testing::TestDatabase;

    struct TestMigrator;
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
            vec![Box::new(CreateFeaturesTable)]
        }
    }

    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    // Initial insert.
    let row = admin::upsert(
        "checkout-v2",
        "",
        true,
        Some("new checkout flow".into()),
        Some("7".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(row.name, "checkout-v2");
    assert_eq!(row.scope_key, "");
    assert!(row.enabled);
    assert_eq!(row.description.as_deref(), Some("new checkout flow"));
    assert_eq!(row.updated_by.as_deref(), Some("7"));
    let initial_id = row.id;

    // Update via the same name+scope_key — `OnConflict` updates in place.
    let updated = admin::upsert(
        "checkout-v2",
        "",
        false,
        Some("rolling back".into()),
        Some("9".to_string()),
    )
    .await
    .unwrap();
    assert_eq!(updated.id, initial_id, "upsert must preserve the row id");
    assert!(!updated.enabled);
    assert_eq!(updated.description.as_deref(), Some("rolling back"));
    assert_eq!(updated.updated_by.as_deref(), Some("9"));
}

#[tokio::test]
async fn admin_list_returns_rows_sorted_by_name_then_scope() {
    use sea_orm_migration::MigratorTrait;
    use suprnova::features::admin;
    use suprnova::features::migrations::CreateFeaturesTable;
    use suprnova::testing::TestDatabase;

    struct TestMigrator;
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
            vec![Box::new(CreateFeaturesTable)]
        }
    }

    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    admin::upsert("zeta", "", true, None, None).await.unwrap();
    admin::upsert("alpha", "user:99", false, None, None)
        .await
        .unwrap();
    admin::upsert("alpha", "", true, None, None).await.unwrap();

    let rows = admin::list().await.unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].name, "alpha");
    assert_eq!(rows[0].scope_key, "");
    assert_eq!(rows[1].name, "alpha");
    assert_eq!(rows[1].scope_key, "user:99");
    assert_eq!(rows[2].name, "zeta");
}

#[tokio::test]
async fn admin_delete_returns_true_then_false_on_repeat() {
    use sea_orm_migration::MigratorTrait;
    use suprnova::features::admin;
    use suprnova::features::migrations::CreateFeaturesTable;
    use suprnova::testing::TestDatabase;

    struct TestMigrator;
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
            vec![Box::new(CreateFeaturesTable)]
        }
    }

    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    admin::upsert("toggle-me", "", true, None, None)
        .await
        .unwrap();

    let first = admin::delete("toggle-me", "", Some("3".to_string()))
        .await
        .unwrap();
    assert!(first, "first delete must report the row was removed");

    let second = admin::delete("toggle-me", "", Some("3".to_string()))
        .await
        .unwrap();
    assert!(
        !second,
        "second delete on the same key must report no-op (false)"
    );

    assert!(admin::get("toggle-me", "").await.unwrap().is_none());
}
