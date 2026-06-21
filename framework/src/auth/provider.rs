//! User provider trait for retrieving authenticated users from storage
//!
//! The application must implement this trait and register it with the container
//! to enable `Auth::user()`.

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::OnceLock;

use super::authenticatable::Authenticatable;
use crate::error::FrameworkError;

/// Precomputed bcrypt hash of an arbitrary string at the framework's
/// OWASP-floor cost (12). Used as the fallback dummy hash when the
/// configured driver can't be resolved or fails to mint one — so
/// [`UserProvider::dummy_verify`] always does *some* representative work
/// and never errors out the auth flow.
const FALLBACK_DUMMY_HASH: &str = "$2b$12$WzkqK0YIMJW8a4hkOEX/cuFNNDU.lI5jvyiQekkLwnAi8sFxlnEv6";

/// Process-wide cache of the dummy hash, minted once from the configured
/// driver. Caching avoids re-running the (deliberately expensive) hash
/// primitive on every enumeration-equalising call; the verify against it
/// still runs the full cost per call.
static DUMMY_HASH: OnceLock<String> = OnceLock::new();

/// A throwaway hash in the *configured* algorithm, suitable for feeding to
/// [`crate::hashing::verify_async`] so the verify cost tracks a real
/// stored-password verify under any `HASH_DRIVER`. Minted once and cached.
///
/// Falls back to [`FALLBACK_DUMMY_HASH`] (bcrypt cost 12) if the configured
/// driver can't be resolved or hashing fails — equalisation degrades to the
/// previous fixed-cost behaviour rather than erroring.
fn dummy_hash() -> String {
    DUMMY_HASH
        .get_or_init(|| {
            crate::hashing::default_driver()
                .and_then(|driver| driver.hash("dummy_password_never_matches"))
                .unwrap_or_else(|_| FALLBACK_DUMMY_HASH.to_string())
        })
        .clone()
}

/// Trait for retrieving authenticated users from storage
///
/// The application must implement this trait and register it with the container
/// to enable `Auth::user()`.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::auth::{UserProvider, Authenticatable};
/// use suprnova::FrameworkError;
/// use async_trait::async_trait;
/// use std::sync::Arc;
///
/// pub struct DatabaseUserProvider;
///
/// #[async_trait]
/// impl UserProvider for DatabaseUserProvider {
///     async fn retrieve_by_id(&self, id: &str) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
///         let id: i64 = id.parse().map_err(|_| FrameworkError::bad_request("user id must be numeric"))?;
///         let user = User::query()
///             .filter(Column::Id.eq(id as i32))
///             .first()
///             .await?;
///         Ok(user.map(|u| Arc::new(u) as Arc<dyn Authenticatable>))
///     }
/// }
/// ```
#[async_trait]
pub trait UserProvider: Send + Sync + 'static {
    /// Retrieve a user by their unique identifier
    ///
    /// The `id` is the string stored in the session's `user_id` field.
    /// For apps with numeric primary keys, parse the string: `id.parse::<i64>()`.
    /// For torii-backed apps, this is the raw torii `UserId` string (e.g. `"usr_<base58>"`).
    async fn retrieve_by_id(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError>;

    /// Retrieve a user by credentials (for custom authentication flows)
    ///
    /// Default implementation returns None (not supported).
    /// Override this if you need to authenticate by credentials other than ID.
    async fn retrieve_by_credentials(
        &self,
        _credentials: &serde_json::Value,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        Ok(None)
    }

    /// Validate credentials against a user
    ///
    /// Default implementation returns false (not supported).
    /// Override this if you need password validation.
    async fn validate_credentials(
        &self,
        _user: &dyn Authenticatable,
        _credentials: &serde_json::Value,
    ) -> Result<bool, FrameworkError> {
        Ok(false)
    }

    /// Run a fixed-cost hash verification to absorb the timing signal
    /// `validate_credentials` would emit on a matched user. The
    /// [`StatefulGuard`](super::StatefulGuard) calls this on the
    /// `retrieve_by_credentials` MISS branch so the wall-clock of
    /// `attempt(...)` for an unknown identifier matches the wall-clock
    /// for a known identifier with the wrong password — closing the
    /// account-enumeration timing oracle that the natural
    /// short-circuit-on-miss flow would otherwise create.
    ///
    /// Returns `Ok(false)` once the dummy verify completes; the
    /// result is discarded by the caller. The default implementation
    /// drives [`crate::hashing::verify_async`] against a throwaway
    /// hash minted by the *configured* driver so providers using the
    /// framework's hashing surface get equalisation for free — and so
    /// the dummy cost tracks the real verify cost regardless of
    /// `HASH_DRIVER` (a bcrypt-cost-12 dummy under `HASH_DRIVER=argon2id`
    /// would re-open the enumeration oracle through the timing gap).
    /// Providers whose `validate_credentials` uses a different verifier
    /// (custom JWT, external IDP) should override this to emit a
    /// comparable-cost no-op against their own primitive.
    async fn dummy_verify(&self) -> Result<bool, FrameworkError> {
        // Verify a throwaway hash produced by the configured driver, so
        // the work matches what a real verify of a stored password would
        // cost. `verify_async`/`verify_with` dispatch on the *stored*
        // hash's algorithm, so as long as the dummy hash is in the
        // configured algorithm the dummy and real paths run the same
        // primitive at the same cost. Any password input is rejected.
        let dummy_hash = dummy_hash();
        let _ = crate::hashing::verify_async("dummy_password_never_matches", &dummy_hash).await;
        Ok(false)
    }

    /// Look up a user by email for the auth-flow facades. Default: not
    /// supported (token-only providers return None).
    async fn retrieve_by_email(
        &self,
        _email: &str,
    ) -> Result<Option<crate::auth::AuthFlowUser>, FrameworkError> {
        Ok(None)
    }

    /// Look up a user by id, returning the auth-flow carrier (email/name).
    /// Used by PasswordReset to address the change-notification mail.
    /// Default: not supported.
    async fn flow_user_by_id(
        &self,
        _id: &str,
    ) -> Result<Option<crate::auth::AuthFlowUser>, FrameworkError> {
        Ok(None)
    }

    /// Mark a user's email verified. Default: unsupported.
    async fn mark_email_verified(&self, _id: &str) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal(
            "this user provider does not support email verification",
        ))
    }

    /// Set a user's password hash. Default: unsupported.
    async fn set_password(&self, _id: &str, _hashed: &str) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal(
            "this user provider does not support password reset",
        ))
    }

    /// Whether a user's email is verified. Default: unsupported.
    async fn is_email_verified(&self, _id: &str) -> Result<bool, FrameworkError> {
        Err(FrameworkError::internal(
            "this user provider does not support email verification",
        ))
    }
}
