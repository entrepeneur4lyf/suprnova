//! Dashboard controller.
//!
//! Renders the post-login landing page with the currently authenticated
//! user's basic profile data.

use suprnova::{handler, inertia_response, Auth, FrameworkError, InertiaProps, Request, Response};
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
    // The registered user provider resolves the typed `User` from the
    // session id; `user_as` downcasts the `Authenticatable` for us.
    let user = Auth::user_as::<User>()
        .await?
        .ok_or(FrameworkError::Unauthorized)?;

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
