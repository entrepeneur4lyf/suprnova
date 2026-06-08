//! SeaORM mirror entity for `payments_subscription_items`.
//!
//! One row per price line inside a parent [`super::subscription::Subscription`].
//! Captures the per-line quantity and unit pricing snapshot so historical
//! invoices remain accurate even if a price object later changes upstream.

use chrono::{DateTime, Utc};

/// Mirror row for one price line within a subscription. `unit_amount_minor`
/// is the smallest currency unit at the time the item was synced; `None`
/// means the provider returned no per-unit price (e.g. usage-billed items).
#[suprnova::model(
    table = "payments_subscription_items",
    timestamps,
    relations = {
        subscription: BelongsTo<crate::payments::entities::subscription::Subscription>,
    },
)]
pub struct SubscriptionItem {
    /// Surrogate primary key.
    pub id: i64,
    /// FK into [`super::subscription::Subscription::id`].
    pub subscription_id: i64,
    /// Provider-issued line-item identifier (e.g. Stripe's `si_…`).
    pub provider_item_id: String,
    /// Provider-issued price-object identifier the line bills against
    /// (e.g. Stripe's `price_…`).
    pub provider_price_id: String,
    /// Billed quantity for this line.
    pub quantity: i32,
    /// Per-unit price in the smallest currency unit at the time of sync.
    /// `None` for usage-billed lines where the provider returns no
    /// fixed unit price.
    pub unit_amount_minor: Option<i64>,
    /// ISO-4217 currency code paired with `unit_amount_minor`.
    pub unit_currency: Option<String>,
    /// Provider's raw item payload, preserved verbatim.
    pub provider_metadata: serde_json::Value,
    /// Row insert timestamp.
    pub created_at: DateTime<Utc>,
    /// Last row update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// SeaORM `Model` re-exported from the inner macro-generated module.
pub use subscription_item::Model;
/// SeaORM `ActiveModel`, `Column`, and `Entity` from the inner module.
pub use subscription_item::{ActiveModel, Column, Entity};
