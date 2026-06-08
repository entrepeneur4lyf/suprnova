//! Checkout-session DTOs — request and flow-tagged payload shapes
//! exchanged with [`super::super::traits::Checkout`].

use crate::payments::Money;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Whether a checkout session bills the customer once or sets up a
/// recurring subscription.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    /// Single charge against the customer's selected payment method.
    OneOff,
    /// Set up a subscription that will be billed recurrently.
    Subscription,
}

/// Request payload for [`super::super::traits::Checkout::start_session`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartSessionRequest {
    /// One-off charge or subscription setup — see [`SessionMode`].
    pub mode: SessionMode,
    /// Provider customer identifier (e.g. Stripe's `cus_…`) the session
    /// will be billed against.
    pub customer_ref: String,
    /// Provider price identifiers (e.g. Stripe's `price_…`) for the line
    /// items.
    pub price_refs: Vec<String>,
    /// Absolute URL the provider redirects to after a successful checkout.
    pub success_return_url: String,
    /// Absolute URL the provider redirects to if the customer cancels.
    pub cancel_return_url: String,
    /// Optional total used as a hint by adapters that do not derive the
    /// amount from `price_refs` alone (e.g. usage-billed flows).
    pub amount_hint: Option<Money>,
    /// Idempotency key forwarded to the provider so retries collapse to a
    /// single session.
    pub idempotency_key: Option<String>,
    /// Free-form metadata to attach to the provider-side session.
    pub metadata: Option<Value>,
}

/// Flow-tagged Inertia payload — frontend SDK dispatches on `flow` to render the right widget.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "flow", rename_all = "snake_case")]
pub enum SessionPayload {
    /// Stripe Elements embed — frontend mounts the Element with the
    /// supplied `client_secret` and `publishable_key`.
    StripeElements {
        /// PaymentIntent / SetupIntent client secret consumed by the
        /// Elements SDK.
        client_secret: String,
        /// Publishable (front-end-safe) Stripe API key.
        publishable_key: String,
        /// Provider session identifier — used for status polling and
        /// reconciliation.
        provider_session_id: String,
    },
    /// Stripe Checkout redirect — frontend issues a top-level navigation
    /// to `url`.
    StripeCheckoutRedirect {
        /// Absolute URL the customer is redirected to.
        url: String,
        /// Provider session identifier.
        provider_session_id: String,
    },
    /// Paddle inline (Drop-in) flow — frontend mounts the Paddle widget
    /// with the supplied tokens.
    PaddleInline {
        /// Paddle transaction identifier consumed by the Drop-in SDK.
        transaction_id: String,
        /// Optional customer token; supplied when an existing Paddle
        /// customer record can be reused.
        customer_token: Option<String>,
        /// Paddle client (frontend-safe) token.
        client_token: String,
    },
    /// Mobile Money flow — no redirect or embed. Frontend displays a
    /// user-facing message instructing the customer to confirm the payment
    /// on their phone (USSD prompt or operator app notification), then polls
    /// the provider via `provider_transaction_id` for status updates.
    MobileMoneyPrompt {
        /// Provider transaction identifier used for status polling.
        provider_transaction_id: String,
        /// Display message — provider-localized when possible
        /// (e.g. "Check your phone for the MTN MoMo prompt").
        message: String,
        /// Operator handling the USSD / app prompt — frontend can use
        /// this to render operator-specific branding.
        operator: super::MobileMoneyOperator,
    },
    /// Generic top-level redirect for providers that do not warrant a
    /// dedicated variant.
    Redirect {
        /// Absolute URL the customer is redirected to.
        url: String,
        /// Provider session identifier.
        provider_session_id: String,
    },
}
