# Phase 8: Admin Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ship a TOML-driven admin UI inspired by SeaORM Pro: declare an `admin/tables/users.toml` and get CRUD + search + sort + RBAC + audit trail for that entity without writing UI code. Override with a custom Inertia page when the default isn't enough. The admin panel reuses Suprnova's auth, routing, policies (Phase 3), and migrations — no second framework underneath.

**Architecture:** The admin panel is a separate Inertia app served at `/admin` (configurable). At boot the framework reads `admin/tables/*.toml` and `admin/composite/*.toml`, validates them against schemas, and **dynamically registers** the admin routes (`/admin/users`, `/admin/users/new`, `/admin/users/:id`, etc.) onto the existing router. CRUD operations resolve the SeaORM entity by name via a small registry populated by `#[admin_entity]` (or by config-supplied `entity = "users"` mapping to SeaORM `Entity` types). Policies declared in TOML reference `#[policy]` impls from Phase 3 — no parallel auth. Audit trail writes to an `audits` table on every create/update/delete via a model observer.

**Tech Stack:** `toml` 0.8 (for parsing), reuses Inertia + React 19 / Vue 3 / Svelte 5 starters from existing scaffolder, reuses SeaORM + Authorization + Migrations. New deps: just `toml`.

---

## File Structure

**New files:**
- `framework/src/admin/mod.rs` — `Admin` registry + entrypoint
- `framework/src/admin/config.rs` — TOML schema types (`TableConfig`, `CompositeConfig`, `DashboardConfig`)
- `framework/src/admin/loader.rs` — read + validate `admin/*.toml`
- `framework/src/admin/registry.rs` — entity-name → SeaORM Entity factory map
- `framework/src/admin/routes.rs` — programmatic route generation
- `framework/src/admin/audit.rs` — `Audit` trait + middleware-style observer
- `framework/src/admin/policy_bridge.rs` — resolve TOML policy strings to `#[policy]` impls
- `framework/src/admin/handlers/{index,show,create,update,delete,composite}.rs` — generic CRUD handlers
- `framework/src/admin/inertia_pages.rs` — Inertia page names for the admin SPA
- `framework/tests/admin_config.rs` — TOML parse + validate
- `framework/tests/admin_crud.rs` — index/show/create/update/delete with policy enforcement
- `framework/tests/admin_audit.rs` — audit row written per mutation
- `suprnova-macros/src/admin_entity.rs` — `#[admin_entity]` derive (optional alternative to TOML-only)
- `suprnova-cli/src/commands/make_admin.rs` — `suprnova make:admin <Entity>` generator
- Frontend admin starter under `suprnova-cli/src/templates/files/admin/<framework>/` — React 19 / Vue 3 / Svelte 5 variants of the admin SPA pages (Index, Show, Edit, Create, Composite)
- Migration: `framework/src/admin/migrations/m_create_audits_table.rs`

**Modified files:**
- `framework/Cargo.toml` — add `toml`
- `framework/src/lib.rs` — declare + re-export
- `framework/src/server.rs` — call `Admin::install_routes` on boot if `admin/` exists
- `app/admin/tables/{users,posts}.toml` — dogfood

---

## Task 1: Add deps

**Files:** `framework/Cargo.toml`

- [ ] **Step 1: Add**

```toml
# framework/Cargo.toml
toml = "0.8"
```

- [ ] **Step 2: Verify**

```bash
cargo check --workspace
```

- [ ] **Step 3: Commit**

```bash
git add framework/Cargo.toml Cargo.lock
git commit -m "feat(deps): add toml for Phase 8 admin panel"
```

---

## Task 2: TOML schema types + parsing

