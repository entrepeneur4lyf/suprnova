//! Route middleware for RBAC checks.

use std::marker::PhantomData;

use async_trait::async_trait;

use crate::auth::Auth;
use crate::http::{HttpResponse, Request, Response};
use crate::middleware::{Middleware, Next};

use super::HasRoles;

fn forbidden_response() -> HttpResponse {
    HttpResponse::json(serde_json::json!({
        "message": "This action is unauthorized."
    }))
    .status(403)
}

/// Middleware that requires the authenticated user to have a role.
///
/// `U` is the concrete authenticated user type stored by [`Auth`].
/// Compose after [`crate::AuthMiddleware`] so unauthenticated users are
/// handled consistently before role checks run.
pub struct RoleMiddleware<U> {
    role: String,
    marker: PhantomData<fn() -> U>,
}

impl<U> RoleMiddleware<U> {
    /// Create middleware requiring `role`.
    pub fn new(role: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            marker: PhantomData,
        }
    }
}

#[async_trait]
impl<U> Middleware for RoleMiddleware<U>
where
    U: HasRoles,
{
    async fn handle(&self, request: Request, next: Next) -> Response {
        let Some(user) = Auth::user_as_arc::<U>().await? else {
            return Err(forbidden_response());
        };

        if user.has_role(&self.role).await? {
            next(request).await
        } else {
            Err(forbidden_response())
        }
    }
}

/// Middleware that requires the authenticated user to have a permission.
///
/// Direct model permissions and permissions inherited through assigned roles
/// are both accepted.
pub struct PermissionMiddleware<U> {
    permission: String,
    marker: PhantomData<fn() -> U>,
}

impl<U> PermissionMiddleware<U> {
    /// Create middleware requiring `permission`.
    pub fn new(permission: impl Into<String>) -> Self {
        Self {
            permission: permission.into(),
            marker: PhantomData,
        }
    }
}

#[async_trait]
impl<U> Middleware for PermissionMiddleware<U>
where
    U: HasRoles,
{
    async fn handle(&self, request: Request, next: Next) -> Response {
        let Some(user) = Auth::user_as_arc::<U>().await? else {
            return Err(forbidden_response());
        };

        if user.has_permission_to(&self.permission).await? {
            next(request).await
        } else {
            Err(forbidden_response())
        }
    }
}
