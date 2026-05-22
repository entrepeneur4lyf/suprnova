use crate::payments::Money;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Pending,
    Succeeded,
    Failed,
    Refunded,
    PartiallyRefunded,
    Disputed,
    Canceled,
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