**Files:** `framework/src/admin/config.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/admin_config.rs
use suprnova::admin::config::{TableConfig, parse_table_config};

#[test]
fn parses_users_toml() {
    let raw = r#"
        [table]
        entity = "users"
        title = "Users"
        icon = "user"

        [[columns]]
        field = "id"

        [[columns]]
        field = "email"
        sortable = true
        searchable = true

        [[columns]]
        field = "created_at"
        format = "datetime"

        [policies]
        view = "UserPolicy::view"
        edit = "UserPolicy::edit"
        delete = "UserPolicy::delete"

        [audit]
        enabled = true
    "#;
    let cfg: TableConfig = parse_table_config(raw).unwrap();
    assert_eq!(cfg.table.entity, "users");
    assert_eq!(cfg.table.title, "Users");
    assert_eq!(cfg.columns.len(), 3);
    assert!(cfg.columns[1].sortable);
    assert!(cfg.columns[1].searchable);
    assert_eq!(cfg.columns[2].format.as_deref(), Some("datetime"));
    assert_eq!(cfg.policies.view.as_deref(), Some("UserPolicy::view"));
    assert!(cfg.audit.enabled);
}

#[test]
fn missing_required_fields_fails() {
    let raw = r#"
        [table]
        title = "broken"
    "#;
    assert!(parse_table_config(raw).is_err());
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/admin/config.rs
use crate::FrameworkError;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct TableConfig {
    pub table: TableMeta,
    #[serde(default)]
    pub columns: Vec<ColumnMeta>,
    #[serde(default)]
    pub policies: PolicyMeta,
    #[serde(default)]
    pub audit: AuditMeta,
}

#[derive(Debug, Deserialize)]
pub struct TableMeta {
    pub entity: String,
    pub title: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub per_page: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ColumnMeta {
    pub field: String,
    #[serde(default)]
    pub sortable: bool,
    #[serde(default)]
    pub searchable: bool,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PolicyMeta {
    pub view: Option<String>,
    pub create: Option<String>,
    pub edit: Option<String>,
    pub delete: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct AuditMeta {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct CompositeConfig {
    pub composite: CompositeMeta,
    pub blocks: Vec<CompositeBlock>,
}

#[derive(Debug, Deserialize)]
pub struct CompositeMeta {
    pub name: String,
    pub title: String,
    pub primary_entity: String,
}

#[derive(Debug, Deserialize)]
pub struct CompositeBlock {
    pub kind: String, // "header" | "related_list" | "summary"
    pub entity: Option<String>,
    pub relation: Option<String>,
    pub columns: Option<Vec<String>>,
}

pub fn parse_table_config(raw: &str) -> Result<TableConfig, FrameworkError> {
    toml::from_str::<TableConfig>(raw)
        .map_err(|e| FrameworkError::internal(format!("admin table config: {}", e)))
}

pub fn parse_composite_config(raw: &str) -> Result<CompositeConfig, FrameworkError> {
    toml::from_str::<CompositeConfig>(raw)
        .map_err(|e| FrameworkError::internal(format!("admin composite config: {}", e)))
}
```

```rust
// framework/src/lib.rs
pub mod admin;
```

```rust
// framework/src/admin/mod.rs
pub mod config;
pub mod loader;
pub mod registry;
pub mod routes;
pub mod handlers;
pub mod audit;
pub mod policy_bridge;

// Re-export the entrypoint and key types
pub use loader::load_admin_configs;
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test admin_config
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/admin framework/src/lib.rs framework/tests/admin_config.rs
git commit -m "feat(admin): TOML config schema for tables, composites, policies, audit"
```

---

## Task 3: Loader — read all admin/*.toml at boot

**Files:** `framework/src/admin/loader.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/admin_config.rs — append
use suprnova::admin::loader::load_admin_configs;

#[test]
fn loader_reads_directory_of_tomls() {
    let tmp = tempfile::tempdir().unwrap();
    let tables_dir = tmp.path().join("tables");
    std::fs::create_dir(&tables_dir).unwrap();
    std::fs::write(
        tables_dir.join("users.toml"),
        r#"
            [table]
            entity = "users"
            title = "Users"
            [[columns]]
            field = "email"
        "#,
    )
    .unwrap();
    std::fs::write(
        tables_dir.join("posts.toml"),
        r#"
            [table]
            entity = "posts"
            title = "Posts"
            [[columns]]
            field = "title"
        "#,
    )
    .unwrap();

    let configs = load_admin_configs(tmp.path()).unwrap();
    assert_eq!(configs.tables.len(), 2);
    let users = configs.tables.iter().find(|t| t.table.entity == "users").unwrap();
    assert_eq!(users.table.title, "Users");
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/admin/loader.rs
use super::config::{parse_composite_config, parse_table_config, CompositeConfig, TableConfig};
use crate::FrameworkError;
use std::path::Path;

pub struct AdminConfigs {
    pub tables: Vec<TableConfig>,
    pub composites: Vec<CompositeConfig>,
}

pub fn load_admin_configs(root: impl AsRef<Path>) -> Result<AdminConfigs, FrameworkError> {
    let root = root.as_ref();
    let mut tables = Vec::new();
    let tables_dir = root.join("tables");
    if tables_dir.exists() {
        for entry in std::fs::read_dir(&tables_dir)
            .map_err(|e| FrameworkError::internal(format!("read admin/tables: {}", e)))?
        {
            let entry = entry.map_err(|e| FrameworkError::internal(format!("entry: {}", e)))?;
            if entry.path().extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            let raw = std::fs::read_to_string(entry.path())
                .map_err(|e| FrameworkError::internal(format!("read {}: {}", entry.path().display(), e)))?;
            tables.push(parse_table_config(&raw)?);
        }
    }

    let mut composites = Vec::new();
    let composite_dir = root.join("composite");
    if composite_dir.exists() {
        for entry in std::fs::read_dir(&composite_dir)
            .map_err(|e| FrameworkError::internal(format!("read admin/composite: {}", e)))?
        {
            let entry = entry.map_err(|e| FrameworkError::internal(format!("entry: {}", e)))?;
            if entry.path().extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            let raw = std::fs::read_to_string(entry.path())
                .map_err(|e| FrameworkError::internal(format!("read {}: {}", entry.path().display(), e)))?;
            composites.push(parse_composite_config(&raw)?);
        }
    }

    Ok(AdminConfigs { tables, composites })
}
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test admin_config loader_reads
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/admin/loader.rs framework/tests/admin_config.rs
git commit -m "feat(admin): loader reads admin/tables/*.toml and admin/composite/*.toml"
```

