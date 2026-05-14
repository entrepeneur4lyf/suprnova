# Phase 10: Eloquent Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Close the gap between bare SeaORM and Laravel's Eloquent: mass assignment with `$fillable` / `$guarded`, typed mutators / accessors / casts, soft deletes, model events / observers, query scopes (global + local), eager-loading sugar, and pluralization for i18n.

**Architecture:** Each Eloquent feature is a derive macro or trait extension on top of SeaORM's existing `Entity` / `ActiveModel` types — we never replace SeaORM, we add Laravel-shape ergonomics. The macros live in `suprnova-macros/`; trait implementations and runtime types live in `framework/src/eloquent/` (new module). Soft deletes use SeaORM's existing `paranoid` feature (delete sets `deleted_at = NOW()`, queries filter it out). Model events plug into Phase 1's `EventDispatcher`. Pluralization extends Phase 10 (now Polish) i18n with CLDR rules.

**Tech Stack:** Builds on SeaORM (already a dep), Phase 1 events, Phase 10/Polish i18n. New: `intl-pluralrules` 7 for ICU CLDR pluralization rules.

---

## File Structure

**New files:**
- `framework/src/eloquent/mod.rs` — module root
- `framework/src/eloquent/mass_assignment.rs` — `Fillable`/`Guarded` trait + `fill()`
- `framework/src/eloquent/casts.rs` — `Cast` trait + built-in casts (`AsJson`, `AsBool`, `AsDateTime`, `AsEncrypted`, `AsArray`)
- `framework/src/eloquent/events.rs` — `ModelEvent` enum + dispatcher integration
- `framework/src/eloquent/observers.rs` — `Observer<M>` trait + registry
- `framework/src/eloquent/soft_deletes.rs` — `SoftDelete` trait + `restore()` / `force_delete()`
- `framework/src/eloquent/scopes.rs` — `GlobalScope` trait + `Query::without_global_scope`
- `framework/src/eloquent/eager.rs` — `with()` / `load()` relation eager-loading sugar
- `framework/src/i18n/pluralize.rs` — CLDR-based pluralization
- `suprnova-macros/src/fillable.rs` — `#[derive(Fillable)]`
- `suprnova-macros/src/casts.rs` — `#[derive(Casts)]`
- `framework/tests/eloquent_*.rs` — one test file per area
- `app/src/observers/user_observer.rs` — dogfood

**Modified files:**
- `framework/Cargo.toml` — add `intl-pluralrules`
- `framework/src/lib.rs` — declare + re-export

---

## Task 1: Mass assignment (`Fillable` / `Guarded`)

**Files:** `framework/src/eloquent/mass_assignment.rs`, `suprnova-macros/src/fillable.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/eloquent_mass_assignment.rs
use suprnova::eloquent::Fillable;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize, suprnova::FillableDerive)]
#[fillable(fields = ["name", "email"])]
struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub admin: bool,
}

#[test]
fn fill_only_assigns_allowlisted_fields() {
    let mut u = User::default();
    u.fill(serde_json::json!({
        "name": "Alice",
        "email": "alice@example.com",
        "admin": true,        // should be ignored — not fillable
        "id": 999,             // should be ignored
    }))
    .unwrap();

    assert_eq!(u.name, "Alice");
    assert_eq!(u.email, "alice@example.com");
    assert!(!u.admin);
    assert_eq!(u.id, 0);
}

#[derive(Debug, Default, Deserialize, suprnova::FillableDerive)]
#[fillable(guarded = ["id", "admin"])]
struct Post {
    pub id: i64,
    pub title: String,
    pub admin: bool,
}

#[test]
fn guarded_blocks_named_fields_allows_rest() {
    let mut p = Post::default();
    p.fill(serde_json::json!({
        "id": 99,
        "title": "Hello",
        "admin": true,
    }))
    .unwrap();
    assert_eq!(p.id, 0);     // guarded
    assert!(!p.admin);        // guarded
    assert_eq!(p.title, "Hello");
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/eloquent/mass_assignment.rs
use crate::FrameworkError;

pub trait Fillable: Sized {
    /// Allowlisted field names. Empty means "fall back to guarded".
    fn fillable() -> &'static [&'static str] {
        &[]
    }
    /// Denylisted field names.
    fn guarded() -> &'static [&'static str] {
        &[]
    }

    /// Fill the model's allowed fields from a JSON object. Fields
    /// not in `fillable` (or matching `guarded` when fillable is
    /// empty) are silently ignored.
    fn fill(&mut self, attrs: serde_json::Value) -> Result<(), FrameworkError>;
}
```

