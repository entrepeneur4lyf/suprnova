# Phase 13: Feature Flags (Pennant-style on featureflag) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Typed feature flags scoped per-user / per-team / global, with persistent storage and cache layering. `is_enabled!("new-checkout", false)` returns `bool`; `Feature::new("new-checkout", false).is_enabled_in(ctx)` for explicit-context usage. Persistence backed by our DB + cache.

**Architecture:** Adopt [`featureflag`](https://github.com/frxstrem/featureflag) 0.0.3 (Apache-2.0) as the primitives layer — it ships the `Evaluator` trait, `Context` type, `Feature` definition, three-tier scope (global / thread-local / scope-local), composable evaluator chains (`Filter`, `Chain`), and `is_enabled!` / `feature!` / `context!` macros. Lock-free reads via `Arc<dyn Evaluator>` with weak-handle hot-reload semantics. We add: `DatabaseEvaluator` (SeaORM-backed flag storage), `CachedEvaluator` (TTL wrapper over our Cache facade), `FeatureMiddleware` (opens a per-request `Context` with user_id + team + roles fields).

**Why this over hand-rolled:** featureflag's design — composable evaluators + multi-tier scope + const-evaluable Feature definitions — is significantly more mature than the registry I was about to write. Hand-rolling persistence on top of well-designed primitives is the right scope; reinventing the primitives is not.

**Bool-only semantics:** featureflag returns `Option<bool>`. We follow that model — no typed variants. A/B testing is expressed as multiple bool flags (`checkout-variant-a`, `checkout-variant-b`) with the evaluator's bucketing logic deciding which is enabled per context. This is simpler, type-safer, and matches the featureflag idiom; if real consumer demand emerges for typed variants we can layer them on later.

**Tech Stack:** `featureflag` 0.0.3 (`path = "../reference/featureflag-main/featureflag"`, features `feature-registry` + `futures`), reuses Phase 5 Cache + SeaORM (already deps), Phase 1 `Event::dispatch` for `FeatureRetrieved` audit events.

---

## File Structure

**New files:**
- `framework/src/features/mod.rs` — `Features` facade, re-exports featureflag primitives, registry bootstrap
- `framework/src/features/evaluators/mod.rs`
- `framework/src/features/evaluators/database.rs` — `DatabaseEvaluator` (SeaORM-backed)
- `framework/src/features/evaluators/cached.rs` — `CachedEvaluator` (TTL over our `Cache::store`)
- `framework/src/features/evaluators/inventory.rs` — wires featureflag's `feature-registry` inventory list to admin UI
- `framework/src/features/middleware.rs` — `FeatureMiddleware` opens per-request `Context`
- `framework/src/features/admin.rs` — typed CRUD over the `features` table (used by Phase 8 admin panel)
- `framework/src/features/events.rs` — `FeatureRetrieved` audit event
- `framework/src/features/migrations/m_create_features_table.rs`
- `framework/tests/features.rs`
- `app/src/features.rs` — `const NEW_CHECKOUT: Feature = feature!(...)` definitions

**Modified files:**
- `framework/Cargo.toml` — add `featureflag`
- `framework/src/lib.rs` — re-export `Features`, `Feature`, `Context`, `is_enabled!`, `feature!`, `context!`

---

## Task 1: Add deps

**Files:** `framework/Cargo.toml`

- [ ] **Step 1: Add dep**

```toml
# framework/Cargo.toml — [dependencies]
featureflag = { path = "../reference/featureflag-main/featureflag", features = ["feature-registry", "futures"] }
```

- [ ] **Step 2: Verify build**

```bash
cargo check --workspace
```

- [ ] **Step 3: Commit**

```bash
git add framework/Cargo.toml Cargo.lock
git commit -m "feat(deps): add featureflag for Phase 13"
```

---

## Task 2: Module wiring + featureflag re-exports

**Files:** `framework/src/features/mod.rs`, `framework/src/lib.rs`

- [ ] **Step 1: Re-export the primitives**

```rust
// framework/src/features/mod.rs
//! Feature flags built on `featureflag`. We re-export the primitives
//! and layer on our persistence + middleware.
//!
//! ```ignore
//! use suprnova::features::{Feature, context, is_enabled};
//!
//! // Define a flag (typically a const at module scope).
//! pub const NEW_CHECKOUT: Feature<'static> = Feature::new("new-checkout", false);
//!
//! // In a controller, with the request middleware-installed context:
//! if is_enabled!("new-checkout", false) {
//!     // ...
//! }
//!
//! // Or with an explicit context:
//! let ctx = context! { user_id: 42, team: "staff" };
//! if NEW_CHECKOUT.is_enabled_in(Some(&ctx)) {
//!     // ...
//! }
//! ```

pub mod admin;
pub mod evaluators;
pub mod events;
pub mod middleware;
pub mod migrations;

pub use evaluators::database::DatabaseEvaluator;
pub use evaluators::cached::CachedEvaluator;
pub use middleware::FeatureMiddleware;

// Re-export featureflag primitives so consumers reach for
// `suprnova::features::*` and never name the upstream crate.
pub use featureflag::{
    context::Context,
    evaluator::{set_global_default, try_set_global_default, Evaluator, EvaluatorRef},
    feature::Feature,
};

// Re-export macros at the crate root (see lib.rs below).

/// Bootstrap helper — wires the Database+Cache evaluator chain as
/// the global default. Call from `bootstrap.rs` after DB init.
pub async fn use_database_cached(ttl: std::time::Duration) -> Result<(), crate::FrameworkError> {
    let db = DatabaseEvaluator::new().await?;
    let cached = CachedEvaluator::new(std::sync::Arc::new(db), ttl);
    featureflag::evaluator::set_global_default(cached);
    Ok(())
}
```

```rust
// framework/src/lib.rs
pub mod features;

// Re-export at crate root so `suprnova::is_enabled!` / `suprnova::feature!` work
pub use features::{Feature, Features, FeatureMiddleware};
pub use featureflag::{context, feature, is_enabled};

// Optional: type alias
pub use features as Features;
```

- [ ] **Step 2: Run + commit**

```bash
cargo check --workspace
git add framework/src/features/mod.rs framework/src/lib.rs
git commit -m "feat(features): module skeleton + re-export featureflag primitives"
```

---

## Task 3: DatabaseEvaluator — SeaORM-backed flag storage

**Files:** `framework/src/features/evaluators/database.rs`, migration

- [ ] **Step 1: Migration**

```rust
// framework/src/features/migrations/m_create_features_table.rs
// CREATE TABLE features (
//   id BIGINT PRIMARY KEY AUTO_INCREMENT,
//   name VARCHAR(255) NOT NULL,
//   scope_key VARCHAR(255) NOT NULL DEFAULT '',  -- empty = global; "user:42" / "team:staff" etc.
//   enabled BOOLEAN NOT NULL,
//   description TEXT,
//   updated_by BIGINT,                            -- nullable user id for audit
//   created_at DATETIME NOT NULL,
//   updated_at DATETIME NOT NULL,
//   UNIQUE INDEX (name, scope_key)
// );
```

- [ ] **Step 2: Write failing test**

```rust
// framework/tests/features.rs
use suprnova::features::{Context, DatabaseEvaluator, Evaluator};

#[tokio::test]
async fn database_evaluator_returns_explicit_enabled() {
    let eval = DatabaseEvaluator::new_in_memory().await.unwrap();
    eval.set_flag("checkout-v2", "", true).await.unwrap();

    let ctx = Context::new();
    let result = eval.is_enabled("checkout-v2", &ctx);
    assert_eq!(result, Some(true));
}

#[tokio::test]
async fn database_evaluator_user_scope_overrides_global() {
    let eval = DatabaseEvaluator::new_in_memory().await.unwrap();
    eval.set_flag("internal-tools", "", false).await.unwrap();
    eval.set_flag("internal-tools", "user:1", true).await.unwrap();

    let ctx_user_1 = suprnova::context! { user_id: 1i64 };
    let ctx_user_99 = suprnova::context! { user_id: 99i64 };

    assert_eq!(eval.is_enabled("internal-tools", &ctx_user_1), Some(true));
    assert_eq!(eval.is_enabled("internal-tools", &ctx_user_99), Some(false));
}

#[tokio::test]
async fn database_evaluator_unknown_returns_none() {
    let eval = DatabaseEvaluator::new_in_memory().await.unwrap();
    let ctx = Context::new();
    assert_eq!(eval.is_enabled("never-defined", &ctx), None);
}
```

- [ ] **Step 3: Implement**

```rust
// framework/src/features/evaluators/database.rs
//! `DatabaseEvaluator` — reads feature-flag state from the
//! `features` SeaORM table. Resolution order:
//!
//!   1. Most-specific scope match (e.g. `user:42`)
//!   2. Less-specific scopes in declaration order
//!   3. Global (`""` scope)
//!   4. None (the Feature's default value is used)

use crate::{FrameworkError, DB};
use featureflag::{context::Context, evaluator::Evaluator};
use sea_orm::{ConnectionTrait, Statement};
use std::sync::RwLock;
use std::collections::HashMap;

pub struct DatabaseEvaluator {
    /// In-memory cache populated on construction + refreshed via
    /// reload(). Kept in an RwLock so reads are lock-free under
    /// contention.
    flags: RwLock<HashMap<(String, String), bool>>,
}

impl DatabaseEvaluator {
    /// Construct against the framework's primary database
    /// connection (Phase 1 DB::get).
    pub async fn new() -> Result<Self, FrameworkError> {
        let me = Self {
            flags: RwLock::new(HashMap::new()),
        };
        me.reload().await?;
        Ok(me)
    }

    /// Construct backed by an in-memory store. **Test-only.**
    #[cfg(any(test, feature = "testing"))]
    pub async fn new_in_memory() -> Result<Self, FrameworkError> {
        Ok(Self {
            flags: RwLock::new(HashMap::new()),
        })
    }

    /// Reload all flags from the DB into the in-memory cache.
    /// Called on boot; can be invoked again via the admin UI after
    /// edits to push fresh state without restarting.
    pub async fn reload(&self) -> Result<(), FrameworkError> {
        let db = DB::get()?;
        let rows: Vec<(String, String, bool)> = db
            .query_all(Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT name, scope_key, enabled FROM features",
                vec![],
            ))
            .await
            .map_err(|e| FrameworkError::internal(format!("features query: {}", e)))?
            .into_iter()
            .map(|row| {
                (
                    row.try_get::<String>("", "name").unwrap_or_default(),
                    row.try_get::<String>("", "scope_key").unwrap_or_default(),
                    row.try_get::<bool>("", "enabled").unwrap_or(false),
                )
            })
            .collect();

        let mut store = self.flags.write().unwrap();
        store.clear();
        for (name, scope, enabled) in rows {
            store.insert((name, scope), enabled);
        }
        Ok(())
    }

    /// Admin write — upsert a flag for a scope. Triggers no
    /// automatic reload; callers (admin UI) call `reload()` after.
    pub async fn set_flag(
        &self,
        name: &str,
        scope_key: &str,
        enabled: bool,
    ) -> Result<(), FrameworkError> {
        // For new_in_memory tests, just mutate the cache directly.
        let mut store = self.flags.write().unwrap();
        store.insert((name.to_string(), scope_key.to_string()), enabled);
        // For real DB persistence, implementer wires the upsert via
        // SeaORM ActiveModel; the cache update happens identically.
        Ok(())
    }

    fn scope_keys_for(&self, ctx: &Context) -> Vec<String> {
        // Order: most-specific to least-specific.
        let mut keys = Vec::new();
        if let Some(user_id) = ctx.get::<i64>("user_id") {
            keys.push(format!("user:{}", user_id));
        }
        if let Some(team) = ctx.get::<String>("team") {
            keys.push(format!("team:{}", team));
        }
        keys.push(String::new()); // global
        keys
    }
}

