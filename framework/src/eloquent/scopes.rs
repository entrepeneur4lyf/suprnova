//! Phase 10C T3 + T4 — local scopes (trait emissions from the
//! `#[suprnova::scopes]` macro live in `suprnova-macros/src/scopes.rs`)
//! and global scopes (this file).
//!
//! Global scopes apply automatically to every [`Model::query`] call.
//! Users register them at boot via [`ScopeRegistry::register::<M, S>`].
//! Each query can opt out of a single scope by type with
//! [`Builder::without_global_scope::<S>`] or bypass the registry
//! entirely with [`Builder::without_global_scopes`].
//!
//! ## Soft deletes coexistence
//!
//! Suprnova's [`SoftDeletes`][crate::eloquent::SoftDeletes] pathway
//! does **not** route through this registry — it ships its own inherent
//! `Model::query` override (emitted by `#[suprnova::model(soft_deletes)]`)
//! that prepends a `deleted_at IS NULL` filter, and a
//! `global_scopes_disabled: Vec<&'static str>` tag system on the
//! builder. The two paths coexist; T4 does not retroactively fold
//! soft-deletes into the registry.
//!
//! ## PK lookups
//!
//! Global scopes apply through [`Model::query`]. [`Model::find`],
//! [`Model::find_many`], and [`Model::all`] go through SeaORM's
//! `find_by_id` / `find().all()` directly and do **not** receive
//! registered scopes — matching Laravel's `Eloquent\Model::find`
//! semantics. Callers that want scoped PK lookups use
//! `Self::query().filter("id", pk).first().await`.
//!
//! [`Model::query`]: crate::eloquent::Model::query
//! [`Model::find`]: crate::eloquent::Model::find
//! [`Model::find_many`]: crate::eloquent::Model::find_many
//! [`Model::all`]: crate::eloquent::Model::all
//! [`Builder::without_global_scope::<S>`]: crate::eloquent::Builder::without_global_scope
//! [`Builder::without_global_scopes`]: crate::eloquent::Builder::without_global_scopes

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use sea_orm::{EntityTrait, IntoActiveModel, PrimaryKeyTrait};
use serde::Serialize;

use crate::eloquent::builder::Builder;
use crate::eloquent::model::Model;

/// A global scope that applies to every [`Model::query`] call for
/// model `M`. Register at boot via [`ScopeRegistry::register`].
///
/// ## Example
///
/// ```ignore
/// use suprnova::eloquent::scopes::{GlobalScope, ScopeRegistry};
/// use suprnova::{Builder, Model};
///
/// pub struct TenantScope;
///
/// impl GlobalScope<Article> for TenantScope {
///     fn apply(&self, query: Builder<Article>) -> Builder<Article> {
///         query.filter("tenant_id", current_tenant_id())
///     }
/// }
///
/// // At boot:
/// ScopeRegistry::register::<Article, _>(TenantScope);
///
/// // Every query is scoped automatically:
/// let rows = Article::query().get().await?;
///
/// // Opt out by type:
/// let unscoped = Article::without_global_scope::<TenantScope>().get().await?;
///
/// // Opt out of everything:
/// let all = Article::without_global_scopes().get().await?;
/// ```
///
/// [`Model::query`]: crate::eloquent::Model::query
///
/// The where-clause re-elaborates [`Model`]'s own bounds because
/// Rust's trait elaboration doesn't transitively propagate
/// associated-type bounds from a supertrait's where-clause to a
/// subtrait's method bodies — the same pattern [`FirstOrCreate`] and
/// [`SoftDeletes`] use for the same reason.
///
/// [`Model`]: crate::eloquent::Model
/// [`FirstOrCreate`]: crate::eloquent::FirstOrCreate
/// [`SoftDeletes`]: crate::eloquent::SoftDeletes
pub trait GlobalScope<M>: Send + Sync + 'static
where
    M: Model,
    M: From<<M::Entity as EntityTrait>::Model>,
    <M::Entity as EntityTrait>::Model: From<M>
        + IntoActiveModel<<M::Entity as EntityTrait>::ActiveModel>
        + Serialize
        + Send
        + Sync,
    <M::Entity as EntityTrait>::ActiveModel: Send,
    <<M::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Mutate the builder however the scope needs. Called once per
    /// `Model::query()` invocation. Return the (possibly modified)
    /// builder. The framework chains scopes in registration order.
    fn apply(&self, query: Builder<M>) -> Builder<M>;
}

/// Type-erased apply closure. The concrete `Arc<S>` is captured at
/// registration time; the closure downcasts a `Box<dyn Any>` back to
/// `Builder<M>`, runs the scope, and re-boxes the result. The
/// `register::<M, S>` generics guarantee the closure is only ever
/// invoked against `Builder<M>` of the matching type — `register`
/// stores the closure under `TypeId::of::<M>()`, and `apply_to::<M>`
/// looks it up under the same key.
type ErasedApply =
    Arc<dyn Fn(Box<dyn Any + Send>) -> Box<dyn Any + Send> + Send + Sync>;

