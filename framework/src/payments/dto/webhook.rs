//! Webhook DTOs — the neutral event taxonomy plus the verification /
//! parsing payloads consumed by [`super::super::traits::WebhookHandler`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider-agnostic event taxonomy. Most domain handlers match `neutral`; provider-specific
/// edge cases fall through to `provider_event_type` + `raw_payload`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NeutralEventKind {
    /// A payment / charge settled successfully.
    PaymentSucceeded,
    /// A payment / charge attempt failed before settlement.
    PaymentFailed,
    /// A payment was refunded in full or in part.
    PaymentRefunded,
    /// A customer disputed a settled payment (chargeback opened).
    PaymentDisputed,
    /// A new subscription was created on the provider.
    SubscriptionCreated,
    /// An existing subscription's status / pricing was updated.
    SubscriptionUpdated,
    /// A subscription was canceled (either immediately or at period end).
    SubscriptionCanceled,
    /// A subscription invoice settled successfully.
    InvoicePaid,
    /// A subscription invoice failed to settle.
    InvoiceFailed,
    /// A new customer record was created on the provider.
    CustomerCreated,
    /// An existing customer record's billing details were updated.
    CustomerUpdated,
}

/// Parsed view of a provider webhook payload — ready for domain
/// dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEvent {
    /// Provider name (kebab-case — `"stripe"`, `"paddle"`, etc.).
    pub provider: String,
    /// Provider-issued event identifier (e.g. Stripe's `evt_…`).
    pub provider_event_id: String,
    /// Provider's raw event-type string (e.g.
    /// `"payment_intent.succeeded"`).
    pub provider_event_type: String,
    /// Neutral classification, when the provider's event type maps
    /// onto a [`NeutralEventKind`]. `None` for provider-specific events
    /// the framework does not normalise — handlers still see the raw
    /// `provider_event_type` and `raw_payload`.
    pub neutral: Option<NeutralEventKind>,
    /// Provider's raw event payload, preserved verbatim.
    pub raw_payload: Value,
}

/// Context passed to [`super::super::traits::WebhookHandler::verify`] —
/// the raw bytes plus the HTTP envelope the framework received.
#[derive(Debug, Clone)]
pub struct WebhookContext<'a> {
    /// Raw request body. Verification implementations sign or HMAC over
    /// these exact bytes — do not pre-normalise.
    pub body: &'a [u8],
    /// Full inbound header map; verification reads provider-specific
    /// signature headers from here (e.g. `Stripe-Signature`).
    pub headers: &'a http::HeaderMap,
    /// Remote peer address, when the framework was able to extract one
    /// from the connection (including proxy headers). Useful for
    /// IP-allowlist verification.
    pub remote_addr: Option<std::net::IpAddr>,
}