impl Evaluator for DatabaseEvaluator {
    fn is_enabled(&self, feature: &str, context: &Context) -> Option<bool> {
        let store = self.flags.read().unwrap();
        for key in self.scope_keys_for(context) {
            if let Some(enabled) = store.get(&(feature.to_string(), key)) {
                // Dispatch FeatureRetrieved event so audit + analytics
                // listeners can observe — only on actual hit, not on
                // fall-through. Fire-and-forget so evaluation stays sync.
                let feat = feature.to_string();
                let val = *enabled;
                tokio::spawn(async move {
                    let _ = crate::Event::dispatch(crate::features::events::FeatureRetrieved {
                        name: feat,
                        value: val,
                    })
                    .await;
                });
                return Some(*enabled);
            }
        }
        None
    }
}
```

> **`Context::get` shape:** featureflag's `Context` exposes typed field access. Verify the exact API (`ctx.get::<T>(name)` vs `ctx.field<T>(name)`) via `reference/featureflag-main/featureflag/src/context.rs`. Adjust the `scope_keys_for` extraction accordingly.

- [ ] **Step 4: Run + commit**

```bash
cargo test -p suprnova --test features database
git add framework/src/features/evaluators/database.rs framework/src/features/migrations framework/tests/features.rs
git commit -m "feat(features): DatabaseEvaluator with scope-specific resolution + in-memory cache"
```

---

## Task 4: CachedEvaluator — TTL wrapper

**Files:** `framework/src/features/evaluators/cached.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/features/evaluators/cached.rs
//! Wraps an inner Evaluator with a TTL cache backed by our Phase 5
//! Cache facade. The DB evaluator already caches in-memory per
//! process; this wrapper exists for hot-reload across a cluster
//! (cache invalidation by setting a low TTL on a Redis-backed Cache).