struct PerModelScopes {
    /// `(TypeId of S, erased apply)` pairs in registration order so
    /// scopes layer onto the WHERE clause in the order the user
    /// declared them.
    entries: Vec<(TypeId, ErasedApply)>,
}

static REGISTRY: OnceLock<RwLock<HashMap<TypeId, PerModelScopes>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<TypeId, PerModelScopes>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// The process-global scope registry.
///
/// Registered at boot, applied automatically by [`Model::query`].
/// Storage is per-model (`TypeId::of::<M>()`), and per-model entries
/// are an ordered `Vec` so scope application matches registration
/// order.
///
/// [`Model::query`]: crate::eloquent::Model::query
pub struct ScopeRegistry;

impl ScopeRegistry {
    /// Register `scope` to apply on every `M::query()` call.
    ///
    /// The scope is wrapped in an `Arc` and stored under the
    /// `TypeId::of::<M>()` slot. Multiple scopes per model are
    /// supported; they apply in registration order.
    ///
    /// `S` must be a unit struct or otherwise carry no per-call
    /// state — the captured `Arc<S>` is shared across every query.
    /// Per-request state (e.g. the current tenant ID) belongs in a
    /// thread-local / `tokio::task_local!` / `AtomicI64` that the
    /// scope reads inside `apply`.
    pub fn register<M, S>(scope: S)
    where
        M: Model + 'static,
        M: From<<M::Entity as EntityTrait>::Model>,
        <M::Entity as EntityTrait>::Model: From<M>
            + IntoActiveModel<<M::Entity as EntityTrait>::ActiveModel>
            + Serialize
            + Send
            + Sync,
        <M::Entity as EntityTrait>::ActiveModel: Send,
        <<M::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType:
            Send + Into<sea_orm::Value>,
        S: GlobalScope<M> + 'static,
    {
        let scope = Arc::new(scope);
        let scope_type_id = TypeId::of::<S>();
        let model_type_id = TypeId::of::<M>();

        let apply: ErasedApply = Arc::new(move |b: Box<dyn Any + Send>| {
            let builder: Builder<M> = *b
                .downcast::<Builder<M>>()
                .expect("ScopeRegistry: erased apply dispatched to wrong model type");
            let result = scope.apply(builder);
            Box::new(result) as Box<dyn Any + Send>
        });

        let mut reg = registry()
            .write()
            .expect("ScopeRegistry: write lock poisoned");
        reg.entry(model_type_id)
            .or_insert_with(|| PerModelScopes {
                entries: Vec::new(),
            })
            .entries
            .push((scope_type_id, apply));
    }

    /// Apply every registered scope for `M` to `builder`. Skips any
    /// scope whose `TypeId` appears in `builder.excluded_scopes`.
    /// Returns `builder` unchanged when `builder.skip_all_scopes` is
    /// set or no scopes are registered for `M`.
    ///
    /// Public so the `#[suprnova::model]` macro can emit calls into
    /// user crates (the soft-delete `query()` override invokes
    /// `apply_to` after layering on `filter_null(deleted_at)`). End
    /// users go through `Model::query()` which dispatches here
    /// automatically; calling it directly is unusual but supported.
    pub fn apply_to<M>(builder: Builder<M>) -> Builder<M>
    where
        M: Model + 'static,
        M: From<<M::Entity as EntityTrait>::Model>,
        <M::Entity as EntityTrait>::Model: From<M>
            + IntoActiveModel<<M::Entity as EntityTrait>::ActiveModel>
            + Serialize
            + Send
            + Sync,
        <M::Entity as EntityTrait>::ActiveModel: Send,
        <<M::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType:
            Send + Into<sea_orm::Value>,
    {
        if builder.skip_all_scopes {
            return builder;
        }

        // Snapshot the (cheap, Arc-cloned) per-model scope list under
        // the read lock, then release the lock before running user
        // code so a scope that itself touches the registry doesn't
        // deadlock.
        let entries: Vec<(TypeId, ErasedApply)> = {
            let reg = registry().read().expect("ScopeRegistry: read lock poisoned");
            match reg.get(&TypeId::of::<M>()) {
                Some(p) => p.entries.clone(),
                None => return builder,
            }
        };

        let excluded = builder.excluded_scopes.clone();
        let mut current: Box<dyn Any + Send> = Box::new(builder);
        for (scope_ty, apply) in entries {
            if excluded.contains(&scope_ty) {
                continue;
            }
            current = apply(current);
        }
        *current
            .downcast::<Builder<M>>()
            .expect("ScopeRegistry: apply pipeline preserved Builder<M> type")
    }

    /// Test-only escape hatch. Drops every registered scope. Used by
    /// the framework's own `#[cfg(test)]` blocks to keep test
    /// isolation simple; production code should NEVER call this.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn __clear_for_tests() {
        let mut reg = registry()
            .write()
            .expect("ScopeRegistry: write lock poisoned");
        reg.clear();
    }
}
