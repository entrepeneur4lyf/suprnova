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
//!     let user_id = Auth::id().unwrap();
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
//! // Login
//! Auth::login(user.id);
//!
//! // Logout
//! Auth::logout();
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
pub mod guard;
pub mod middleware;
pub mod provider;

pub use authenticatable::Authenticatable;
pub use guard::Auth;
pub use middleware::{AuthMiddleware, GuestMiddleware};
pub use provider::UserProvider;
