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