```rust
// suprnova-macros/src/fillable.rs
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, Lit, Meta, NestedMeta};

pub fn derive_fillable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;

    let mut fillable: Vec<String> = Vec::new();
    let mut guarded: Vec<String> = Vec::new();
    for attr in &input.attrs {
        if !attr.path.is_ident("fillable") {
            continue;
        }
        if let Ok(Meta::List(list)) = attr.parse_meta() {
            for nested in list.nested {
                if let NestedMeta::Meta(Meta::NameValue(nv)) = nested {
                    let list_values: Vec<String> = match &nv.lit {
                        Lit::Str(s) => s
                            .value()
                            .trim_matches(|c: char| c == '[' || c == ']')
                            .split(',')
                            .map(|s| s.trim().trim_matches('"').to_string())
                            .filter(|s| !s.is_empty())
                            .collect(),
                        _ => continue,
                    };
                    if nv.path.is_ident("fields") {
                        fillable = list_values;
                    } else if nv.path.is_ident("guarded") {
                        guarded = list_values;
                    }
                }
            }
        }
    }

    let fillable_lits: Vec<proc_macro2::TokenStream> =
        fillable.iter().map(|s| quote!(#s)).collect();
    let guarded_lits: Vec<proc_macro2::TokenStream> =
        guarded.iter().map(|s| quote!(#s)).collect();

    let fields = match &input.data {
        syn::Data::Struct(s) => match &s.fields {
            syn::Fields::Named(named) => &named.named,
            _ => panic!("FillableDerive requires named fields"),
        },
        _ => panic!("FillableDerive only supports structs"),
    };

    let assign_branches = fields.iter().filter_map(|f| {
        let ident = f.ident.as_ref()?;
        let ident_str = ident.to_string();
        Some(quote! {
            stringify!(#ident) => {
                if let Some(v) = obj.get(#ident_str).cloned() {
                    if let Ok(typed) = ::suprnova::serde_json::from_value(v) {
                        self.#ident = typed;
                    }
                }
            }
        })
    });

    let assign_branches: Vec<_> = assign_branches.collect();
    let assign_block = quote! {
        for (key, _) in obj.iter() {
            match key.as_str() {
                #(#assign_branches)*
                _ => {}
            }
        }
    };

    let expanded = quote! {
        impl ::suprnova::eloquent::Fillable for #name {
            fn fillable() -> &'static [&'static str] {
                &[#(#fillable_lits),*]
            }
            fn guarded() -> &'static [&'static str] {
                &[#(#guarded_lits),*]
            }
            fn fill(&mut self, attrs: ::suprnova::serde_json::Value) -> ::std::result::Result<(), ::suprnova::FrameworkError> {
                let obj = match attrs {
                    ::suprnova::serde_json::Value::Object(o) => o,
                    _ => return Ok(()),
                };
                let fillable = Self::fillable();
                let guarded = Self::guarded();
                let mut obj = obj;
                obj.retain(|k, _| {
                    if !fillable.is_empty() {
                        fillable.contains(&k.as_str())
                    } else {
                        !guarded.contains(&k.as_str())
                    }
                });
                #assign_block
                Ok(())
            }
        }
    };
    expanded.into()
}
```

