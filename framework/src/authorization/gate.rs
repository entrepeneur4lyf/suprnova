use std::future::Future;

use super::registry::global;
use crate::FrameworkError;

/// Authorization gate facade.
///
/// ```ignore
/// Gate::define::<User, Post>("view", |user, post| post.is_public || user.is_admin);
///
/// if Gate::allows("view", &user, &post) {
///     // ...
/// }
/// ```
pub struct Gate;

impl Gate {
    // ── Sync API ──────────────────────────────────────────────────────────────

    /// Define a synchronous authorization closure for a given action.
    pub fn define<U: 'static, R: 'static>(
        action: &str,
        f: impl Fn(&U, &R) -> bool + Send + Sync + 'static,
    ) {
        global().register::<U, R>(action, f);
    }

    /// Returns `true` when the gate exists and allows the action.
    /// Missing gates **deny by default**.
    ///
    /// Calling `allows` on an async-registered gate returns `false` (default
    /// deny). Use [`allows_async`] to invoke async gates correctly.
    pub fn allows<U: 'static, R: 'static>(action: &str, user: &U, resource: &R) -> bool {
        global().invoke(action, user, resource).unwrap_or(false)
    }

    /// Returns `true` when the gate denies the action.
    pub fn denies<U: 'static, R: 'static>(action: &str, user: &U, resource: &R) -> bool {
        !Self::allows(action, user, resource)
    }

    /// Return `Err(FrameworkError::Unauthorized)` when denied.
    pub fn authorize<U: 'static, R: 'static>(
        action: &str,
        user: &U,
        resource: &R,
    ) -> Result<(), FrameworkError> {
        if Self::allows(action, user, resource) {
            Ok(())
        } else {
            Err(FrameworkError::Unauthorized)
        }
    }

    // ── Async API ─────────────────────────────────────────────────────────────

    /// Define an asynchronous authorization closure for a given action.
    ///
    /// The closure must produce an *owned* future — references to `user` and
    /// `resource` cannot be held past the closure return. Copy or clone any
    /// data needed inside the future body before returning it.
    ///
    /// # Sync compatibility
    ///
    /// Async-registered gates return `false` from the sync [`allows`] /
    /// [`denies`] / [`authorize`] methods (default deny). Always use
    /// [`allows_async`] / [`denies_async`] / [`authorize_async`] for gates
    /// registered with `define_async`.
    pub fn define_async<U, R, F, Fut>(action: &str, f: F)
    where
        U: 'static,
        R: 'static,
        F: Fn(&U, &R) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = bool> + Send + 'static,
    {
        global().register_async::<U, R, F, Fut>(action, f);
    }

    /// Async version of [`allows`]. Works for both sync- and async-registered gates.
    pub async fn allows_async<U: 'static, R: 'static>(
        action: &str,
        user: &U,
        resource: &R,
    ) -> bool {
        global()
            .invoke_async(action, user, resource)
            .await
            .unwrap_or(false)
    }

    /// Async version of [`denies`].
    pub async fn denies_async<U: 'static, R: 'static>(
        action: &str,
        user: &U,
        resource: &R,
    ) -> bool {
        !Self::allows_async(action, user, resource).await
    }

    /// Async version of [`authorize`].
    pub async fn authorize_async<U: 'static, R: 'static>(
        action: &str,
        user: &U,
        resource: &R,
    ) -> Result<(), FrameworkError> {
        if Self::allows_async(action, user, resource).await {
            Ok(())
        } else {
            Err(FrameworkError::Unauthorized)
        }
    }
}
