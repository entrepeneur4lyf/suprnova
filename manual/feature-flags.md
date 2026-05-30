# Feature Flags

Suprnova ships a feature-flag system that combines **compile-time declarations** with **runtime overrides** persisted to your database. A flag's behaviour is determined by, in order:

1. A scoped row in the `features` table — e.g. `user:42` or `team:staff`.
2. The global row in the `features` table (scope `""`).
3. The compile-time `default` baked into the `Feature` declaration.

Toggles via the admin CRUD propagate to live evaluators **before the call returns** — kill-switch flags actually disable in real time, not "within the next TTL window."

## Quick start

```rust
// app/src/features.rs — every flag your app references lives here.
use suprnova::features::Feature;

pub const NEW_CHECKOUT_FLOW: Feature<'static> = Feature::new("new-checkout-flow", false);
```

```rust
// app/src/bootstrap.rs — wire the chain once during boot.
use std::time::Duration;
use suprnova::features::{bootstrap_database_cached, FeatureMiddleware};

pub async fn register() {
    // ... DB::init, session, etc.

    bootstrap_database_cached(Duration::from_secs(60))
        .await
        .expect("feature flags wired");

    global_middleware!(FeatureMiddleware::new());
}
```

```rust
// any handler — Feature::is_enabled() resolves against the per-request context.
use crate::features::NEW_CHECKOUT_FLOW;

pub async fn index(req: Request) -> Response {
    let banner = if NEW_CHECKOUT_FLOW.is_enabled() {
        Some("Try the new checkout — faster, fewer steps.")
    } else {
        None
    };
    // ...
}
```

```rust
// flip the flag from an admin route or CLI:
use suprnova::features::admin;

admin::upsert("new-checkout-flow", "", true, None, Some(actor_user_id)).await?;
//                                  ^   ^                  ^
//                                  |   |                  └ audit: who toggled it (Option<String>)
//                                  |   └ enabled
//                                  └ scope_key: "" = global, "user:42" = scoped override
```

The next `NEW_CHECKOUT_FLOW.is_enabled()` call observes `true` — including the cached evaluator entry, which was invalidated synchronously inside `admin::upsert`.

## The pieces

### `Feature<'a>`

The compile-time declaration. Carries the flag name and a default-when-absent value.

```rust
pub const KILL_SWITCH_PAYMENTS: Feature<'static> =
    Feature::new("kill-switch.payments", true);
//                                      ^ default: true (payments enabled until disabled)
```

Centralising every declaration in `app/src/features.rs` gives you:

- a single place to grep when an operator asks "what flags exist?"
- compile-time uniqueness for the flag name — a typo at the call site doesn't compile
- the obvious place to put a doc comment explaining what the flag controls

