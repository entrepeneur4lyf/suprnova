//! Authentication guard (facade)

use std::sync::Arc;

use crate::container::App;
use crate::session::{
    auth_user_id, clear_auth_user, generate_csrf_token, regenerate_session_id, session_mut,
    set_auth_user,
};

use super::authenticatable::Authenticatable;
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
///     let user_id = Auth::id().unwrap();
/// }
///
/// // Log in
/// Auth::login(user_id);
///
/// // Log out
/// Auth::logout();
/// ```
pub struct Auth;

impl Auth {
    /// Get the authenticated user's ID
    ///
    /// Returns None if not authenticated.
    pub fn id() -> Option<i64> {
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
    pub fn login(user_id: i64) {
        // Regenerate session ID to prevent session fixation
        regenerate_session_id();

        // Set the authenticated user
        set_auth_user(user_id);

        // Regenerate CSRF token for extra security
        session_mut(|session| {
            session.csrf_token = generate_csrf_token();
        });
    }

    /// Log in a user with "remember me" functionality
    ///
    /// This extends the session lifetime for persistent login.
    ///
    /// # Arguments
    ///
    /// * `user_id` - The user's ID
    /// * `remember_token` - A secure token for remember me cookie
    pub fn login_remember(user_id: i64, _remember_token: &str) {
        // For now, just do a regular login
        // Remember me cookie handling is done in the controller
        Self::login(user_id);
    }

    /// Log out the current user
    ///
    /// Clears the authenticated user from the session.
    ///
    /// # Security
    ///
    /// This regenerates the CSRF token to prevent any cached tokens from being reused.
    pub fn logout() {
        // Clear the authenticated user
        clear_auth_user();

        // Regenerate CSRF token for security
        session_mut(|session| {
            session.csrf_token = generate_csrf_token();
        });
    }

    /// Log out and invalidate the entire session
    ///
    /// Use this for complete session destruction (e.g., "logout everywhere").
    pub fn logout_and_invalidate() {
        session_mut(|session| {
            session.flush();
            session.csrf_token = generate_csrf_token();
        });
    }

    /// Attempt to authenticate with a validator function
    ///
    /// The validator function should return the user ID if credentials are valid.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let user_id = Auth::attempt(async {
    ///     // Validate credentials
    ///     let user = User::find_by_email(&email).await?;
    ///     if user.verify_password(&password)? {
    ///         Ok(Some(user.id))
    ///     } else {
    ///         Ok(None)
    ///     }
    /// }).await?;
    ///
    /// if let Some(id) = user_id {
    ///     // Authentication successful
    /// }
    /// ```
    pub async fn attempt<F, Fut>(validator: F) -> Result<Option<i64>, crate::error::FrameworkError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Option<i64>, crate::error::FrameworkError>>,
    {
        let result = validator().await?;
        if let Some(user_id) = result {
            Self::login(user_id);
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

        provider.retrieve_by_id(user_id).await
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
    pub async fn user_as<T: Authenticatable + Clone>(
    ) -> Result<Option<T>, crate::error::FrameworkError> {
        let user = Self::user().await?;
        Ok(user.and_then(|u| u.as_any().downcast_ref::<T>().cloned()))
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
