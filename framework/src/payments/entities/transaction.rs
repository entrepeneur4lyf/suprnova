//! SeaORM mirror entity for `payments_transactions`.
//!
//! One row per money movement — one-off charges, subscription invoices,
//! refunds — recorded after a provider webhook confirms the outcome.
//! `provider_transaction_id` + `provider` are the natural key; the
//! `provider_metadata` blob preserves the full provider payload so
//! downstream consumers can read fields the mirror schema doesn't
//! flatten.

use chrono::{DateTime, Utc};

/// Mirror row for a provider-side transaction (charge, invoice payment,
/// or refund). `amount_total_minor` / `amount_tax_minor` are the smallest
/// currency unit (cents, satang, etc.). `paid_at` stays `None` for
/// pending or failed entries.
#[suprnova::model(table = "payments_transactions", timestamps)]
pub struct Transaction {
    /// Surrogate primary key.
    pub id: i64,
    /// Provider name (kebab-case — `"stripe"`, `"paddle"`, etc.).
    pub provider: String,
    /// Provider-issued transaction / payment identifier (e.g. Stripe's
    /// `pi_…` or `ch_…`). Natural key with `provider`.
    pub provider_transaction_id: String,
    /// FK reference back to the owning provider customer record.
    pub provider_customer_id: String,
    /// Provider subscription identifier when this is a subscription
    /// invoice. `None` for one-off charges.
    pub provider_subscription_id: Option<String>,
    /// Total amount in the smallest currency unit (cents, satang, etc.).
    pub amount_total_minor: i64,
    /// Tax component in the smallest currency unit; `0` when the
    /// provider reports no tax breakdown.
    pub amount_tax_minor: i64,
    /// ISO-4217 currency code paired with the `_minor` columns.
    pub currency: String,
    /// Provider-reported status string (e.g. `"succeeded"`, `"refunded"`,
    /// `"failed"`).
    pub status: String,
    /// Provider's raw transaction payload, preserved verbatim.
    pub provider_metadata: serde_json::Value,
    /// Wall-clock time the payment settled. `None` for pending or
    /// failed entries.
    pub paid_at: Option<DateTime<Utc>>,
    /// Row insert timestamp.
    pub created_at: DateTime<Utc>,
    /// Last row update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// SeaORM `Model` re-exported from the inner macro-generated module.
pub use transaction::Model;
/// SeaORM `ActiveModel`, `Column`, and `Entity` from the inner module.
pub use transaction::{ActiveModel, Column, Entity};