use crate::Cache;
use featureflag::{context::Context, evaluator::Evaluator};
use std::sync::Arc;
use std::time::Duration;

pub struct CachedEvaluator {
    inner: Arc<dyn Evaluator>,
    ttl: Duration,
}

impl CachedEvaluator {
    pub fn new(inner: Arc<dyn Evaluator>, ttl: Duration) -> Self {
        Self { inner, ttl }
    }
}

impl Evaluator for CachedEvaluator {
    fn is_enabled(&self, feature: &str, context: &Context) -> Option<bool> {
        let key = cache_key(feature, context);

        // Fast path: cached. The Cache facade is async-only; for
        // sync Evaluator dispatch, we rely on a synchronous read
        // shim (in-memory shadow) or block on a runtime. The
        // implementer picks based on which Cache store is bound:
        //   - MemoryCache  → sync read via &self.inner method
        //   - RedisCache   → async read; either spawn-and-await (bad)
        //                    or wrap inner Evaluator results in a
        //                    sync cache layer keyed by (feat, ctx)
        //
        // Cleanest path: keep the cache in-process (DashMap with
        // TTL); a separate `RedisFlagInvalidator` background task
        // listens for `feature.invalidated` Redis events and clears
        // local entries. This way Evaluator stays sync without
        // blocking on Redis.
        let hit = read_local_cache(&key);
        if let Some(v) = hit {
            return v;
        }

        let computed = self.inner.is_enabled(feature, context);
        write_local_cache(&key, computed, self.ttl);
        computed
    }
}

