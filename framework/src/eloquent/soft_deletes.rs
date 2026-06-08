//! Soft deletes — tombstone column + auto-applied global scope.
//!
//! Models annotated `#[suprnova::model(soft_deletes)]` swap their
//! `delete()` semantics: instead of `DELETE FROM table WHERE pk = ?`,
//! the row's `deleted_at` column is set to `Utc::now()`. The default
//! query scope filters `WHERE deleted_at IS NULL` so trashed rows are
//! invisible to ordinary `find` / `query` calls; callers opt in via
//! [`with_trashed`] (includes both alive + trashed) or [`only_trashed`]
//! (only trashed).
//!
//! [`with_trashed`]: #method.with_trashed
//! [`only_trashed`]: #method.only_trashed
//!
//! ## Trait
//!
//! The macro emits an `impl SoftDeletes for #struct` block exposing the
//! tombstone column name and a `is_trashed()` accessor. The companion
//! inherent methods (`delete`, `force_delete`, `restore`, `trashed`,
//! `with_trashed`, `only_trashed`) live on `impl #struct {}` so they
//! can take `self` by value — matching the `Model::delete(self)`
//! signature and dodging the auto-ref method-resolution trap that
//! would silently route through the trait default instead of the
//! override.

use sea_orm::{EntityTrait, IntoActiveModel, PrimaryKeyTrait};
use serde::Serialize;

use crate::eloquent::builder::{Builder, WhereTerm};

/// Marker trait emitted by `#[suprnova::model(soft_deletes)]`. Exposes
/// the tombstone column name + a `is_trashed()` accessor. The
/// inherent overrides (`delete` / `force_delete` / `restore`) live on
/// the model's `impl` block, not this trait, so the by-value
/// `self` signature can match `Model::delete(self)` and override
/// cleanly through Rust's method-resolution rules.
///
/// The where-clause re-elaborates the [`Model`][crate::eloquent::Model]
/// trait's own bounds because Rust's trait elaboration doesn't
/// transitively propagate associated-type bounds from a supertrait's
/// where-clause to a subtrait's method bodies. Same pattern as
/// `FirstOrCreate` for the same reason.
pub trait SoftDeletes: crate::eloquent::Model
where
    Self: From<<Self::Entity as EntityTrait>::Model>,
    <Self::Entity as EntityTrait>::Model: From<Self>
        + IntoActiveModel<<Self::Entity as EntityTrait>::ActiveModel>
        + Serialize
        + Send
        + Sync,
    <Self::Entity as EntityTrait>::ActiveModel: Send,
    <<Self::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Column name carrying the tombstone timestamp. Defaults to
    /// `"deleted_at"`; overridable via
    /// `#[model(soft_deletes_column = "...")]`.
    fn deleted_at_column() -> &'static str;

    /// Whether the row is currently soft-deleted (`deleted_at IS NOT
    /// NULL` at row materialisation time). Cheap accessor — does not
    /// touch the database.
    fn is_trashed(&self) -> bool;
}

/// Builder modifiers for soft-deleted models — `Self::with_trashed()`
/// and `Self::only_trashed()` are *inherent* methods on the model
/// struct (emitted by `#[suprnova::model(soft_deletes)]`) because
/// they construct a fresh, unscoped builder. These two are the
/// *chainable* variants that operate on an existing builder — they're
/// what enables:
///
/// - `Model::query().with_trashed()` — equivalent to the static
///   `Model::with_trashed()`, but discoverable from a builder
///   you already have in hand.
/// - `User::query().with_where(("posts", |q: Builder<Post>| q.with_trashed()))`
///   — the only path through the eager-load closure surface for
///   widening child scope, because the closure receives a
///   `Builder<R>` not a `Post`-the-struct.
/// - `user.posts().with_trashed()` — relation wrappers forward to
///   these methods on their inner `Builder<R>`.
///
/// **Mutation strategy.** The soft-delete scope is installed by
/// `Self::query()` as a direct `filter_null(deleted_at)` call (see
/// the macro emission in
/// `suprnova-macros/src/model/derive_eloquent.rs` for the
/// `query_override`). The `Vec<WhereTerm>` is the canonical storage,
/// so `with_trashed` retains every term that isn't the tombstone
/// null-check; `only_trashed` swaps it for `NotNull(deleted_at)`.
/// Both append the `"soft_deletes"` tag via the framework-internal
/// `__disable_named_scope` so the typed Phase 10C T4 scope registry
/// can layer on top without double-applying.
impl<M> Builder<M>
where
    M: SoftDeletes,
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
    /// Widen the query to include trashed rows. Removes the
    /// `deleted_at IS NULL` term the global scope installed via
    /// `Self::query()`. Idempotent — calling it twice does nothing
    /// the first call didn't already do.
    pub fn with_trashed(mut self) -> Self {
        let col = M::deleted_at_column();
        self.where_terms
            .retain(|t| !matches!(t, WhereTerm::Null(c) if c == col));
        self.global_scopes_disabled.push("soft_deletes");
        self
    }

    /// Restrict the query to *only* trashed rows. Removes the
    /// `deleted_at IS NULL` term and appends `deleted_at IS NOT NULL`.
    /// Idempotent.
    pub fn only_trashed(mut self) -> Self {
        let col = M::deleted_at_column();
        self.where_terms
            .retain(|t| !matches!(t, WhereTerm::Null(c) if c == col));
        // Avoid double-stamping NotNull on repeated calls.
        if !self
            .where_terms
            .iter()
            .any(|t| matches!(t, WhereTerm::NotNull(c) if c == col))
        {
            self = self.filter_not_null(col);
        }
        self.global_scopes_disabled.push("soft_deletes");
        self
    }
}