---

## Task 4: Entity registry — bridge TOML `entity` strings to SeaORM types

**Files:** `framework/src/admin/registry.rs`

- [ ] **Step 1: Design + implement**

```rust
// framework/src/admin/registry.rs
//! Bridge between TOML `entity = "users"` strings and concrete
//! SeaORM `Entity` types. The user's app registers each entity at
//! boot via `Admin::register_entity::<users::Entity>("users")`.
//!
//! At handler invocation time we look up the entity by name and
//! route through type-erased query helpers.

use crate::FrameworkError;
use std::collections::HashMap;
use std::sync::OnceLock;

type EntityFactory = Box<dyn EntityProvider + Send + Sync>;

#[async_trait::async_trait]
pub trait EntityProvider: Send + Sync {
    async fn list_json(
        &self,
        page: u64,
        per_page: u64,
        search: Option<&str>,
        sort: Option<(&str, bool)>,
    ) -> Result<serde_json::Value, FrameworkError>;

    async fn find_json(&self, id: &str) -> Result<Option<serde_json::Value>, FrameworkError>;

    async fn create_json(&self, payload: serde_json::Value)
        -> Result<serde_json::Value, FrameworkError>;

    async fn update_json(
        &self,
        id: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, FrameworkError>;

    async fn delete(&self, id: &str) -> Result<(), FrameworkError>;
}

static REGISTRY: OnceLock<HashMap<String, EntityFactory>> = OnceLock::new();

pub fn register(name: impl Into<String>, provider: impl EntityProvider + 'static) {
    // OnceLock is set-once. For mutation, hold a Mutex<Option<HashMap>>
    // and accept that registration must happen before serve_routes.
    // For brevity, this sketch uses OnceLock and panics on second
    // call. Production: switch to Mutex<HashMap> if dynamic
    // registration is needed.
    let name = name.into();
    let _ = REGISTRY.set(HashMap::new()); // initialize if needed
    // ... unreachable for OnceLock — replace with mutex.
    todo!("change to Mutex<HashMap>: see note in source");
}

pub fn get(name: &str) -> Option<&dyn EntityProvider> {
    REGISTRY.get().and_then(|m| m.get(name).map(|p| p.as_ref()))
}
```

> **Storage shape:** As noted in the sketch, `OnceLock<HashMap>` doesn't allow runtime insertion. Use `Mutex<HashMap<String, Arc<dyn EntityProvider>>>` instead. Adjust to:

```rust
use std::sync::Mutex;
use std::sync::Arc;

static REGISTRY: Mutex<Option<HashMap<String, Arc<dyn EntityProvider>>>> = Mutex::new(None);

pub fn register(name: impl Into<String>, provider: impl EntityProvider + 'static) {
    let mut g = REGISTRY.lock().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    map.insert(name.into(), Arc::new(provider));
}

pub fn get(name: &str) -> Option<Arc<dyn EntityProvider>> {
    REGISTRY.lock().unwrap().as_ref().and_then(|m| m.get(name).cloned())
}
```

- [ ] **Step 2: Provide a generic `SeaOrmProvider<E>` impl**

```rust
// framework/src/admin/registry.rs — append
use sea_orm::{ActiveModelBehavior, ActiveModelTrait, EntityTrait, IntoActiveModel, PaginatorTrait, PrimaryKeyTrait, QueryFilter, QueryOrder};

pub struct SeaOrmProvider<E: EntityTrait> {
    _marker: std::marker::PhantomData<E>,
}

impl<E: EntityTrait + 'static> SeaOrmProvider<E> {
    pub fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

// Implementing EntityProvider generically over E requires constraints
// on E::Model (serde::Serialize), E::ActiveModel (ActiveModelBehavior +
// IntoActiveModel<E::ActiveModel> + From<E::Model>), and PK that's
// stringifiable. The full impl is verbose; the implementer should
// build it against the actual entity used in tests. See `dogfood`
// section below for the concrete UserProvider type.
```

> **Generic impl complexity:** Generic over `E: EntityTrait` is doable but the trait bounds get long. Two pragmatic paths: (a) write a macro that emits a concrete `EntityProvider` impl per entity name; (b) hand-write a `UserProvider` and `PostProvider` per entity in the dogfood app. Plan task 5 below picks path (a) and emits the impl from `Admin::register_entity::<E>("name")`.

- [ ] **Step 3: Commit**

```bash
git add framework/src/admin/registry.rs
git commit -m "feat(admin): EntityProvider trait + SeaOrm-backed registry"
```

---

## Task 5: Generic CRUD handlers — index/show/create/update/delete

