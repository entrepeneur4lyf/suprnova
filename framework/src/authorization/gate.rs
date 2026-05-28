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

    // ── Introspection ─────────────────────────────────────────────────────────

    /// `true` iff a gate (sync **or** async) is registered for the
    /// `(action, U, R)` tuple. Mirrors Laravel's `Gate::has` — handy
    /// for diagnostic UIs ("is this ability defined?") and for
    /// frontend Inertia props that ship the user's full ability map.
    ///
    /// Note: a registered gate is not the same as an allowed gate.
    /// `has` answers "does the framework know how to decide?", not
    /// "is the answer yes?".
    pub fn has<U: 'static, R: 'static>(action: &str) -> bool {
        global().has::<U, R>(action)
    }

    /// Every distinct action name registered across all `(U, R)`
    /// tuples, sorted + deduped. Mirrors Laravel's
    /// `Gate::abilities()`. Useful for admin UIs that need to list
    /// every defined ability for picker / role-mapping forms.
    pub fn abilities() -> Vec<String> {
        global().abilities()
    }

    // ── Multi-action dispatch (sync) ──────────────────────────────────────────

    /// `true` iff **any** of the supplied actions allow against the
    /// same `(user, resource)`. Mirrors Laravel's
    /// `Gate::any($abilities, $arguments)`. Short-circuits on the
    /// first allow — does not evaluate later actions.
    ///
    /// A missing gate among `actions` is treated as deny (matches
    /// the single-action [`allows`] semantic).
    pub fn any<U: 'static, R: 'static>(actions: &[&str], user: &U, resource: &R) -> bool {
        actions.iter().any(|a| Self::allows(a, user, resource))
    }

    /// `true` iff **none** of the supplied actions allow against the
    /// same `(user, resource)`. Mirrors Laravel's
    /// `Gate::none($abilities, $arguments)`. Short-circuits on the
    /// first allow (returning `false`).
    pub fn none<U: 'static, R: 'static>(actions: &[&str], user: &U, resource: &R) -> bool {
        !Self::any(actions, user, resource)
    }

    /// `true` iff **every** action allows against the same
    /// `(user, resource)`. Mirrors Laravel's array-form
    /// `Gate::check([abilities], $arguments)`. Short-circuits on
    /// the first deny.
    ///
    /// An empty `actions` slice returns `true` (vacuously) —
    /// matches the standard `Iterator::all` semantic.
    pub fn check<U: 'static, R: 'static>(actions: &[&str], user: &U, resource: &R) -> bool {
        actions.iter().all(|a| Self::allows(a, user, resource))
    }

    // ── Multi-action dispatch (async) ─────────────────────────────────────────

    /// Async sibling of [`any`]. Sequentially awaits each gate; works
    /// for sync and async registrations alike (via
    /// [`allows_async`]'s dispatch). Sequential rather than
    /// concurrent because most policy bodies are cheap and
    /// short-circuiting on the first allow saves the rest.
    pub async fn any_async<U: 'static, R: 'static>(
        actions: &[&str],
        user: &U,
        resource: &R,
    ) -> bool {
        for action in actions {
            if Self::allows_async(action, user, resource).await {
                return true;
            }
        }
        false
    }

    /// Async sibling of [`none`].
    pub async fn none_async<U: 'static, R: 'static>(
        actions: &[&str],
        user: &U,
        resource: &R,
    ) -> bool {
        !Self::any_async(actions, user, resource).await
    }

    /// Async sibling of [`check`].
    pub async fn check_async<U: 'static, R: 'static>(
        actions: &[&str],
        user: &U,
        resource: &R,
    ) -> bool {
        for action in actions {
            if !Self::allows_async(action, user, resource).await {
                return false;
            }
        }
        true
    }
}
