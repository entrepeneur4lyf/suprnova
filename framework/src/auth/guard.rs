//! Authentication guard (facade)

use std::sync::Arc;

use crate::container::App;
use crate::session::{
    auth_user_id, clear_auth_user, generate_csrf_token, regenerate_session_id, session_mut,
    set_auth_user,
};

use super::authenticatable::Authenticatable;
use super::contract::{Credentials, Guard, StatefulGuard};
use super::manager::AuthManager;
use super::provider::UserProvider;
use super::{events, request_state};
use crate::events::EventFacade;

/// Authentication facade
///
/// Provides Laravel-like static methods for authentication operations.
///
/// # Example
///
/// ```rust,no_run
/// use suprnova::{Auth, Credentials};
///
/// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
/// # let email = "alice@example.com";
/// # let password = "s3cret!";
/// # let remember = true;
/// # let user_id = "42";
/// // Check if authenticated (sync, session-backed)
/// if Auth::check() {
///     let user_id: String = Auth::id().unwrap();
/// }
///
/// // Log in by credentials against the default guard — fires Login +
/// // Authenticated and supports remember-me (requires an AuthManager).
/// Auth::attempt(&Credentials::password(email, password), remember).await?;
///
/// // Or establish the session directly from a known id (sync primitive:
/// // no provider, no AuthManager, no events). Returns Err when called
/// // outside a request scope.
/// Auth::login_id(user_id.to_string())?;
///
/// // Log out — clears the session + request user, revokes remember-me,
/// // and fires Logout.
/// Auth::logout().await?;
/// # Ok(()) }
/// ```
pub struct Auth;

impl Auth {
    /// Get the authenticated user's ID
    ///
    /// Returns None if not authenticated.
    pub fn id() -> Option<String> {
        auth_user_id()
    }

    /// Check if a user is currently authenticated
    pub fn check() -> bool {
        Self::id().is_some()
    }

    /// Check if the current user is a guest (not authenticated)
    pub fn guest() -> bool {
        !Self::check()
    }

    /// Establish a session for a known user id — the synchronous session
    /// primitive behind login.
    ///
    /// The Suprnova-native fast path for "I already have a verified user id,
    /// authenticate it": it writes the id into the session without a
    /// [`UserProvider`] lookup, an [`AuthManager`], lifecycle events, or a
    /// remember-me token. For the Laravel-shaped, guard-backed login — events,
    /// remember-me, provider resolution — use [`login`](Self::login),
    /// [`attempt`](Self::attempt), or [`login_using_id`](Self::login_using_id).
    ///
    /// # Errors
    ///
    /// Returns [`crate::FrameworkError::internal`] when called outside a
    /// request scope (no [`SessionMiddleware`](crate::SessionMiddleware) installed,
    /// or a unit test that forgot to wrap the call in
    /// [`crate::session::session_scope_for_test`]). The previous infallible
    /// signature silently dropped the write — a "successful login" that
    /// never landed — which the caller had no way to detect.
    ///
    /// # Security
    ///
    /// Regenerates the session ID to prevent session fixation, and rotates the
    /// CSRF token.
    pub fn login_id(user_id: impl Into<String>) -> Result<(), crate::error::FrameworkError> {
        if !crate::session::middleware::session_scope_installed() {
            return Err(crate::error::FrameworkError::internal(
                "Auth::login_id called outside a request scope: no SessionMiddleware is \
                 active (or the test forgot session_scope_for_test). The session write \
                 would have been dropped silently.",
            ));
        }

        // Regenerate session ID to prevent session fixation
        regenerate_session_id();

        // Set the authenticated user
        set_auth_user(user_id);

        // Regenerate CSRF token for extra security
        session_mut(|session| {
            session.csrf_token = generate_csrf_token();
        });

        Ok(())
    }

    /// Log in a user AND issue a remember-me token.
    ///
    /// This does a normal session login (regenerating session id + CSRF
    /// token) and additionally:
    ///
    /// 1. Inserts a fresh hashed token into `remember_tokens`.
    /// 2. Queues an encrypted, HttpOnly, SameSite=Lax cookie (Secure
    ///    when `SESSION_SECURE=true`) for the outgoing response.
    /// 3. Sets the cookie's `Max-Age` to match the token's TTL.
    ///
    /// On a future request where the session is missing or expired,
    /// `SessionMiddleware` will verify the cookie's plaintext against
    /// the hashed row, rotate the token (delete + reissue), and
    /// hydrate the session — the user is logged back in transparently.
    ///
    /// # Arguments
    ///
    /// * `user_id` — the user's ID (string).
    /// * `ttl_minutes` — token + cookie lifetime in minutes. Pass
    ///   `SessionConfig::remember_lifetime` (as minutes) for the
    ///   configured default; pass a smaller value for short-lived
    ///   "stay signed in for an hour" UX.
    pub async fn login_remember(
        user_id: impl Into<String>,
        ttl_minutes: i64,
    ) -> Result<(), crate::error::FrameworkError> {
        let user_id = user_id.into();
        // Regular session login (regen session id + CSRF, set user). This
        // also verifies the session scope is installed — failing loud here
        // before the DB row gets written by `issue_remember_cookie`.
        Self::login_id(user_id.clone())?;
        // Issue the row + queue the cookie.
        Self::issue_remember_cookie(&user_id, ttl_minutes).await
    }