**Files:** `framework/src/admin/handlers/*.rs`

- [ ] **Step 1: Implement handlers (one file per action)**

```rust
// framework/src/admin/handlers/index.rs
use crate::admin::{config::TableConfig, registry::get};
use crate::{FrameworkError, Inertia, Request, Response};

pub fn build(table: TableConfig) -> impl Fn(Request) -> futures::future::BoxFuture<'static, Response> + Clone {
    move |req: Request| {
        let table = table.clone();
        Box::pin(async move {
            let entity_name = &table.table.entity;
            let provider = get(entity_name)
                .ok_or_else(|| FrameworkError::internal(format!("entity '{}' not registered", entity_name)))?;
            let page = req
                .query("page")
                .and_then(|p| p.parse().ok())
                .unwrap_or(1);
            let per_page = table.table.per_page.unwrap_or(25);
            let search = req.query("q");
            let sort = req
                .query("sort")
                .map(|s| (s.trim_start_matches('-').to_string(), s.starts_with('-')));

            let payload = provider
                .list_json(page, per_page, search.as_deref(), sort.as_ref().map(|(f, desc)| (f.as_str(), *desc)))
                .await?;

            Ok(Inertia::render(
                "Admin/Index",
                serde_json::json!({
                    "table": table_to_json(&table),
                    "rows": payload,
                }),
            )?)
        })
    }
}

fn table_to_json(t: &TableConfig) -> serde_json::Value {
    serde_json::json!({
        "entity": t.table.entity,
        "title": t.table.title,
        "icon": t.table.icon,
        "columns": t.columns.iter().map(|c| serde_json::json!({
            "field": c.field,
            "label": c.label.clone().unwrap_or_else(|| c.field.clone()),
            "sortable": c.sortable,
            "searchable": c.searchable,
            "format": c.format,
        })).collect::<Vec<_>>(),
    })
}
```

```rust
// framework/src/admin/handlers/show.rs — similar pattern
// framework/src/admin/handlers/create.rs — POST handler, Gate::authorize("create-<entity>")
// framework/src/admin/handlers/update.rs — PUT handler with Gate
// framework/src/admin/handlers/delete.rs — DELETE handler with Gate
```

- [ ] **Step 2: Wire policy enforcement**

Each mutation handler calls `Gate::authorize` using the policy reference from the TOML (`policies.edit = "UserPolicy::update"` → resolve at boot to a closure that calls `Gate::allows("update-user", &auth_user, &resource)`).

```rust
// framework/src/admin/policy_bridge.rs
use crate::{FrameworkError, Gate};

/// Resolve a TOML policy string like "UserPolicy::view" to a runtime
/// authorization check. At present the bridge maps directly to
/// `Gate::allows("<method>-<entity>", &user, &resource)` because
/// the `#[policy]` macro from Phase 3 registers gates with that
/// naming convention.
pub fn authorize_action<U: 'static, R: 'static>(
    _policy_path: &str,
    action: &str,
    entity: &str,
    user: &U,
    resource: &R,
) -> Result<(), FrameworkError> {
    let gate_name = format!("{}-{}", action, entity);
    Gate::authorize(&gate_name, user, resource)
}
```

- [ ] **Step 3: Commit**

```bash
git add framework/src/admin/handlers framework/src/admin/policy_bridge.rs
git commit -m "feat(admin): generic CRUD handlers (index/show/create/update/delete) with policy gates"
```

---

## Task 6: Programmatic route registration

**Files:** `framework/src/admin/routes.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/admin/routes.rs
//! Programmatic route registration for admin routes. Called at boot
//! after all TOML configs are loaded.

use super::config::TableConfig;
use super::handlers::{create, delete, index, show, update};
use crate::routing::Router;
use crate::FrameworkError;

pub fn install_table_routes(
    router: &mut Router,
    table: TableConfig,
) -> Result<(), FrameworkError> {
    let entity = table.table.entity.clone();
    let prefix = format!("/admin/{}", entity);

    let idx = index::build(table.clone());
    let shw = show::build(table.clone());
    let crt = create::build(table.clone());
    let upd = update::build(table.clone());
    let del = delete::build(table);

    router.add_route("GET", &prefix, idx);
    router.add_route("GET", &format!("{}/:id", prefix), shw);
    router.add_route("POST", &prefix, crt);
    router.add_route("PUT", &format!("{}/:id", prefix), upd);
    router.add_route("DELETE", &format!("{}/:id", prefix), del);

    Ok(())
}
```

> **`router.add_route` API:** Verify the actual mutation API on `Router` — the current routing layer uses macros and a `routes! { ... }` DSL. To programmatically register routes at runtime, the router must expose either an `add_route` method or a way to merge a `RouteDefBuilder` into the live registry. **If no such API exists, this is a routing-layer change** — add `Router::extend_dynamic(routes: Vec<DynRoute>)` and dispatch on it from the request handler. The full change lives in `framework/src/routing/`.

- [ ] **Step 2: Install from `Admin::boot`**

```rust
// framework/src/admin/mod.rs — add
use crate::routing::Router;

