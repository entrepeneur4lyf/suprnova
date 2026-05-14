//! Session middleware for suprnova framework

use crate::http::cookie::{Cookie, SameSite};
use crate::http::Response;
use crate::middleware::{Middleware, Next};
use crate::Request;
use async_trait::async_trait;
use rand::Rng;
use std::cell::RefCell;
use std::sync::Arc;

use super::config::SessionConfig;
use super::driver::DatabaseSessionDriver;
use super::store::{SessionData, SessionStore};

// Thread-local session context for storing the current request's session data
thread_local! {
    static SESSION_CONTEXT: RefCell<Option<SessionData>> = const { RefCell::new(None) };
}

/// Get the current session (read-only)
///
/// Returns a clone of the current session data if available.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::session::session;
///
/// if let Some(session) = session() {
///     let name: Option<String> = session.get("name");
/// }
/// ```
pub fn session() -> Option<SessionData> {
    SESSION_CONTEXT.with(|ctx| ctx.borrow().clone())
}

/// Get the current session and modify it
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::session::session_mut;
///
/// session_mut(|session| {
///     session.put("name", "John");
/// });
/// ```
pub fn session_mut<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut SessionData) -> R,
{
    SESSION_CONTEXT.with(|ctx| {
        let mut session_opt = ctx.borrow_mut();
        session_opt.as_mut().map(f)
    })
}

/// Set the session context for the current request
pub fn set_session(session: SessionData) {
    SESSION_CONTEXT.with(|ctx| {
        *ctx.borrow_mut() = Some(session);
    });
}

/// Clear the session context
pub fn clear_session() {
    SESSION_CONTEXT.with(|ctx| {
        *ctx.borrow_mut() = None;
    });
}

/// Take the session out of the context (for saving)
pub fn take_session() -> Option<SessionData> {
    SESSION_CONTEXT.with(|ctx| ctx.borrow_mut().take())
}

/// Generate a cryptographically secure session ID
///
/// Generates a 40-character alphanumeric string.
pub fn generate_session_id() -> String {
    let mut rng = rand::thread_rng();
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

    (0..40)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// Generate a CSRF token
///
/// Same format as session ID for consistency.
pub fn generate_csrf_token() -> String {
    generate_session_id()
}

/// Session middleware
///
/// Handles session lifecycle:
/// 1. Reads session ID from cookie
/// 2. Loads session data from storage
/// 3. Makes session available during request
/// 4. Saves session after request
/// 5. Sets session cookie on response
pub struct SessionMiddleware {
    config: SessionConfig,
    store: Arc<dyn SessionStore>,
}

impl SessionMiddleware {
    /// Create a new session middleware with the given configuration
    pub fn new(config: SessionConfig) -> Self {
        let store = Arc::new(DatabaseSessionDriver::new(config.lifetime));
        Self { config, store }
    }

    /// Create session middleware with a custom store
    pub fn with_store(config: SessionConfig, store: Arc<dyn SessionStore>) -> Self {
        Self { config, store }
    }

    fn create_session_cookie(&self, session_id: &str) -> Cookie {
        let mut cookie = Cookie::new(&self.config.cookie_name, session_id)
            .http_only(self.config.cookie_http_only)
            .secure(self.config.cookie_secure)
            .path(&self.config.cookie_path)
            .max_age(self.config.lifetime);

        cookie = match self.config.cookie_same_site.to_lowercase().as_str() {
            "strict" => cookie.same_site(SameSite::Strict),
            "none" => cookie.same_site(SameSite::None),
            _ => cookie.same_site(SameSite::Lax),
        };

        cookie
    }
}

#[async_trait]
impl Middleware for SessionMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Get session ID from cookie or generate new one
        let session_id = request
            .cookie(&self.config.cookie_name)
            .unwrap_or_else(generate_session_id);

        // Load session from store
        let mut session = match self.store.read(&session_id).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                // Create new session
                SessionData::new(session_id.clone(), generate_csrf_token())
            }
            Err(e) => {
                eprintln!("Session read error: {}", e);
                SessionData::new(session_id.clone(), generate_csrf_token())
            }
        };

        // Age flash data from previous request
        session.age_flash_data();

        // Store session in thread-local context
        set_session(session);

        // Process the request
        let response = next(request).await;

        // Get the potentially modified session
        let session = take_session();

        // Save session and add cookie to response
        if let Some(session) = session {
            // Always save to update last_activity
            if let Err(e) = self.store.write(&session).await {
                eprintln!("Session write error: {}", e);
            }

            // Add session cookie to response
            let cookie = self.create_session_cookie(&session.id);

            match response {
                Ok(res) => Ok(res.cookie(cookie)),
                Err(res) => Err(res.cookie(cookie)),
            }
        } else {
            response
        }
    }
}

/// Regenerate the session ID (for security after login)
///
/// This creates a new session ID while preserving session data,
/// which helps prevent session fixation attacks.
pub fn regenerate_session_id() {
    session_mut(|session| {
        session.id = generate_session_id();
        session.dirty = true;
    });
}

/// Invalidate the current session (clear all data)
pub fn invalidate_session() {
    session_mut(|session| {
        session.flush();
        session.csrf_token = generate_csrf_token();
    });
}

/// Helper to get the CSRF token from current session
pub fn get_csrf_token() -> Option<String> {
    session().map(|s| s.csrf_token)
}

/// Helper to check if user is authenticated
pub fn is_authenticated() -> bool {
    session().map(|s| s.user_id.is_some()).unwrap_or(false)
}

/// Helper to get the authenticated user ID
pub fn auth_user_id() -> Option<i64> {
    session().and_then(|s| s.user_id)
}

/// Helper to set the authenticated user
pub fn set_auth_user(user_id: i64) {
    session_mut(|session| {
        session.user_id = Some(user_id);
        session.dirty = true;
    });
}

/// Helper to clear the authenticated user (logout)
pub fn clear_auth_user() {
    session_mut(|session| {
        session.user_id = None;
        session.dirty = true;
    });
}
