//! Dashboard controller.
//!
//! Renders the post-login landing page with the currently authenticated
//! user's basic profile data.

use suprnova::{handler, inertia_response, Auth, FrameworkError, InertiaProps, Model, Request, Response};
use serde::Serialize;

use crate::models::user::User;

#[derive(Serialize)]
pub struct UserInfo {
    pub id: i64,
    pub name: String,
    pub email: String,
}

#[derive(InertiaProps)]
pub struct DashboardProps {
    pub user: UserInfo,
}

#[handler]
pub async fn index(req: Request) -> Response {
    // `Auth::id` stores ids as strings so the session layer stays
    // type-agnostic. Parse back into the model's `i64` primary key
    // before hitting the database.
    let user_id: i64 = Auth::id()
        .ok_or(FrameworkError::Unauthorized)?
        .parse()
        .map_err(|_| FrameworkError::internal("auth user id is not a valid i64"))?;

    let user = User::find(user_id)
        .await?
        .ok_or_else(|| FrameworkError::model_not_found("User"))?;

    inertia_response!(
        &req,
        "Dashboard",
        DashboardProps {
            user: UserInfo {
                id: user.id,
                name: user.name,
                email: user.email,
            }
        }
    )
}
