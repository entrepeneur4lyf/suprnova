# Phase 13: Feature Flags (Pennant-style) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Typed feature flags with A/B variants, scoped per-user / per-team / global. `Feature::active("new-checkout", &user).await?` returns `bool`; `Feature::value("homepage-hero", &user).await?` returns the resolved variant (`"control"` / `"variant-a"` / `"variant-b"`). Drivers: in-memory (default, dev), database-backed, cache-backed (Redis for high-traffic resolution).

**Architecture:** `framework/src/features/` ships a `Feature` facade backed by a `FeatureDriver` trait + registry. Each driver stores `(feature_name, scope_key) → variant` and exposes `resolve(name, scope) -> Variant`. Variants are JSON values; bool flags are a convenience over `Variant::Bool(true/false)`. Definitions are registered at boot via `Feature::define(name, resolver_closure)` — the closure decides the variant for a given scope. The DB driver caches definitions for fast resolution.

**Tech Stack:** No new crates — uses SeaORM for the DB driver, the existing Cache facade for the cache driver, Phase 1 events for `FeatureRetrieved` / `FeatureUpdated` audit hooks.

---

## File Structure

**New files:**
- `framework/src/features/mod.rs` — `Feature` facade, `Variant`, scope types
- `framework/src/features/driver.rs` — `FeatureDriver` trait + registry
- `framework/src/features/drivers/memory.rs` — in-memory driver
- `framework/src/features/drivers/database.rs` — `features` table driver
- `framework/src/features/drivers/cache.rs` — cache wrapper (caches DB lookups)
- `framework/src/features/middleware.rs` — `FeatureMiddleware` (gates routes)
- `framework/src/features/events.rs` — `FeatureRetrieved`, `FeatureUpdated`
- `framework/src/features/migrations/m_create_features_table.rs`
- `framework/tests/features.rs`
- `app/src/features/mod.rs` — definitions

---

## Task 1: Variant + Scope types + Feature facade

**Files:** `framework/src/features/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/features.rs
use suprnova::{Feature, features::{Scope, Variant}};

#[tokio::test]
async fn memory_driver_resolves_static_bool() {
    Feature::use_memory();
    Feature::define("new-checkout", |_scope| async move { Variant::Bool(true) }).await;

    let active = Feature::active("new-checkout", &Scope::global()).await;
    assert!(active);
}

#[tokio::test]
async fn resolver_can_inspect_scope() {
    Feature::use_memory();
    Feature::define("internal-tools", |scope: Scope| async move {
        if scope.is_user_in_team("staff") {
            Variant::Bool(true)
        } else {
            Variant::Bool(false)
        }
    })
    .await;

    let staff = Scope::for_user(1).with_team("staff");
    let public = Scope::for_user(99);
    assert!(Feature::active("internal-tools", &staff).await);
    assert!(!Feature::active("internal-tools", &public).await);
}

#[tokio::test]
async fn variant_resolution_returns_typed_value() {
    Feature::use_memory();
    Feature::define("homepage-hero", |scope: Scope| async move {
        match scope.user_id() {
            Some(id) if id % 2 == 0 => Variant::String("variant-a".into()),
            Some(_) => Variant::String("variant-b".into()),
            None => Variant::String("control".into()),
        }
    })
    .await;

    assert_eq!(
        Feature::value("homepage-hero", &Scope::for_user(2)).await,
        Variant::String("variant-a".into())
    );
    assert_eq!(
        Feature::value("homepage-hero", &Scope::for_user(3)).await,
        Variant::String("variant-b".into())
    );
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/features/mod.rs
//! Feature flags with typed variants.
//!
//! ```ignore
//! Feature::define("new-checkout", |scope| async move {
//!     if scope.user_id().map(|id| id < 100).unwrap_or(false) {
//!         Variant::Bool(true)   // beta users
//!     } else {
//!         Variant::Bool(false)
//!     }
//! }).await;
//!
//! if Feature::active("new-checkout", &user_scope).await {
//!     // ...
//! }
//! ```

pub mod driver;
pub mod drivers;
pub mod events;
pub mod middleware;

use crate::FrameworkError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub use driver::FeatureDriver;

/// A resolved feature value. JSON-like — bools and strings cover
/// the common cases; arbitrary JSON for complex experiments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Variant {
    Bool(bool),
    String(String),
    Json(serde_json::Value),
}