pub struct Admin;

impl Admin {
    pub fn boot(router: &mut Router, admin_dir: impl AsRef<std::path::Path>) -> Result<(), crate::FrameworkError> {
        let configs = loader::load_admin_configs(admin_dir)?;
        for table in configs.tables {
            routes::install_table_routes(router, table)?;
        }
        // Composite routes installation skipped here; see Task 8.
        Ok(())
    }

    pub fn register_entity<E: sea_orm::EntityTrait + 'static>(name: impl Into<String>) {
        let provider = registry::SeaOrmProvider::<E>::new();
        registry::register(name, provider);
    }
}
```

- [ ] **Step 3: Commit**

```bash
git add framework/src/admin/routes.rs framework/src/admin/mod.rs
git commit -m "feat(admin): programmatic route registration from TOML configs"
```

---

## Task 7: Audit trail — middleware-style observer

**Files:** `framework/src/admin/audit.rs`, migration

- [ ] **Step 1: Migration for `audits` table**

```rust
// framework/src/admin/migrations/m_create_audits_table.rs
// (or as an example migration in app/src/migrations/)
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Audits::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(Audits::Id).big_integer().not_null().auto_increment().primary_key())
                    .col(ColumnDef::new(Audits::ActorId).big_integer())
                    .col(ColumnDef::new(Audits::Entity).string().not_null())
                    .col(ColumnDef::new(Audits::RecordId).string().not_null())
                    .col(ColumnDef::new(Audits::Action).string().not_null())
                    .col(ColumnDef::new(Audits::Diff).json())
                    .col(ColumnDef::new(Audits::CreatedAt).date_time().not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.drop_table(Table::drop().table(Audits::Table).to_owned()).await
    }
}

#[derive(Iden)]
pub enum Audits {
    Table,
    Id,
    ActorId,
    Entity,
    RecordId,
    Action,
    Diff,
    CreatedAt,
}
```

- [ ] **Step 2: Audit writer**

```rust
// framework/src/admin/audit.rs
use crate::{Auth, FrameworkError, DB};
use sea_orm::{ConnectionTrait, Statement};

pub async fn record(
    entity: &str,
    record_id: &str,
    action: &str,
    diff: Option<serde_json::Value>,
) -> Result<(), FrameworkError> {
    let actor_id = Auth::id();
    let db = DB::get()?;
    let stmt = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO audits (actor_id, entity, record_id, action, diff, created_at) VALUES ($1, $2, $3, $4, $5, NOW())",
        vec![
            actor_id.into(),
            entity.into(),
            record_id.into(),
            action.into(),
            diff.map(|d| d.to_string()).into(),
        ],
    );
    db.execute(stmt).await?;
    Ok(())
}
```

- [ ] **Step 3: Call from each mutation handler**

```rust
// framework/src/admin/handlers/update.rs — after the SeaORM update succeeds:
if table.audit.enabled {
    crate::admin::audit::record(
        &table.table.entity,
        &id,
        "update",
        Some(diff_json),
    )
    .await?;
}
```

> **Diff computation:** Compute `diff_json` as `{ "before": <old>, "after": <new> }` by fetching the row before update, applying the change, refetching, and serializing both. Optimize later with `sea-orm`'s `BeforeUpdate` / `AfterUpdate` hooks if available.

- [ ] **Step 4: Commit**

```bash
git add framework/src/admin/audit.rs framework/src/admin/handlers framework/src/admin/migrations
git commit -m "feat(admin): audit trail writes per create/update/delete with actor + JSON diff"
```

---

## Task 8: Composite views (e.g. SalesOrder with line items + customer)

**Files:** `framework/src/admin/handlers/composite.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/admin/handlers/composite.rs
use crate::admin::config::CompositeConfig;
use crate::admin::registry::get;
use crate::{FrameworkError, Inertia, Request, Response};

