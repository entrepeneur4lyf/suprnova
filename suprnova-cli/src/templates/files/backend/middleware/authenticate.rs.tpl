//! Authentication middleware helpers

pub use suprnova::{AuthMiddleware, GuestMiddleware};

/// Create auth middleware that redirects unauthenticated users to login
pub fn auth() -> AuthMiddleware {
    AuthMiddleware::redirect_to("/login")
}

/// Create guest middleware that redirects authenticated users to dashboard
pub fn guest() -> GuestMiddleware {
    GuestMiddleware::redirect_to("/dashboard")
}
