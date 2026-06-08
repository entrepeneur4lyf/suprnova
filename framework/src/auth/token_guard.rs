//! The token guard — Laravel's `TokenGuard`, for stateless API auth.
//!
//! Unlike [`SessionGuard`](super::SessionGuard), `TokenGuard` is
//! **read-only**: it implements [`Guard`] but **not**
//! [`StatefulGuard`](super::StatefulGuard), because a bearer-token API
//! has no "login" or "logout" — the token *is* the credential. This
//! type-level distinction is the point: `Auth::stateful_guard("api")`
//! fails fast rather than letting a caller persist a session-cookie
//! login through a token-named guard.
//!
//! # Depends on `BearerTokenMiddleware`
//!
//! `TokenGuard` reads the authenticated id from the per-request session
//! scope (`session::auth_user_id`). It does **not** parse the
//! `Authorization` header itself — that is
//! [`crate::torii_integration::middleware::BearerTokenMiddleware`]'s job:
//! it validates the `Authorization: Bearer <token>` header against the
//! Torii session store and binds the resolved `user_id` into the request
//! scope. **Register `BearerTokenMiddleware` on token-guarded routes**,
//! or `TokenGuard` will always report a guest. The guard then resolves
//! the full user via its [`UserProvider`].

use std::sync::Arc;

use async_trait::async_trait;

use super::authenticatable::Authenticatable;
use super::contract::{Credentials, Guard};
use super::provider::UserProvider;
use super::request_state;
use crate::error::FrameworkError;

/// Stateless, bearer-token authentication guard.
///
/// See the [module docs](self) for the `BearerTokenMiddleware`
/// dependency. Resolves the current user from the request-scoped id and
/// the guard's [`UserProvider`].
pub struct TokenGuard {
    /// The user provider this guard resolves and validates against.
    provider: Arc<dyn UserProvider>,
}

impl TokenGuard {
    /// Create a token guard with the given provider. Guards are named
    /// at the dispatcher level (see [`crate::auth::AuthManager`]), so
    /// the guard itself doesn't carry its own name.
    pub fn new(provider: Arc<dyn UserProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Guard for TokenGuard {
    async fn user(&self) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        // Per-request cache (a prior resolution, or a `set_user`).
        if let Some(user) = request_state::current_user() {
            return Ok(Some(user));
        }

        let id = match crate::session::auth_user_id() {
            Some(id) => id,
            None => return Ok(None),
        };

        let user = self.provider.retrieve_by_id(&id).await?;
        if let Some(user) = &user {
            request_state::set_current_user(user.clone());
        }
        Ok(user)
    }

    async fn id(&self) -> Result<Option<String>, FrameworkError> {
        Ok(crate::session::auth_user_id())
    }

    async fn validate(&self, credentials: &Credentials) -> Result<bool, FrameworkError> {
        let creds = credentials.as_value();
        match self.provider.retrieve_by_credentials(&creds).await? {
            Some(user) => self.provider.validate_credentials(&*user, &creds).await,
            None => Ok(false),
        }
    }

    async fn set_user(&self, user: Arc<dyn Authenticatable>) {
        request_state::set_current_user(user);
    }

    async fn has_user(&self) -> bool {
        request_state::has_current_user()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;

    use crate::auth::request_state;
    use crate::session::{new_session_slot_for_test, session_scope_for_test, set_auth_user};

    #[derive(Clone)]
    struct TestUser {
        id: String,
    }

    impl Authenticatable for TestUser {
        fn get_auth_identifier(&self) -> String {
            self.id.clone()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    struct FakeProvider;

    #[async_trait]
    impl UserProvider for FakeProvider {
        async fn retrieve_by_id(
            &self,
            id: &str,
        ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
            Ok((id == "7")
                .then(|| Arc::new(TestUser { id: "7".into() }) as Arc<dyn Authenticatable>))
        }
    }

    fn guard() -> TokenGuard {
        TokenGuard::new(Arc::new(FakeProvider))
    }

    async fn with_scopes<F: std::future::Future>(fut: F) -> F::Output {
        let slot = new_session_slot_for_test();
        session_scope_for_test(slot, request_state::scope(fut)).await
    }

    #[tokio::test]
    async fn guest_without_a_bearer_populated_id() {
        with_scopes(async {
            let g = guard();
            assert!(g.guest().await.unwrap());
            assert!(g.user().await.unwrap().is_none());
            assert!(!g.has_user().await);
        })
        .await;
    }

    #[tokio::test]
    async fn resolves_user_from_request_scoped_id() {
        with_scopes(async {
            // Stand in for BearerTokenMiddleware binding the id into the
            // request scope.
            set_auth_user("7");
            let g = guard();
            assert!(g.check().await.unwrap());
            assert_eq!(g.id().await.unwrap(), Some("7".to_string()));
            let user = g.user().await.unwrap().expect("user resolved");
            assert_eq!(user.get_auth_identifier(), "7");
        })
        .await;
    }
}
