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
    auth_user_id, clear_auth_user, generate_csrf_token, generate_session_id,
    get_csrf_token, invalidate_session, is_authenticated, regenerate_session_id, session,
    session_mut, set_auth_user, SessionMiddleware,
};
pub use store::{SessionData, SessionStore};

// Test helpers — these mirror the per-request session scope that
// `SessionMiddleware` sets up at runtime. Crates outside the framework
// shouldn't need these; they exist for unit/integration tests that
// drive code paths reading the session without booting a full server.
#[doc(hidden)]
pub fn new_session_slot_for_test()
-> std::sync::Arc<std::sync::Mutex<Option<SessionData>>> {
    std::sync::Arc::new(std::sync::Mutex::new(Some(SessionData::new(
        "test_session".to_string(),
        "test_csrf_token".to_string(),
    ))))
}

#[doc(hidden)]
pub async fn session_scope_for_test<F: std::future::Future>(
    slot: std::sync::Arc<std::sync::Mutex<Option<SessionData>>>,
    fut: F,
) -> F::Output {
    middleware::SESSION_CONTEXT.scope(slot, fut).await
}

/// Test-only: a fresh pending-cookies slot. Use with
/// [`pending_cookies_scope_for_test`] to drive `Auth::login_remember`
/// and friends without booting `SessionMiddleware`.
#[doc(hidden)]
pub fn new_pending_cookies_slot_for_test()
-> std::sync::Arc<std::sync::Mutex<Vec<crate::http::cookie::Cookie>>> {
    std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))
}

#[doc(hidden)]
pub async fn pending_cookies_scope_for_test<F: std::future::Future>(
    slot: std::sync::Arc<std::sync::Mutex<Vec<crate::http::cookie::Cookie>>>,
    fut: F,
) -> F::Output {
    middleware::PENDING_COOKIES.scope(slot, fut).await
}
