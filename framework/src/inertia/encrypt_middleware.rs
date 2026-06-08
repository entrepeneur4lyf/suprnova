//! `EncryptHistory` middleware — opts a route or group of routes into
//! Inertia history-state encryption.
//!
//! Per the v3 protocol, when the page object carries `encryptHistory:
//! true`, the client AES-encrypts its history-state entry before
//! pushing. After logout/cleared-history, the key is rotated so the
//! prior entries can't be decrypted — protecting privileged data the
//! user navigated through while authenticated.
//!
//! ## Precedence
//!
//! `InertiaResponse::resolve` decides the final `encryptHistory` flag
//! in this order (later wins):
//!
//! 1. [`InertiaConfig::encrypt_history_default`](crate::InertiaConfig::encrypt_history_default)
//! 2. This middleware (`tokio::task_local` per request)
//! 3. Per-response `InertiaResponse::encrypt_history(bool)` (handler
//!    override — usually wins, including to opt OUT of group-level
//!    encryption)

use crate::http::{Request, Response};
use crate::inertia::flash::ENCRYPT_HISTORY;
use crate::middleware::{Middleware, Next};
use async_trait::async_trait;

/// Middleware that sets the per-request `encryptHistory` flag so the Inertia
/// client encrypts its history snapshot for sensitive pages.
pub struct EncryptHistoryMiddleware;

impl EncryptHistoryMiddleware {
    /// Build a new `EncryptHistoryMiddleware`. Stateless — no arguments needed.
    pub fn new() -> Self {
        Self
    }
}

impl Default for EncryptHistoryMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for EncryptHistoryMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        ENCRYPT_HISTORY
            .scope(true, async move { next(request).await })
            .await
    }
}
