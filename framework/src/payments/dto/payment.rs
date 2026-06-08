//! Payment / charge DTOs — request, status, and result shapes exchanged
//! with [`super::super::traits::Payment`].

use crate::payments::Money;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Lifecycle status of a provider-side transaction.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    /// Payment object created but not yet attempted.
    Created,
    /// Awaiting customer or merchant action (3DS / SCA / USSD prompt).
    RequiresAction,
    /// Submitted to provider; not yet finalized.
    Pending,
    /// Provider is processing asynchronously (Stripe `processing`).
    Processing,
    /// Authorized but not yet captured (separate-capture flow).
    Authorized,
    /// Checkout session or authorization expired before payment completed.
    Expired,
    /// Successfully completed.
    Succeeded,
    /// Payment failed.
    Failed,
    /// Customer or merchant canceled.
    Canceled,
    /// Fully refunded.
    Refunded,
    /// Partially refunded.
    PartiallyRefunded,
    /// Customer disputed the charge (chargeback opened).
    Disputed,
}

/// Request payload for [`super::super::traits::Payment::charge`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChargeRequest {
    /// Provider customer identifier being billed.
    pub customer_ref: String,
    /// Provider payment-method identifier to charge.
    pub payment_method_ref: String,
    /// Amount + currency.
    pub amount: Money,
    /// Short human-readable description; surfaced on receipts and
    /// dashboards.
    pub description: Option<String>,
    /// Idempotency key forwarded to the provider so retries do not
    /// double-charge.
    pub idempotency_key: Option<String>,
    /// Free-form metadata to attach to the provider-side charge.
    pub metadata: Option<Value>,
}

/// Result of `Payment::charge`. Stripe-shape providers may capture immediately (`Completed`);
/// some flows require redirect (off-session card with 3DS step-up) or a client-side action
/// (Stripe Elements completing the PaymentIntent in-browser).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChargeResult {
    /// Charge settled server-side — no further customer action needed.
    Completed {
        /// Provider transaction identifier (e.g. Stripe's `pi_…`).
        provider_transaction_id: String,
        /// Final amount captured.
        amount: Money,
        /// Final lifecycle status (typically [`PaymentStatus::Succeeded`]).
        status: PaymentStatus,
        /// Provider's raw transaction payload, preserved verbatim.
        provider_metadata: Value,
    },
    /// Charge requires a top-level redirect (e.g. 3DS step-up). Frontend
    /// navigates to `url`; provider redirects back to `return_to` once
    /// the customer completes the step.
    RedirectRequired {
        /// Provider transaction identifier — used for status polling
        /// once the customer returns.
        provider_transaction_id: String,
        /// Absolute URL the customer is redirected to.
        url: String,
        /// URL the provider should send the customer back to after the
        /// step completes. `None` falls back to a provider default.
        return_to: Option<String>,
    },
    /// Charge requires an in-browser action (e.g. Stripe Elements
    /// completing a PaymentIntent client-side).
    RequiresClientAction {
        /// Provider transaction identifier.
        provider_transaction_id: String,
        /// Action discriminator (provider-defined, kebab-case).
        action_kind: String,
        /// Client secret consumed by the front-end SDK, when the action
        /// uses one.
        client_secret: Option<String>,
        /// Publishable (front-end-safe) API key, when the action
        /// requires one.
        publishable_key: Option<String>,
    },
}

/// Request payload for [`super::super::traits::Payment::refund`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefundRequest {
    /// Provider transaction identifier to refund.
    pub provider_transaction_id: String,
    /// Partial-refund amount; `None` refunds the full charged amount.
    pub amount: Option<Money>,
    /// Optional human-readable reason; some providers surface this on
    /// the refund record.
    pub reason: Option<String>,
    /// Idempotency key forwarded to the provider so retries collapse to
    /// a single refund.
    pub idempotency_key: Option<String>,
}

/// Result of [`super::super::traits::Payment::refund`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefundResult {
    /// Provider refund identifier (e.g. Stripe's `re_…`).
    pub provider_refund_id: String,
    /// Provider transaction identifier the refund applies to.
    pub provider_transaction_id: String,
    /// Refunded amount.
    pub amount: Money,
    /// Provider's raw refund payload, preserved verbatim.
    pub provider_metadata: Value,
}
