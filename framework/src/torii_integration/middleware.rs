//! Bearer-token middleware for API-style authentication.
//!
//! Extracts an `Authorization: Bearer <token>` header from each request,
//! validates it against the Torii session store, and binds the resolved
//! `user_id` into the current per-request session scope so that
//! [`crate::Auth::check`] / [`crate::Auth::id`] work in downstream handlers.
//!
//! # Behaviour
//!
//! - Header present **and** token valid **and** session exists → call
//!   [`crate::session::set_auth_user`] with a stable `i64` derived from the
//!   torii `UserId` string, then pass the request through.
//! - Header missing **or** token invalid **or** session not found → pass the
//!   request through unchanged. The middleware never returns `401`; that is
//!   [`crate::auth::AuthMiddleware`]'s responsibility.
//!
//! # Deriving an `i64` from torii's `UserId`
//!
//! Torii's `UserId` is a prefixed opaque string (`"usr_<base58>"`), not a
//! numeric value. Suprnova's session layer stores the authenticated user as an
//! `i64`. The middleware converts the string to a **stable, deterministic**
//! `i64` using FNV-1a 64-bit hashing cast to `i64`. This value is used only as
//! a session marker — `Auth::id()` returns it and `Auth::user_as::<T>()` passes
//! it to the application's `UserProvider::retrieve_by_id`. Applications that
//! use torii-backed users (not a custom `UserProvider`) should use
//! `session.user_id` (the raw torii `UserId` string) from the torii session
//! object directly; the `i64` is purely a session-layer token.
//!
//! # Registration
//!
//! ```rust,ignore
//! use suprnova::{global_middleware, torii_integration::middleware::BearerTokenMiddleware};
//!
//! pub async fn register() {
//!     global_middleware!(BearerTokenMiddleware);
//! }
//! ```

use async_trait::async_trait;
use torii::SessionToken;

use crate::http::Response;
use crate::middleware::{Middleware, Next};
use crate::session::set_auth_user;
use crate::Request;

use super::instance;

/// Converts a torii `UserId` string to a stable `i64` using FNV-1a 64-bit hashing.
///
/// FNV-1a is chosen because it is fast, dependency-free, and deterministic
/// across platforms and restarts. The cast to `i64` is safe — the bit pattern
/// is preserved and the sign bit is treated as data.
pub(crate) fn user_id_to_i64(user_id_str: &str) -> i64 {
    // FNV-1a 64-bit constants
    const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;
    const FNV_PRIME: u64 = 1_099_511_628_211;

    let mut hash = FNV_OFFSET;
    for byte in user_id_str.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash as i64
}

/// Middleware that authenticates API requests via `Authorization: Bearer <token>`.
///
/// See [module documentation](self) for full behaviour.
pub struct BearerTokenMiddleware;

#[async_trait]
impl Middleware for BearerTokenMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Extract the Bearer token from the Authorization header.
        // hyper's HeaderMap uses case-insensitive keys, so this handles
        // both "Authorization" and "authorization".
        if let Some(auth_header) = request.header("Authorization")
            && let Some(token_str) = auth_header.strip_prefix("Bearer ") {
                let token_str = token_str.trim();
                if !token_str.is_empty() {
                    // Attempt to look up the session in the global Torii instance.
                    // Any failure (Torii not initialised, token invalid, session
                    // expired / not found) is treated as "no session" — pass through.
                    if let Ok(torii) = instance() {
                        let session_token = SessionToken::from(token_str);
                        if let Ok(session) = torii.get_session(&session_token).await {
                            let user_id = user_id_to_i64(session.user_id.as_str());
                            set_auth_user(user_id);
                        }
                    }
                }
            }

        next(request).await
    }
}
