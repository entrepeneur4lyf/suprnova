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

// --------------------------------------------------------------------
// R5 — Composition tests: Cached(Database) chain wired via
// `FeatureSync` propagation, end-to-end through admin CRUD.
//
// These are the regression tests for Phase 13 R1: a kill-switch flag
// toggled via `admin::upsert` MUST be visible to `is_enabled` on the
// cached chain by the time `upsert.await` returns. Without R1, the
// initial `None` answer would stay cached past the upsert, or the
// database snapshot would lag behind the DB row, both of which mean
// the operator's "disable feature now" click silently does nothing.
//
// Each test uses [`TestDatabase`] for hermetic per-test DB isolation
// AND binds a fresh [`CompositeFeatureSync`] into the same test
// container — `bootstrap_database_cached` would set featureflag's
// process-global default, which would leak between parallel tests
// and cross-contaminate (advisor flag #5). We construct the chain
// manually here so each test starts from a clean evaluator slate.

#[tokio::test]
async fn cached_chain_sees_upsert_without_manual_reload_or_ttl_wait() {
    use sea_orm_migration::MigratorTrait;
    use std::sync::Arc;
    use std::time::Duration;
    use suprnova::features::sync::FeatureSync;
    use suprnova::features::{
        admin, CachedEvaluator, CompositeFeatureSync, DatabaseEvaluator,
    };
    use suprnova::features::migrations::CreateFeaturesTable;
    use suprnova::testing::{TestContainer, TestDatabase};

    struct TestMigrator;
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
            vec![Box::new(CreateFeaturesTable)]
        }
    }

    // 1. Boot the test DB. Binds DbConnection into TestContainer.
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    // 2. Build Cached(Database) sharing the test DB.
    let database = Arc::new(DatabaseEvaluator::new().await.unwrap());
    let cached = Arc::new(CachedEvaluator::new(
        database.clone() as Arc<dyn Evaluator + Send + Sync>,
        Duration::from_secs(60), // a TTL long enough that R1, not the TTL, is what we're testing.
    ));

    // 3. Wire the composite — data sources first, caches second —
    //    into the TestContainer so admin::upsert's notify() resolves
    //    it.
    let composite = Arc::new(CompositeFeatureSync::new(
        vec![database.clone() as Arc<dyn FeatureSync>],
        vec![cached.clone() as Arc<dyn FeatureSync>],
    ));
    // Bind into the *test* container, not the global App container —
    // parallel tests would otherwise stomp on each other's binding
    // and resolve the wrong evaluator (advisor flag #5).
    TestContainer::bind::<dyn FeatureSync>(composite);

    // 4. Build a user-scoped context. The `featureflag::context!` macro
    //    needs an evaluator in scope to translate raw fields into
    //    typed extensions, so we wrap with `with_default` for just
    //    the construction step. The context outlives the scope
    //    because Extensions are owned by the context itself.
    let ctx = featureflag::evaluator::with_default(cached.clone(), || {
        featureflag::context! { user_id = "user-42".to_string() }
    });

    // 5. Baseline: flag not configured → cached returns None.
    //    The cache will memoize this None entry — exactly what R1 has
    //    to invalidate when upsert lands.
    assert_eq!(cached.is_enabled("kill-switch", &ctx), None);

    // 6. Upsert the flag globally. If R1 is wired:
    //      - admin::upsert writes the DB
    //      - notify() → CompositeFeatureSync → database.reload() →
    //        cached.invalidate("kill-switch")
    //      - returns
    //    The cached chain's next is_enabled call sees the new value.
    admin::upsert("kill-switch", "", true, None, None)
        .await
        .unwrap();

    assert_eq!(
        cached.is_enabled("kill-switch", &ctx),
        Some(true),
        "R1: admin::upsert must propagate to cached chain before returning. \
         If this fails: notify() didn't fire, the composite isn't bound, \
         or the cache wasn't invalidated.",
    );
}