    /// Issue the remember-me row + queue the remember-me cookie for the
    /// outgoing response, **without** touching the session id, CSRF
    /// token, or session user slot.
    ///
    /// The remember-me half of [`Auth::login_remember`], factored out so
    /// callers that have already rotated the session id and set the
    /// auth user themselves can opt in without redoing the session
    /// dance. The principal user today is
    /// [`crate::auth_flows::TwoFactor::complete_challenge`]: it rotates
    /// session id + CSRF + sets the auth user as part of promoting
    /// pending → authed, then conditionally calls this when the user
    /// requested remember-me at password-login time.
    ///
    /// For a fresh login flow that has not yet established the session,
    /// call [`login_remember`](Self::login_remember) instead — it
    /// handles session id rotation + CSRF + auth user + remember-me in
    /// one step.
    pub async fn issue_remember_cookie(
        user_id: &str,
        ttl_minutes: i64,
    ) -> Result<(), crate::error::FrameworkError> {
        // Pre-flight: refuse to insert the DB row when no pending-cookies
        // scope is installed. Otherwise `push_pending_cookie` below would
        // silently drop the cookie and the row would be orphaned — the
        // client receives no durable login state but the database holds
        // a live token. Single task = task-local cannot vanish mid-fn,
        // so no TOCTOU between this check and the push.
        if !crate::session::middleware::pending_cookies_scope_installed() {
            return Err(crate::error::FrameworkError::internal(
                "Auth::issue_remember_cookie called outside a request scope: no \
                 SessionMiddleware is active (or the test forgot \
                 pending_cookies_scope_for_test). Refusing to insert a remember-me \
                 row that would orphan when the cookie cannot reach the response.",
            ));
        }

        // Issue the row. Returns the plaintext destined for the cookie.
        let plaintext = super::remember::issue(user_id, ttl_minutes).await?;

        // Build + queue the cookie. The cookie's `Max-Age` matches the
        // row's TTL (`ttl_minutes` converted to seconds) so the browser
        // stops sending the cookie the moment the row expires — the
        // attribute does not "lie" about validity.
        let config = crate::session::SessionConfig::from_env();
        let max_age =
            std::time::Duration::from_secs((ttl_minutes.max(0) as u64).saturating_mul(60));
        let cookie =
            crate::session::middleware::create_remember_cookie(&config, &plaintext, max_age)?;
        let queued = crate::session::middleware::push_pending_cookie(cookie);
        // The pre-flight above guarantees `queued == true`; the assert is
        // a belt-and-suspenders against future refactors removing the
        // pre-flight without removing the row write.
        debug_assert!(
            queued,
            "pending-cookies scope was installed at pre-flight but lost by push time"
        );

        Ok(())
    }

    /// Revoke every remember-me token for the currently-authenticated
    /// user AND queue a "clear" cookie on the outgoing response.
    ///
    /// Chained automatically from `Auth::logout`. Also the right hook
    /// for a "log me out everywhere" account-security button — call
    /// it without a preceding `logout` to invalidate every device
    /// while keeping the current session active.
    ///
    /// Returns the number of rows deleted (0 if the user had no
    /// remember-me tokens).
    pub async fn revoke_remember_tokens() -> Result<u64, crate::error::FrameworkError> {
        match Self::id() {
            Some(id) => Self::revoke_remember_tokens_for_user(&id).await,
            None => {
                // No authenticated user — still queue the clear cookie
                // so any stale one in the browser's jar is dropped.
                Self::queue_remember_clear_cookie();
                Ok(0)
            }
        }
    }

    /// Revoke every remember-me token for `user_id` AND queue a
    /// "clear" cookie on the outgoing response. Identical to
    /// [`revoke_remember_tokens`](Self::revoke_remember_tokens)
    /// but identifies the target user explicitly instead of via
    /// [`Auth::id()`](Self::id), so the caller can safely tear
    /// down the session's auth slot **before** invoking the
    /// revoke (rather than ordering revoke first so `Auth::id()`
    /// still resolves).
    ///
    /// The canonical user is
    /// [`crate::auth_flows::TwoFactor::start_challenge`], which
    /// needs fail-closed ordering: clear the auth slot first so a
    /// transient revoke failure cannot leave a fully-authed
    /// session bypassing the 2FA gate. With this method, the
    /// challenge demote saves `Auth::id()` to a local, clears
    /// auth, and only then calls the revoke — a mid-step failure
    /// can no longer strand the session in an authed-but-pending
    /// state.
    ///
    /// # Cookie-first ordering
    ///
    /// The clear cookie is queued **before** the row-delete query.
    /// If the delete fails (DB transient outage, lock timeout),
    /// the response still carries the clear-cookie directive — the
    /// browser drops the cookie on the way out and cannot re-
    /// authenticate via remember-me on the next request, even
    /// though the stale row in the database hasn't been removed
    /// yet (a future `prune_expired` sweep will). Reversing the
    /// order would leave a "row alive + cookie alive" window after
    /// a transient revoke error, which the remember-me middleware
    /// would happily use to log the user back in — defeating the
    /// revoke entirely.
    ///
    /// Returns the number of rows deleted (0 if the user had no
    /// remember-me tokens). Same audit listeners fire (none from
    /// the framework's auth surface; the `remember_tokens` table
    /// has no event).
    pub async fn revoke_remember_tokens_for_user(
        user_id: &str,
    ) -> Result<u64, crate::error::FrameworkError> {
        // Queue the clear cookie FIRST. queue_remember_clear_cookie
        // is infallible (env-config read + sync Cookie::build + sync
        // push into the per-request slot), so this branch never
        // errors before the response carries the clear directive.
        Self::queue_remember_clear_cookie();
        // THEN attempt the row delete. A failure here propagates as
        // Err to the caller — but the browser will drop the cookie
        // regardless, so the stale DB row cannot be exploited.
        super::remember::revoke_all_for_user(user_id).await
    }

