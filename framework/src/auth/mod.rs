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
//! // Login (numeric apps: convert to string at the boundary)
//! Auth::login(user.id.to_string());
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
pub mod contract;
pub mod events;
pub mod guard;
pub mod middleware;
pub mod provider;
pub mod remember;
pub mod request_state;
pub mod session_guard;

pub use authenticatable::Authenticatable;
pub use contract::{Credentials, Guard, StatefulGuard};
pub use guard::Auth;
pub use middleware::{AuthMiddleware, GuestMiddleware};
pub use provider::UserProvider;
pub use session_guard::SessionGuard;