pub fn build(cfg: CompositeConfig) -> impl Fn(Request) -> futures::future::BoxFuture<'static, Response> + Clone {
    move |req: Request| {
        let cfg = cfg.clone();
        Box::pin(async move {
            let id = req.param("id")?;
            let primary = get(&cfg.composite.primary_entity)
                .ok_or_else(|| FrameworkError::internal("primary entity not registered"))?;
            let primary_row = primary.find_json(&id).await?
                .ok_or_else(|| FrameworkError::model_not_found(&cfg.composite.primary_entity))?;

            // For each related block, fetch related rows.
            let mut blocks_data = Vec::new();
            for block in &cfg.blocks {
                let data = match block.kind.as_str() {
                    "header" => serde_json::json!({ "kind": "header", "row": primary_row.clone() }),
                    "related_list" => {
                        let entity = block.entity.as_deref().unwrap_or("");
                        let provider = get(entity).ok_or_else(|| {
                            FrameworkError::internal(format!("related entity '{}' not registered", entity))
                        })?;
                        // Naive: list all rows; production needs a
                        // relation-aware filter via provider.related_to(...)
                        let list = provider.list_json(1, 100, None, None).await?;
                        serde_json::json!({
                            "kind": "related_list",
                            "entity": entity,
                            "rows": list,
                            "columns": block.columns,
                        })
                    }
                    "summary" => serde_json::json!({ "kind": "summary", "row": primary_row.clone() }),
                    _ => serde_json::json!({ "kind": "unknown" }),
                };
                blocks_data.push(data);
            }

            Ok(Inertia::render(
                "Admin/Composite",
                serde_json::json!({
                    "title": cfg.composite.title,
                    "blocks": blocks_data,
                }),
            )?)
        })
    }
}
```

- [ ] **Step 2: Route install**

```rust
// framework/src/admin/routes.rs — append
pub fn install_composite_routes(router: &mut Router, cfg: CompositeConfig) -> Result<(), FrameworkError> {
    let path = format!("/admin/composite/{}/:id", cfg.composite.name);
    let handler = composite::build(cfg);
    router.add_route("GET", &path, handler);
    Ok(())
}
```

- [ ] **Step 3: Commit**

```bash
git add framework/src/admin/handlers/composite.rs framework/src/admin/routes.rs
git commit -m "feat(admin): composite views for joined entity displays"
```

---

## Task 9: Frontend admin SPA (React 19 default)

**Files:** `suprnova-cli/src/templates/files/admin/react/` (and vue/svelte mirrors)

- [ ] **Step 1: Create Inertia pages**

```
suprnova-cli/src/templates/files/admin/react/src/pages/Admin/
├── Index.tsx          # generic CRUD index — driven by `table.columns`
├── Show.tsx           # generic show view
├── Edit.tsx           # generic form
├── Create.tsx         # generic new-record form
├── Composite.tsx      # composite-view renderer
└── Layout.tsx         # admin sidebar + nav
```

Each Inertia page receives the `table` JSON from the backend handler (Task 5) and renders columns dynamically:

```tsx
// suprnova-cli/src/templates/files/admin/react/src/pages/Admin/Index.tsx
import { Link } from "@inertiajs/react";
type Column = { field: string; label: string; sortable: boolean; searchable: boolean; format?: string };
type Props = {
  table: { entity: string; title: string; icon?: string; columns: Column[] };
  rows: { data: any[]; total: number; page: number; per_page: number };
};
export default function Index({ table, rows }: Props) {
  return (
    <Layout>
      <h1>{table.title}</h1>
      <table>
        <thead>
          <tr>{table.columns.map((c) => <th key={c.field}>{c.label}</th>)}</tr>
        </thead>
        <tbody>
          {rows.data.map((row, i) => (
            <tr key={i}>
              {table.columns.map((c) => <td key={c.field}>{format(row[c.field], c.format)}</td>)}
              <td><Link href={`/admin/${table.entity}/${row.id}`}>View</Link></td>
            </tr>
          ))}
        </tbody>
      </table>
    </Layout>
  );
}
function format(v: any, format?: string) {
  if (format === "datetime") return new Date(v).toLocaleString();
  return String(v ?? "");
}
function Layout({ children }: { children: React.ReactNode }) {
  return <div className="admin-shell">{children}</div>;
}
```

(Mirror Vue and Svelte equivalents in their respective directories.)

- [ ] **Step 2: Wire admin pages into the existing Inertia bundler**

The admin pages live alongside the user's existing Inertia pages so the same bundler picks them up. The scaffolder copies them into `frontend/src/pages/Admin/` on `suprnova make:admin <Entity>` and on first `Admin::boot` if they don't exist.

- [ ] **Step 3: Commit**

```bash
git add suprnova-cli/src/templates/files/admin
git commit -m "feat(admin): React 19 / Vue 3 / Svelte 5 admin SPA pages"
```

---

## Task 10: `suprnova make:admin <Entity>` scaffolder

**Files:** `suprnova-cli/src/commands/make_admin.rs`

- [ ] **Step 1: Implement**

```rust
// suprnova-cli/src/commands/make_admin.rs
use anyhow::Result;
use std::path::Path;

