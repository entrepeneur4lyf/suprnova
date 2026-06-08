//! SeaORM mirror entity for `payments_customers`.
//!
//! Stores the provider-side customer record created when a user first
//! enters the billing flow. `user_id` is a string so it carries any
//! opaque user-id format (UUID, ULID, numeric).
//!
//! The `provider_metadata` JSON binary column preserves the full
//! provider response (e.g. Stripe's `Customer` object) without schema
//! coupling — providers add fields without requiring a migration.

use chrono::{DateTime, Utc};

/// Mirror row for a provider-side customer record.
#[suprnova::model(table = "payments_customers", timestamps)]
pub struct Customer {
    /// Surrogate primary key.
    pub id: i64,
    /// Provider name (kebab-case — `"stripe"`, `"paddle"`, etc.) matching
    /// the value [`super::super::traits::PaymentProvider::name`] returns.
    pub provider: String,
    /// Provider-issued customer identifier (e.g. Stripe's `cus_…`).
    pub provider_customer_id: String,
    /// App-side user identifier — kept opaque (`String`) so any format
    /// (UUID, ULID, numeric) round-trips unchanged.
    pub user_id: String,
    /// Customer's billing email.
    pub email: String,
    /// JSON snapshot of the provider's customer object — preserved
    /// verbatim so app code can read fields the mirror schema doesn't
    /// flatten.
    pub provider_metadata: serde_json::Value,
    /// Row insert timestamp.
    pub created_at: DateTime<Utc>,
    /// Last row update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// SeaORM `Model` re-exported from the inner macro-generated module.
pub use customer::Model;
/// SeaORM `ActiveModel`, `Column`, and `Entity` from the inner module.
pub use customer::{ActiveModel, Column, Entity};