    /// Queue the "forget remember-me" cookie on the outgoing response.
    /// Internal helper — callers want `revoke_remember_tokens` or
    /// `revoke_remember_tokens_for_user`, which queue the clear cookie
    /// as part of their contract. Centralised here so the cookie
    /// attributes stay in one place.
    fn queue_remember_clear_cookie() {
        let config = crate::session::SessionConfig::from_env();
        let clear = crate::session::middleware::create_forget_remember_cookie(&config);
        // A clear cookie is a defensive add — when there is no scope to push
        // into (no SessionMiddleware) there is no orphan state to worry about,
        // and the browser simply keeps whatever stale cookie it held. Dropping
        // the result here is intentional, unlike the issue path which gates a
        // DB row write on the push succeeding.
        let _ = crate::session::middleware::push_pending_cookie(clear);
    }

    /// Tear down all authentication state for the current request: revoke the
    /// user's remember-me tokens, clear the session user, clear the
    /// request-scoped current user, clear any in-flight 2FA pending state,
    /// and rotate the CSRF token.
    ///
    /// The event-free core shared by [`logout`](Self::logout) and a guard's
    /// `logout`; the caller dispatches the [`Logout`](crate::auth::events::Logout)
    /// event so it is emitted exactly once, attributed to the right guard.
    ///
    /// # Fail-closed ordering
    ///
    /// The session-state teardown runs **before** the remember-me revoke.
    /// If the revoke errors (DB transient outage, lock timeout), this
    /// method still returns `Err` — but the session is already in a
    /// logged-out state, so a stale auth slot cannot survive the failed
    /// logout. Reversing the order would leave a user fully authenticated
    /// when the revoke failed (the original implementation's gap).
    pub(crate) async fn clear_authentication() -> Result<(), crate::error::FrameworkError> {
        // Capture the authenticated id BEFORE clearing — the revoke
        // below needs it, and `Auth::id()` is about to become `None`.
        let saved_id = Self::id();

        // STEP 1: Clear session auth + request-scoped cache + 2FA
        // pending state, and rotate the CSRF token. Clearing
        // request_state is essential: `Auth::id` consults it ahead of
        // the session, so a user resolved this request would otherwise
        // survive logout and `Auth::id()` would keep reporting it.
        // Both 2FA pending slots (user-id + remember preference) are
        // authentication state — a tear-down that drops one but leaves
        // the other strands the state machine.
        clear_auth_user();
        request_state::clear_current_user();
        crate::session::middleware::clear_two_factor_pending();
        crate::session::middleware::clear_two_factor_pending_remember();
        session_mut(|session| {
            session.csrf_token = generate_csrf_token();
        });

        // STEP 2: Revoke remember-me using the saved id. A failure
        // here propagates as `Err` to the caller, but the session is
        // already in a safe logged-out state — the failed revoke
        // queues the clear cookie first (cookie-first ordering on
        // `revoke_remember_tokens_for_user`), so the browser drops
        // the cookie regardless, and the stale DB row cannot be
        // exploited until a future prune sweeps it.
        match saved_id {
            Some(id) => {
                Self::revoke_remember_tokens_for_user(&id).await?;
            }
            None => {
                // No prior auth — still queue the clear cookie in
                // case the browser holds a stale one from another
                // session. Matches `revoke_remember_tokens`'s
                // contract for the no-id branch.
                Self::queue_remember_clear_cookie();
            }
        }

        Ok(())
    }

    /// Log out the current user.
    ///
    /// Clears the session user and the request-scoped current user, revokes
    /// every remember-me token for that user (so reopening the browser does
    /// not silently log them back in), rotates the CSRF token, and dispatches
    /// a [`Logout`](crate::auth::events::Logout) event attributed to the
    /// default guard. Mirrors Laravel's `Auth::logout()`.
    ///
    /// Works without an [`AuthManager`]: the event is attributed to the
    /// configured default guard name when a manager is registered, and to
    /// `"web"` otherwise.
    pub async fn logout() -> Result<(), crate::error::FrameworkError> {
        // Capture the id before clearing so the event is attributed.
        let user_id = Self::id();
        Self::clear_authentication().await?;
        EventFacade::dispatch(events::Logout {
            guard: Self::default_guard_name(),
            user_id,
        })
        .await?;
        Ok(())
    }

    /// Log out and invalidate the entire session.
    ///
    /// Use this for complete session destruction (e.g. "log out everywhere").
    /// Flushes the whole session (not just the auth user), revokes every
    /// remember-me token for the user, clears the request-scoped current user,
    /// and dispatches a [`Logout`](crate::auth::events::Logout) event.
    pub async fn logout_and_invalidate() -> Result<(), crate::error::FrameworkError> {
        // Capture the id before flushing so the event is attributed
        // AND so the revoke below can target the right user after the
        // session is gone. `Auth::id()` returns `None` once we flush.
        let user_id = Self::id();

        // STEP 1: Destroy session + request_state FIRST. Even if the
        // revoke errors below, the session is already gone — every
        // bit of auth state is wiped. Reversing the order (revoke
        // first, then flush) leaves a fully-authed session if the
        // revoke fails, defeating the point of `logout_and_invalidate`.
        //
        // The session-id rotation matters here in a way it doesn't
        // for plain `logout`: `flush()` clears `data` + `user_id` but
        // leaves `session.id` intact, so without an explicit
        // `regenerate_session_id` the destroyed session and the next
        // session would share an ID — defeating "complete session
        // destruction." Laravel's `session()->invalidate()` is
        // explicitly `flush()` + `regenerate()`; we match that here.
        regenerate_session_id();
        session_mut(|session| {
            session.flush();
            session.csrf_token = generate_csrf_token();
        });
        request_state::clear_current_user();

        // STEP 2: Revoke remember-me using the captured id. Same
        // cookie-first ordering as `clear_authentication`: even if
        // the DB DELETE fails, the response carries the clear cookie
        // and the browser drops the remember-me cookie on its way out.
        if let Some(ref id) = user_id {
            Self::revoke_remember_tokens_for_user(id).await?;
        } else {
            // No prior auth — queue the clear cookie defensively.
            Self::queue_remember_clear_cookie();
        }

        EventFacade::dispatch(events::Logout {
            guard: Self::default_guard_name(),
            user_id,
        })
        .await?;
        Ok(())
    }

