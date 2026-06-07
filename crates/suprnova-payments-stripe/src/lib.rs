//! Stripe reference adapter for Suprnova's generic Payments surface.
//!
//! This crate implements the `Checkout`, `Payment`, `Subscription`, `CustomerStore`,
//! and `WebhookHandler` traits from `suprnova::payments` against the Stripe API via
//! `async-stripe` 1.0.0-rc.5. It also provides the top-level `PaymentProvider`
//! umbrella impl that ties them all together.
//!
//! # Usage
//!
//! ```rust,no_run
//! use suprnova_payments_stripe::StripeProvider;
//!
//! let provider = StripeProvider::new("sk_test_...", "pk_test_...", "whsec_...");
//! ```

mod checkout;
mod customer;
mod event_map;
mod payment;
mod subscription;
mod webhook;

// Trait impls (Checkout/Payment/Subscription/CustomerStore/WebhookHandler for
// StripeProvider) live inside the submodules. event_map::stripe_event_to_neutral
// is re-exported for callers that want to map Stripe event strings outside of
// the webhook handler.
pub use event_map::stripe_event_to_neutral;

use stripe::Client;
use suprnova::payments::traits::{Payment, PaymentProvider};

/// Default tolerance for Stripe webhook signature timestamps, matching the
/// 300-second window enforced by Stripe's official client libraries.
///
/// Webhook verification rejects payloads whose `t=<ts>` claim differs from
/// the local clock by more than this delta — a captured signed body cannot
/// then be replayed indefinitely. Override with
/// [`StripeProvider::with_signature_tolerance`] when tests need to lock the
/// clock or production has unusual NTP skew.
pub const DEFAULT_WEBHOOK_SIGNATURE_TOLERANCE_SECONDS: i64 = 300;

/// The Stripe adapter for Suprnova's provider-neutral payments surface.
///
/// Holds an authenticated `stripe::Client` (hyper-backed, async), the
/// publishable key for client-side widget initialisation, and the webhook
/// signing secret for `WebhookHandler::verify`.
///
/// Clone is cheap — `stripe::Client` is internally `Arc`-backed.
#[derive(Clone, Debug)]
pub struct StripeProvider {
    client: Client,
    /// Stripe publishable key, surfaced in `SessionPayload::StripeElements` and
    /// `SessionPayload::StripeCheckoutRedirect` so the frontend can initialise
    /// Stripe.js without a separate config lookup.
    publishable_key: String,
    /// Webhook signing secret (`whsec_…`) used to verify the HMAC-SHA256
    /// signature on incoming webhook payloads.
    webhook_signing_secret: String,
    /// Maximum tolerated drift, in seconds, between the timestamp Stripe
    /// includes in the signature header and the local wall clock. Webhook
    /// payloads outside this window are rejected. Defaults to
    /// [`DEFAULT_WEBHOOK_SIGNATURE_TOLERANCE_SECONDS`].
    webhook_signature_tolerance_seconds: i64,
}

impl StripeProvider {
    /// Construct a new provider.
    ///
    /// * `secret_key`             — Stripe secret key (`sk_live_…` / `sk_test_…`).
    /// * `publishable_key`        — Stripe publishable key (`pk_live_…` / `pk_test_…`).
    /// * `webhook_signing_secret` — Webhook endpoint signing secret (`whsec_…`).
    ///
    /// The webhook signature tolerance defaults to
    /// [`DEFAULT_WEBHOOK_SIGNATURE_TOLERANCE_SECONDS`]; override with
    /// [`Self::with_signature_tolerance`].
    ///
    /// # Panics
    /// Panics if `secret_key` cannot be used as an HTTP header value (i.e. it
    /// contains non-ASCII or control characters).  All real Stripe keys are safe.
    pub fn new(
        secret_key: impl Into<String>,
        publishable_key: impl Into<String>,
        webhook_signing_secret: impl Into<String>,
    ) -> Self {
        Self {
            client: Client::new(secret_key),
            publishable_key: publishable_key.into(),
            webhook_signing_secret: webhook_signing_secret.into(),
            webhook_signature_tolerance_seconds: DEFAULT_WEBHOOK_SIGNATURE_TOLERANCE_SECONDS,
        }
    }

    /// Construct a provider from environment variables.
    ///
    /// Reads:
    /// - `STRIPE_SECRET_KEY`
    /// - `STRIPE_PUBLISHABLE_KEY`
    /// - `STRIPE_WEBHOOK_SIGNING_SECRET`
    ///
    /// Returns an error string if any variable is missing.
    pub fn from_env() -> Result<Self, String> {
        let secret_key = std::env::var("STRIPE_SECRET_KEY")
            .map_err(|_| "STRIPE_SECRET_KEY env var not set".to_string())?;
        let publishable_key = std::env::var("STRIPE_PUBLISHABLE_KEY")
            .map_err(|_| "STRIPE_PUBLISHABLE_KEY env var not set".to_string())?;
        let webhook_signing_secret = std::env::var("STRIPE_WEBHOOK_SIGNING_SECRET")
            .map_err(|_| "STRIPE_WEBHOOK_SIGNING_SECRET env var not set".to_string())?;
        Ok(Self::new(
            secret_key,
            publishable_key,
            webhook_signing_secret,
        ))
    }

    /// Returns a reference to the underlying `stripe::Client`.
    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    /// Returns the publishable key for use in client-side payloads.
    pub(crate) fn publishable_key(&self) -> &str {
        &self.publishable_key
    }

    /// Returns the webhook signing secret for HMAC-SHA256 signature verification.
    pub(crate) fn webhook_signing_secret(&self) -> &str {
        &self.webhook_signing_secret
    }

    /// Returns the current webhook signature timestamp tolerance, in seconds.
    pub(crate) fn webhook_signature_tolerance_seconds(&self) -> i64 {
        self.webhook_signature_tolerance_seconds
    }

    /// Override the webhook signature timestamp tolerance, in seconds.
    ///
    /// Stripe's official client libraries default to 300 seconds; lower the
    /// window to tighten replay-resistance, raise it when the deployment has
    /// known clock skew that Stripe's retry cadence would otherwise reject.
    /// A negative value would reject every payload — clamped to zero so the
    /// minimum behaviour is "exact-timestamp match" rather than always-fail.
    ///
    /// ```ignore
    /// use suprnova_payments_stripe::StripeProvider;
    /// let provider = StripeProvider::new("sk_test", "pk_test", "whsec_test")
    ///     .with_signature_tolerance(60);
    /// ```
    pub fn with_signature_tolerance(mut self, tolerance_seconds: i64) -> Self {
        self.webhook_signature_tolerance_seconds = tolerance_seconds.max(0);
        self
    }
}

// ---------------------------------------------------------------------------
// PaymentProvider umbrella impl
// ---------------------------------------------------------------------------

impl PaymentProvider for StripeProvider {
    fn name(&self) -> &'static str {
        "stripe"
    }

    /// Returns `Some(self)` — Stripe exposes server-capture via PaymentIntents,
    /// so the `Payment` trait is implemented for `StripeProvider`.
    fn as_payment(&self) -> Option<&dyn Payment> {
        Some(self)
    }
}
