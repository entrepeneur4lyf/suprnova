//! Authentication middleware

use crate::Request;
use crate::http::{HttpResponse, Response};
use crate::middleware::{Middleware, Next};
use async_trait::async_trait;

use super::guard::Auth;

/// Authentication middleware
///
/// Protects routes that require authentication. Unauthenticated requests
/// are either redirected to a login page or receive a 401 response.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{AuthMiddleware, group, get};
///
/// // API routes - return 401 for unauthenticated
/// group!("/api")
///     .middleware(AuthMiddleware::new())
///     .routes([...]);
///
/// // Web routes - redirect to login
/// group!("/dashboard")
///     .middleware(AuthMiddleware::redirect_to("/login"))
///     .routes([...]);
/// ```
pub struct AuthMiddleware {
    /// Path to redirect to if not authenticated (None = return 401)
    redirect_to: Option<String>,
}

impl AuthMiddleware {
    /// Create middleware that returns 401 Unauthorized if not authenticated
    ///
    /// Best for API routes.
    pub fn new() -> Self {
        Self { redirect_to: None }
    }

    /// Create middleware that redirects to a login page if not authenticated
    ///
    /// Best for web routes.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// AuthMiddleware::redirect_to("/login")
    /// ```
    pub fn redirect_to(path: impl Into<String>) -> Self {
        Self {
            redirect_to: Some(path.into()),
        }
    }
}

impl Default for AuthMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for AuthMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        if Auth::check() {
            // User is authenticated, proceed
            return next(request).await;
        }

        // User is not authenticated
        match &self.redirect_to {
            Some(path) => {
                // For Inertia requests, return 409 with redirect location
                // This tells Inertia to do a full page visit to the login page
                if request.is_inertia() {
                    Err(HttpResponse::text("")
                        .status(409)
                        .header("X-Inertia-Location", path.clone()))
                } else {
                    // Regular redirect for non-Inertia requests
                    Err(HttpResponse::new()
                        .status(302)
                        .header("Location", path.clone()))
                }
            }
            None => {
                // Return 401 Unauthorized
                Err(HttpResponse::json(serde_json::json!({
                    "message": "Unauthenticated."
                }))
                .status(401))
            }
        }
    }
}

/// Guest middleware
///
/// Protects routes that should only be accessible to guests (non-authenticated users).
/// Useful for login and registration pages.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{GuestMiddleware, group, get};
///
/// group!("/")
///     .middleware(GuestMiddleware::redirect_to("/dashboard"))
///     .routes([
///         get!("/login", auth::show_login),
///         get!("/register", auth::show_register),
///     ]);
/// ```
pub struct GuestMiddleware {
    /// Path to redirect to if authenticated
    redirect_to: String,
}

impl GuestMiddleware {
    /// Create middleware that redirects authenticated users
    ///
    /// # Arguments
    ///
    /// * `redirect_to` - Path to redirect authenticated users to
    pub fn redirect_to(path: impl Into<String>) -> Self {
        Self {
            redirect_to: path.into(),
        }
    }

    /// Alias for `redirect_to` with a default path
    pub fn new() -> Self {
        Self::redirect_to("/")
    }
}

impl Default for GuestMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for GuestMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        if Auth::guest() {
            // User is a guest, proceed
            return next(request).await;
        }

        // User is authenticated, redirect them away
        if request.is_inertia() {
            // For Inertia requests, return 409 with redirect location
            Err(HttpResponse::text("")
                .status(409)
                .header("X-Inertia-Location", &self.redirect_to))
        } else {
            // Regular redirect for non-Inertia requests
            Err(HttpResponse::new()
                .status(302)
                .header("Location", &self.redirect_to))
        }
    }
}
