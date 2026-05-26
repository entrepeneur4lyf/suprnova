use chrono::{DateTime, Utc};

#[suprnova::model(
    table = "payments_subscriptions",
    timestamps,
    relations = {
        items: HasMany<crate::payments::entities::subscription_item::SubscriptionItem>,
    },
)]
pub struct Subscription {
    pub id: i64,
    pub provider: String,
    pub provider_subscription_id: String,
    pub provider_customer_id: String,
    pub status: String,
    pub current_period_start: DateTime<Utc>,
    pub current_period_end: DateTime<Utc>,
    pub cancel_at_period_end: bool,
    pub canceled_at: Option<DateTime<Utc>>,
    pub provider_metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub use subscription::Model;
pub use subscription::{ActiveModel, Column, Entity};