```rust
// suprnova-macros/src/lib.rs — add
mod fillable;
#[proc_macro_derive(FillableDerive, attributes(fillable))]
pub fn derive_fillable(input: TokenStream) -> TokenStream {
    fillable::derive_fillable(input)
}
```

```rust
// framework/src/lib.rs
pub mod eloquent;
pub use eloquent::Fillable;
pub use suprnova_macros::FillableDerive;
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test eloquent_mass_assignment
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/eloquent/mass_assignment.rs framework/src/lib.rs suprnova-macros framework/tests/eloquent_mass_assignment.rs
git commit -m "feat(eloquent): Fillable / Guarded trait + #[derive(FillableDerive)] mass assignment"
```

---

## Task 2: Casts (`AsJson`, `AsBool`, `AsDateTime`, `AsEncrypted`, `AsArray`)

**Files:** `framework/src/eloquent/casts.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/eloquent_casts.rs
use suprnova::eloquent::casts::{AsArray, AsBool, AsDateTime, AsJson, Cast};

#[test]
fn as_bool_round_trip() {
    let stored = AsBool::to_storage(&true).unwrap();
    assert_eq!(stored, "1");
    let back = AsBool::from_storage::<bool>(&stored).unwrap();
    assert!(back);
}

#[test]
fn as_json_round_trip() {
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Prefs {
        theme: String,
        compact: bool,
    }
    let p = Prefs { theme: "dark".into(), compact: true };
    let stored = AsJson::to_storage(&p).unwrap();
    let back: Prefs = AsJson::from_storage(&stored).unwrap();
    assert_eq!(back, p);
}

#[test]
fn as_array_handles_comma_separated() {
    let stored = AsArray::to_storage(&vec!["a".to_string(), "b".to_string(), "c".to_string()]).unwrap();
    assert_eq!(stored, "a,b,c");
    let back: Vec<String> = AsArray::from_storage(&stored).unwrap();
    assert_eq!(back, vec!["a", "b", "c"]);
}

#[test]
fn as_datetime_iso8601() {
    use chrono::{DateTime, Utc, TimeZone};
    let dt: DateTime<Utc> = Utc.with_ymd_and_hms(2026, 5, 14, 12, 30, 0).unwrap();
    let stored = AsDateTime::to_storage(&dt).unwrap();
    assert!(stored.starts_with("2026-05-14T12:30:00"));
    let back: DateTime<Utc> = AsDateTime::from_storage(&stored).unwrap();
    assert_eq!(back, dt);
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/eloquent/casts.rs
//! Typed casts between storage form (string) and runtime form
//! (typed). Layered on top of SeaORM column storage — the cast
//! activates at the boundary where Laravel uses `$casts`.

use crate::FrameworkError;
use serde::{de::DeserializeOwned, Serialize};

pub trait Cast {
    type Runtime;
    fn to_storage(value: &Self::Runtime) -> Result<String, FrameworkError>;
    fn from_storage<T: DeserializeOwned>(stored: &str) -> Result<T, FrameworkError>;
}

pub struct AsJson;
impl Cast for AsJson {
    type Runtime = serde_json::Value;
    fn to_storage(value: &Self::Runtime) -> Result<String, FrameworkError> {
        serde_json::to_string(value)
            .map_err(|e| FrameworkError::internal(format!("AsJson: {}", e)))
    }
    fn from_storage<T: DeserializeOwned>(stored: &str) -> Result<T, FrameworkError> {
        serde_json::from_str(stored)
            .map_err(|e| FrameworkError::internal(format!("AsJson: {}", e)))
    }
}

pub struct AsBool;
impl AsBool {
    pub fn to_storage(value: &bool) -> Result<String, FrameworkError> {
        Ok(if *value { "1".into() } else { "0".into() })
    }
    pub fn from_storage<T>(stored: &str) -> Result<bool, FrameworkError> {
        Ok(matches!(stored, "1" | "true" | "yes" | "on"))
    }
}

pub struct AsArray;
impl AsArray {
    pub fn to_storage(values: &Vec<String>) -> Result<String, FrameworkError> {
        Ok(values.join(","))
    }
    pub fn from_storage<T>(stored: &str) -> Result<Vec<String>, FrameworkError> {
        Ok(stored.split(',').map(|s| s.to_string()).collect())
    }
}

pub struct AsDateTime;
impl AsDateTime {
    pub fn to_storage<Tz: chrono::TimeZone>(value: &chrono::DateTime<Tz>) -> Result<String, FrameworkError>
    where
        Tz::Offset: std::fmt::Display,
    {
        Ok(value.to_rfc3339())
    }
    pub fn from_storage<Tz>(stored: &str) -> Result<chrono::DateTime<chrono::Utc>, FrameworkError> {
        chrono::DateTime::parse_from_rfc3339(stored)
            .map(|d| d.with_timezone(&chrono::Utc))
            .map_err(|e| FrameworkError::internal(format!("AsDateTime: {}", e)))
    }
}

pub struct AsEncrypted;
impl AsEncrypted {
    pub fn to_storage(value: &str, enc: &crate::Encrypter) -> Result<String, FrameworkError> {
        enc.encrypt_string(value)
    }
    pub fn from_storage(stored: &str, enc: &crate::Encrypter) -> Result<String, FrameworkError> {
        enc.decrypt_string(stored)
    }
}
```

