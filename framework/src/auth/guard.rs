//! Authentication guard (facade)

use std::sync::Arc;

use crate::container::App;
use crate::session::{
    auth_user_id, clear_auth_user, generate_csrf_token, regenerate_session_id, session_mut,
    set_auth_user,
};

use super::authenticatable::Authenticatable;
use super::contract::{Guard, StatefulGuard};
use super::manager::AuthManager;
use super::provider::UserProvider;

/// Authentication facade
///
/// Provides Laravel-like static methods for authentication operations.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::Auth;
///
/// // Check if authenticated
/// if Auth::check() {
///     let user_id: String = Auth::id().unwrap();
/// }
///
/// // Log in (numeric apps: convert to string at the boundary)
/// Auth::login(user_id.to_string());
///
/// // Log out (async — also revokes remember-me tokens for the user)
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

    /// Log in a user by their ID
    ///
    /// This sets the user ID in the session, making them authenticated.
    ///
    /// # Security
    ///
    /// This method regenerates the session ID to prevent session fixation attacks.
    pub fn login(user_id: impl Into<String>) {
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
    /// # Closes
    ///
    /// Codex review finding #13 — the pre-fix implementation ignored
    /// its `remember_token` parameter and performed only a session
    /// login. There was no DB row, no cookie, no rotation, and
    /// `logout()` did not clear any persistent state.
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
        Self::login(user_id.clone());

        // Issue the row. Returns the plaintext destined for the cookie.
        let plaintext = super::remember::issue(&user_id, ttl_minutes).await?;

        // Build + queue the cookie. The cookie's `Max-Age` matches the
        // row's TTL (`ttl_minutes` converted to seconds) so the browser
        // stops sending the cookie the moment the row expires — the
        // attribute does not "lie" about validity. Codex finding #13
        // required "expires-at matches token expiration."
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

    /// Log out the current user
    ///
    /// Clears the authenticated user from the session AND revokes
    /// every remember-me token for that user (so closing the browser
    /// and reopening it does not silently log them back in).
    ///
    /// # Security
    ///
    /// This regenerates the CSRF token to prevent any cached tokens
    /// from being reused.
    pub async fn logout() -> Result<(), crate::error::FrameworkError> {
        // Revoke remember-me first, while we still have access to the
        // authenticated user id. `revoke_remember_tokens` no-ops cleanly
        // when there is no logged-in user.
        Self::revoke_remember_tokens().await?;

        // Clear the authenticated user
        clear_auth_user();

        // Regenerate CSRF token for security
        session_mut(|session| {
            session.csrf_token = generate_csrf_token();
        });

        Ok(())
    }

    /// Log out and invalidate the entire session
    ///
    /// Use this for complete session destruction (e.g., "logout everywhere").
    /// Also revokes every remember-me token for the user.
    pub async fn logout_and_invalidate() -> Result<(), crate::error::FrameworkError> {
        Self::revoke_remember_tokens().await?;
        session_mut(|session| {
            session.flush();
            session.csrf_token = generate_csrf_token();
        });
        Ok(())
    }

    /// Attempt to authenticate with a validator function
    ///
    /// The validator function should return the user ID (as a `String`) if credentials are valid.
    /// Numeric-id apps can use `.to_string()` on the id before returning it.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let user_id = Auth::attempt(async {
    ///     // Validate credentials
    ///     let user = User::find_by_email(&email).await?;
    ///     if user.verify_password(&password)? {
    ///         Ok(Some(user.id.to_string()))
    ///     } else {
    ///         Ok(None)
    ///     }
    /// }).await?;
    ///
    /// if let Some(id) = user_id {
    ///     // Authentication successful
    /// }
    /// ```
    pub async fn attempt<F, Fut>(
        validator: F,
    ) -> Result<Option<String>, crate::error::FrameworkError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Option<String>, crate::error::FrameworkError>>,
    {
        let result = validator().await?;
        if let Some(ref user_id) = result {
            Self::login(user_id.clone());
        }
        Ok(result)
    }

    /// Validate credentials without logging in
    ///
    /// Useful for password confirmation dialogs.
    pub async fn validate<F, Fut>(validator: F) -> Result<bool, crate::error::FrameworkError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<bool, crate::error::FrameworkError>>,
    {
        validator().await
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
        let user_id = match Self::id() {
            Some(id) => id,
            None => return Ok(None),
        };

        let provider = App::make::<dyn UserProvider>().ok_or_else(|| {
            crate::error::FrameworkError::internal(
                "No UserProvider registered. Register one in bootstrap.rs with: \
                 bind!(dyn UserProvider, YourUserProvider)"
                    .to_string(),
            )
        })?;

        provider.retrieve_by_id(&user_id).await
    }

    /// Get the authenticated user, cast to a concrete type
    ///
    /// This is a convenience method that retrieves the user and downcasts
    /// it to your concrete User type.
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

    struct FakeProvider;
    #[async_trait]
    impl UserProvider for FakeProvider {
        async fn retrieve_by_id(
            &self,
            id: &str,
        ) -> Result<Option<Arc<dyn Authenticatable>>, crate::error::FrameworkError> {
            Ok((id == "7").then(|| Arc::new(TestUser) as Arc<dyn Authenticatable>))
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
}
