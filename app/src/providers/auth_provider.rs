//! Authentication provider for user retrieval
//!
//! This provider implements the `UserProvider` trait to fetch users from the database.

use async_trait::async_trait;
use std::sync::Arc;
use suprnova::auth::{Authenticatable, UserProvider};
use suprnova::FrameworkError;

use crate::models::users::User;

/// Database-backed user provider for authentication
///
/// This provider fetches users from the database by their ID.
/// Register it in `bootstrap.rs` to enable `Auth::user()`:
///
/// ```rust,ignore
/// use suprnova::bind;
/// use suprnova::UserProvider;
/// use crate::providers::DatabaseUserProvider;
///
/// bind!(dyn UserProvider, DatabaseUserProvider);
/// ```
#[derive(Default)]
pub struct DatabaseUserProvider;

#[async_trait]
impl UserProvider for DatabaseUserProvider {
    async fn retrieve_by_id(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        let numeric_id: i64 = id
            .parse()
            .map_err(|_| FrameworkError::bad_request("user id must be numeric"))?;

        let user = User::find_by_id(numeric_id).await?;

        Ok(user.map(|u| Arc::new(u) as Arc<dyn Authenticatable>))
    }
}
