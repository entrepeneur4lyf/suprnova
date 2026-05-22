use crate::payments::Money;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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
    Redirect {
        url: String,
        provider_session_id: String,
    },
}