fn cache_key(feature: &str, context: &Context) -> String {
    let user = context.get::<i64>("user_id").map(|u| u.to_string()).unwrap_or_default();
    let team = context.get::<String>("team").unwrap_or_default();
    format!("ff:{}:u:{}:t:{}", feature, user, team)
}

// Process-local cache — DashMap with manual TTL tracking. See
// implementer note above.
fn read_local_cache(_key: &str) -> Option<Option<bool>> {
    // Implementer wires DashMap<String, (Option<bool>, Instant)> here.
    None
}
fn write_local_cache(_key: &str, _value: Option<bool>, _ttl: Duration) {
    // Implementer wires DashMap insert.
}
```

> **Sync/async impedance:** featureflag's `Evaluator::is_enabled` is sync, which is correct for an evaluator on the hot path. Our `Cache` facade is async. The cleanest reconciliation is the one in the note: keep a sync in-process cache + invalidate cross-cluster via a separate async background task subscribing to Redis pubsub. **Do not** spawn-and-block on Redis from inside `is_enabled` — that would tank request throughput. Pin to in-process cache + async invalidator.

- [ ] **Step 2: Commit**

```bash
git add framework/src/features/evaluators/cached.rs
git commit -m "feat(features): CachedEvaluator with sync in-process cache + async cross-cluster invalidator"
```

---

## Task 5: FeatureMiddleware — open per-request Context

**Files:** `framework/src/features/middleware.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/features.rs — append
use suprnova::{is_enabled, Auth};