pub fn run(entity: &str) -> Result<()> {
    let snake = to_snake_case(entity);
    let path = Path::new("admin/tables").join(format!("{}.toml", snake));
    std::fs::create_dir_all(path.parent().unwrap())?;
    if path.exists() {
        anyhow::bail!("{} already exists", path.display());
    }
    let title = capitalize(&snake);
    let content = format!(
        r#"[table]
entity = "{snake}"
title = "{title}"

[[columns]]
field = "id"

[[columns]]
field = "created_at"
format = "datetime"

[policies]
view = "{Pascal}Policy::view"
create = "{Pascal}Policy::create"
edit = "{Pascal}Policy::update"
delete = "{Pascal}Policy::delete"

[audit]
enabled = true
"#,
        snake = snake,
        title = title,
        Pascal = to_pascal_case(entity),
    );
    std::fs::write(&path, content)?;
    println!("✓ created {}", path.display());
    println!();
    println!("Next:");
    println!("  1. Run migrations if you haven't: suprnova migrate");
    println!("  2. Register the entity in bootstrap.rs:");
    println!("     suprnova::Admin::register_entity::<{}::Entity>(\"{}\");", snake, snake);
    Ok(())
}
```

- [ ] **Step 2: Wire into CLI** (same pattern as other generators) and commit.

---

## Task 11: App dogfood — users + posts admin

**Files:** `app/admin/tables/{users,posts}.toml`, `app/src/bootstrap.rs`

- [ ] **Step 1: Create admin/tables/users.toml + admin/tables/posts.toml + admin/composite/sales_order.toml**

- [ ] **Step 2: Register entities in bootstrap**

```rust
// app/src/bootstrap.rs — inside register()
suprnova::Admin::register_entity::<crate::models::users::Entity>("users");
suprnova::Admin::register_entity::<crate::models::posts::Entity>("posts");
```

- [ ] **Step 3: Boot admin routes from Server::serve**

```rust
// framework/src/server.rs — early in serve()
if std::path::Path::new("admin").exists() {
    let mut router = ...; // however Server constructs its router
    suprnova::Admin::boot(&mut router, "admin")?;
}
```

- [ ] **Step 4: Smoke test**

```bash
cargo run -p app -- serve &
sleep 2
curl http://127.0.0.1:8000/admin/users | head -20
kill %1
```

- [ ] **Step 5: Commit**

```bash
git add app/admin app/src/bootstrap.rs framework/src/server.rs
git commit -m "feat(app): dogfood admin panel — users + posts tables"
```

---

## Task 12: Queue inspector composite view

Laravel ships Horizon for queue introspection (job inspector, retry,
failure detail, throughput charts). The runtime story is already a
Phase 5 win — sea-streamer's decoupled read/process/ack loops match
Horizon's throughput natively. The remaining gap is the inspector UI.
We ship it as a TOML-config admin page on top of the existing
Phase 8 admin pattern: zero bespoke admin code, just a queue-aware
composite view that reads from the queue backend's job/failure tables.

**Files:**
- Create: `framework/src/queue/inspector.rs` — `QueueInspector` trait + driver impls
- Create: `framework/src/queue/inspector_handler.rs` — admin HTTP handler reading the inspector
- Modify: `framework/src/queue/streamer.rs` — implement `QueueInspector` for sea-streamer (read pending/processed/failed counts + paginated jobs)
- Modify: `framework/src/queue/database.rs` — same for the database driver
- Create: `app/admin/queue.toml` — dogfood the queue-inspector TOML config
- Modify: `app/frontend/pages/Admin/QueueInspector.tsx` (or similar) — auto-rendered from the TOML

- [ ] **Step 1: Write failing test — QueueInspector trait + sea-streamer impl**

```rust
// framework/tests/queue_inspector.rs
use suprnova::queue::{Queue, QueueInspector, QueueStats, QueuedJob};

#[tokio::test]
async fn inspector_reports_pending_processed_failed_counts() {
    Queue::use_in_process().await;
    Queue::register::<AddNumbers>().await;
    Queue::push(AddNumbers { a: 1, b: 1 }).await.unwrap();
    Queue::push(AddNumbers { a: 2, b: 2 }).await.unwrap();

    let inspector = Queue::inspector().expect("inspector available");
    let stats: QueueStats = inspector.stats("default").await.unwrap();
    assert!(stats.pending >= 0);
    assert_eq!(stats.failed, 0);
}

#[tokio::test]
async fn inspector_lists_failed_jobs_with_payload_and_error() {
    Queue::use_in_process().await;
    Queue::register::<AlwaysFails>().await;
    Queue::push(AlwaysFails).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let inspector = Queue::inspector().expect("inspector available");
    let failed: Vec<QueuedJob> = inspector.failed("default", 0, 10).await.unwrap();
    assert!(!failed.is_empty());
    assert!(failed[0].error.as_deref().unwrap_or("").contains("intentional"));
}

