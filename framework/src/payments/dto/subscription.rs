//! Subscription DTOs — request, result, and item-snapshot shapes
//! exchanged with [`super::super::traits::Subscription`].

use crate::payments::Money;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider-neutral subscription lifecycle status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    /// In a free trial period; provider has not yet attempted billing.
    Trialing,
    /// Currently billed and in good standing.
    Active,
    /// Provider's most recent invoice failed and dunning is in progress.
    PastDue,
    /// Subscription has terminated.
    Canceled,
    /// Setup not yet complete (e.g. initial payment requires action).
    Incomplete,
    /// Temporarily paused — billing suspended pending resume.
    Paused,
}

/// Request payload for [`super::super::traits::Subscription::subscribe`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeRequest {
    /// Provider customer identifier the subscription is billed to.
    pub customer_ref: String,
    /// Provider price identifiers for the lines that make up this
    /// subscription.
    pub price_refs: Vec<String>,
    /// Optional trial duration in days; `None` for no trial.
    pub trial_days: Option<u32>,
    /// Idempotency key forwarded to the provider.
    pub idempotency_key: Option<String>,
    /// Free-form metadata to attach to the provider-side subscription.
    pub metadata: Option<Value>,
}

/// Request payload for [`super::super::traits::Subscription::update`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateSubscriptionRequest {
    /// Provider subscription identifier to mutate.
    pub provider_subscription_id: String,
    /// Replacement price identifiers, or `None` to keep current lines.
    pub new_price_refs: Option<Vec<String>>,
    /// Set to schedule (`Some(true)`) or rescind (`Some(false)`) a
    /// period-end cancellation; `None` leaves cancellation state alone.
    pub cancel_at_period_end: Option<bool>,
    /// Idempotency key forwarded to the provider.
    pub idempotency_key: Option<String>,
}

/// Result of any `Subscription::*` call — a fresh snapshot of the
/// provider-side subscription state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionResult {
    /// Provider subscription identifier (e.g. Stripe's `sub_…`).
    pub provider_subscription_id: String,
    /// Provider customer identifier this subscription is billed to.
    pub provider_customer_id: String,
    /// Provider-neutral lifecycle status.
    pub status: SubscriptionStatus,
    /// Per-line breakdown — see [`SubscriptionItemSnapshot`].
    pub items: Vec<SubscriptionItemSnapshot>,
    /// Start of the currently-billed period.
    pub current_period_start: DateTime<Utc>,
    /// End of the currently-billed period.
    pub current_period_end: DateTime<Utc>,
    /// `true` when the subscription is scheduled to terminate at the
    /// end of `current_period_end`.
    pub cancel_at_period_end: bool,
    /// Provider's raw subscription payload, preserved verbatim.
    pub provider_metadata: Value,
}

/// Single price line inside a [`SubscriptionResult`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionItemSnapshot {
    /// Provider line-item identifier (e.g. Stripe's `si_…`).
    pub provider_item_id: String,
    /// Provider price identifier the line bills against.
    pub provider_price_id: String,
    /// Billed quantity for this line.
    pub quantity: u32,
    /// Per-unit price at the time of the snapshot. `None` for
    /// usage-billed lines where the provider returns no fixed unit price.
    pub unit_amount: Option<Money>,
}