    /// Get the currently authenticated user
    ///
    /// Returns `None` if not authenticated or if no `UserProvider` is registered.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use suprnova::Auth;
    ///
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// if let Some(user) = Auth::user().await? {
    ///     println!("Logged in as user {}", user.auth_identifier());
    /// }
    /// # Ok(()) }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if no `UserProvider` is registered in the container.
    /// Make sure to register a `UserProvider` in your `bootstrap.rs`:
    ///
    /// ```rust,no_run
    /// # use suprnova::{bind, UserProvider, DatabaseUserProvider};
    /// bind!(dyn UserProvider, DatabaseUserProvider::new("users"));
    /// ```
    pub async fn user() -> Result<Option<Arc<dyn Authenticatable>>, crate::error::FrameworkError> {
        // Named-guard system when configured: the default guard checks the
        // request-scoped cache, resolves through the AuthManager's provider, and
        // caches the result — so this stays consistent with `Auth::attempt` /
        // `Auth::guard("web").user()`. `default_guard()` resolves the provider
        // eagerly, so a registered-but-providerless AuthManager returns `Err`
        // here and falls through to the legacy branch rather than half-resolving.
        if let Ok(guard) = Self::default_guard() {
            return guard.user().await;
        }

        // Legacy fallback (no AuthManager): the request-scoped cache first (so
        // `set_user` / `once` users surface), then a globally-bound
        // `UserProvider`, caching the result for the rest of the request.
        if let Some(user) = request_state::current_user() {
            return Ok(Some(user));
        }
        let user_id = match Self::id() {
            Some(id) => id,
            None => return Ok(None),
        };
        let provider = App::make::<dyn UserProvider>().ok_or_else(|| {
            crate::error::FrameworkError::internal(
                "No user provider configured. Register one with \
                 Auth::register_provider(\"users\", Arc::new(...)) (named-guard system), \
                 or bind!(dyn UserProvider, ...) in bootstrap.rs (legacy)."
                    .to_string(),
            )
        })?;
        let user = provider.retrieve_by_id(&user_id).await?;
        if let Some(ref u) = user {
            request_state::set_current_user(u.clone());
        }
        Ok(user)
    }

    /// Get the currently authenticated user, or fail with an unauthorised
    /// error.
    ///
    /// Mirrors Laravel's `Auth::userOrFail()`. Use this in handlers that
    /// have already passed an auth middleware — the request is known to be
    /// authenticated, so resolving the user is expected to succeed, and a
    /// missing user means the precondition was violated rather than that
    /// the handler must branch on a `None`. The `?` operator then turns the
    /// error into the framework's standard "unauthorised" response.
    ///
    /// # Errors
    ///
    /// - [`crate::FrameworkError::Unauthorized`] when no user is currently
    ///   authenticated (i.e. [`Auth::user`](Self::user) returns
    ///   `Ok(None)`).
    /// - Whatever error [`Auth::user`](Self::user) returns when the
    ///   underlying provider lookup fails.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use suprnova::Auth;
    ///
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// // In a handler behind `Authenticate` middleware:
    /// let user = Auth::user_or_fail().await?;
    /// println!("Welcome, user {}!", user.get_auth_identifier());
    /// # Ok(()) }
    /// ```
    pub async fn user_or_fail() -> Result<Arc<dyn Authenticatable>, crate::error::FrameworkError> {
        match Self::user().await? {
            Some(user) => Ok(user),
            None => Err(crate::error::FrameworkError::Unauthorized),
        }
    }

    /// Get the authenticated user, cast to a concrete type
    ///
    /// This is a convenience method that retrieves the user and downcasts
    /// it to your concrete User type. Returns `None` when no user is
    /// authenticated *or* when the resolved user is not a `T` (e.g. after
    /// `set_user` of a different type).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use std::any::Any;
    /// # use std::sync::Arc;
    /// # use suprnova::Authenticatable;
    /// use suprnova::Auth;
    ///
    /// # #[derive(Clone)]
    /// # struct User { id: u64 }
    /// # impl Authenticatable for User {
    /// #     fn get_auth_identifier(&self) -> String { self.id.to_string() }
    /// #     fn as_any(&self) -> &dyn Any { self }
    /// #     fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> { self }
    /// # }
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// if let Some(user) = Auth::user_as::<User>().await? {
    ///     println!("Welcome, user #{}!", user.id);
    /// }
    /// # Ok(()) }
    /// ```
    ///
    /// # Performance — when to prefer the `Arc` sibling
    ///
    /// Each call clones the concrete `T`. Handlers that need the user
    /// in multiple `tokio::spawn` arms — or look it up several times
    /// per request — should reach for [`Auth::user_as_arc`] instead and
    /// share the `Arc<T>`. The first call still pays the underlying
    /// clone (the trait surface keeps `Arc<dyn Authenticatable>`
    /// internally rather than `Arc<dyn Any>`); subsequent shares are
    /// pointer copies.
    ///
    /// # Type Parameters
    ///
    /// * `T` - The concrete user type that implements `Authenticatable` and `Clone`
    pub async fn user_as<T: Authenticatable + Clone>()
    -> Result<Option<T>, crate::error::FrameworkError> {
        let user = Self::user().await?;
        Ok(user.and_then(|u| u.as_any().downcast_ref::<T>().cloned()))
    }