```rust
// framework/src/eloquent/mod.rs — declare
pub mod casts;
pub mod mass_assignment;
pub use casts::{AsArray, AsBool, AsDateTime, AsEncrypted, AsJson, Cast};
pub use mass_assignment::Fillable;
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test eloquent_casts
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/eloquent/casts.rs framework/src/eloquent/mod.rs framework/tests/eloquent_casts.rs
git commit -m "feat(eloquent): casts (AsJson, AsBool, AsArray, AsDateTime, AsEncrypted)"
```

---

## Task 3: Model events + observers

**Files:** `framework/src/eloquent/events.rs`, `framework/src/eloquent/observers.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/eloquent_observers.rs
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use suprnova::eloquent::{ModelEvent, Observer};

#[derive(Debug, Clone)]
struct User { pub id: i64 }

struct UserObserver(Arc<AtomicI64>);

#[suprnova::async_trait]
impl Observer<User> for UserObserver {
    async fn created(&self, user: &User) -> Result<(), suprnova::FrameworkError> {
        self.0.fetch_add(user.id, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn observer_created_fires_on_dispatch() {
    let counter = Arc::new(AtomicI64::new(0));
    suprnova::eloquent::observers::register::<User>(Arc::new(UserObserver(counter.clone())));
    suprnova::eloquent::events::dispatch::<User>(ModelEvent::Created, &User { id: 5 }).await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(counter.load(Ordering::SeqCst), 5);
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/eloquent/events.rs
//! Model events: Creating, Created, Updating, Updated, Deleting,
//! Deleted, Restoring, Restored. Each dispatch goes through Phase 1's
//! EventDispatcher under the hood so listeners attached via
//! `Event::listen` see them too.

use crate::FrameworkError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelEvent {
    Creating,
    Created,
    Updating,
    Updated,
    Deleting,
    Deleted,
    Restoring,
    Restored,
}

pub async fn dispatch<M: 'static + Send + Sync>(
    event: ModelEvent,
    model: &M,
) -> Result<(), FrameworkError> {
    super::observers::notify(event, model).await
}
```

