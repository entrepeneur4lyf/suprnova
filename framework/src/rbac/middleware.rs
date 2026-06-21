//! Route middleware for RBAC checks.

use std::marker::PhantomData;

use async_trait::async_trait;

use crate::auth::Auth;
use crate::error::FrameworkError;
use crate::http::{HttpResponse, Request, Response};
use crate::middleware::{Middleware, Next};

use super::HasRoles;

fn unauthorized_response(redirect_to: Option<&str>, request: &Request) -> HttpResponse {
    match redirect_to {
        Some(path) if request.is_inertia() => HttpResponse::text("")
            .status(409)
            .header("X-Inertia-Location", path),
        Some(path) => HttpResponse::new().status(302).header("Location", path),
        None => FrameworkError::Unauthorized.into(),
    }
}

/// Middleware that requires the authenticated user to have a role.
///
/// `U` is the concrete authenticated user type stored by [`Auth`].
/// Compose after [`crate::AuthMiddleware`] so unauthenticated users are
/// handled consistently before role checks run.
pub struct RoleMiddleware<U> {
    role: String,
    redirect_to: Option<String>,
    marker: PhantomData<fn() -> U>,
}

impl<U> RoleMiddleware<U> {
    /// Create middleware requiring `role`.
    pub fn new(role: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            redirect_to: None,
            marker: PhantomData,
        }
    }

    /// Create middleware requiring `role`, redirecting denials to `path`.
    ///
    /// Inertia requests receive `409` with `X-Inertia-Location`; normal browser
    /// requests receive `302 Location`.
    pub fn redirect_to(role: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            redirect_to: Some(path.into()),
            marker: PhantomData,
        }
    }

    fn unauthorized_response(&self, request: &Request) -> HttpResponse {
        unauthorized_response(self.redirect_to.as_deref(), request)
    }
}

#[async_trait]
impl<U> Middleware for RoleMiddleware<U>
where
    U: HasRoles,
{
    async fn handle(&self, request: Request, next: Next) -> Response {
        let Some(user) = Auth::user_as_arc::<U>().await? else {
            return Err(self.unauthorized_response(&request));
        };

        if user.has_role(&self.role).await? {
            next(request).await
        } else {
            Err(self.unauthorized_response(&request))
        }
    }
}

/// Middleware that requires the authenticated user to have a permission.
///
/// Direct model permissions and permissions inherited through assigned roles
/// are both accepted.
pub struct PermissionMiddleware<U> {
    permission: String,
    redirect_to: Option<String>,
    marker: PhantomData<fn() -> U>,
}

impl<U> PermissionMiddleware<U> {
    /// Create middleware requiring `permission`.
    pub fn new(permission: impl Into<String>) -> Self {
        Self {
            permission: permission.into(),
            redirect_to: None,
            marker: PhantomData,
        }
    }

    /// Create middleware requiring `permission`, redirecting denials to `path`.
    ///
    /// Inertia requests receive `409` with `X-Inertia-Location`; normal browser
    /// requests receive `302 Location`.
    pub fn redirect_to(permission: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            permission: permission.into(),
            redirect_to: Some(path.into()),
            marker: PhantomData,
        }
    }

    fn unauthorized_response(&self, request: &Request) -> HttpResponse {
        unauthorized_response(self.redirect_to.as_deref(), request)
    }
}

#[async_trait]
impl<U> Middleware for PermissionMiddleware<U>
where
    U: HasRoles,
{
    async fn handle(&self, request: Request, next: Next) -> Response {
        let Some(user) = Auth::user_as_arc::<U>().await? else {
            return Err(self.unauthorized_response(&request));
        };

        if user.has_permission_to(&self.permission).await? {
            next(request).await
        } else {
            Err(self.unauthorized_response(&request))
        }
    }
}
