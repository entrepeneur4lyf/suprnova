use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider-agnostic event taxonomy. Most domain handlers match `neutral`; provider-specific
/// edge cases fall through to `provider_event_type` + `raw_payload`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NeutralEventKind {
    PaymentSucceeded,
    PaymentFailed,
    PaymentRefunded,
    PaymentDisputed,
    SubscriptionCreated,
    SubscriptionUpdated,
    SubscriptionCanceled,
    InvoicePaid,
    InvoiceFailed,
    CustomerCreated,
    CustomerUpdated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEvent {
    pub provider: String,
    pub provider_event_id: String,
    pub provider_event_type: String,
    pub neutral: Option<NeutralEventKind>,
    pub raw_payload: Value,
}

#[derive(Debug, Clone)]
pub struct WebhookContext<'a> {
    pub body: &'a [u8],
    pub headers: &'a http::HeaderMap,
    pub remote_addr: Option<std::net::IpAddr>,
}