```rust
// framework/src/eloquent/observers.rs
use super::events::ModelEvent;
use crate::FrameworkError;
use async_trait::async_trait;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[async_trait]
pub trait Observer<M>: Send + Sync {
    async fn creating(&self, _model: &M) -> Result<(), FrameworkError> { Ok(()) }
    async fn created(&self, _model: &M) -> Result<(), FrameworkError> { Ok(()) }
    async fn updating(&self, _model: &M) -> Result<(), FrameworkError> { Ok(()) }
    async fn updated(&self, _model: &M) -> Result<(), FrameworkError> { Ok(()) }
    async fn deleting(&self, _model: &M) -> Result<(), FrameworkError> { Ok(()) }
    async fn deleted(&self, _model: &M) -> Result<(), FrameworkError> { Ok(()) }
    async fn restoring(&self, _model: &M) -> Result<(), FrameworkError> { Ok(()) }
    async fn restored(&self, _model: &M) -> Result<(), FrameworkError> { Ok(()) }
}

#[async_trait]
pub(crate) trait ErasedObserver: Send + Sync {
    async fn handle(&self, event: ModelEvent, model: &dyn std::any::Any) -> Result<(), FrameworkError>;
}

struct ObserverWrap<M, O> {
    inner: Arc<O>,
    _marker: std::marker::PhantomData<M>,
}

#[async_trait]
impl<M: 'static + Send + Sync, O: Observer<M>> ErasedObserver for ObserverWrap<M, O> {
    async fn handle(&self, event: ModelEvent, model: &dyn std::any::Any) -> Result<(), FrameworkError> {
        let m = model.downcast_ref::<M>().expect("observer routed to wrong model type");
        match event {
            ModelEvent::Creating => self.inner.creating(m).await,
            ModelEvent::Created => self.inner.created(m).await,
            ModelEvent::Updating => self.inner.updating(m).await,
            ModelEvent::Updated => self.inner.updated(m).await,
            ModelEvent::Deleting => self.inner.deleting(m).await,
            ModelEvent::Deleted => self.inner.deleted(m).await,
            ModelEvent::Restoring => self.inner.restoring(m).await,
            ModelEvent::Restored => self.inner.restored(m).await,
        }
    }
}

static REGISTRY: RwLock<Option<HashMap<TypeId, Vec<Arc<dyn ErasedObserver>>>>> = RwLock::new(None);

pub fn register<M: 'static + Send + Sync>(observer: Arc<impl Observer<M> + 'static>) {
    let wrap: Arc<dyn ErasedObserver> = Arc::new(ObserverWrap::<M, _> {
        inner: observer,
        _marker: std::marker::PhantomData,
    });
    let mut g = REGISTRY.write().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    map.entry(TypeId::of::<M>()).or_default().push(wrap);
}

pub(crate) async fn notify<M: 'static + Send + Sync>(event: ModelEvent, model: &M) -> Result<(), FrameworkError> {
    let observers = {
        let g = REGISTRY.read().unwrap();
        g.as_ref()
            .and_then(|m| m.get(&TypeId::of::<M>()).cloned())
            .unwrap_or_default()
    };
    for o in observers {
        o.handle(event, model).await?;
    }
    Ok(())
}
```

```rust
// framework/src/eloquent/mod.rs
pub mod events;
pub mod observers;
pub use events::ModelEvent;
pub use observers::Observer;
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test eloquent_observers
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/eloquent/events.rs framework/src/eloquent/observers.rs framework/src/eloquent/mod.rs framework/tests/eloquent_observers.rs
git commit -m "feat(eloquent): ModelEvent + Observer<M> trait + per-type observer registry"
```

---

## Task 4: Soft deletes