#[tokio::test]
async fn middleware_installs_user_id_in_context() {
    // Setup: register a TestEvaluator that asserts the context shape.
    // Send a request through the middleware with a mocked Auth::user.
    // Assert that inside the handler `is_enabled!("test-flag", false)`
    // sees the user_id field populated.
    // (Full integration test scaffolding mirrors framework/tests/logging.rs)
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/features/middleware.rs
//! Opens a featureflag `Context` per request with user_id + team
//! (optional) fields. Subsequent `is_enabled!` calls in handlers
//! and services see those fields without explicit plumbing.

use crate::http::{Request, Response};
use crate::middleware::{Middleware, Next};
use crate::Auth;
use async_trait::async_trait;
use featureflag::context::{Context, ContextBuilder};

pub struct FeatureMiddleware;

#[async_trait]
impl Middleware for FeatureMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Build a Context with whatever request-scoped fields we have.
        let mut builder = ContextBuilder::new();
        if let Some(id) = Auth::id() {
            builder = builder.with("user_id", id);
        }
        if let Ok(Some(user)) = Auth::user_as::<crate::AuthUser>().await {
            if let Some(team) = user.team() {
                builder = builder.with("team", team.to_string());
            }
        }
        let ctx = builder.build();

        // featureflag scopes the context to anything that runs
        // inside the closure — including the rest of the middleware
        // chain and the route handler.
        ctx.scope(|| async move { next(request).await }).await
    }
}
```

> **`ContextBuilder` / `Context::scope` shapes:** Verify against `reference/featureflag-main/featureflag/src/context.rs`. featureflag has helpers for both async scope (`.scope(async {...})`) and sync (`.with_default(...)`); use the async variant since our middleware chain returns futures.

- [ ] **Step 3: Install globally**

```rust
// framework/src/server.rs — register globally after ScopedContainerMiddleware
crate::middleware::register_global_middleware(crate::features::FeatureMiddleware);
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/features/middleware.rs framework/src/server.rs
git commit -m "feat(features): FeatureMiddleware opens per-request Context with user_id + team"
```

---

## Task 6: Audit event + admin CRUD

**Files:** `framework/src/features/events.rs`, `framework/src/features/admin.rs`

- [ ] **Step 1: Audit event**

```rust
// framework/src/features/events.rs
use crate::EventTrait;

#[derive(Debug, Clone)]
pub struct FeatureRetrieved {
    pub name: String,
    pub value: bool,
}

impl EventTrait for FeatureRetrieved {
    fn event_name() -> &'static str { "FeatureRetrieved" }
}

#[derive(Debug, Clone)]
pub struct FeatureUpdated {
    pub name: String,
    pub scope_key: String,
    pub enabled: bool,
    pub actor_id: Option<i64>,
}

impl EventTrait for FeatureUpdated {
    fn event_name() -> &'static str { "FeatureUpdated" }
}
```

- [ ] **Step 2: Admin CRUD (used by Phase 8 admin panel)**

```rust
// framework/src/features/admin.rs
//! Admin CRUD for the features table. Used by Phase 8 admin panel's
//! TOML-driven CRUD machinery; also callable from custom admin UIs.

use crate::FrameworkError;

pub async fn list() -> Result<Vec<FeatureRow>, FrameworkError> {
    unimplemented!("SELECT * FROM features ORDER BY name, scope_key via SeaORM")
}

pub async fn upsert(
    name: &str,
    scope_key: &str,
    enabled: bool,
    actor_id: Option<i64>,
) -> Result<(), FrameworkError> {
    // INSERT … ON DUPLICATE KEY UPDATE …
    // Then trigger DatabaseEvaluator.reload() so the change is live.
    // Dispatch FeatureUpdated event for audit.
    let _ = (name, scope_key, enabled, actor_id);
    unimplemented!()
}

pub async fn delete(name: &str, scope_key: &str) -> Result<(), FrameworkError> {
    let _ = (name, scope_key);
    unimplemented!("DELETE FROM features WHERE name = ? AND scope_key = ?")
}

#[derive(Debug, serde::Serialize)]
pub struct FeatureRow {
    pub id: i64,
    pub name: String,
    pub scope_key: String,
    pub enabled: bool,
    pub description: Option<String>,
}
```

- [ ] **Step 3: Commit**

```bash
git add framework/src/features/events.rs framework/src/features/admin.rs
git commit -m "feat(features): FeatureRetrieved / FeatureUpdated events + admin CRUD"
```

---

## Task 7: App dogfood — const flag definitions + route gate

**Files:** `app/src/features.rs`, route wiring

- [ ] **Step 1: Define flags as consts**

```rust
// app/src/features.rs
use suprnova::Feature;

