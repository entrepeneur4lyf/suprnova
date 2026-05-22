use crate::payments::Money;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    Trialing,
    Active,
    PastDue,
    Canceled,
    Incomplete,
    Paused,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeRequest {
    pub customer_ref: String,
    pub price_refs: Vec<String>,
    pub trial_days: Option<u32>,
    pub idempotency_key: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSubscriptionRequest {
    pub provider_subscription_id: String,
    pub new_price_refs: Option<Vec<String>>,
    pub cancel_at_period_end: Option<bool>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionResult {
    pub provider_subscription_id: String,
    pub provider_customer_id: String,
    pub status: SubscriptionStatus,
    pub items: Vec<SubscriptionItemSnapshot>,
    pub current_period_start: DateTime<Utc>,
    pub current_period_end: DateTime<Utc>,
    pub cancel_at_period_end: bool,
    pub provider_metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionItemSnapshot {
    pub provider_item_id: String,
    pub provider_price_id: String,
    pub quantity: u32,
    pub unit_amount: Option<Money>,
}
