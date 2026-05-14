//! Session management for suprnova framework
//!
//! Provides Laravel-like session handling with database storage.
//!
//! # Features
//!
//! - Secure session cookies (HttpOnly, Secure, SameSite)
//! - Database-backed storage for scalability
//! - CSRF token generation per session
//! - Flash messages for one-time notifications
//! - Session data stored as JSON
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::session::{session, session_mut};
//!
//! // Read from session
//! if let Some(s) = session() {
//!     let name: Option<String> = s.get("name");
//! }
//!
//! // Write to session
//! session_mut(|s| {
//!     s.put("name", "John");
//!     s.flash("success", "Item saved!");
//! });
//! ```
//!
//! # Setup
//!
//! Add the `SessionMiddleware` to your bootstrap:
//!
//! ```rust,ignore
//! use suprnova::{global_middleware, SessionMiddleware, SessionConfig};
//!
//! pub async fn register() {
//!     let config = SessionConfig::from_env();
//!     global_middleware!(SessionMiddleware::new(config));
//! }
//! ```

pub mod config;
pub mod driver;
pub mod middleware;
pub mod store;

pub use config::SessionConfig;
pub use driver::DatabaseSessionDriver;
pub use middleware::{
    auth_user_id, clear_auth_user, clear_session, generate_csrf_token, generate_session_id,
    get_csrf_token, invalidate_session, is_authenticated, regenerate_session_id, session,
    session_mut, set_auth_user, set_session, take_session, SessionMiddleware,
};
pub use store::{SessionData, SessionStore};