    /// `Arc`-returning sibling of [`Auth::user_as`]. Use when the
    /// handler needs to share the user value across multiple spawned
    /// tasks or borrow it through multiple lookup sites — the
    /// returned `Arc<T>` is the same allocation the request-state
    /// cache already holds, so neither the lookup nor downstream
    /// shares perform a `T` clone.
    ///
    /// `Auth::user()` already returns `Arc<dyn Authenticatable>`;
    /// `Authenticatable::into_arc_any` is the trait hook that turns
    /// that into `Arc<dyn Any + Send + Sync>` so
    /// `Arc::downcast::<T>` can succeed without copying the concrete
    /// user value. Returns `None` when no user is authenticated **or**
    /// when the resolved user is not a `T`.
    pub async fn user_as_arc<T: Authenticatable>()
    -> Result<Option<Arc<T>>, crate::error::FrameworkError> {
        let Some(user) = Self::user().await? else {
            return Ok(None);
        };
        let any_arc = user.into_arc_any();
        Ok(any_arc.downcast::<T>().ok())
    }

    // ── Named guards (AuthManager) ──────────────────────────────────────────────

    /// Resolve the [`AuthManager`] from the container, with a remediation
    /// message when it has not been registered.
    fn manager() -> Result<AuthManager, crate::error::FrameworkError> {
        App::get::<AuthManager>().ok_or_else(|| {
            crate::error::FrameworkError::internal(
                "No AuthManager registered. Register one in bootstrap.rs with: \
                 App::singleton(AuthManager::new(AuthConfig::from_env()))"
                    .to_string(),
            )
        })
    }

    /// The default guard's name for event attribution: from the registered
    /// [`AuthManager`] when present, falling back to `"web"` so logout events
    /// stay attributed even before a manager is wired up.
    ///
    /// `pub` so peer subsystems (notably the 2FA challenge flow) can
    /// attribute their own `Login` / `Authenticated` dispatches to the
    /// same guard the rest of the auth surface uses, without having to
    /// pull `AuthManager` directly.
    pub fn default_guard_name() -> String {
        App::get::<AuthManager>()
            .map(|m| m.default_guard_name().to_string())
            .unwrap_or_else(|| "web".to_string())
    }

    /// Register a [`UserProvider`] under `name` on the [`AuthManager`].
    ///
    /// Guards reference providers by this name (see [`crate::GuardConfig`]).
    /// This is the Rust-native half of Laravel's `config/auth.php`
    /// `providers` section: the instance is registered programmatically
    /// because a Suprnova provider carries a Rust type.
    ///
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use suprnova::{Auth, DatabaseUserProvider};
    /// # fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// # let my_user_provider = DatabaseUserProvider::new("users");
    /// Auth::register_provider("users", Arc::new(my_user_provider))?;
    /// # Ok(()) }
    /// ```
    pub fn register_provider(
        name: impl Into<String>,
        provider: Arc<dyn UserProvider>,
    ) -> Result<(), crate::error::FrameworkError> {
        Self::manager()?.register_provider(name, provider);
        Ok(())
    }

    /// Resolve a named guard as the read-only [`Guard`] contract.
    ///
    /// ```rust,no_run
    /// # use suprnova::Auth;
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// if Auth::guard("api")?.check().await? { /* ... */ }
    /// # Ok(()) }
    /// ```
    pub fn guard(name: &str) -> Result<Arc<dyn Guard>, crate::error::FrameworkError> {
        Self::manager()?.guard(name)
    }

    /// Resolve a named guard as a [`StatefulGuard`] (login/logout/attempt).
    ///
    /// Errors if the named guard is stateless (a token guard).
    ///
    /// ```rust,no_run
    /// # use suprnova::{Auth, Credentials};
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// # let email = "alice@example.com";
    /// # let password = "s3cret!";
    /// # let remember = true;
    /// let user = Auth::stateful_guard("web")?
    ///     .attempt(&Credentials::password(email, password), remember)
    ///     .await?;
    /// # Ok(()) }
    /// ```
    pub fn stateful_guard(
        name: &str,
    ) -> Result<Arc<dyn StatefulGuard>, crate::error::FrameworkError> {
        Self::manager()?.stateful_guard(name)
    }

    // ── Laravel default-guard delegation ────────────────────────────────────────
    //
    // Laravel's `Auth::attempt/login/once/validate/...` proxy to the
    // application's *default* guard. These mirror that: each resolves the
    // default guard from the container-bound `AuthManager` and forwards the
    // call, so they require a manager (`App::singleton(AuthManager::new(...))`)
    // and fail loud with that remediation otherwise — the same contract as
    // [`Auth::guard`](Self::guard). The sync session fast paths
    // (`id`/`check`/`guest`/`via_remember`) and the session primitives
    // (`login_id`/`logout`) stay manager-free.

    /// Resolve the default guard as a read-only [`Guard`].
    fn default_guard() -> Result<Arc<dyn Guard>, crate::error::FrameworkError> {
        Self::manager()?.default_guard()
    }

    /// Resolve the default guard as a [`StatefulGuard`].
    fn default_stateful_guard() -> Result<Arc<dyn StatefulGuard>, crate::error::FrameworkError> {
        Self::manager()?.default_stateful_guard()
    }

    /// Validate credentials and, on success, log the user into the default
    /// guard — optionally issuing a remember-me token. Mirrors Laravel's
    /// `Auth::attempt($credentials, $remember)`.
    ///
    /// Returns the resolved user on success (richer than Laravel's `bool` — no
    /// follow-up [`Auth::user`](Self::user) call needed), `Ok(None)` on bad
    /// credentials, and `Err` only on an underlying failure (database, hashing,
    /// or no [`AuthManager`] registered).
    pub async fn attempt(
        credentials: &Credentials,
        remember: bool,
    ) -> Result<Option<Arc<dyn Authenticatable>>, crate::error::FrameworkError> {
        Self::default_stateful_guard()?
            .attempt(credentials, remember)
            .await
    }