Call `flag.is_enabled()` to read against the ambient context (set up by [`FeatureMiddleware`](#featuremiddleware)) or `flag.is_enabled_in(Some(&ctx))` to pass a specific [`Context`](https://docs.rs/featureflag/latest/featureflag/context/struct.Context.html).

### `DatabaseEvaluator`

Reads the `features` table into an in-memory snapshot at boot and on every [`reload()`](#flow-control-flag-propagation). The hot path (`is_enabled`) is fully synchronous — no DB query per request, no `block_on` inside the evaluator.

Resolution order on lookup, most specific first:

1. `user:{id}` — when the request context carries a `UserIdField`.
2. `team:{name}` — when the context carries a `TeamField`.
3. `""` — the global flag.
4. `None` — the row doesn't exist, the compile-time default takes over.

### `CachedEvaluator`

Memoizes `(feature, user, team)` lookups behind a `DashMap` with a TTL you pick. The hot path stays sync; entries are dropped synchronously when [`admin::upsert`](#admin-crud) writes a flag.

A TTL of zero degenerates to "no cache" — every call falls through to the inner evaluator. Useful for low-flag-count apps that want the propagation plumbing without the cache.

### `FeatureMiddleware`

Opens a per-request featureflag context populated by user-defined extractors. Defaults:

- `user_id` — from `Auth::id()`.
- `team` — none.

Override either via the builder:

```rust
let middleware = FeatureMiddleware::new()
    .with_user_id_extractor(|req| {
        // Custom: pull from a header instead of the session.
        req.header("X-User-Id").map(String::from)
    })
    .with_team_from_header("X-Team");
// or: .with_team_extractor(|req| your_custom_team_resolver(req))

global_middleware!(middleware);
```

### Admin CRUD

`suprnova::features::admin` is the persistence layer for the `features` table. Use it from admin handlers, CLI tools, deployment scripts — anywhere a flag needs to flip:

```rust
use suprnova::features::admin;

// Create or update a global flag.
admin::upsert("kill-switch.payments", "", false, Some("ops-2026-05-19".into()), actor_id).await?;
// args: name, scope_key, enabled, description, actor_id

// User-scoped override (beats the global).
admin::upsert("new-checkout-flow", "user:42", true, None, actor_id).await?;

// Remove a row entirely — flag falls back to compile-time default.
admin::delete("kill-switch.payments", "", actor_id).await?;

// Read for an admin UI table.
let all_flags = admin::list().await?;
let one_row = admin::get("kill-switch.payments", "").await?;
```

Every mutation fires the corresponding [event](#events) and calls [`features::sync::notify`](#flow-control-flag-propagation) so any live evaluator bound into the App container refreshes before the call returns.

`actor_id: Option<String>` is the audit pointer. Pass the operator's user id (the same one your auth layer issues); leave `None` for system-initiated changes (CLI, deploy migration, etc.).

## Flow control: flag propagation

The trait that makes "admin toggle visible immediately" work:

```rust
#[async_trait]
pub trait FeatureSync: Send + Sync + 'static {
    async fn on_flag_changed(&self, feature: &str, scope_key: &str);
}
```

Implementors react to mutations:

- `DatabaseEvaluator::on_flag_changed` calls `self.reload()` — pulls the full snapshot.
- `CachedEvaluator::on_flag_changed` calls `self.invalidate(feature)` — drops every cached entry for that name.

The canonical chain is a `CompositeFeatureSync`, which **orders data sources before caches** — caches must invalidate *after* the data source refreshes, or a concurrent reader can hit the empty cache, fall through to the stale data source, and repopulate the cache with the old value.

```rust
let composite = CompositeFeatureSync::new(
    vec![database.clone() as Arc<dyn FeatureSync>], // data sources first
    vec![cached.clone() as Arc<dyn FeatureSync>],   // caches second
);
App::bind::<dyn FeatureSync>(composite);
```

`features::sync::notify(feature, scope_key)` resolves `Arc<dyn FeatureSync>` from the container and awaits `on_flag_changed`. No-op when no sync is bound — the right behaviour for out-of-process admin tools that only write the DB and have no live evaluator to refresh.

## Bootstrap helper

`bootstrap_database_cached(ttl)` wires everything in one call:

```rust
let features = bootstrap_database_cached(Duration::from_secs(60))
    .await
    .expect("feature flags wired");

// Optional: hold onto features.database to schedule periodic reloads or
// expose admin diff views. Most apps drop the handle and let
// notify-driven refresh do the work.
```

What it does:

1. Constructs `DatabaseEvaluator` against the primary DB connection.
2. Wraps it in `CachedEvaluator` with the requested TTL.
3. Calls `install_evaluator(cached)` — sets the global featureflag default *and* flips a framework-owned "installed" tracker so the middleware doesn't log the "no evaluator" warning.
4. Builds a `CompositeFeatureSync` with the right slot order and binds it into the App container.

Returns `BootstrappedFeatures { database, cached }` for callers that want direct handles to either layer.

If your topology isn't `Cached(Database)` — a Redis-backed cache, a remote sync source, a multi-tier chain — wire the chain manually using the same primitives. `bootstrap_database_cached` is convenience, not a contract.

## Migrations

The framework owns the `features` table schema:

```rust
// app/src/migrations/mod.rs
vec![
    // ... your app's migrations ...
    Box::new(suprnova::features::migrations::CreateFeaturesTable),
]
```

Schema:

```sql
features (
    id          BIGINT      PRIMARY KEY AUTO_INCREMENT,
    name        VARCHAR(255) NOT NULL,
    scope_key   VARCHAR(255) NOT NULL DEFAULT '',
    enabled     BOOLEAN     NOT NULL,
    description TEXT,
    updated_by  VARCHAR(255),
    created_at  TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE INDEX (name, scope_key)
)
```

`scope_key` carries the scope kind inline (`"user:42"`, `"team:staff"`, `""` for global) so the read path stays a single string lookup against a unique index.

## User and team ids

`UserIdField` and `TeamField` are typed extensions stashed into the featureflag `Context::extensions`. Both are string-typed so torii's opaque (UUID / ULID) user ids and numeric `users.id` columns coexist behind the same shape.

Building a context manually (outside the middleware):

```rust
use featureflag::context;
use std::sync::Arc;

let ctx = featureflag::evaluator::with_default(cached.clone(), || {
    // string user ids — UUIDs, ULIDs, anything opaque.
    context! { user_id = "01HZK6V3J7Q5G4P8X9N2D1B0M3".to_string(), team = "staff".to_string() }
});

// numeric ids still work — the framework coerces i64 → String at on_new_context time.
let ctx_numeric = featureflag::evaluator::with_default(cached.clone(), || {
    context! { user_id = 42_i64 }
});
```

## Events

Two events fire from the admin CRUD path:

```rust
pub struct FeatureUpdated {
    pub name: String,
    pub scope_key: String,
    pub enabled: bool,
    pub actor_id: Option<String>,
}

pub struct FeatureDeleted {
    pub name: String,
    pub scope_key: String,
    pub actor_id: Option<String>,
}
```

Listen for them via the framework's event dispatcher to feed an audit log, Slack alert, or whatever downstream pipeline you need:

```rust
EventFacade::listen::<FeatureUpdated, _>(Arc::new(FlagChangeAuditor)).await;
```

**`is_enabled` does not fire a read-path event** in v1. Every request that checks a flag would multiply the event volume by the number of flags checked — fine for an audit-of-mutations story, prohibitive for read-path tracing. If your deployment needs sampled read-path audit, layer a custom evaluator that records into a bounded log channel (a Redis stream or a fanout queue, depending on scale).

## Missing-evaluator detection

If `FeatureMiddleware` is installed but no evaluator was registered via `install_evaluator` / `bootstrap_database_cached`, every flag silently returns its compile-time default — a hard misconfiguration to catch in QA. The middleware emits exactly one `tracing::warn!` per process on the first request that observes this state:

```
WARN suprnova::features: FeatureMiddleware is in the stack but no feature-flag evaluator is installed.
     is_enabled!() calls will return compile-time defaults until features::bootstrap_database_cached(...)
     or features::install_evaluator(...) is called during app boot.
```

The flip uses an `AtomicBool::swap` so a concurrent request storm at boot serializes to a single warning emission, not one per worker.

## Testing

Two patterns, depending on what you're verifying.

### Unit-test a Feature in isolation

Use `featureflag::evaluator::with_default` to scope a stand-in evaluator inside a sync closure:

```rust
#[test]
fn flag_enabled_returns_new_path() {
    use featureflag::evaluator::with_default;
    use suprnova::features::DatabaseEvaluator;

    let flagger = Arc::new(tokio_test::block_on(async {
        let e = DatabaseEvaluator::new_in_memory().await.unwrap();
        e.set_flag("new-checkout-flow", "", true).await.unwrap();
        e
    }));

    with_default(flagger, || {
        assert!(crate::features::NEW_CHECKOUT_FLOW.is_enabled());
    });
}
```

`DatabaseEvaluator::new_in_memory()` is a test-only helper that boots its own SQLite + runs `CreateFeaturesTable` so the test stays hermetic. Don't use it in production paths.

### Integration-test propagation end-to-end

Use `TestDatabase::fresh::<TestMigrator>()` for the DB and `TestContainer::bind` (NOT `App::bind`) for the FeatureSync — parallel tests on the same process would otherwise overwrite each other's binding via the global container:

```rust
#[tokio::test]
async fn admin_upsert_propagates_to_cached_chain() {
    use std::sync::Arc;
    use std::time::Duration;
    use suprnova::features::sync::FeatureSync;
    use suprnova::features::{admin, CachedEvaluator, CompositeFeatureSync, DatabaseEvaluator};
    use suprnova::features::migrations::CreateFeaturesTable;
    use suprnova::testing::{TestContainer, TestDatabase};

    struct TestMigrator;
    impl sea_orm_migration::MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
            vec![Box::new(CreateFeaturesTable)]
        }
    }

    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let database = Arc::new(DatabaseEvaluator::new().await.unwrap());
    let cached = Arc::new(CachedEvaluator::new(
        database.clone() as Arc<dyn featureflag::evaluator::Evaluator + Send + Sync>,
        Duration::from_secs(60),
    ));
    let composite = Arc::new(CompositeFeatureSync::new(
        vec![database.clone() as Arc<dyn FeatureSync>],
        vec![cached.clone() as Arc<dyn FeatureSync>],
    ));
    TestContainer::bind::<dyn FeatureSync>(composite);

    let ctx = featureflag::evaluator::with_default(cached.clone(), || {
        featureflag::context! { user_id = "user-42".to_string() }
    });

    assert_eq!(cached.is_enabled("new-feature", &ctx), None);
    admin::upsert("new-feature", "", true, None, None).await.unwrap();
    assert_eq!(cached.is_enabled("new-feature", &ctx), Some(true)); // propagates instantly
}
```

See `framework/tests/features.rs` for the full set of composition tests shipped with Phase 13.

## Design notes

- **Why a sync evaluator over async?** featureflag's `is_enabled` is the hot path. An async evaluator would force a `block_on` (deadlock-prone) or push every handler to `.await` on flag reads (ergonomic disaster). The framework bridges sync ↔ async via an in-memory snapshot refreshed asynchronously by `FeatureSync`.

- **Why a separate `FeatureSync` trait instead of extending `Evaluator`?** featureflag's `Evaluator` is owned by an upstream crate; we can't add methods to it. `FeatureSync` is a sibling trait apps implement on the same concrete types. The trait object is bound separately in the App container so a process can layer multiple evaluators while still routing notifications correctly.

- **Why is `set_flag` `pub` on `DatabaseEvaluator`?** Test convenience. The production write path is `admin::upsert`; `set_flag` exists so tests can seed flags without setting up an `EventFacade` listener. Both paths call `features::sync::notify` so the propagation contract holds either way.

- **Why no `FeatureRetrieved` event?** Volume. A handler checking ten flags per request fires ten events per request — for a 1k req/s service that's 36M events/hour, far above any audit pipeline's signal-to-noise ratio. Read-path sampling is a Phase 14 problem; mutation-path audit (`FeatureUpdated` / `FeatureDeleted`) is what v1 ships.

## Related

- `suprnova::features::admin` — full API reference for the CRUD facade. Run `cargo doc --open -p suprnova` and navigate to `features::admin`.
- [`docs/core/middleware.md`](middleware.md.md) — middleware ordering primer; `FeatureMiddleware` belongs after `SessionMiddleware`.
- [featureflag crate docs](https://docs.rs/featureflag) — the upstream primitives layer (`Evaluator`, `Context`, `Feature`).