#[tokio::test]
async fn cached_chain_sees_admin_delete_without_manual_reload() {
    use sea_orm_migration::MigratorTrait;
    use std::sync::Arc;
    use std::time::Duration;
    use suprnova::features::sync::FeatureSync;
    use suprnova::features::{
        admin, CachedEvaluator, CompositeFeatureSync, DatabaseEvaluator,
    };
    use suprnova::features::migrations::CreateFeaturesTable;
    use suprnova::testing::{TestContainer, TestDatabase};

    struct TestMigrator;
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
            vec![Box::new(CreateFeaturesTable)]
        }
    }

    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let database = Arc::new(DatabaseEvaluator::new().await.unwrap());
    let cached = Arc::new(CachedEvaluator::new(
        database.clone() as Arc<dyn Evaluator + Send + Sync>,
        Duration::from_secs(60),
    ));
    let composite = Arc::new(CompositeFeatureSync::new(
        vec![database.clone() as Arc<dyn FeatureSync>],
        vec![cached.clone() as Arc<dyn FeatureSync>],
    ));
    // Bind into the *test* container, not the global App container —
    // parallel tests would otherwise stomp on each other's binding
    // and resolve the wrong evaluator (advisor flag #5).
    TestContainer::bind::<dyn FeatureSync>(composite);

    let ctx = featureflag::evaluator::with_default(cached.clone(), || {
        featureflag::context! { user_id = "user-7".to_string() }
    });

    // Seed: flag enabled globally.
    admin::upsert("preview", "", true, None, None).await.unwrap();
    assert_eq!(
        cached.is_enabled("preview", &ctx),
        Some(true),
        "seeded flag must be visible — guards the upsert path before we test delete",
    );

    // Delete: cached chain must observe the removal immediately. Falls
    // back to the compile-time default (which the test framework can't
    // express since we're querying by name, so `is_enabled` returns
    // `None` to mean "no configured value").
    let deleted = admin::delete("preview", "", None).await.unwrap();
    assert!(deleted);

    assert_eq!(
        cached.is_enabled("preview", &ctx),
        None,
        "R1: admin::delete must invalidate cache + reload DB snapshot. \
         A stale cache here would mean the deleted flag still reports `Some(true)`.",
    );
}

#[tokio::test]
async fn cached_chain_handles_scoped_override_then_delete() {
    // Slightly richer ordering: global flag + user override + delete the
    // override. Verifies the cache is keyed (and invalidated) finely
    // enough that the override scope's removal exposes the global, not
    // a stale override.
    use sea_orm_migration::MigratorTrait;
    use std::sync::Arc;
    use std::time::Duration;
    use suprnova::features::sync::FeatureSync;
    use suprnova::features::{
        admin, CachedEvaluator, CompositeFeatureSync, DatabaseEvaluator,
    };
    use suprnova::features::migrations::CreateFeaturesTable;
    use suprnova::testing::{TestContainer, TestDatabase};

    struct TestMigrator;
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
            vec![Box::new(CreateFeaturesTable)]
        }
    }

    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let database = Arc::new(DatabaseEvaluator::new().await.unwrap());
    let cached = Arc::new(CachedEvaluator::new(
        database.clone() as Arc<dyn Evaluator + Send + Sync>,
        Duration::from_secs(60),
    ));
    let composite = Arc::new(CompositeFeatureSync::new(
        vec![database.clone() as Arc<dyn FeatureSync>],
        vec![cached.clone() as Arc<dyn FeatureSync>],
    ));
    // Bind into the *test* container, not the global App container —
    // parallel tests would otherwise stomp on each other's binding
    // and resolve the wrong evaluator (advisor flag #5).
    TestContainer::bind::<dyn FeatureSync>(composite);

    // user-1 context. We deliberately encode the user id with the
    // exact format DatabaseEvaluator's scope_keys_for produces
    // (`user:{id}`) for the override row.
    let ctx = featureflag::evaluator::with_default(cached.clone(), || {
        featureflag::context! { user_id = "1".to_string() }
    });

    // Global: off. user-1 override: on.
    admin::upsert("new-checkout", "", false, None, None)
        .await
        .unwrap();
    admin::upsert("new-checkout", "user:1", true, None, None)
        .await
        .unwrap();

    assert_eq!(
        cached.is_enabled("new-checkout", &ctx),
        Some(true),
        "user-scoped override must beat global flag",
    );

    // Remove the override. Global is still false; user-1 should now
    // see the global value (false).
    admin::delete("new-checkout", "user:1", None).await.unwrap();

    assert_eq!(
        cached.is_enabled("new-checkout", &ctx),
        Some(false),
        "after deleting the user override, the global value takes over. \
         If this returns Some(true), the cache still holds the override entry.",
    );
}
