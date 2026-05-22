use crate::payments::Money;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    OneOff,
    Subscription,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartSessionRequest {
    pub mode: SessionMode,
    pub customer_ref: String,
    pub price_refs: Vec<String>,
    pub success_return_url: String,
    pub cancel_return_url: String,
    pub amount_hint: Option<Money>,
    pub idempotency_key: Option<String>,
    pub metadata: Option<Value>,
}

/// Flow-tagged Inertia payload — frontend SDK dispatches on `flow` to render the right widget.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "flow", rename_all = "snake_case")]
pub enum SessionPayload {
    StripeElements {
        client_secret: String,
        publishable_key: String,
        provider_session_id: String,
    },
    StripeCheckoutRedirect {
        url: String,
        provider_session_id: String,
    },
    PaddleInline {
        transaction_id: String,
        customer_token: Option<String>,
        client_token: String,
    },
    /// Mobile Money flow — no redirect or embed. Frontend displays a
    /// user-facing message instructing the customer to confirm the payment
    /// on their phone (USSD prompt or operator app notification), then polls
    /// the provider via `provider_transaction_id` for status updates.
    MobileMoneyPrompt {
        provider_transaction_id: String,
        /// Display message — provider-localized when possible
        /// (e.g. "Check your phone for the MTN MoMo prompt").
        message: String,
        operator: super::MobileMoneyOperator,
    },
    Redirect {
        url: String,
        provider_session_id: String,
    },
}
