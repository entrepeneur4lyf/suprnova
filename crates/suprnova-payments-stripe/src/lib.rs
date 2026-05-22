//! Stripe reference adapter for Suprnova's generic Payments surface.
//!
//! This crate implements the `Checkout`, `Payment`, and `Subscription` traits
//! from `suprnova::payments` against the Stripe API via `async-stripe` 1.0.0-rc.5.
//!
//! `CustomerStore`, `WebhookHandler`, and the top-level `PaymentProvider` composition
//! are implemented in the companion T10 crate layer once all four traits are in place.
//!
//! # Usage
//!
//! ```rust,no_run
//! use suprnova_payments_stripe::StripeProvider;
//!
//! let provider = StripeProvider::new("sk_test_...", "pk_test_...");
//! ```

mod checkout;
mod event_map;
mod payment;
mod subscription;

// Trait impls (Checkout/Payment/Subscription for StripeProvider) live inside the
// submodules; no items need re-exporting. event_map::map_stripe_event_type is used
// by T10's webhook handler — re-export it at the crate root for that.
pub use event_map::stripe_event_to_neutral;

use stripe::Client;

/// The Stripe adapter for Suprnova's provider-neutral payments surface.
///
/// Holds an authenticated `stripe::Client` (hyper-backed, async) and the
/// publishable key for client-side widget initialisation.
///
/// Clone is cheap — `stripe::Client` is internally `Arc`-backed.
#[derive(Clone, Debug)]
pub struct StripeProvider {
    client: Client,
    /// Stripe publishable key, surfaced in `SessionPayload::StripeElements` and
    /// `SessionPayload::StripeCheckoutRedirect` so the frontend can initialise
    /// Stripe.js without a separate config lookup.
    publishable_key: String,
}

impl StripeProvider {
    /// Construct a new provider.
    ///
    /// * `secret_key`     — Stripe secret key (`sk_live_…` / `sk_test_…`).
    /// * `publishable_key` — Stripe publishable key (`pk_live_…` / `pk_test_…`).
    ///
    /// # Panics
    /// Panics if `secret_key` cannot be used as an HTTP header value (i.e. it
    /// contains non-ASCII or control characters).  All real Stripe keys are safe.
    pub fn new(secret_key: impl Into<String>, publishable_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(secret_key),
            publishable_key: publishable_key.into(),
        }
    }

    /// Returns a reference to the underlying `stripe::Client`.
    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    /// Returns the publishable key for use in client-side payloads.
    pub(crate) fn publishable_key(&self) -> &str {
        &self.publishable_key
    }
}