    /// Validate credentials and authenticate for the current request only (no
    /// session persistence). Mirrors Laravel's `Auth::once($credentials)`.
    pub async fn once(credentials: &Credentials) -> Result<bool, crate::error::FrameworkError> {
        Self::default_stateful_guard()?.once(credentials).await
    }

    /// Log a known user into the default guard, optionally issuing a
    /// remember-me token. Mirrors Laravel's `Auth::login($user, $remember)`.
    ///
    /// For the synchronous "I only have a verified id" path that needs no
    /// provider or manager, use [`login_id`](Self::login_id).
    pub async fn login(
        user: Arc<dyn Authenticatable>,
        remember: bool,
    ) -> Result<(), crate::error::FrameworkError> {
        Self::default_stateful_guard()?.login(user, remember).await
    }

    /// Log a user into the default guard by their identifier, optionally
    /// issuing a remember-me token. Mirrors Laravel's
    /// `Auth::loginUsingId($id, $remember)`.
    ///
    /// Returns the resolved user (richer than Laravel's `Authenticatable|false`),
    /// or `Ok(None)` if the provider has no such id.
    pub async fn login_using_id(
        id: &str,
        remember: bool,
    ) -> Result<Option<Arc<dyn Authenticatable>>, crate::error::FrameworkError> {
        Self::default_stateful_guard()?
            .login_using_id(id, remember)
            .await
    }

