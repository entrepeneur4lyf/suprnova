//! Dashboard controller

use suprnova::{handler, inertia_response, Auth, InertiaProps, Model, Request, Response};
use serde::Serialize;

use crate::models::user::{Entity as UserEntity, User};

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
pub async fn index(_req: Request) -> Response {
    // Get the authenticated user
    let user_id = Auth::id().expect("User must be authenticated");

    let user = UserEntity::find_by_pk(user_id)
        .await?
        .expect("User must exist");

    inertia_response!(
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
