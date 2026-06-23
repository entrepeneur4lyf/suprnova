//! The session guard — Laravel's `SessionGuard`.
//!
//! Resolves and persists authentication through the session
//! (`crate::session`) and the remember-me token table
//! (`crate::auth::remember`). It is the implementation behind the
//! default `web` guard and the sugar that the static [`Auth`] facade
//! delegates to.
//!
//! The guard owns no per-request state itself (it is a container
//! singleton). The "who is authenticated this request" cache and the
//! via-remember flag live in [`crate::auth::request_state`], scoped once
//! per request, so repeated `user()` calls don't re-query the provider
//! and `once`/`set_user` are visible to the whole request.

use std::sync::Arc;

use async_trait::async_trait;

use super::authenticatable::Authenticatable;
use super::contract::{Credentials, Guard, StatefulGuard};
use super::guard::Auth;
use super::provider::UserProvider;
use super::{events, request_state};
use crate::error::FrameworkError;
use crate::events::EventFacade;

/// Session-backed authentication guard.
///
/// Construct one with a [`UserProvider`]; the manager wires it up under
/// a name. Most apps reach it through the static [`Auth`] facade rather
/// than constructing it directly.
///
/// ```rust,no_run
/// use suprnova::{SessionGuard, StatefulGuard, Credentials};
/// use std::sync::Arc;
/// # use suprnova::DatabaseUserProvider;
/// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
/// # let my_provider = DatabaseUserProvider::new("users");
/// let guard = SessionGuard::new(Arc::new(my_provider));
/// let user = guard
///     .attempt(&Credentials::password("alice@example.com", "s3cret"), true)
///     .await?;
/// # Ok(()) }
/// ```
pub struct SessionGuard {
    /// The guard's name (e.g. `"web"`), carried on dispatched events.
    name: String,
    /// The user provider this guard resolves and validates against.
    provider: Arc<dyn UserProvider>,
    /// Remember-me token + cookie lifetime in minutes.
    remember_ttl_minutes: i64,
}

impl SessionGuard {
    /// Create a session guard named `"web"` with the given provider, using
    /// the environment's remember-me lifetime (`REMEMBER_LIFETIME`, default
    /// 30 days).
    pub fn new(provider: Arc<dyn UserProvider>) -> Self {
        Self::named("web", provider)
    }

    /// Create a session guard with an explicit name (the manager passes the
    /// registered guard name so lifecycle events are attributed correctly).
    pub fn named(name: impl Into<String>, provider: Arc<dyn UserProvider>) -> Self {
        let remember_ttl_minutes = i64::try_from(
            crate::session::SessionConfig::from_env()
                .remember_lifetime
                .as_secs()
                / 60,
        )
        .unwrap_or(i64::MAX);
        Self {
            name: name.into(),
            provider,
            remember_ttl_minutes,
        }
    }

    /// Override the remember-me token/cookie lifetime (minutes).
    pub fn with_remember_ttl(mut self, minutes: i64) -> Self {
        self.remember_ttl_minutes = minutes;
        self
    }
}

#[async_trait]
impl Guard for SessionGuard {
    async fn user(&self) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        // Per-request cache: a prior resolution, or a `once`/`set_user`.
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

#[async_trait]
impl StatefulGuard for SessionGuard {
    async fn attempt(
        &self,
        credentials: &Credentials,
        remember: bool,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        EventFacade::dispatch(events::Attempting {
            guard: self.name.clone(),
            remember,
        })
        .await?;

        let creds = credentials.as_value();
        if let Some(user) = self.provider.retrieve_by_credentials(&creds).await? {
            if self.provider.validate_credentials(&*user, &creds).await? {
                // login() fires Login + Authenticated.
                self.login(user.clone(), remember).await?;
                return Ok(Some(user));
            }
            // Identifier matched, credentials did not.
            EventFacade::dispatch(events::Failed {
                guard: self.name.clone(),
                user_id: Some(user.get_auth_identifier()),
            })
            .await?;
            return Ok(None);
        }

        // No user matched the supplied credentials. Drive a dummy
        // password-verify so the unknown-identifier wall-clock
        // matches the known-identifier-wrong-password wall-clock —
        // otherwise the difference (cheap DB-miss vs full bcrypt
        // cost) is a side-channel that lets an attacker probe the
        // user database without ever triggering the brute-force
        // lockout (which only counts attempts against KNOWN
        // accounts).
        let _ = self.provider.dummy_verify().await;
        EventFacade::dispatch(events::Failed {
            guard: self.name.clone(),
            user_id: None,
        })
        .await?;
        Ok(None)
    }

