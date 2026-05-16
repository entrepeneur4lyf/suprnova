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
//!   [`crate::session::set_auth_user`] with the raw torii `UserId` string
//!   (e.g. `"usr_<base58>"`), then pass the request through.
//! - Header missing **or** token invalid **or** session not found → pass the
//!   request through unchanged. The middleware never returns `401`; that is
//!   [`crate::auth::AuthMiddleware`]'s responsibility.
//!
//! # User ID storage
//!
//! The raw torii `UserId` string is stored directly in the session's `user_id`
//! field. `Auth::id()` returns it as `Option<String>`, and
//! `Auth::user_as::<T>()` passes it to `UserProvider::retrieve_by_id(&str)`.
//! Applications that implement `UserProvider` receive the raw torii `UserId`
//! and can look up the user in their own store by that string.
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
                            set_auth_user(session.user_id.as_str());
                        }
                    }
                }
            }

        next(request).await
    }
}
