//! UserResource -- JSON:API representation of a User.
//!
//! `#[derive(Data)]` generates Serialize, Deserialize, and FormRequest impls.
//! `#[json_resource("users")]` generates the `IntoJsonResource` impl that
//! `Resource::collection` and `Resource::single` require.
//!
//! The `id` field is used as the JSON:API resource identifier.
//! All other fields become JSON:API attributes.

use suprnova::{Data, Validate};

use crate::models::user::User;

#[derive(Data, Validate)]
#[json_resource("users")]
pub struct UserResource {
    pub id: String,
    pub email: String,
}

impl From<User> for UserResource {
    fn from(user: User) -> Self {
        Self {
            id: user.id.to_string(),
            email: user.email,
        }
    }
}
