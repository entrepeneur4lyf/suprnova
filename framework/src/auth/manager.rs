//! The auth manager — Laravel's `AuthManager`.
//!
//! Holds the [`AuthConfig`] wiring plus the registered
//! [`UserProvider`]s, and resolves named guards on demand. Lives in the
//! service container (`App::singleton(AuthManager::new(config))`); the
//! static [`crate::Auth`] facade reaches it through `App::get`.
//!
//! Guard instances are built per resolution rather than cached: a
//! Suprnova guard is a cheap value object (name + provider handle), and
//! all per-request state lives in [`crate::auth::request_state`] / the
//! session, never on the instance. Building fresh each call sidesteps
//! cache invalidation and keeps the manager `Clone` + `Send + Sync`
//! without locking guard instances.
//!
//! `AuthManager` is `Clone`; clones share one provider registry (it is
//! `Arc<RwLock<…>>`), so `App::get::<AuthManager>()` returning a clone
//! still sees providers registered through any other handle.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::config::{AuthConfig, GuardDriver};
use super::contract::{Guard, StatefulGuard};
use super::provider::UserProvider;
use super::session_guard::SessionGuard;
use super::token_guard::TokenGuard;
use crate::error::FrameworkError;

/// Resolves named guards from configuration + registered providers.
#[derive(Clone)]
pub struct AuthManager {
    config: Arc<AuthConfig>,
    providers: Arc<RwLock<HashMap<String, Arc<dyn UserProvider>>>>,
}

impl AuthManager {
    /// Create a manager for the given configuration with no providers yet
    /// registered.
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config: Arc::new(config),
            providers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// The configuration this manager resolves against.
    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// The default guard's name (from [`AuthConfig::default_guard`]).
    pub fn default_guard_name(&self) -> &str {
        &self.config.default_guard
    }

    /// Register a [`UserProvider`] under `name`. Guards reference providers
    /// by this name in their [`crate::GuardConfig`].
    ///
    /// Idempotent-by-replacement: registering the same name twice keeps the
    /// last provider.
    pub fn register_provider(&self, name: impl Into<String>, provider: Arc<dyn UserProvider>) {
        // Recover-in-place on poison: a poisoned providers registry must not
        // take auth resolution down for every subsequent request.
        let mut map = self.providers.write().unwrap_or_else(|e| e.into_inner());
        map.insert(name.into(), provider);
    }

    /// Look up a registered provider by name.
    fn provider(&self, name: &str) -> Result<Arc<dyn UserProvider>, FrameworkError> {
        let map = self.providers.read().unwrap_or_else(|e| e.into_inner());
        map.get(name).cloned().ok_or_else(|| {
            FrameworkError::internal(format!(
                "No UserProvider registered under '{name}'. Register one with: \
                 Auth::register_provider(\"{name}\", Arc::new(YourProvider))"
            ))
        })
    }

    /// Resolve a guard by name as the read-only [`Guard`] contract.
    ///
    /// Works for every driver (session and token). For login/logout/attempt,
    /// use [`stateful_guard`](Self::stateful_guard).
    pub fn guard(&self, name: &str) -> Result<Arc<dyn Guard>, FrameworkError> {
        let config = self.guard_config(name)?;
        let provider = self.provider(&config.provider)?;
        Ok(match config.driver {
            GuardDriver::Session => Arc::new(SessionGuard::named(name, provider)) as Arc<dyn Guard>,
            GuardDriver::Token => Arc::new(TokenGuard::new(provider)) as Arc<dyn Guard>,
        })
    }

    /// Resolve a guard by name as a [`StatefulGuard`] (login/logout/attempt).
    ///
    /// Errors if the named guard's driver is stateless (a token guard):
    /// stateless API auth has no login, and surfacing that as an error
    /// rather than a silent `None` tells the caller exactly why.
    pub fn stateful_guard(&self, name: &str) -> Result<Arc<dyn StatefulGuard>, FrameworkError> {
        let config = self.guard_config(name)?;
        let provider = self.provider(&config.provider)?;
        match config.driver {
            GuardDriver::Session => {
                Ok(Arc::new(SessionGuard::named(name, provider)) as Arc<dyn StatefulGuard>)
            }
            GuardDriver::Token => Err(FrameworkError::internal(format!(
                "Guard '{name}' is a token guard (stateless): it has no login/logout/attempt. \
                 Use Auth::guard(\"{name}\") for read-only access."
            ))),
        }
    }

