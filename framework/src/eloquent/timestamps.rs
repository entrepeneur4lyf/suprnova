//! Auto-managed timestamps + `touch()`.
//!
//! When a `#[suprnova::model]` struct carries both `created_at` and
//! `updated_at` fields (typed `chrono::DateTime<chrono::Utc>`), the
//! macro:
//!
//! - sets BOTH to `Utc::now()` on `create()`
//! - bumps `updated_at` on every `save()` and `update(attrs)`
//! - emits an `impl Touchable for YourStruct` so callers can write
//!   `user.touch().await?` to bump `updated_at` without touching any
//!   other column
//!
//! Auto-detect honours `#[model(timestamps = false)]` (opt-out) and
//! `#[model(created_at = "creado_en", updated_at = "actualizado_en")]`
//! (custom column names). When the struct has only ONE of the two
//! columns, the macro emits a `compile_error!` — almost always a
//! typo (e.g. `craeted_at`) we want to surface loudly rather than
//! silently swallow.
//!
//! Storage uses RFC-3339 / ISO-8601 TEXT via the [`AsDateTime`] cast
//! that the macro auto-injects for timestamp columns. The cast lets
//! the same `DateTime<Utc>` value round-trip across all three
//! SeaORM drivers (SQLite / MySQL / PostgreSQL) without forcing
//! users to pick a database-specific timestamp type.
//!
//! [`AsDateTime`]: crate::eloquent::AsDateTime

use crate::error::FrameworkError;

/// Bump `updated_at` on this row without changing any other column.
///
/// Implemented by the `#[suprnova::model]` macro on every struct that
/// has timestamps enabled (the default when both `created_at` and
/// `updated_at` fields are present). Models without timestamp columns
/// don't get a `Touchable` impl — calling `.touch()` on them fails to
/// compile.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{model, Touchable};
/// use chrono::{DateTime, Utc};
///
/// #[model(table = "posts")]
/// pub struct Post {
///     pub id: i64,
///     pub title: String,
///     pub created_at: DateTime<Utc>,
///     pub updated_at: DateTime<Utc>,
/// }
///
/// // Somewhere in a handler:
/// post.touch().await?;
/// ```
#[async_trait::async_trait]
pub trait Touchable {
    /// Update `updated_at` to `Utc::now()` for this row. The PK is
    /// preserved; no other column is touched.
    ///
    /// Errors propagate from the database driver.
    async fn touch(&self) -> Result<(), FrameworkError>;
}