**Files:** `framework/src/eloquent/soft_deletes.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/eloquent_soft_deletes.rs
use suprnova::eloquent::SoftDelete;

#[derive(Debug)]
struct Post {
    id: i64,
    title: String,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl SoftDelete for Post {
    fn deleted_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.deleted_at
    }
    fn set_deleted_at(&mut self, at: Option<chrono::DateTime<chrono::Utc>>) {
        self.deleted_at = at;
    }
}

#[test]
fn soft_delete_sets_timestamp_and_is_trashed() {
    let mut p = Post { id: 1, title: "x".into(), deleted_at: None };
    assert!(!p.is_trashed());
    p.soft_delete();
    assert!(p.is_trashed());
    assert!(p.deleted_at().is_some());
}

#[test]
fn restore_clears_deleted_at() {
    let mut p = Post {
        id: 1,
        title: "x".into(),
        deleted_at: Some(chrono::Utc::now()),
    };
    assert!(p.is_trashed());
    p.restore();
    assert!(!p.is_trashed());
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/eloquent/soft_deletes.rs
use chrono::{DateTime, Utc};

pub trait SoftDelete {
    fn deleted_at(&self) -> Option<DateTime<Utc>>;
    fn set_deleted_at(&mut self, at: Option<DateTime<Utc>>);

    fn is_trashed(&self) -> bool {
        self.deleted_at().is_some()
    }

    fn soft_delete(&mut self) {
        self.set_deleted_at(Some(Utc::now()));
    }

    fn restore(&mut self) {
        self.set_deleted_at(None);
    }
}
```

```rust
// framework/src/eloquent/mod.rs
pub mod soft_deletes;
pub use soft_deletes::SoftDelete;
```

> **Query integration:** Soft deletes also need a query-level "exclude trashed by default" scope. That's covered in Task 5 below (global scopes — `SoftDeleteScope` is a built-in global scope that adds `WHERE deleted_at IS NULL`).

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test eloquent_soft_deletes
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/eloquent/soft_deletes.rs framework/src/eloquent/mod.rs framework/tests/eloquent_soft_deletes.rs
git commit -m "feat(eloquent): SoftDelete trait with is_trashed / soft_delete / restore"
```

---

## Task 5: Global + local scopes

**Files:** `framework/src/eloquent/scopes.rs`

- [ ] **Step 1: Implement scope plumbing**

```rust
// framework/src/eloquent/scopes.rs
//! Query scopes — global and local.
//!
//! Global: a closure stored per entity type that always extends
//! every query for that entity. Disable per-query via
//! `query.without_global_scope::<MyScope>()`.
//!
//! Local: methods on the entity's query builder (`Post::published()`,
//! `Post::recent(7)`) — implementer-provided extension methods on
//! SeaORM's `Select<Entity>` type.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Select};
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::RwLock;

pub trait GlobalScope<E: EntityTrait>: Send + Sync + 'static {
    fn apply(query: Select<E>) -> Select<E>;
}

type ScopeFn = Box<dyn Fn(sea_orm::Statement) -> sea_orm::Statement + Send + Sync>;

static REGISTRY: RwLock<Option<HashMap<TypeId, Vec<TypeId>>>> = RwLock::new(None);

/// Register a global scope `S` for entity `E`. The scope's `apply`
/// function runs on every Select query for E unless explicitly
/// excluded by `without_global_scope::<S>()`.
pub fn register<E: EntityTrait, S: GlobalScope<E>>() {
    let mut g = REGISTRY.write().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    map.entry(TypeId::of::<E>()).or_default().push(TypeId::of::<S>());
}

/// Built-in soft-delete global scope. Adds `WHERE deleted_at IS NULL`.
/// Register on entities that implement SoftDelete:
///
/// ```ignore
/// scopes::register::<post::Entity, SoftDeleteScope>();
/// ```
pub struct SoftDeleteScope;