    async fn once(&self, credentials: &Credentials) -> Result<bool, FrameworkError> {
        EventFacade::dispatch(events::Attempting {
            guard: self.name.clone(),
            remember: false,
        })
        .await?;

        let creds = credentials.as_value();
        if let Some(user) = self.provider.retrieve_by_credentials(&creds).await? {
            if self.provider.validate_credentials(&*user, &creds).await? {
                let user_id = user.get_auth_identifier();
                request_state::set_current_user(user);
                EventFacade::dispatch(events::Authenticated {
                    guard: self.name.clone(),
                    user_id,
                })
                .await?;
                return Ok(true);
            }
            EventFacade::dispatch(events::Failed {
                guard: self.name.clone(),
                user_id: Some(user.get_auth_identifier()),
            })
            .await?;
            return Ok(false);
        }

        // No user matched — drive dummy_verify to equalise timing
        // against the wrong-password branch above. See `attempt` for
        // the full rationale.
        let _ = self.provider.dummy_verify().await;
        EventFacade::dispatch(events::Failed {
            guard: self.name.clone(),
            user_id: None,
        })
        .await?;
        Ok(false)
    }

    async fn login(
        &self,
        user: Arc<dyn Authenticatable>,
        remember: bool,
    ) -> Result<(), FrameworkError> {
        let user_id = user.get_auth_identifier();

        // Delegate session persistence (+ remember-me row/cookie) to the
        // proven facade helpers: both regenerate the session id and CSRF
        // token to defeat session fixation.
        if remember {
            Auth::login_remember(user_id.clone(), self.remember_ttl_minutes).await?;
        } else {
            Auth::login_id(user_id.clone())?;
        }

        // Cache the resolved user for the rest of the request.
        request_state::set_current_user(user);

        EventFacade::dispatch(events::Login {
            guard: self.name.clone(),
            user_id: user_id.clone(),
            remember,
        })
        .await?;
        EventFacade::dispatch(events::Authenticated {
            guard: self.name.clone(),
            user_id,
        })
        .await?;
        Ok(())
    }

    async fn login_using_id(
        &self,
        id: &str,
        remember: bool,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        match self.provider.retrieve_by_id(id).await? {
            Some(user) => {
                self.login(user.clone(), remember).await?;
                Ok(Some(user))
            }
            None => Ok(None),
        }
    }

    async fn once_using_id(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        match self.provider.retrieve_by_id(id).await? {
            Some(user) => {
                let user_id = user.get_auth_identifier();
                request_state::set_current_user(user.clone());
                EventFacade::dispatch(events::Authenticated {
                    guard: self.name.clone(),
                    user_id,
                })
                .await?;
                Ok(Some(user))
            }
            None => Ok(None),
        }
    }

    fn via_remember(&self) -> bool {
        request_state::via_remember()
    }

