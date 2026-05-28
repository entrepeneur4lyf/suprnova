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
/// ```rust,ignore
/// use suprnova::{Auth, Credentials};
///
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
/// // no provider, no AuthManager, no events).
/// Auth::login_id(user_id.to_string());
///
/// // Log out — clears the session + request user, revokes remember-me,
/// // and fires Logout.
/// Auth::logout().await?;
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
    /// # Security
    ///
    /// Regenerates the session ID to prevent session fixation, and rotates the
    /// CSRF token.
    pub fn login_id(user_id: impl Into<String>) {
        // Regenerate session ID to prevent session fixation
        regenerate_session_id();

        // Set the authenticated user
        set_auth_user(user_id);

        // Regenerate CSRF token for extra security
        session_mut(|session| {
            session.csrf_token = generate_csrf_token();
        });
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
        // Regular session login (regen session id + CSRF, set user).
        Self::login_id(user_id.clone());

        // Issue the row. Returns the plaintext destined for the cookie.
        let plaintext = super::remember::issue(&user_id, ttl_minutes).await?;

        // Build + queue the cookie. The cookie's `Max-Age` matches the
        // row's TTL (`ttl_minutes` converted to seconds) so the browser
        // stops sending the cookie the moment the row expires — the
        // attribute does not "lie" about validity.
        let config = crate::session::SessionConfig::from_env();
        let max_age =
            std::time::Duration::from_secs((ttl_minutes.max(0) as u64).saturating_mul(60));
        let cookie =
            crate::session::middleware::create_remember_cookie(&config, &plaintext, max_age)?;
        crate::session::middleware::push_pending_cookie(cookie);

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
        let removed = match Self::id() {
            Some(id) => super::remember::revoke_all_for_user(&id).await?,
            None => 0,
        };

        // Always queue the clear cookie — even when the user has no
        // remember-me row, the browser might still hold a stale one
        // from a previous account, and clearing is the polite default.
        let config = crate::session::SessionConfig::from_env();
        let clear = crate::session::middleware::create_forget_remember_cookie(&config);
        crate::session::middleware::push_pending_cookie(clear);

        Ok(removed)
    }

    /// Tear down all authentication state for the current request: revoke the
    /// user's remember-me tokens, clear the session user, clear the
    /// request-scoped current user, and rotate the CSRF token.
    ///
    /// The event-free core shared by [`logout`](Self::logout) and a guard's
    /// `logout`; the caller dispatches the [`Logout`](crate::auth::events::Logout)
    /// event so it is emitted exactly once, attributed to the right guard.
    pub(crate) async fn clear_authentication() -> Result<(), crate::error::FrameworkError> {
        // Revoke remember-me first, while the authenticated id is still
        // resolvable. `revoke_remember_tokens` no-ops cleanly when there is no
        // logged-in user.
        Self::revoke_remember_tokens().await?;

        // Clear the session user *and* the request-scoped cache. Clearing the
        // request state is essential: `Auth::id` consults it ahead of the
        // session, so a user resolved this request would otherwise survive
        // logout and `Auth::id()` would keep reporting it.
        clear_auth_user();
        request_state::clear_current_user();
        // Also clear any 2FA challenge that was mid-flight when logout
        // landed — pending and authed are both authentication state,
        // and a tear-down that drops one but leaves the other strands
        // the state machine.
        crate::session::middleware::clear_two_factor_pending();

        // Rotate the CSRF token so any cached token cannot be reused.
        session_mut(|session| {
            session.csrf_token = generate_csrf_token();
        });

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
        // Capture the id before flushing so the event is attributed.
        let user_id = Self::id();
        Self::revoke_remember_tokens().await?;
        session_mut(|session| {
            session.flush();
            session.csrf_token = generate_csrf_token();
        });
        request_state::clear_current_user();
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
    /// ```rust,ignore
    /// use suprnova::Auth;
    ///
    /// if let Some(user) = Auth::user().await? {
    ///     println!("Logged in as user {}", user.auth_identifier());
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if no `UserProvider` is registered in the container.
    /// Make sure to register a `UserProvider` in your `bootstrap.rs`:
    ///
    /// ```rust,ignore
    /// bind!(dyn UserProvider, DatabaseUserProvider);
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

    /// Get the authenticated user, cast to a concrete type
    ///
    /// This is a convenience method that retrieves the user and downcasts
    /// it to your concrete User type. Returns `None` when no user is
    /// authenticated *or* when the resolved user is not a `T` (e.g. after
    /// `set_user` of a different type).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use suprnova::Auth;
    /// use crate::models::users::User;
    ///
    /// if let Some(user) = Auth::user_as::<User>().await? {
    ///     println!("Welcome, user #{}!", user.id);
    /// }
    /// ```
    ///
    /// # Type Parameters
    ///
    /// * `T` - The concrete user type that implements `Authenticatable` and `Clone`
    pub async fn user_as<T: Authenticatable + Clone>()
    -> Result<Option<T>, crate::error::FrameworkError> {
        let user = Self::user().await?;
        Ok(user.and_then(|u| u.as_any().downcast_ref::<T>().cloned()))
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
    fn default_guard_name() -> String {
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
    /// ```rust,ignore
    /// Auth::register_provider("users", Arc::new(my_user_provider))?;
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
    /// ```rust,ignore
    /// if Auth::guard("api")?.check().await? { /* ... */ }
    /// ```
    pub fn guard(name: &str) -> Result<Arc<dyn Guard>, crate::error::FrameworkError> {
        Self::manager()?.guard(name)
    }

    /// Resolve a named guard as a [`StatefulGuard`] (login/logout/attempt).
    ///
    /// Errors if the named guard is stateless (a token guard).
    ///
    /// ```rust,ignore
    /// let user = Auth::stateful_guard("web")?
    ///     .attempt(&Credentials::password(email, password), remember)
    ///     .await?;
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
    /// ```rust,ignore
    /// use suprnova::Auth;
    ///
    /// let user = Auth::password().register("alice@example.com", "s3cret!").await?;
    /// let (user, session) = Auth::password()
    ///     .authenticate("alice@example.com", "s3cret!", None, None)
    ///     .await?;
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
    /// ```rust,ignore
    /// use suprnova::Auth;
    /// use suprnova::torii_integration::oauth::OAuthProviderConfig;
    ///
    /// Auth::oauth("github").configure(OAuthProviderConfig {
    ///     client_id: "...".into(),
    ///     client_secret: "...".into(),
    ///     redirect_url: "http://localhost:8000/auth/oauth/github/callback".into(),
    ///     scopes: vec!["user:email".into()],
    ///     endpoints_override: None, // use the well-known GitHub endpoints
    /// });
    ///
    /// let kickoff = Auth::oauth("github").begin().await?;
    /// // Redirect user to kickoff.authorization_url, store kickoff.state in session.
    ///
    /// let (user, session) = Auth::oauth("github").complete(&code, &state).await?;
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
        fn auth_identifier(&self) -> i64 {
            7
        }
        fn get_auth_identifier(&self) -> String {
            "7".to_string()
        }
        fn as_any(&self) -> &dyn Any {
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
}