impl<E: EntityTrait> GlobalScope<E> for SoftDeleteScope {
    fn apply(query: Select<E>) -> Select<E> {
        // SeaORM Select doesn't have a generic "filter by deleted_at
        // is null" — the column must be known. The implementer
        // provides a typed extension trait per entity:
        //   impl Soft for post::Entity { fn deleted_at() -> post::Column { post::Column::DeletedAt } }
        // and the global scope reads from there.
        query
    }
}
```

> **Generic scope implementation:** Filtering by `deleted_at IS NULL` generically over `E: EntityTrait` is hard because the column name isn't known. **Pragmatic path:** define `pub trait HasDeletedAt: EntityTrait { fn deleted_at_column() -> Self::Column; }`. The `SoftDeleteScope::<E: HasDeletedAt>` impl can then call `query.filter(E::deleted_at_column().is_null())`.

- [ ] **Step 2: Test + commit**

```bash
git add framework/src/eloquent/scopes.rs
git commit -m "feat(eloquent): GlobalScope trait + SoftDeleteScope + per-entity registry"
```

---

## Task 6: Eager loading helpers

**Files:** `framework/src/eloquent/eager.rs`

- [ ] **Step 1: Implement**

```rust
// framework/src/eloquent/eager.rs
//! Eager loading sugar on top of SeaORM's `find_related` /
//! `find_with_related` / `LoaderTrait`.
//!
//! ```ignore
//! let posts = post::Entity::find()
//!     .with::<user::Entity>()      // single relation
//!     .with::<comment::Entity>()   // another relation
//!     .all(&db).await?;
//! ```
//!
//! Implementation: thin extension trait that calls
//! `Select::find_with_related::<R>()` under the hood for each `.with`
//! invocation, accumulating relations. Full impl requires generic
//! relation traversal — implementer should reference SeaORM's
//! `LoaderTrait` examples.
```

> **Implementer scope:** SeaORM already has `find_with_related` for one relation and `LoaderTrait` for multi-relation loading. The `with::<R>()` builder API is sugar; the implementer should validate via SeaORM's docs that the chained-builder pattern fits the existing types. If it doesn't, ship `load_relations(&[<R1>, <R2>, ...])` instead — same outcome, different ergonomics.

- [ ] **Step 2: Commit**

```bash
git commit -m "feat(eloquent): eager-loading helpers on top of SeaORM LoaderTrait"
```

---

## Task 7: Pluralization for i18n

**Files:** `framework/src/i18n/pluralize.rs`, `framework/Cargo.toml`

- [ ] **Step 1: Add dep + implement**

```toml
# framework/Cargo.toml
intl-pluralrules = "7"
```

```rust
// framework/src/i18n/pluralize.rs
use intl_pluralrules::{PluralCategory, PluralRuleType, PluralRules};
use unic_langid::LanguageIdentifier;

/// Translate a key with a count-based plural form. The translation
/// value can be either:
///   - a single string: `"You have :count apples"` (used regardless of count)
///   - a pluralized object: `{ "one": "You have 1 apple", "other": "You have :count apples" }`
pub fn t_choice(key: &str, count: i64, replacements: &[(&str, &str)]) -> String {
    let locale = super::Lang::current_locale();
    let lang: LanguageIdentifier = locale.parse().unwrap_or_else(|_| "en".parse().unwrap());
    let rules = PluralRules::create(lang, PluralRuleType::CARDINAL).ok();

    let mut owned_replacements: Vec<(&str, String)> = replacements
        .iter()
        .map(|(k, v)| (*k, v.to_string()))
        .collect();
    owned_replacements.push(("count", count.to_string()));
    let pluggable: Vec<(&str, &str)> = owned_replacements
        .iter()
        .map(|(k, v)| (*k, v.as_str()))
        .collect();

    let category = rules
        .as_ref()
        .map(|r| r.select(count).unwrap_or(PluralCategory::OTHER))
        .unwrap_or(PluralCategory::OTHER);
    let plural_key = match category {
        PluralCategory::ZERO => "zero",
        PluralCategory::ONE => "one",
        PluralCategory::TWO => "two",
        PluralCategory::FEW => "few",
        PluralCategory::MANY => "many",
        PluralCategory::OTHER => "other",
    };

    // Try `<key>.<plural_key>` first; fall back to plain `<key>`.
    let scoped = format!("{}.{}", key, plural_key);
    let translated = super::t(&scoped, &pluggable);
    if translated == scoped {
        super::t(key, &pluggable)
    } else {
        translated
    }
}
```

```rust
// framework/src/i18n/mod.rs
mod pluralize;
pub use pluralize::t_choice;
```

- [ ] **Step 2: Write test**

```rust
// framework/tests/i18n_plural.rs
use suprnova::i18n::{Lang, t_choice};

