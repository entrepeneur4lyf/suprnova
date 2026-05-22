use crate::payments::Money;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChargeRequest {
    pub customer_ref: String,
    pub payment_method_ref: String,
    pub amount: Money,
    pub description: Option<String>,
    pub idempotency_key: Option<String>,
    pub metadata: Option<Value>,
}

/// Result of `Payment::charge`. Stripe-shape providers may capture immediately (`Completed`);
/// some flows require redirect (off-session card with 3DS step-up) or a client-side action
/// (Stripe Elements completing the PaymentIntent in-browser).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChargeResult {
    Completed {
        provider_transaction_id: String,
        amount: Money,
        status: PaymentStatus,
        provider_metadata: Value,
    },
    RedirectRequired {
        provider_transaction_id: String,
        url: String,
        return_to: Option<String>,
    },
    RequiresClientAction {
        provider_transaction_id: String,
        action_kind: String,
        client_secret: Option<String>,
        publishable_key: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefundRequest {
    pub provider_transaction_id: String,
    pub amount: Option<Money>,
    pub reason: Option<String>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefundResult {
    pub provider_refund_id: String,
    pub provider_transaction_id: String,
    pub amount: Money,
    pub provider_metadata: Value,
}
