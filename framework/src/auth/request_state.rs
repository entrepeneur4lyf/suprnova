//! Request-scoped authentication state.
//!
//! Laravel caches the resolved user on the guard *instance* for the
//! duration of a request. Suprnova's guards are container singletons,
//! not per-request objects, so the per-request "who is authenticated
//! right now" slot lives here in a [`tokio::task_local!`] scoped once at
//! the request boundary (`handle_request`), alongside the Inertia flash
//! bag and the SSR-disable flag.
//!
//! This layer is **guard-agnostic on purpose**. Both the session guard
//! and the (stateless) token guard resolve through it, so
//! [`Guard::set_user`](super::Guard::set_user), `once`, and
//! [`Guard::has_user`](super::Guard::has_user) work for token-only
//! requests that never install `SessionMiddleware`.
//!
//! It serves two jobs:
//!
//! 1. **Current user** — the [`Authenticatable`] resolved for this
//!    request. Set by `once`/`once_using_id`/`set_user`, and by a guard's
//!    first `user()` resolution (a per-request cache so repeated lookups
//!    don't re-query the provider — closing a divergence where the old
//!    `Auth::user()` re-queried on every call). `current_user_id` feeds
//!    `Auth::id()` so the static facade sees `once`/`set_user`.
//! 2. **Via-remember flag** — whether the current user was
//!    re-authenticated from a remember-me cookie *this request* (set by
//!    `SessionMiddleware`'s hydration path) rather than from an active
//!    session, surfaced through `StatefulGuard::via_remember`.
//!
//! # Deliberate v1 divergence
//!
//! A single current-user slot means `Auth::guard("api").user()` and
//! `Auth::guard("web").user()` within the *same* request resolve to the
//! same cached user. Laravel caches per-guard. Almost no application
//! mixes guards within one request, so v1 keeps the single slot;
//! per-guard caching can be layered on later without changing this
//! surface.

use std::sync::{Arc, Mutex};

use super::authenticatable::Authenticatable;

/// The per-request authentication slot. See the module docs.
#[derive(Default)]
struct AuthRequestState {
    /// The user resolved for this request, if any.
    current_user: Option<Arc<dyn Authenticatable>>,
    /// Whether the current user came from a remember-me cookie this
    /// request rather than an active session.
    via_remember: bool,
}

tokio::task_local! {
    // `Arc<Mutex<…>>` rather than a bare cell: the future inside
    // `scope` may move across worker threads at `.await` points (so the
    // value must be `Send + Sync`), and setters mutate it after the
    // scope is installed. The guard is only ever held across synchronous
    // closures — never across an `.await` — so the std mutex is sound.
    static AUTH_STATE: Arc<Mutex<AuthRequestState>>;
}

/// Run `fut` with a fresh request-scoped auth state installed.
///
/// Called once per request from `handle_request`, nested next to the
/// Inertia flash-bag and SSR scopes so every middleware and handler
/// downstream can read and write the current user.
pub(crate) async fn scope<F: std::future::Future>(fut: F) -> F::Output {
    AUTH_STATE
        .scope(Arc::new(Mutex::new(AuthRequestState::default())), fut)
        .await
}

/// Set the user resolved for this request.
///
/// No-op when called outside a request scope (e.g. a unit test that did
/// not install one) — the same fail-quiet posture as the session
/// helpers.
pub(crate) fn set_current_user(user: Arc<dyn Authenticatable>) {
    let _ = AUTH_STATE.try_with(|state| {
        state.lock().unwrap_or_else(|e| e.into_inner()).current_user = Some(user);
    });
}

/// The user resolved for this request, if any.
pub(crate) fn current_user() -> Option<Arc<dyn Authenticatable>> {
    AUTH_STATE
        .try_with(|state| {
            state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .current_user
                .clone()
        })
        .ok()
        .flatten()
}

/// The current request user's identifier ([`Authenticatable::get_auth_identifier`]),
/// if a user is resolved. Consulted by `session::auth_user_id` ahead of
/// the persisted session so `once`/`set_user` are visible to the static
/// `Auth` facade.
pub(crate) fn current_user_id() -> Option<String> {
    current_user().map(|user| user.get_auth_identifier())
}

/// Clear the resolved request user (used by `logout`).
pub(crate) fn clear_current_user() {
    let _ = AUTH_STATE.try_with(|state| {
        state.lock().unwrap_or_else(|e| e.into_inner()).current_user = None;
    });
}

/// Whether a user instance has been resolved for this request — without
/// triggering provider resolution. Backs [`Guard::has_user`](super::Guard::has_user).
pub(crate) fn has_current_user() -> bool {
    AUTH_STATE
        .try_with(|state| {
            state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .current_user
                .is_some()
        })
        .unwrap_or(false)
}

/// Mark whether the current user was re-authenticated from a remember-me
/// cookie this request. Set by `SessionMiddleware`'s hydration path.
pub(crate) fn set_via_remember(value: bool) {
    let _ = AUTH_STATE.try_with(|state| {
        state.lock().unwrap_or_else(|e| e.into_inner()).via_remember = value;
    });
}

/// Whether the current user came from a remember-me cookie this request.
/// Backs [`StatefulGuard::via_remember`](super::StatefulGuard::via_remember).
pub(crate) fn via_remember() -> bool {
    AUTH_STATE
        .try_with(|state| state.lock().unwrap_or_else(|e| e.into_inner()).via_remember)
        .unwrap_or(false)
}

/// Test-only: run `fut` with a fresh request-scoped auth state.
///
/// Mirrors the per-request scope `handle_request` installs at runtime,
/// for unit/integration tests that drive guards without booting a
/// server. Crates outside the framework should not need this.
#[doc(hidden)]
pub async fn request_state_scope_for_test<F: std::future::Future>(fut: F) -> F::Output {
    scope(fut).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;

    #[derive(Clone)]
    struct TestUser {
        id: String,
    }

    impl Authenticatable for TestUser {
        fn get_auth_identifier(&self) -> String {
            self.id.clone()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    #[tokio::test]
    async fn current_user_round_trips_within_scope() {
        scope(async {
            assert!(current_user().is_none());
            assert!(!has_current_user());
            assert_eq!(current_user_id(), None);

            set_current_user(Arc::new(TestUser { id: "42".into() }));
            assert!(has_current_user());
            assert_eq!(current_user_id(), Some("42".to_string()));

            clear_current_user();
            assert!(!has_current_user());
            assert_eq!(current_user_id(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn via_remember_round_trips_within_scope() {
        scope(async {
            assert!(!via_remember());
            set_via_remember(true);
            assert!(via_remember());
        })
        .await;
    }

    #[tokio::test]
    async fn helpers_are_inert_outside_a_scope() {
        // No scope installed: getters fall back to None/false and
        // setters silently no-op rather than panic.
        assert!(current_user().is_none());
        assert_eq!(current_user_id(), None);
        assert!(!has_current_user());
        assert!(!via_remember());
        set_current_user(Arc::new(TestUser { id: "1".into() }));
        set_via_remember(true);
        assert!(current_user().is_none());
        assert!(!via_remember());
    }
}