impl Variant {
    pub fn as_bool(&self) -> bool {
        match self {
            Variant::Bool(b) => *b,
            Variant::String(s) => !s.is_empty() && s != "false" && s != "0",
            Variant::Json(v) => v.as_bool().unwrap_or(true),
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Variant::String(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// Resolution scope — combines user, team, and arbitrary tags.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    user_id: Option<i64>,
    team_ids: Vec<String>,
    tags: HashMap<String, String>,
}

impl Scope {
    pub fn global() -> Self {
        Self::default()
    }
    pub fn for_user(id: i64) -> Self {
        Self {
            user_id: Some(id),
            ..Self::default()
        }
    }
    pub fn with_team(mut self, team: impl Into<String>) -> Self {
        self.team_ids.push(team.into());
        self
    }
    pub fn with_tag(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.tags.insert(k.into(), v.into());
        self
    }
    pub fn user_id(&self) -> Option<i64> {
        self.user_id
    }
    pub fn is_user_in_team(&self, team: &str) -> bool {
        self.team_ids.iter().any(|t| t == team)
    }
    pub fn tag(&self, k: &str) -> Option<&str> {
        self.tags.get(k).map(|s| s.as_str())
    }

    /// Stable string used for cache keys.
    pub fn cache_key(&self) -> String {
        let mut parts: Vec<String> = vec![];
        if let Some(uid) = self.user_id {
            parts.push(format!("u:{}", uid));
        }
        let mut teams = self.team_ids.clone();
        teams.sort();
        for t in teams {
            parts.push(format!("t:{}", t));
        }
        let mut tag_pairs: Vec<(&String, &String)> = self.tags.iter().collect();
        tag_pairs.sort_by(|a, b| a.0.cmp(b.0));
        for (k, v) in tag_pairs {
            parts.push(format!("{}:{}", k, v));
        }
        parts.join("|")
    }
}

pub type ResolveFn = std::sync::Arc<
    dyn Fn(Scope) -> futures::future::BoxFuture<'static, Variant> + Send + Sync,
>;

pub struct Feature;

impl Feature {
    pub async fn define<F, Fut>(name: impl Into<String>, resolver: F)
    where
        F: Fn(Scope) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Variant> + Send + 'static,
    {
        let resolver: ResolveFn = std::sync::Arc::new(move |scope| Box::pin(resolver(scope)));
        let driver = driver::current().expect("Feature driver not set");
        driver.define(&name.into(), resolver).await;
    }

    pub async fn value(name: &str, scope: &Scope) -> Variant {
        let driver = match driver::current() {
            Some(d) => d,
            None => return Variant::Bool(false),
        };
        let v = driver.resolve(name, scope.clone()).await.unwrap_or(Variant::Bool(false));
        let _ = crate::Event::dispatch(events::FeatureRetrieved {
            name: name.to_string(),
            scope_key: scope.cache_key(),
            value: v.clone(),
        })
        .await;
        v
    }

    pub async fn active(name: &str, scope: &Scope) -> bool {
        Self::value(name, scope).await.as_bool()
    }

    pub fn use_memory() {
        driver::set(std::sync::Arc::new(drivers::memory::MemoryDriver::new()));
    }

    pub async fn use_database() -> Result<(), FrameworkError> {
        let d = drivers::database::DatabaseDriver::new().await?;
        driver::set(std::sync::Arc::new(d));
        Ok(())
    }

    pub fn use_cached(ttl: std::time::Duration) {
        // Wrap the current driver in a cache layer.
        if let Some(inner) = driver::current() {
            let cached = drivers::cache::CacheDriver::new(inner, ttl);
            driver::set(std::sync::Arc::new(cached));
        }
    }
}
```

```rust
// framework/src/features/driver.rs
use super::{ResolveFn, Scope, Variant};
use crate::FrameworkError;
use async_trait::async_trait;
use std::sync::{Arc, OnceLock, RwLock};

#[async_trait]
pub trait FeatureDriver: Send + Sync {
    async fn define(&self, name: &str, resolver: ResolveFn);
    async fn resolve(&self, name: &str, scope: Scope) -> Result<Variant, FrameworkError>;
}

static DRIVER: OnceLock<RwLock<Option<Arc<dyn FeatureDriver>>>> = OnceLock::new();

pub fn set(driver: Arc<dyn FeatureDriver>) {
    let lock = DRIVER.get_or_init(|| RwLock::new(None));
    *lock.write().unwrap() = Some(driver);
}

pub fn current() -> Option<Arc<dyn FeatureDriver>> {
    DRIVER.get_or_init(|| RwLock::new(None)).read().unwrap().clone()
}
```

```rust
// framework/src/features/drivers/memory.rs
use crate::features::{driver::FeatureDriver, ResolveFn, Scope, Variant};
use crate::FrameworkError;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

pub struct MemoryDriver {
    definitions: RwLock<HashMap<String, ResolveFn>>,
}

impl MemoryDriver {
    pub fn new() -> Self {
        Self { definitions: RwLock::new(HashMap::new()) }
    }
}

#[async_trait]
impl FeatureDriver for MemoryDriver {
    async fn define(&self, name: &str, resolver: ResolveFn) {
        self.definitions.write().unwrap().insert(name.to_string(), resolver);
    }

    async fn resolve(&self, name: &str, scope: Scope) -> Result<Variant, FrameworkError> {
        let resolver = self
            .definitions
            .read()
            .unwrap()
            .get(name)
            .cloned();
        match resolver {
            Some(r) => Ok(r(scope).await),
            None => Err(FrameworkError::internal(format!("feature '{}' not defined", name))),
        }
    }
}
```

```rust
// framework/src/features/events.rs
use crate::EventTrait;

#[derive(Debug, Clone)]
pub struct FeatureRetrieved {
    pub name: String,
    pub scope_key: String,
    pub value: super::Variant,
}

impl EventTrait for FeatureRetrieved {
    fn event_name() -> &'static str { "FeatureRetrieved" }
}

#[derive(Debug, Clone)]
pub struct FeatureUpdated {
    pub name: String,
}

impl EventTrait for FeatureUpdated {
    fn event_name() -> &'static str { "FeatureUpdated" }
}
```

```rust
// framework/src/lib.rs
pub mod features;
pub use features::{Feature, features::Variant};
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test features
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/features framework/src/lib.rs framework/tests/features.rs
git commit -m "feat(features): Feature facade + Variant + Scope + in-memory driver"
```

---

## Task 2: Database driver

**Files:** `framework/src/features/drivers/database.rs`

- [ ] **Step 1: Migration**

```rust
// framework/src/features/migrations/m_create_features_table.rs
// CREATE TABLE features (
//   id BIGINT PRIMARY KEY AUTO_INCREMENT,
//   name VARCHAR(255) NOT NULL,
//   scope_key VARCHAR(255) NOT NULL,
//   value JSONB NOT NULL,
//   created_at DATETIME NOT NULL,
//   updated_at DATETIME NOT NULL,
//   UNIQUE INDEX (name, scope_key)
// );
```

- [ ] **Step 2: Implement**

```rust
// framework/src/features/drivers/database.rs
use crate::features::{driver::FeatureDriver, ResolveFn, Scope, Variant};
use crate::{FrameworkError, DB};
use async_trait::async_trait;
use sea_orm::{ConnectionTrait, Statement};
use std::collections::HashMap;
use std::sync::RwLock;

pub struct DatabaseDriver {
    // Definitions live in-memory; resolved values persist to DB.
    definitions: RwLock<HashMap<String, ResolveFn>>,
}

impl DatabaseDriver {
    pub async fn new() -> Result<Self, FrameworkError> {
        Ok(Self { definitions: RwLock::new(HashMap::new()) })
    }
}

#[async_trait]
impl FeatureDriver for DatabaseDriver {
    async fn define(&self, name: &str, resolver: ResolveFn) {
        self.definitions.write().unwrap().insert(name.to_string(), resolver);
    }

    async fn resolve(&self, name: &str, scope: Scope) -> Result<Variant, FrameworkError> {
        let scope_key = scope.cache_key();
        let db = DB::get()?;

        // 1. Look up cached resolution in `features` table.
        let cached: Option<(serde_json::Value,)> = db
            .query_one(Statement::from_sql_and_values(
                db.get_database_backend(),
                "SELECT value FROM features WHERE name = ? AND scope_key = ? LIMIT 1",
                vec![name.into(), scope_key.clone().into()],
            ))
            .await
            .map_err(|e| FrameworkError::internal(format!("features db: {}", e)))?
            .map(|row| {
                let v: serde_json::Value = row
                    .try_get("", "value")
                    .unwrap_or(serde_json::Value::Null);
                (v,)
            });

        if let Some((v,)) = cached {
            return Ok(match v {
                serde_json::Value::Bool(b) => Variant::Bool(b),
                serde_json::Value::String(s) => Variant::String(s),
                other => Variant::Json(other),
            });
        }

        // 2. Compute via resolver, persist.
        let resolver = self
            .definitions
            .read()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| FrameworkError::internal(format!("feature '{}' not defined", name)))?;
        let v = resolver(scope.clone()).await;
        let json = match &v {
            Variant::Bool(b) => serde_json::Value::Bool(*b),
            Variant::String(s) => serde_json::Value::String(s.clone()),
            Variant::Json(j) => j.clone(),
        };
        db.execute(Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO features (name, scope_key, value, created_at, updated_at) VALUES (?, ?, ?, NOW(), NOW())",
            vec![
                name.into(),
                scope_key.into(),
                json.to_string().into(),
            ],
        ))
        .await
        .map_err(|e| FrameworkError::internal(format!("features insert: {}", e)))?;
        Ok(v)
    }
}
```

- [ ] **Step 3: Commit**

```bash
git add framework/src/features/drivers/database.rs framework/src/features/migrations
git commit -m "feat(features): DatabaseDriver persists resolved variants per (name, scope)"
```

---

## Task 3: Cache wrapper driver

**Files:** `framework/src/features/drivers/cache.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/features/drivers/cache.rs
use crate::features::{driver::FeatureDriver, ResolveFn, Scope, Variant};
use crate::{Cache, FrameworkError};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

pub struct CacheDriver {
    inner: Arc<dyn FeatureDriver>,
    ttl: Duration,
}

impl CacheDriver {
    pub fn new(inner: Arc<dyn FeatureDriver>, ttl: Duration) -> Self {
        Self { inner, ttl }
    }
}

#[async_trait]
impl FeatureDriver for CacheDriver {
    async fn define(&self, name: &str, resolver: ResolveFn) {
        self.inner.define(name, resolver).await;
    }

