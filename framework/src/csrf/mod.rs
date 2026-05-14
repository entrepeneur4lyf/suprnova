//! CSRF protection for suprnova framework
//!
//! Provides Laravel-like CSRF protection using per-session tokens.
//!
//! # How it works
//!
//! 1. Each session has a unique CSRF token
//! 2. The token is included in HTML responses via a meta tag
//! 3. JavaScript (Inertia.js) reads the token and sends it with requests
//! 4. The middleware validates the token on state-changing requests
//!
//! # Setup
//!
//! Add the middleware after SessionMiddleware:
//!
//! ```rust,ignore
//! use suprnova::{global_middleware, SessionMiddleware, CsrfMiddleware, SessionConfig};
//!
//! pub async fn register() {
//!     let config = SessionConfig::from_env();
//!     global_middleware!(SessionMiddleware::new(config));
//!     global_middleware!(CsrfMiddleware::new());
//! }
//! ```
//!
//! # Frontend Integration
//!
//! Add the CSRF meta tag to your HTML:
//!
//! ```html
//! <meta name="csrf-token" content="{{ csrf_token() }}">
//! ```
//!
//! Configure Axios/fetch to include the token:
//!
//! ```javascript
//! axios.defaults.headers.common['X-CSRF-TOKEN'] =
//!     document.querySelector('meta[name="csrf-token"]').content;
//! ```

pub mod middleware;

pub use middleware::CsrfMiddleware;

use crate::session::get_csrf_token;

/// Get the current CSRF token
///
/// Returns None if no session is active.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::csrf::csrf_token;
///
/// if let Some(token) = csrf_token() {
///     // Use token in response
/// }
/// ```
pub fn csrf_token() -> Option<String> {
    get_csrf_token()
}

/// Generate a CSRF meta tag for HTML responses
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::csrf::csrf_meta_tag;
///
/// let meta = csrf_meta_tag();
/// // Returns: <meta name="csrf-token" content="...">
/// ```
pub fn csrf_meta_tag() -> String {
    csrf_token()
        .map(|token| format!(r#"<meta name="csrf-token" content="{}">"#, token))
        .unwrap_or_default()
}

/// Generate a hidden CSRF input field for forms
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::csrf::csrf_field;
///
/// let field = csrf_field();
/// // Returns: <input type="hidden" name="_token" value="...">
/// ```
pub fn csrf_field() -> String {
    csrf_token()
        .map(|token| format!(r#"<input type="hidden" name="_token" value="{}">"#, token))
        .unwrap_or_default()
}
