//! SeaORM mirror entity for `payments_subscriptions`.
//!
//! Tracks the lifecycle state of a provider-side subscription. Updated by
//! the webhook ingress when the provider notifies the framework of a
//! status change. The `items` relation exposes line-level breakdown
//! (`SubscriptionItem`) for multi-price subscriptions.

use chrono::{DateTime, Utc};

/// Mirror row for a provider-side subscription. `status` mirrors the
/// provider's lifecycle term (`active`, `trialing`, `past_due`, etc.).
/// `cancel_at_period_end` reflects a scheduled cancellation; `canceled_at`
/// is set once the cancellation actually takes effect.
#[suprnova::model(
    table = "payments_subscriptions",
    timestamps,
    relations = {
        items: HasMany<crate::payments::entities::subscription_item::SubscriptionItem>,
    },
)]
pub struct Subscription {
    /// Surrogate primary key.
    pub id: i64,
    /// Provider name (kebab-case — `"stripe"`, `"paddle"`, etc.).
    pub provider: String,
    /// Provider-issued subscription identifier (e.g. Stripe's `sub_…`).
    pub provider_subscription_id: String,
    /// FK reference back to the owning provider customer record.
    pub provider_customer_id: String,
    /// Lifecycle status string mirroring the provider's terminology —
    /// `"active"`, `"trialing"`, `"past_due"`, `"canceled"`, etc.
    pub status: String,
    /// Start of the currently-billed period.
    pub current_period_start: DateTime<Utc>,
    /// End of the currently-billed period — invoice generation lands
    /// at or shortly after this instant.
    pub current_period_end: DateTime<Utc>,
    /// `true` when the subscription is scheduled to terminate at
    /// `current_period_end` instead of renewing.
    pub cancel_at_period_end: bool,
    /// Wall-clock time the subscription actually canceled. `None` while
    /// the subscription is still in any active state.
    pub canceled_at: Option<DateTime<Utc>>,
    /// Provider's raw subscription payload, preserved verbatim.
    pub provider_metadata: serde_json::Value,
    /// Row insert timestamp.
    pub created_at: DateTime<Utc>,
    /// Last row update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// SeaORM `Model` re-exported from the inner macro-generated module.
pub use subscription::Model;
/// SeaORM `ActiveModel`, `Column`, and `Entity` from the inner module.
pub use subscription::{ActiveModel, Column, Entity};
