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

tokio::task_local! {
    /// Task-local "touches disabled" flag. When `true`, the macro-
    /// emitted [`Touchable::touch`] impls short-circuit to `Ok(())`
    /// without bumping `updated_at`. Mirrors Laravel's
    /// `Model::withoutTouching` scope but task-scoped so concurrent
    /// requests on other tasks remain unaffected.
    static TOUCHES_DISABLED: bool;
}

/// Whether the current task is inside a [`without_touching`] scope.
/// Called by the macro-emitted [`Touchable::touch`] impl to honour the
/// scope.
pub fn touches_disabled() -> bool {
    TOUCHES_DISABLED.try_with(|b| *b).unwrap_or(false)
}

/// Run `fut` with touches disabled for the current async task —
/// every `model.touch()` call inside the scope short-circuits. Suprnova
/// analogue of Laravel's `Model::withoutTouching(closure)`.
///
/// The flag is a `tokio::task_local!` so it doesn't leak across
/// `tokio::spawn` boundaries and concurrent requests on other tasks
/// continue to honour their own scope (or its absence).
///
/// ```rust,no_run
/// use suprnova::eloquent::{without_touching, Touchable};
/// # struct Post;
/// # #[suprnova::async_trait]
/// # impl Touchable for Post {
/// #     async fn touch(&self) -> Result<(), suprnova::FrameworkError> { Ok(()) }
/// # }
/// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
/// # let post = Post;
/// without_touching(async {
///     // Inside this scope, `post.touch().await` is a no-op even
///     // though the touchable impl is wired through.
///     post.touch().await?;
///     Ok::<(), suprnova::FrameworkError>(())
/// }).await?;
/// # Ok(()) }
/// ```
pub async fn without_touching<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    TOUCHES_DISABLED.scope(true, fut).await
}

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