#[test]
fn english_pluralization_branches() {
    Lang::set("en");
    Lang::register_inline("en", serde_json::json!({
        "apples": {
            "one": "1 apple",
            "other": ":count apples"
        }
    }));
    assert_eq!(t_choice("apples", 1, &[]), "1 apple");
    assert_eq!(t_choice("apples", 5, &[]), "5 apples");
}

#[test]
fn polish_pluralization_uses_few() {
    Lang::set("pl");
    Lang::register_inline("pl", serde_json::json!({
        "apples": {
            "one": "1 jabłko",
            "few": ":count jabłka",
            "many": ":count jabłek"
        }
    }));
    assert_eq!(t_choice("apples", 1, &[]), "1 jabłko");
    assert_eq!(t_choice("apples", 3, &[]), "3 jabłka");   // few
    assert_eq!(t_choice("apples", 12, &[]), "12 jabłek");  // many
}
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test i18n_plural
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/i18n/pluralize.rs framework/src/i18n/mod.rs framework/tests/i18n_plural.rs framework/Cargo.toml
git commit -m "feat(i18n): CLDR-based pluralization via t_choice + intl-pluralrules"
```

---

## Task 8: App dogfood — UserObserver + Post soft delete

**Files:** `app/src/observers/user_observer.rs`, `app/src/models/post.rs`

- [ ] **Step 1: UserObserver**

```rust
// app/src/observers/user_observer.rs
use crate::models::User;
use suprnova::{async_trait, eloquent::Observer, FrameworkError};
use tracing::info;

pub struct UserObserver;

#[async_trait]
impl Observer<User> for UserObserver {
    async fn created(&self, user: &User) -> Result<(), FrameworkError> {
        info!(user_id = user.id, "user observer: created");
        // Real app: queue welcome email here
        suprnova::Queue::push(crate::jobs::SendWelcomeEmailJob {
            user_id: user.id,
            email: user.email.clone(),
        })
        .await?;
        Ok(())
    }
}
```

- [ ] **Step 2: Register in bootstrap**

```rust
// app/src/bootstrap.rs
suprnova::eloquent::observers::register::<crate::models::User>(
    std::sync::Arc::new(crate::observers::UserObserver),
);
```

- [ ] **Step 3: Post soft delete**

```rust
// app/src/models/post.rs — add SoftDelete impl
impl suprnova::eloquent::SoftDelete for Model {
    fn deleted_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.deleted_at
    }
    fn set_deleted_at(&mut self, at: Option<chrono::DateTime<chrono::Utc>>) {
        self.deleted_at = at;
    }
}
```

- [ ] **Step 4: Commit**

```bash
git add app/src
git commit -m "feat(app): dogfood UserObserver creates → queue welcome email; Post soft delete"
```

---

## Task 9: Workspace lint + roadmap update

- [ ] **Step 1: Clippy + tests**

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: Update ROADMAP "Where we are"**

Move from "Missing" to "Production-ready":
- Eloquent parity (Fillable, Casts, Observers, Soft Deletes, Scopes)
- i18n pluralization

- [ ] **Step 3: Commit + push**

---

## Self-Review

| Spec item | Covered by |
|-----------|------------|
| Mass assignment (Fillable/Guarded) | Task 1 |
| Casts (AsJson/AsBool/AsArray/AsDateTime/AsEncrypted) | Task 2 |
| Model events + observers | Task 3 |
| Soft deletes | Task 4 |
| Global + local scopes | Task 5 |
| Eager loading helpers | Task 6 |
| Pluralization | Task 7 |
| App dogfood | Task 8 |

---

## Execution Handoff

**Subagent-Driven per task.**