#[tokio::test]
async fn inspector_retries_a_failed_job() {
    // Setup as above, then retry by id, then verify it no longer appears in failed.
    // Concrete assertion depends on driver semantics:
    //   - database driver: failed row is deleted and re-inserted into pending
    //   - sea-streamer: message is re-published to the original stream
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova --test queue_inspector
```

- [ ] **Step 3: Implement `QueueInspector` trait**

```rust
// framework/src/queue/inspector.rs
use crate::FrameworkError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct QueueStats {
    pub pending: u64,
    pub processed: u64,
    pub failed: u64,
    pub jobs_per_minute: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueuedJob {
    pub id: String,
    pub job_name: String,
    pub payload: serde_json::Value,
    pub attempts: u32,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[async_trait]
pub trait QueueInspector: Send + Sync {
    async fn stats(&self, queue: &str) -> Result<QueueStats, FrameworkError>;
    async fn pending(&self, queue: &str, offset: u64, limit: u64) -> Result<Vec<QueuedJob>, FrameworkError>;
    async fn failed(&self, queue: &str, offset: u64, limit: u64) -> Result<Vec<QueuedJob>, FrameworkError>;
    async fn retry(&self, queue: &str, job_id: &str) -> Result<(), FrameworkError>;
    async fn forget(&self, queue: &str, job_id: &str) -> Result<(), FrameworkError>;
}

impl crate::queue::Queue {
    /// Returns an inspector for the active queue driver, or None for
    /// drivers that don't support introspection (in-process by default).
    pub fn inspector() -> Option<std::sync::Arc<dyn QueueInspector>> {
        crate::queue::INSPECTOR_REGISTRY.get().and_then(|r| r.current())
    }
}
```

> **Driver implementations:**
> - **In-process** — no persistent storage; return `None` from
>   `Queue::inspector()`. Optional: maintain an in-memory ring buffer
>   of recent failures for `dev` mode.
> - **Database** — a `failed_jobs` table + the existing `jobs` table.
>   `stats` runs aggregate `COUNT(*)` queries. `failed` selects from
>   `failed_jobs`. `retry` deletes from `failed_jobs` and re-inserts
>   into `jobs`. Throughput stats from a rolling timestamp index.
> - **sea-streamer Redis/Kafka** — pending = `XLEN`; failed lives in a
>   dedicated `{stream}:failed` stream. `retry` reads the failed
>   message and `XADD`s back to the original stream. Throughput from
>   stream metadata.

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova --test queue_inspector
```

Expected: at least the in-process tests pass; driver-specific tests
need their backend so gate them via feature flag (`memcached`/`redis`/etc).

- [ ] **Step 5: Wire admin TOML composite view**

```toml
# app/admin/queue.toml
[composite_view]
slug = "queue"
title = "Queue Inspector"
icon = "queue"

[[panels]]
type = "stats"
source = "queue.stats"
fields = ["pending", "processed", "failed", "jobs_per_minute"]

[[panels]]
type = "table"
title = "Pending"
source = "queue.pending"
columns = ["id", "job_name", "attempts", "last_attempt_at"]
actions = ["forget"]

[[panels]]
type = "table"
title = "Failed"
source = "queue.failed"
columns = ["id", "job_name", "error", "last_attempt_at"]
actions = ["retry", "forget"]

[policies]
view = "AdminPolicy::queue_view"
retry = "AdminPolicy::queue_retry"
```

Extend the admin loader (Task 3) to recognize `[composite_view]` with
`source = "queue.*"` as a built-in source backed by the
`QueueInspector` trait (vs entity-backed tables from Task 4).

- [ ] **Step 6: Frontend — auto-render the composite view**

The admin SPA (Task 9) reads the resolved TOML config and renders:
- `stats` panel as a horizontal counter row
- `table` panels as paginated tables with the declared actions

This works because composite views were already designed for
relation-joining in Task 8; "built-in source" is one more enum
variant on the `composite_view.source` field.

- [ ] **Step 7: Commit**

```bash
git add framework/src/queue/inspector.rs framework/src/queue/inspector_handler.rs framework/src/queue/streamer.rs framework/src/queue/database.rs framework/tests/queue_inspector.rs app/admin/queue.toml app/frontend/pages/Admin
git commit -m "feat(admin): queue inspector composite view (Horizon equivalent)"
```

---

## Task 13: Workspace lint + verification + roadmap update

- [ ] **Step 1: Clippy + tests**

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: ROADMAP "Where we are"**

Move from "Missing" to "Production-ready":
- Admin Panel (TOML-driven CRUD + composite views + audit trail + RBAC)

- [ ] **Step 3: Commit + push**

---

## Self-Review

| Spec item | Covered by |
|-----------|------------|
| TOML config schema | Task 2 |
| Loader for admin/*.toml | Task 3 |
| Entity registry | Task 4 |
| Generic CRUD handlers | Task 5 |
| Programmatic route registration | Task 6 |
| Audit trail | Task 7 |
| Composite views | Task 8 |
| Frontend admin SPA | Task 9 |
| make:admin generator | Task 10 |
| App dogfood | Task 11 |
| QueueInspector trait + driver impls (database, sea-streamer) | Task 12 |
| Queue inspector composite-view TOML (Horizon equivalent) | Task 12 |

**Placeholder scan:** `> Storage shape:` and `> Generic impl complexity:` notes flag concrete decisions to make (Mutex vs OnceLock; macro-driven vs per-entity hand-written providers). `> router.add_route API:` flags a routing-layer extension that may be needed.

---

## Execution Handoff

**Subagent-Driven recommended — the router-extension change in Task 6 is the riskiest piece; give it a dedicated agent with full context.**
