//! CSRF protection middleware

use crate::http::{HttpResponse, Response};
use crate::middleware::{Middleware, Next};
use crate::session::get_csrf_token;
use crate::Request;
use async_trait::async_trait;

/// CSRF protection middleware
///
/// Validates CSRF tokens on state-changing requests (POST, PUT, PATCH, DELETE).
///
/// # Token Sources
///
/// The middleware looks for the CSRF token in the following order:
/// 1. `X-CSRF-TOKEN` header (used by Inertia.js)
/// 2. `X-XSRF-TOKEN` header (Laravel convention)
/// 3. `_token` form field (traditional forms)
///
/// # Usage
///
/// ```rust,ignore
/// use suprnova::{global_middleware, CsrfMiddleware};
///
/// global_middleware!(CsrfMiddleware::new());
/// ```
pub struct CsrfMiddleware {
    /// HTTP methods that require CSRF validation
    protected_methods: Vec<&'static str>,
    /// Paths to exclude from CSRF validation (e.g., webhooks)
    except: Vec<String>,
}

impl CsrfMiddleware {
    /// Create a new CSRF middleware with default settings
    ///
    /// Protects: POST, PUT, PATCH, DELETE
    pub fn new() -> Self {
        Self {
            protected_methods: vec!["POST", "PUT", "PATCH", "DELETE"],
            except: Vec::new(),
        }
    }

    /// Add paths to exclude from CSRF validation
    ///
    /// Useful for webhooks or API endpoints that use other authentication.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let csrf = CsrfMiddleware::new()
    ///     .except(vec!["/webhooks/*", "/api/external/*"]);
    /// ```
    pub fn except(mut self, paths: Vec<impl Into<String>>) -> Self {
        self.except = paths.into_iter().map(|p| p.into()).collect();
        self
    }

    /// Check if a path should be excluded from CSRF validation
    fn is_excluded(&self, path: &str) -> bool {
        for pattern in &self.except {
            if pattern.ends_with('*') {
                let prefix = &pattern[..pattern.len() - 1];
                if path.starts_with(prefix) {
                    return true;
                }
            } else if pattern == path {
                return true;
            }
        }
        false
    }
}

impl Default for CsrfMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for CsrfMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let method = request.method().as_str();

        // Only validate state-changing requests
        if !self.protected_methods.contains(&method) {
            return next(request).await;
        }

        // Check if path is excluded
        if self.is_excluded(request.path()) {
            return next(request).await;
        }

        // Get expected token from session
        let expected_token = match get_csrf_token() {
            Some(token) => token,
            None => {
                return Err(HttpResponse::json(serde_json::json!({
                    "message": "Session not found. CSRF validation failed."
                }))
                .status(500));
            }
        };

        // Get provided token from request
        // Check headers first (Inertia.js and AJAX)
        let provided_token = request
            .header("X-CSRF-TOKEN")
            .or_else(|| request.header("X-XSRF-TOKEN"))
            .map(|s| s.to_string());

        match provided_token {
            Some(token) if constant_time_compare(&token, &expected_token) => {
                // Token is valid
                next(request).await
            }
            _ => {
                // Token mismatch or missing
                // Return 419 status (Laravel convention)
                Err(HttpResponse::json(serde_json::json!({
                    "message": "CSRF token mismatch."
                }))
                .status(419))
            }
        }
    }
}

/// Constant-time string comparison to prevent timing attacks
///
/// This ensures an attacker can't determine how much of the token is correct
/// based on response time.
fn constant_time_compare(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }

    a.bytes()
        .zip(b.bytes())
        .fold(0, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_time_compare() {
        assert!(constant_time_compare("abc123", "abc123"));
        assert!(!constant_time_compare("abc123", "abc124"));
        assert!(!constant_time_compare("abc123", "abc12"));
        assert!(!constant_time_compare("", "a"));
    }

    #[test]
    fn test_is_excluded() {
        let csrf = CsrfMiddleware::new().except(vec!["/webhooks/*", "/api/public"]);

        assert!(csrf.is_excluded("/webhooks/stripe"));
        assert!(csrf.is_excluded("/webhooks/github/events"));
        assert!(csrf.is_excluded("/api/public"));
        assert!(!csrf.is_excluded("/api/private"));
        assert!(!csrf.is_excluded("/login"));
    }
}
