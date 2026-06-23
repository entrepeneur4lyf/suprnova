use std::future::Future;

use super::Response;
use super::registry::global;
use crate::FrameworkError;

/// Authorization gate facade.
///
/// ```rust,no_run
/// # use suprnova::Gate;
/// # struct User { is_admin: bool }
/// # struct Post { is_public: bool }
/// # let user = User { is_admin: false };
/// # let post = Post { is_public: true };
/// Gate::define::<User, Post>("view", |user, post| post.is_public || user.is_admin);
///
/// if Gate::allows("view", &user, &post) {
///     // ...
/// }
/// ```
///
/// # No `forUser`
///
/// Laravel's `Gate::forUser($user)->allows(...)` rebinds the gate's *implicit*
/// current-user resolver to a different user. Suprnova's gate takes the user
/// **explicitly** on every call — `Gate::allows(action, &user, &resource)` —
/// so "check as a different user" is just passing that user. There is no
/// implicit resolver to rebind, which makes `forUser` redundant rather than
/// missing: the explicit API is strictly more general.
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

    /// Define a synchronous gate whose closure returns a rich [`Response`]
    /// rather than a bare `bool` — so a denial can carry a message, code, and
    /// HTTP status that [`inspect`](Self::inspect) and [`Self::authorize`](Self::authorize)
    /// surface.
    ///
    /// ```rust,no_run
    /// use suprnova::authorization::Response;
    /// # use suprnova::Gate;
    /// # struct User { id: u64 }
    /// # struct Post { author_id: u64 }
    ///
    /// Gate::define_with::<User, Post>("update", |user, post| {
    ///     if post.author_id == user.id {
    ///         Response::allow()
    ///     } else {
    ///         Response::deny_with("You do not own this post.")
    ///     }
    /// });
    /// ```
    pub fn define_with<U: 'static, R: 'static>(
        action: &str,
        f: impl Fn(&U, &R) -> Response + Send + Sync + 'static,
    ) {
        global().register_with::<U, R>(action, f);
    }

    /// Returns `true` when the gate exists and allows the action.
    /// Missing gates **deny by default**.
    ///
    /// Routes through [`inspect`](Self::inspect), so `before`/`after` hooks
    /// apply. Calling `allows` on an async-registered gate returns `false`
    /// (default deny). Use [`Self::allows_async`](Self::allows_async) to invoke async
    /// gates correctly.
    pub fn allows<U: 'static, R: 'static>(action: &str, user: &U, resource: &R) -> bool {
        Self::inspect(action, user, resource).allowed()
    }

    /// Returns `true` when the gate denies the action.
    pub fn denies<U: 'static, R: 'static>(action: &str, user: &U, resource: &R) -> bool {
        !Self::allows(action, user, resource)
    }

    /// Authorize the action, returning the denial as an error.
    ///
    /// A bare denial maps to `FrameworkError::Unauthorized` (403). A rich
    /// denial — from a [`define_with`](Self::define_with) gate that returned a
    /// `Response` with a custom message/status — maps to
    /// `FrameworkError::Domain` carrying that message and status (e.g. 404 from
    /// `Response::deny_as_not_found()`).
    pub fn authorize<U: 'static, R: 'static>(
        action: &str,
        user: &U,
        resource: &R,
    ) -> Result<(), FrameworkError> {
        Self::inspect(action, user, resource)
            .authorize()
            .map(|_| ())
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
    /// Async-registered gates return `false` from the sync [`Self::allows`] /
    /// [`Self::denies`] / [`Self::authorize`] methods (default deny). Always use
    /// [`Self::allows_async`] / [`Self::denies_async`] / [`Self::authorize_async`] for gates
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

    /// Define an asynchronous gate whose future resolves to a rich [`Response`]
    /// (the async sibling of [`define_with`](Self::define_with)).
    pub fn define_async_with<U, R, F, Fut>(action: &str, f: F)
    where
        U: 'static,
        R: 'static,
        F: Fn(&U, &R) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        global().register_async_with::<U, R, F, Fut>(action, f);
    }

    /// Async version of [`Self::allows`]. Works for both sync- and async-registered gates.
    pub async fn allows_async<U: 'static, R: 'static>(
        action: &str,
        user: &U,
        resource: &R,
    ) -> bool {
        Self::inspect_async(action, user, resource).await.allowed()
    }

    /// Async version of [`Self::denies`].
    pub async fn denies_async<U: 'static, R: 'static>(
        action: &str,
        user: &U,
        resource: &R,
    ) -> bool {
        !Self::allows_async(action, user, resource).await
    }

    /// Async version of [`Self::authorize`].
    pub async fn authorize_async<U: 'static, R: 'static>(
        action: &str,
        user: &U,
        resource: &R,
    ) -> Result<(), FrameworkError> {
        Self::inspect_async(action, user, resource)
            .await
            .authorize()
            .map(|_| ())
    }

    // ── Rich decisions: inspect / raw + before / after hooks ────────────────

    /// Evaluate the action and return the rich [`Response`] — the
    /// allow/deny decision plus any message, code, and HTTP status. Mirrors
    /// Laravel's `Gate::inspect`. An undefined ability (no gate, no hook
    /// decision) yields a default deny.
    ///
    /// This is the evaluation core: [`Self::allows`](Self::allows),
    /// [`Self::denies`](Self::denies), and [`Self::authorize`](Self::authorize) all route
    /// through it, so `before`/`after` hooks apply uniformly.
    pub fn inspect<U: 'static, R: 'static>(action: &str, user: &U, resource: &R) -> Response {
        global()
            .raw::<U, R>(action, user, resource)
            .unwrap_or_else(Response::deny)
    }

    /// Async sibling of [`inspect`](Self::inspect).
    pub async fn inspect_async<U: 'static, R: 'static>(
        action: &str,
        user: &U,
        resource: &R,
    ) -> Response {
        global()
            .raw_async::<U, R>(action, user, resource)
            .await
            .unwrap_or_else(Response::deny)
    }

    /// The raw evaluation result, preserving the *undefined* case as `None`.
    ///
    /// Unlike [`inspect`](Self::inspect) (which normalizes `None` to a default
    /// deny), `raw` returns `None` when nothing decided — no `before` hook
    /// fired, no gate is registered for `(action, U, R)`, and no `after` hook
    /// filled in. This distinguishes "explicitly denied" from "no rule
    /// defined", mirroring Laravel's `Gate::raw`.
    pub fn raw<U: 'static, R: 'static>(action: &str, user: &U, resource: &R) -> Option<Response> {
        global().raw::<U, R>(action, user, resource)
    }

    /// Async sibling of [`raw`](Self::raw).
    pub async fn raw_async<U: 'static, R: 'static>(
        action: &str,
        user: &U,
        resource: &R,
    ) -> Option<Response> {
        global().raw_async::<U, R>(action, user, resource).await
    }

    /// Register a hook that runs **before** any gate for the user type `U`.
    ///
    /// Returning `Some(decision)` short-circuits all gates and other before
    /// hooks for that user type (first `Some` wins); returning `None` lets
    /// evaluation continue to the gate. The canonical use is a global override
    /// such as "administrators may do anything":
    ///
    /// ```rust,no_run
    /// # use suprnova::Gate;
    /// # struct User { is_admin: bool }
    /// Gate::before::<User>(|user, _action| user.is_admin.then_some(true));
    /// ```
    ///
    /// Hooks are keyed by the **user type** (`U`), not by resource, so a hook
    /// fires for every `(action, U, R)` regardless of resource — put
    /// resource-specific logic in the gate. Hooks are synchronous predicates;
    /// for async authorization logic use [`define_async`](Self::define_async)
    /// or [`define_async_with`](Self::define_async_with). They apply to the
    /// async evaluation path too.
    pub fn before<U: 'static>(f: impl Fn(&U, &str) -> Option<bool> + Send + Sync + 'static) {
        global().register_before::<U>(f);
    }

    /// Register a hook that runs **after** the gate for the user type `U`.
    ///
    /// Every after hook runs (so it can log the outcome), receiving the running
    /// decision as `Option<bool>` (`None` while still undecided). Following
    /// Laravel's `$result ??= $afterResult` semantic, an after hook can only
    /// **fill in** an undecided result — it cannot override an allow or deny
    /// that a before hook or gate already produced. Return `None` to record a
    /// no-op.
    ///
    /// ```rust,no_run
    /// # use suprnova::Gate;
    /// # struct User { is_superuser: bool }
    /// // Grant a fallback only when no gate is defined for the action:
    /// Gate::after::<User>(|user, _action, decided| {
    ///     decided.is_none().then(|| user.is_superuser)
    /// });
    /// ```
    pub fn after<U: 'static>(
        f: impl Fn(&U, &str, Option<bool>) -> Option<bool> + Send + Sync + 'static,
    ) {
        global().register_after::<U>(f);
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
    /// the single-action [`Self::allows`] semantic).
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

    /// Async sibling of [`Self::any`]. Sequentially awaits each gate; works
    /// for sync and async registrations alike (via
    /// [`Self::allows_async`]'s dispatch). Sequential rather than
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

    /// Async sibling of [`Self::none`].
    pub async fn none_async<U: 'static, R: 'static>(
        actions: &[&str],
        user: &U,
        resource: &R,
    ) -> bool {
        !Self::any_async(actions, user, resource).await
    }

    /// Async sibling of [`Self::check`].
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