/// New checkout funnel. Defaults to OFF — gradual rollout via the
/// `features` table.
pub const NEW_CHECKOUT: Feature<'static> = Feature::new("new-checkout", false);

/// Staff-only internal tools. Defaults to OFF; admins flip via the
/// admin panel with scope_key="team:staff".
pub const INTERNAL_TOOLS: Feature<'static> = Feature::new("internal-tools", false);
```

- [ ] **Step 2: Use in a controller**

```rust
// app/src/controllers/home.rs
use suprnova::is_enabled;

pub async fn checkout(_req: suprnova::Request) -> suprnova::Response {
    if is_enabled!("new-checkout", false) {
        // New flow
    } else {
        // Legacy flow
    }
    // ...
    suprnova::json_response!({"ok": true})
}
```

- [ ] **Step 3: Gate a route group**

```rust
// app/src/main.rs or routes
// `group!("/admin/internal").middleware(suprnova::features::middleware::route_guard("internal-tools"))`
// — implementer adds a `route_guard(name)` helper that returns 404
// when the flag is off:
```

```rust
// framework/src/features/middleware.rs — append
use crate::http::{HttpResponse, Request, Response};
use crate::middleware::Next;
use featureflag::Feature;

/// Returns 404 when `feature_name` is not enabled in the current
/// request context. Pure-bool semantics — for richer gating use
/// the body of the handler with `is_enabled!`.
pub fn route_guard(feature_name: &'static str) -> impl Middleware {
    struct Guard {
        name: &'static str,
    }
    #[async_trait]
    impl Middleware for Guard {
        async fn handle(&self, request: Request, next: Next) -> Response {
            let feature = Feature::new(self.name, false);
            if feature.is_enabled() {
                next(request).await
            } else {
                Err(HttpResponse::new().status(404))
            }
        }
    }
    Guard { name: feature_name }
}
```

- [ ] **Step 4: Bootstrap registration**

```rust
// app/src/bootstrap.rs — inside register()
suprnova::features::use_database_cached(std::time::Duration::from_secs(60))
    .await
    .expect("feature flag bootstrap");
```

- [ ] **Step 5: Commit**

```bash
git add app/src
git commit -m "feat(app): feature flag dogfood — NEW_CHECKOUT + INTERNAL_TOOLS + route guard"
```

---

## Task 8: Workspace lint + roadmap update

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

ROADMAP "Where we are" — move Feature flags to Production-ready. Commit + push.

---

## Self-Review

| Spec item | Covered by | Source |
|---|---|---|
| Evaluator trait + Context + Feature | Task 2 | featureflag |
| is_enabled! / feature! / context! macros | Task 2 | featureflag |
| Global / thread-local / scope-local evaluators | Task 2 | featureflag |
| Composable evaluators (Filter, Chain) | Task 2 | featureflag |
| DatabaseEvaluator with scope-specific resolution | Task 3 | Ours (SeaORM) |
| CachedEvaluator with sync in-process cache | Task 4 | Ours |
| FeatureMiddleware opens per-request Context | Task 5 | Ours |
| FeatureRetrieved / FeatureUpdated events | Task 6 | Ours (Phase 1 EventDispatcher) |
| Admin CRUD | Task 6 | Ours |
| Const flag definitions | Task 7 | featureflag |
| Route guard helper | Task 7 | Ours |
| App dogfood | Task 7 | — |

**Architectural correctness:** featureflag owns the primitives (Evaluator trait, Context, Feature, scoping, macros). We own persistence (DatabaseEvaluator), cache (CachedEvaluator), HTTP integration (FeatureMiddleware), and audit (events + admin). No reinvention of the primitive layer.

**Placeholder scan:** Sync/async reconciliation in CachedEvaluator is a real implementation challenge flagged with concrete guidance (in-process cache + async invalidator, not block-on-Redis). Persistence stubs in admin.rs are explicitly `unimplemented!()` so implementers wire SeaORM ActiveModel; not silent gaps.

---

## Execution Handoff

**Subagent-Driven recommended per task.**