    async fn logout(&self) -> Result<(), FrameworkError> {
        // Capture the id before clearing so the Logout event is attributed.
        let user_id = crate::session::auth_user_id();

        // Tear down session + remember-me + request-scoped user. We call the
        // event-free primitive rather than `Auth::logout` so the Logout event
        // is dispatched exactly once, here, attributed to *this* guard's name.
        Auth::clear_authentication().await?;

        EventFacade::dispatch(events::Logout {
            guard: self.name.clone(),
            user_id,
        })
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;

    use crate::auth::request_state;
    use crate::session::{new_session_slot_for_test, session, session_scope_for_test};

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

    /// A provider that knows one user: id `"7"`, email `"a@b.com"`,
    /// password `"secret"`.
    struct FakeProvider;

    fn the_user() -> Arc<dyn Authenticatable> {
        Arc::new(TestUser { id: "7".into() })
    }

    #[async_trait]
    impl UserProvider for FakeProvider {
        async fn retrieve_by_id(
            &self,
            id: &str,
        ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
            Ok((id == "7").then(the_user))
        }

        async fn retrieve_by_credentials(
            &self,
            credentials: &serde_json::Value,
        ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
            let email = credentials.get("email").and_then(|v| v.as_str());
            Ok((email == Some("a@b.com")).then(the_user))
        }

        async fn validate_credentials(
            &self,
            _user: &dyn Authenticatable,
            credentials: &serde_json::Value,
        ) -> Result<bool, FrameworkError> {
            Ok(credentials.get("password").and_then(|v| v.as_str()) == Some("secret"))
        }
    }

    fn guard() -> SessionGuard {
        SessionGuard::new(Arc::new(FakeProvider))
    }

    /// Run `fut` inside both a fresh session scope and a fresh auth
    /// request-state scope — the two task-locals `SessionGuard` reads and
    /// writes at runtime.
    async fn with_scopes<F: std::future::Future>(fut: F) -> F::Output {
        let slot = new_session_slot_for_test();
        session_scope_for_test(slot, request_state::scope(fut)).await
    }

    #[tokio::test]
    async fn guest_when_no_session_user() {
        with_scopes(async {
            let g = guard();
            assert_eq!(g.id().await.unwrap(), None);
            assert!(!g.check().await.unwrap());
            assert!(g.guest().await.unwrap());
            assert!(g.user().await.unwrap().is_none());
            assert!(!g.has_user().await);
            assert!(!g.via_remember());
        })
        .await;
    }

    #[tokio::test]
    async fn login_persists_to_session_and_caches_user() {
        with_scopes(async {
            let g = guard();
            g.login(the_user(), false).await.unwrap();

            // Persisted to the session.
            assert_eq!(session().unwrap().user_id, Some("7".to_string()));
            // Visible through the guard.
            assert_eq!(g.id().await.unwrap(), Some("7".to_string()));
            assert!(g.check().await.unwrap());
            assert!(g.has_user().await);
            // user() returns the cached instance.
            let u = g.user().await.unwrap().expect("user resolved");
            assert_eq!(u.get_auth_identifier(), "7");
        })
        .await;
    }

    // A login→logout round-trip exercises remember-me revocation, which
    // needs a database; that path (and the lifecycle events) is covered by
    // the `tests/auth_session_guard.rs` integration test. Here we only
    // assert the DB-free guarantee: logging out when nobody is logged in is
    // safe and idempotent (no DB call, no panic).
    #[tokio::test]
    async fn logout_when_not_logged_in_is_safe() {
        with_scopes(async {
            let g = guard();
            g.logout().await.unwrap();
            assert!(g.guest().await.unwrap());
            assert!(!g.has_user().await);
        })
        .await;
    }

    #[tokio::test]
    async fn attempt_with_valid_credentials_logs_in() {
        with_scopes(async {
            let g = guard();
            let user = g
                .attempt(&Credentials::password("a@b.com", "secret"), false)
                .await
                .unwrap();
            assert_eq!(user.map(|u| u.get_auth_identifier()), Some("7".to_string()));
            assert_eq!(session().unwrap().user_id, Some("7".to_string()));
            assert!(g.check().await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn attempt_with_wrong_password_does_not_log_in() {
        with_scopes(async {
            let g = guard();
            let user = g
                .attempt(&Credentials::password("a@b.com", "wrong"), false)
                .await
                .unwrap();
            assert!(user.is_none());
            assert_eq!(session().unwrap().user_id, None);
            assert!(g.guest().await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn attempt_with_unknown_user_does_not_log_in() {
        with_scopes(async {
            let g = guard();
            let user = g
                .attempt(&Credentials::password("nobody@b.com", "secret"), false)
                .await
                .unwrap();
            assert!(user.is_none());
            assert!(g.guest().await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn validate_checks_credentials_without_logging_in() {
        with_scopes(async {
            let g = guard();
            assert!(
                g.validate(&Credentials::password("a@b.com", "secret"))
                    .await
                    .unwrap()
            );
            assert!(
                !g.validate(&Credentials::password("a@b.com", "wrong"))
                    .await
                    .unwrap()
            );
            // validate never authenticates.
            assert!(g.guest().await.unwrap());
            assert_eq!(session().unwrap().user_id, None);
        })
        .await;
    }

    #[tokio::test]
    async fn once_authenticates_without_persisting() {
        with_scopes(async {
            let g = guard();
            assert!(
                g.once(&Credentials::password("a@b.com", "secret"))
                    .await
                    .unwrap()
            );
            // Authenticated this request...
            assert!(g.check().await.unwrap());
            assert_eq!(g.id().await.unwrap(), Some("7".to_string()));
            assert!(g.has_user().await);
            // ...but never written to the session.
            assert_eq!(session().unwrap().user_id, None);
        })
        .await;
    }

    #[tokio::test]
    async fn once_with_bad_credentials_returns_false() {
        with_scopes(async {
            let g = guard();
            assert!(
                !g.once(&Credentials::password("a@b.com", "wrong"))
                    .await
                    .unwrap()
            );
            assert!(g.guest().await.unwrap());
        })
        .await;
    }

    #[tokio::test]
    async fn login_using_id_resolves_known_user() {
        with_scopes(async {
            let g = guard();
            let ok = g.login_using_id("7", false).await.unwrap();
            assert_eq!(ok.map(|u| u.get_auth_identifier()), Some("7".to_string()));
            assert!(g.check().await.unwrap());
            assert_eq!(session().unwrap().user_id, Some("7".to_string()));
        })
        .await;
    }

    #[tokio::test]
    async fn login_using_id_with_unknown_id_does_not_log_in() {
        with_scopes(async {
            let g = guard();
            let missing = g.login_using_id("999", false).await.unwrap();
            assert!(missing.is_none());
            assert!(g.guest().await.unwrap());
            assert_eq!(session().unwrap().user_id, None);
        })
        .await;
    }

    #[tokio::test]
    async fn once_using_id_authenticates_without_persisting() {
        with_scopes(async {
            let g = guard();
            let user = g.once_using_id("7").await.unwrap();
            assert_eq!(user.map(|u| u.get_auth_identifier()), Some("7".to_string()));
            assert!(g.check().await.unwrap());
            assert_eq!(session().unwrap().user_id, None);

            assert!(g.once_using_id("999").await.unwrap().is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn set_user_sets_request_user_without_persisting() {
        with_scopes(async {
            let g = guard();
            assert!(!g.has_user().await);
            g.set_user(the_user()).await;
            assert!(g.has_user().await);
            assert_eq!(g.id().await.unwrap(), Some("7".to_string()));
            // Not persisted.
            assert_eq!(session().unwrap().user_id, None);
        })
        .await;
    }
}
