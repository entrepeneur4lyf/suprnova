//! Paddle reference adapter for Suprnova's generic Payments surface.
//!
//! Paddle is a Merchant of Record — it owns subscription lifecycle, tax,
//! dunning. Consequently Paddle does NOT expose server-side capture, so
//! the `Payment` trait is intentionally NOT implemented for `PaddleProvider`.
//! `PaymentProvider::as_payment()` returns `None`, and a test enforces this
//! invariant.
//!
//! Subscriptions are created indirectly via checkout completion: domain code
//! calls `Checkout::start_session` and awaits the `SubscriptionCreated`
//! webhook for the resulting subscription_id. `Subscription::subscribe`
//! returns `PaymentError::NotSupported` with a clear migration message.

mod checkout;
mod customer;
mod event_map;
mod subscription;
mod webhook;

pub use event_map::paddle_event_to_neutral;

use paddle_rust_sdk::Paddle;
use std::sync::Arc;
use suprnova::payments::PaymentProvider;

/// Paddle environment selector — Sandbox for testing, Production for live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaddleEnvironment {
    Sandbox,
    Production,
}

/// The Paddle adapter for Suprnova's provider-neutral payments surface.
#[derive(Clone)]
pub struct PaddleProvider {
    client: Arc<Paddle>,
    /// Webhook notification destination secret (`pdl_ntfset_…`) used to verify
    /// the signed-envelope signature on incoming webhook payloads via
    /// `Paddle::unmarshal`.
    webhook_key: String,
    /// Paddle client-side token (`live_…` / `test_…`) — surfaced in
    /// `SessionPayload::PaddleInline` so the frontend can initialise paddle.js
    /// without a separate config lookup.
    client_token: String,
    environment: PaddleEnvironment,
}

impl PaddleProvider {
    /// Construct a new provider.
    ///
    /// * `api_key`      — Paddle API key (`pdl_live_apikey_…` / `pdl_sdbx_apikey_…`).
    /// * `webhook_key`  — Notification destination secret (`pdl_ntfset_…`).
    /// * `client_token` — Client-side token (`live_…` / `test_…`).
    /// * `environment`  — Sandbox or Production.
    #[allow(clippy::result_large_err)] // paddle_rust_sdk::Error is large; not worth boxing.
    pub fn new(
        api_key: impl Into<String>,
        webhook_key: impl Into<String>,
        client_token: impl Into<String>,
        environment: PaddleEnvironment,
    ) -> Result<Self, paddle_rust_sdk::Error> {
        let endpoint = match environment {
            PaddleEnvironment::Sandbox => Paddle::SANDBOX,
            PaddleEnvironment::Production => Paddle::PRODUCTION,
        };
        Ok(Self {
            client: Arc::new(Paddle::new(api_key.into(), endpoint)?),
            webhook_key: webhook_key.into(),
            client_token: client_token.into(),
            environment,
        })
    }

    /// Construct a provider from environment variables.
    ///
    /// Reads:
    /// - `PADDLE_API_KEY`
    /// - `PADDLE_WEBHOOK_KEY`
    /// - `PADDLE_CLIENT_TOKEN`
    /// - `PADDLE_ENVIRONMENT` (optional, defaults to "sandbox")
    pub fn from_env() -> Result<Self, String> {
        let api_key =
            std::env::var("PADDLE_API_KEY").map_err(|_| "PADDLE_API_KEY not set".to_string())?;
        let webhook_key = std::env::var("PADDLE_WEBHOOK_KEY")
            .map_err(|_| "PADDLE_WEBHOOK_KEY not set".to_string())?;
        let client_token = std::env::var("PADDLE_CLIENT_TOKEN")
            .map_err(|_| "PADDLE_CLIENT_TOKEN not set".to_string())?;
        let env = match std::env::var("PADDLE_ENVIRONMENT").as_deref() {
            Ok("production") => PaddleEnvironment::Production,
            _ => PaddleEnvironment::Sandbox,
        };
        Self::new(api_key, webhook_key, client_token, env).map_err(|e| format!("{e}"))
    }

    pub(crate) fn client(&self) -> &Paddle {
        &self.client
    }
    pub(crate) fn webhook_key(&self) -> &str {
        &self.webhook_key
    }
    pub(crate) fn client_token(&self) -> &str {
        &self.client_token
    }
    pub fn environment(&self) -> PaddleEnvironment {
        self.environment
    }
}

impl PaymentProvider for PaddleProvider {
    fn name(&self) -> &'static str {
        "paddle"
    }
    // Intentionally NOT overriding `as_payment` — defaults to None.
    // Paddle is Merchant-of-Record and does not expose server-side capture.
}