    /// Resolve the default guard as the read-only [`Guard`] contract.
    pub fn default_guard(&self) -> Result<Arc<dyn Guard>, FrameworkError> {
        self.guard(&self.config.default_guard)
    }

    /// Resolve the default guard as a [`StatefulGuard`].
    pub fn default_stateful_guard(&self) -> Result<Arc<dyn StatefulGuard>, FrameworkError> {
        self.stateful_guard(&self.config.default_guard)
    }

    fn guard_config(&self, name: &str) -> Result<super::config::GuardConfig, FrameworkError> {
        self.config.guard_config(name).cloned().ok_or_else(|| {
            FrameworkError::internal(format!(
                "Auth guard '{name}' is not defined. Define it in your AuthConfig \
                 (e.g. AuthConfig::new(\"web\").guard(\"{name}\", GuardConfig::session(\"users\")))."
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Authenticatable;
    use crate::auth::contract::Credentials;
    use async_trait::async_trait;

    struct FakeProvider;
    #[async_trait]
    impl UserProvider for FakeProvider {
        async fn retrieve_by_id(
            &self,
            _id: &str,
        ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
            Ok(None)
        }
    }

    fn manager_with_users() -> AuthManager {
        let m = AuthManager::new(AuthConfig::default());
        m.register_provider("users", Arc::new(FakeProvider));
        m
    }

    #[test]
    fn resolves_session_guard_as_both_contracts() {
        let m = manager_with_users();
        assert!(m.guard("web").is_ok());
        assert!(m.stateful_guard("web").is_ok());
        assert_eq!(m.default_guard_name(), "web");
        assert!(m.default_guard().is_ok());
        assert!(m.default_stateful_guard().is_ok());
    }

    #[test]
    fn token_guard_resolves_as_guard_but_not_stateful() {
        let m = manager_with_users();
        assert!(m.guard("api").is_ok());
        let err = m
            .stateful_guard("api")
            .err()
            .expect("expected a 'token guard is stateless' error");
        assert!(
            err.to_string().contains("token guard"),
            "expected a 'token guard is stateless' message, got: {err}"
        );
    }

    #[test]
    fn unknown_guard_is_an_error() {
        let m = manager_with_users();
        let err = m
            .guard("ghost")
            .err()
            .expect("expected unknown-guard error");
        assert!(err.to_string().contains("not defined"));
    }

    #[test]
    fn missing_provider_is_an_error_with_remediation() {
        // No provider registered for the default "users" name.
        let m = AuthManager::new(AuthConfig::default());
        let err = m
            .guard("web")
            .err()
            .expect("expected missing-provider error");
        assert!(
            err.to_string()
                .contains("No UserProvider registered under 'users'")
        );
        assert!(err.to_string().contains("Auth::register_provider"));
    }

    #[test]
    fn register_provider_shared_across_clones() {
        let m = AuthManager::new(AuthConfig::default());
        let clone = m.clone();
        // Register through the clone; original must see it (shared registry).
        clone.register_provider("users", Arc::new(FakeProvider));
        assert!(m.guard("web").is_ok());
    }

    // A guard resolved from the manager is the real contract object.
    #[tokio::test]
    async fn resolved_guard_validate_routes_through_provider() {
        let m = manager_with_users();
        let g = m.guard("web").unwrap();
        // FakeProvider returns None for retrieve_by_credentials (trait
        // default), so validate is false — proves the wiring resolves.
        assert!(
            !g.validate(&Credentials::password("a@b.com", "x"))
                .await
                .unwrap()
        );
    }
}