    /// Authenticate by id against the default guard for the current request
    /// only (no session persistence). Mirrors Laravel's `Auth::onceUsingId($id)`.
    ///
    /// Returns the resolved user, or `Ok(None)` if the provider has no such id.
    pub async fn once_using_id(
        id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, crate::error::FrameworkError> {
        Self::default_stateful_guard()?.once_using_id(id).await
    }

    /// Validate credentials against the default guard's provider without
    /// logging in. Mirrors Laravel's `Auth::validate($credentials)`.
    pub async fn validate(credentials: &Credentials) -> Result<bool, crate::error::FrameworkError> {
        Self::default_guard()?.validate(credentials).await
    }

    /// Whether the current user was authenticated via a remember-me cookie
    /// this request (rather than from an active session). Mirrors Laravel's
    /// `Auth::viaRemember()`.
    ///
    /// Reads the request-scoped auth state directly, so — like
    /// [`id`](Self::id) / [`check`](Self::check) — it needs no [`AuthManager`]
    /// and never fails.
    pub fn via_remember() -> bool {
        request_state::via_remember()
    }

    /// Set the current user for this request **without** persisting to the
    /// session — the in-memory equivalent of [`once`](Self::once). Mirrors
    /// Laravel's `Auth::setUser($user)`.
    ///
    /// After this call, [`id`](Self::id) / [`check`](Self::check) /
    /// [`user`](Self::user) reflect `user` for the remainder of the request.
    /// The request-scoped current user is shared by every guard, so the facade
    /// writes it directly — no [`AuthManager`] needed (the same manager-free
    /// fast path as `id`/`check`/`guest`).
    pub fn set_user(user: Arc<dyn Authenticatable>) {
        request_state::set_current_user(user);
    }

    /// Whether a user has already been resolved for this request, without
    /// triggering provider resolution. Mirrors Laravel's `Auth::hasUser()`.
    ///
    /// `true` after a `login`/`once`/`set_user` or a prior `user()` lookup;
    /// `false` when only a session id is present but no user has been fetched.
    /// Reads the request-scoped state directly (manager-free).
    pub fn has_user() -> bool {
        request_state::has_current_user()
    }

    // ── Torii-backed authentication providers ──────────────────────────────────

    /// Access password-based authentication operations.
    ///
    /// Requires that [`crate::torii_integration::init_torii`] has been called first.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use suprnova::Auth;
    ///
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let user = Auth::password().register("alice@example.com", "s3cret!").await?;
    /// let (user, session) = Auth::password()
    ///     .authenticate("alice@example.com", "s3cret!", None, None)
    ///     .await?;
    /// # Ok(()) }
    /// ```
    pub fn password() -> crate::torii_integration::password::PasswordAuth {
        crate::torii_integration::password::PasswordAuth
    }

    /// Access OAuth authentication operations for a given provider.
    ///
    /// # Arguments
    ///
    /// * `provider` - The OAuth provider name (e.g., `"github"`, `"google"`).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use suprnova::Auth;
    /// use suprnova::torii_integration::oauth::OAuthProviderConfig;
    ///
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// # let code = String::from("auth-code-from-callback");
    /// # let state = String::from("state-from-callback");
    /// Auth::oauth("github").configure(OAuthProviderConfig {
    ///     client_id: "...".into(),
    ///     client_secret: "...".into(),
    ///     redirect_url: "http://localhost:8000/auth/oauth/github/callback".into(),
    ///     scopes: vec!["user:email".into()],
    ///     endpoints_override: None, // use the well-known GitHub endpoints
    ///     apple_key_pair: None,
    ///     apple_team_id: None,
    /// });
    ///
    /// let kickoff = Auth::oauth("github").begin().await?;
    /// // Redirect user to kickoff.authorization_url, store kickoff.state in session.
    ///
    /// let (user, session) = Auth::oauth("github").complete(&code, &state).await?;
    /// # Ok(()) }
    /// ```
    pub fn oauth(provider: impl Into<String>) -> crate::torii_integration::oauth::OAuthAuth {
        crate::torii_integration::oauth::OAuthAuth::new(provider.into())
    }

    /// Access passkey (WebAuthn/FIDO2) authentication operations.
    ///
    /// Full implementation coming in P3T7.
    pub fn passkey() -> crate::torii_integration::passkey::PasskeyAuth {
        crate::torii_integration::passkey::PasskeyAuth
    }

    /// Access magic-link authentication operations.
    ///
    /// Full implementation coming in P3T5.
    pub fn magic_link() -> crate::torii_integration::magic_link::MagicLinkAuth {
        crate::torii_integration::magic_link::MagicLinkAuth
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthConfig, AuthManager};
    use crate::container::testing::TestContainer;
    use async_trait::async_trait;
    use std::any::Any;

    #[derive(Clone)]
    struct TestUser;
    impl Authenticatable for TestUser {
        fn get_auth_identifier(&self) -> String {
            "7".to_string()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    // Knows one user: id `"7"`, email `"a@b.com"`, password `"secret"`.
    struct FakeProvider;
    #[async_trait]
    impl UserProvider for FakeProvider {
        async fn retrieve_by_id(
            &self,
            id: &str,
        ) -> Result<Option<Arc<dyn Authenticatable>>, crate::error::FrameworkError> {
            Ok((id == "7").then(|| Arc::new(TestUser) as Arc<dyn Authenticatable>))
        }

        async fn retrieve_by_credentials(
            &self,
            credentials: &serde_json::Value,
        ) -> Result<Option<Arc<dyn Authenticatable>>, crate::error::FrameworkError> {
            let email = credentials.get("email").and_then(|v| v.as_str());
            Ok((email == Some("a@b.com")).then(|| Arc::new(TestUser) as Arc<dyn Authenticatable>))
        }

        async fn validate_credentials(
            &self,
            _user: &dyn Authenticatable,
            credentials: &serde_json::Value,
        ) -> Result<bool, crate::error::FrameworkError> {
            Ok(credentials.get("password").and_then(|v| v.as_str()) == Some("secret"))
        }
    }

    // The facade resolves the container-bound manager, registers a provider
    // through it, and projects both guard contracts. Uses TestContainer
    // (thread-local) so it stays parallel-safe — never `App::singleton`.
    #[tokio::test]
    async fn facade_registers_provider_and_resolves_named_guards() {
        let _scope = TestContainer::fake();
        TestContainer::singleton(AuthManager::new(AuthConfig::default()));

        Auth::register_provider("users", Arc::new(FakeProvider)).expect("register provider");

        // The session guard projects as both Guard and StatefulGuard.
        assert!(Auth::guard("web").is_ok());
        assert!(Auth::stateful_guard("web").is_ok());

        // The token guard projects as Guard but not StatefulGuard.
        assert!(Auth::guard("api").is_ok());
        assert!(Auth::stateful_guard("api").is_err());
    }

    // `Auth::validate` routes through the default guard's provider without
    // logging anyone in (no events, no session), so it is parallel-safe to
    // assert in-crate. Event-dispatching delegation (`attempt`/`login`/`once`)
    // is covered in `tests/auth_session_guard.rs`, which isolates the
    // process-global event fake in its own test binary.
    #[tokio::test]
    async fn validate_delegates_to_default_guard() {
        let _scope = TestContainer::fake();
        TestContainer::singleton(AuthManager::new(AuthConfig::default()));
        Auth::register_provider("users", Arc::new(FakeProvider)).expect("register provider");

        assert!(
            Auth::validate(&Credentials::password("a@b.com", "secret"))
                .await
                .expect("validate routes through provider")
        );
        assert!(
            !Auth::validate(&Credentials::password("a@b.com", "wrong"))
                .await
                .expect("validate routes through provider")
        );
    }

    // The stateful facade methods fail loud (a remediation error, never a
    // silent success) when no AuthManager is registered. They error in
    // `manager()` before reaching any guard, so this needs no request scope.
    #[tokio::test]
    async fn stateful_methods_fail_loud_without_manager() {
        let _scope = TestContainer::fake();
        // No AuthManager registered.
        let err = Auth::attempt(&Credentials::password("a@b.com", "secret"), false)
            .await
            .err()
            .expect("attempt without a manager must error");
        assert!(err.to_string().contains("AuthManager"), "got: {err}");
    }

    // `set_user`/`has_user` write+read the request-scoped current user directly
    // (manager-free); `Auth::id` then sees it because it consults request_state
    // ahead of the session. Needs only a request scope — no manager, no events.
    #[tokio::test]
    async fn set_user_and_has_user_round_trip_in_request_scope() {
        request_state::request_state_scope_for_test(async {
            assert!(!Auth::has_user());
            assert_eq!(Auth::id(), None);

            Auth::set_user(Arc::new(TestUser));
            assert!(Auth::has_user());
            assert_eq!(Auth::id(), Some("7".to_string()));
        })
        .await;
    }

    // `Auth::user()` routes through the default guard (the AuthManager provider)
    // and the request-scoped cache — so `set_user` surfaces and it no longer
    // depends on a legacy `bind!(dyn UserProvider)`. Before unification this
    // errored here: the old impl went straight to `App::make`, which has no
    // binding under `TestContainer`.
    #[tokio::test]
    async fn user_resolves_through_default_guard_and_request_state() {
        let _scope = TestContainer::fake();
        TestContainer::singleton(AuthManager::new(AuthConfig::default()));
        Auth::register_provider("users", Arc::new(FakeProvider)).expect("register provider");

        request_state::request_state_scope_for_test(async {
            // Nothing resolved yet.
            assert!(Auth::user().await.expect("user ok").is_none());

            // `set_user` surfaces through `Auth::user()` via the request cache.
            Auth::set_user(Arc::new(TestUser));
            let user = Auth::user().await.expect("user ok").expect("user present");
            assert_eq!(user.get_auth_identifier(), "7");

            // And downcasts through `user_as`.
            assert!(
                Auth::user_as::<TestUser>()
                    .await
                    .expect("user_as ok")
                    .is_some()
            );
        })
        .await;
    }

    // `user_or_fail` is the Laravel `Auth::userOrFail()` analogue: surface
    // the authenticated user OR turn the absence into a typed error so a
    // handler can propagate via `?`. Inside a request scope with no user
    // resolved it must return `Err(Unauthorized)`; with a `set_user` it
    // must surface that same user.
    #[tokio::test]
    async fn user_or_fail_errors_when_unauthenticated_and_returns_user_otherwise() {
        let _scope = TestContainer::fake();
        TestContainer::singleton(AuthManager::new(AuthConfig::default()));
        Auth::register_provider("users", Arc::new(FakeProvider)).expect("register provider");

        request_state::request_state_scope_for_test(async {
            // No user resolved → typed Unauthorized error. `Arc<dyn Authenticatable>`
            // is not `Debug`, so unwrap-by-match rather than `expect_err`.
            match Auth::user_or_fail().await {
                Err(crate::error::FrameworkError::Unauthorized) => {}
                Err(other) => panic!("expected FrameworkError::Unauthorized; got: {other:?}"),
                Ok(_) => panic!("user_or_fail must error when no user is authenticated"),
            }

            // After `set_user` the request-scoped current user surfaces.
            Auth::set_user(Arc::new(TestUser));
            let user = Auth::user_or_fail()
                .await
                .expect("user_or_fail must return the authenticated user");
            assert_eq!(user.get_auth_identifier(), "7");
        })
        .await;
    }

    // `login_id` writes into the session via the per-request task-local. Inside
    // a session scope it must succeed AND the write must be visible via
    // `Auth::id()`. The audit's "silent no-op" was that the write disappeared
    // when the scope was absent — that case is the next test.
    #[tokio::test]
    async fn login_id_succeeds_inside_session_scope() {
        let slot = crate::session::new_session_slot_for_test();
        crate::session::session_scope_for_test(slot, async {
            Auth::login_id("alice-42").expect("login_id should succeed inside a session scope");
            // The id surfaces immediately via the session-backed read path.
            assert_eq!(Auth::id(), Some("alice-42".to_string()));
        })
        .await;
    }

    // Outside a session scope `login_id` previously dropped the write and still
    // returned `()`. It must now return a loud `Err` so the caller cannot
    // mistake a no-op for a successful login.
    #[tokio::test]
    async fn login_id_errors_outside_session_scope() {
        let err = Auth::login_id("alice-42")
            .expect_err("login_id outside a session scope must error, not silently no-op");
        let msg = err.to_string();
        assert!(
            msg.contains("login_id") && msg.contains("session"),
            "error must point at the cause; got: {msg}"
        );
    }

    // `issue_remember_cookie` writes a DB row AND queues a cookie. With no
    // pending-cookies scope installed, the cookie would be dropped — the
    // audit's orphan-token bug. It must pre-flight and refuse before the
    // row write, so this test only needs a session scope to exercise the
    // pending-cookies absence (we never reach the DB).
    #[tokio::test]
    async fn issue_remember_cookie_errors_without_pending_cookies_scope() {
        let slot = crate::session::new_session_slot_for_test();
        crate::session::session_scope_for_test(slot, async {
            let err = Auth::issue_remember_cookie("alice-42", 60)
                .await
                .expect_err("issue_remember_cookie without pending-cookies scope must error");
            let msg = err.to_string();
            assert!(
                msg.contains("issue_remember_cookie") && msg.contains("scope"),
                "error must point at the cause; got: {msg}"
            );
        })
        .await;
    }

    // `login_remember` is the public entry. It calls `login_id` first, so
    // the no-session-scope case errors out before the row write — the same
    // fail-loud guarantee. The pending-cookies path is exercised in
    // `tests/remember_me.rs` against a live DB.
    #[tokio::test]
    async fn login_remember_errors_outside_session_scope() {
        let err = Auth::login_remember("alice-42", 60)
            .await
            .expect_err("login_remember outside a session scope must error");
        let msg = err.to_string();
        assert!(
            msg.contains("login_id") && msg.contains("session"),
            "error must point at the underlying login_id cause; got: {msg}"
        );
    }

    // Pre-fix, `Auth::user_as_arc::<T>` delegated to
    // `user_as::<T>().map(Arc::new)` — each call cloned the concrete
    // user before wrapping in `Arc`. The trait-surgery fix wires
    // `Authenticatable::into_arc_any` through `Arc::downcast::<T>` so
    // the returned Arc IS the request-state cache entry, not a copy.
    // `Arc::ptr_eq` is the load-bearing assertion: a clone path would
    // fail this.
    #[tokio::test]
    async fn user_as_arc_reuses_request_state_allocation_without_cloning() {
        crate::auth::request_state::scope(async {
            let user_arc = Arc::new(TestUser);
            let user_arc_clone = Arc::clone(&user_arc);
            Auth::set_user(user_arc_clone as Arc<dyn Authenticatable>);

            let resolved = Auth::user_as_arc::<TestUser>()
                .await
                .expect("user_as_arc must not error")
                .expect("user_as_arc must find the cached user");

            assert!(
                Arc::ptr_eq(&user_arc, &resolved),
                "user_as_arc must hand back the same Arc the cache holds, not a clone"
            );
        })
        .await;
    }

    // Wrong-type downcast through `into_arc_any` → `Arc::downcast`
    // must miss cleanly and return `None`, not panic.
    #[tokio::test]
    async fn user_as_arc_wrong_type_returns_none() {
        #[derive(Clone)]
        struct OtherUser;
        impl Authenticatable for OtherUser {
            fn get_auth_identifier(&self) -> String {
                "other".to_string()
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
                self
            }
        }

        crate::auth::request_state::scope(async {
            Auth::set_user(Arc::new(OtherUser) as Arc<dyn Authenticatable>);
            let resolved = Auth::user_as_arc::<TestUser>()
                .await
                .expect("call must not error");
            assert!(resolved.is_none(), "wrong-type downcast returns None");
        })
        .await;
    }
}
