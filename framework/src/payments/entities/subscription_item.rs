use chrono::{DateTime, Utc};

#[suprnova::model(
    table = "payments_subscription_items",
    timestamps,
    relations = {
        subscription: BelongsTo<crate::payments::entities::subscription::Subscription>,
    },
)]
pub struct SubscriptionItem {
    pub id: i64,
    pub subscription_id: i64,
    pub provider_item_id: String,
    pub provider_price_id: String,
    pub quantity: i32,
    pub unit_amount_minor: Option<i64>,
    pub unit_currency: Option<String>,
    pub provider_metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub use subscription_item::{ActiveModel, Column, Entity};
pub use subscription_item::Model;