    async fn resolve(&self, name: &str, scope: Scope) -> Result<Variant, FrameworkError> {
        let key = format!("feature:{}:{}", name, scope.cache_key());

        if let Some(cached) = Cache::store("default").get::<String>(&key).await? {
            return serde_json::from_str::<Variant>(&cached)
                .map_err(|e| FrameworkError::internal(format!("variant decode: {}", e)));
        }

        let v = self.inner.resolve(name, scope).await?;
        let serialized = serde_json::to_string(&v)
            .map_err(|e| FrameworkError::internal(format!("variant encode: {}", e)))?;
        Cache::store("default").put_with_ttl(&key, serialized, self.ttl).await?;
        Ok(v)
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add framework/src/features/drivers/cache.rs
git commit -m "feat(features): CacheDriver wraps inner driver with TTL-cached resolution"
```

---

## Task 4: FeatureMiddleware

**Files:** `framework/src/features/middleware.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/features/middleware.rs
//! Route gating by feature flag. Returns 404 if the flag is inactive
//! for the requesting user (the implicit scope).
//!
//! ```ignore
//! group!("/new-checkout")
//!     .middleware(FeatureMiddleware::require("new-checkout"))
//!     .routes([ /* ... */ ]);
//! ```

use super::{Feature, Scope};
use crate::http::{HttpResponse, Request, Response};
use crate::middleware::{Middleware, Next};
use crate::Auth;
use async_trait::async_trait;

pub struct FeatureMiddleware {
    feature_name: &'static str,
}

impl FeatureMiddleware {
    pub fn require(name: &'static str) -> Self {
        Self { feature_name: name }
    }
}

#[async_trait]
impl Middleware for FeatureMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let scope = match Auth::id() {
            Some(id) => Scope::for_user(id),
            None => Scope::global(),
        };
        if Feature::active(self.feature_name, &scope).await {
            next(request).await
        } else {
            Err(HttpResponse::new().status(404))
        }
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add framework/src/features/middleware.rs
git commit -m "feat(features): FeatureMiddleware gates routes by flag"
```

---

## Task 5: App dogfood

**Files:** `app/src/features/mod.rs`

- [ ] **Step 1: Define flags in bootstrap**

```rust
// app/src/features/mod.rs
use suprnova::features::{Scope, Variant};
use suprnova::Feature;

pub async fn register() {
    Feature::define("new-checkout", |scope: Scope| async move {
        // 25% rollout based on user-id modulo
        let active = scope
            .user_id()
            .map(|id| id % 4 == 0)
            .unwrap_or(false);
        Variant::Bool(active)
    })
    .await;

    Feature::define("internal-tools", |scope: Scope| async move {
        Variant::Bool(scope.is_user_in_team("staff"))
    })
    .await;
}
```

```rust
// app/src/bootstrap.rs — inside register()
crate::features::register().await;
```

- [ ] **Step 2: Gate a route**

```rust
// Where /admin routes are declared:
group!("/admin")
    .middleware(suprnova::features::middleware::FeatureMiddleware::require("internal-tools"))
    .routes([ /* admin routes */ ]);
```

- [ ] **Step 3: Commit**

```bash
git add app/src
git commit -m "feat(app): feature flag definitions + admin route gated by internal-tools"
```

---

## Task 6: Workspace lint + roadmap update

- [ ] **Step 1: Clippy + tests**

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: ROADMAP "Where we are" — move Feature flags to Production-ready. Commit + push.**

---

## Self-Review

| Spec item | Covered by |
|-----------|------------|
| Feature::define / active / value | Task 1 |
| Scope (user / team / tags) | Task 1 |
| Variant (Bool, String, Json) | Task 1 |
| In-memory driver | Task 1 |
| Database driver | Task 2 |
| Cache wrapper | Task 3 |
| FeatureMiddleware | Task 4 |
| FeatureRetrieved event | Task 1 |
| Dogfood | Task 5 |

---

## Execution Handoff

**Subagent-Driven recommended.**
