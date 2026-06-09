//! Authentication module for suprnova framework
//!
//! Provides Laravel-like authentication with guards and middleware.
//!
//! # Overview
//!
//! suprnova provides a simple, session-based authentication system:
//!
//! - `Auth` facade for login/logout operations
//! - `AuthMiddleware` for protecting routes
//! - `GuestMiddleware` for guest-only routes
//! - `Authenticatable` trait for user models
//! - `UserProvider` trait for user retrieval
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::{Auth, AuthMiddleware, GuestMiddleware};
//!
//! // In a controller
//! if Auth::check() {
//!     let user_id: String = Auth::id().unwrap();
//! }
//!
//! // Get the currently authenticated user
//! if let Some(user) = Auth::user().await? {
//!     println!("User ID: {}", user.auth_identifier());
//! }
//!
//! // Get as concrete User type
//! if let Some(user) = Auth::user_as::<User>().await? {
//!     println!("Welcome, user #{}!", user.id);
//! }
//!
//! // Establish a session from a known id (sync primitive). For the
//! // guard-backed form with events + remember-me, use
//! // `Auth::attempt(&creds, remember).await?` / `Auth::login(user, remember).await?`.
//! Auth::login_id(user.id.to_string())?;
//!
//! // Logout (async — also revokes remember-me tokens for the user)
//! Auth::logout().await?;
//!
//! // In routes
//! group!("/dashboard")
//!     .middleware(AuthMiddleware::redirect_to("/login"))
//!     .routes([...]);
//!
//! group!("/")
//!     .middleware(GuestMiddleware::redirect_to("/dashboard"))
//!     .routes([
//!         get!("/login", auth::show_login),
//!     ]);
//! ```

pub mod authenticatable;
pub mod config;
pub mod contract;
pub mod database_provider;
pub mod eloquent_provider;
pub mod events;
pub mod generic_user;
pub mod guard;
pub mod manager;
pub mod middleware;
pub mod must_verify_email;
pub mod provider;
pub mod remember;
pub mod request_state;
pub mod session_guard;
pub mod token_guard;

pub use authenticatable::Authenticatable;
pub use config::{AuthConfig, GuardConfig, GuardDriver};
pub use contract::{Credentials, Guard, StatefulGuard};
pub use database_provider::DatabaseUserProvider;
pub use eloquent_provider::EloquentUserProvider;
pub use generic_user::GenericUser;
pub use guard::Auth;
pub use manager::AuthManager;
pub use middleware::{AuthMiddleware, BasicAuthMiddleware, GuestMiddleware};
pub use must_verify_email::{AuthFlowUser, CanResetPassword, MustVerifyEmail};
pub use provider::UserProvider;
pub use session_guard::SessionGuard;
pub use token_guard::TokenGuard;

use std::sync::Arc;

/// Resolve the active [`UserProvider`] for the request — the same provider
/// [`Auth::user`] would resolve against, minus the guard's request-scoped
/// user cache.
///
/// Resolution order mirrors [`Auth::user`]:
///
/// 1. A container-bound [`AuthManager`]'s default-guard provider
///    (registered via `Auth::register_provider("users", ...)`).
/// 2. A legacy globally-bound `App::make::<dyn UserProvider>()`.
///
/// The auth-flow facades ([`crate::auth_flows::EmailVerification`],
/// [`crate::auth_flows::PasswordReset`]) call this so a lookup-by-email or a
/// mark-verified hits whichever provider the rest of the auth surface uses.
/// `Auth::user` deliberately keeps its own body: it resolves a *guard* (to
/// preserve request-scoped caching of the resolved user), not a bare provider,
/// so it is not routed through this helper.
///
/// # Errors
///
/// Returns [`crate::error::FrameworkError::internal`] with the same
/// remediation [`Auth::user`] uses when no provider is configured by either
/// path.
pub(crate) fn active_user_provider() -> Result<Arc<dyn UserProvider>, crate::error::FrameworkError> {
    if let Some(manager) = crate::container::App::get::<AuthManager>()
        && let Ok(provider) = manager.default_provider()
    {
        return Ok(provider);
    }
    crate::container::App::make::<dyn UserProvider>().ok_or_else(|| {
        crate::error::FrameworkError::internal(
            "No user provider configured. Register one with \
             Auth::register_provider(\"users\", Arc::new(...)) (named-guard system), \
             or bind!(dyn UserProvider, ...) in bootstrap.rs (legacy)."
                .to_string(),
        )
    })
}
