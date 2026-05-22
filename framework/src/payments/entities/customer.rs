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

#[suprnova::model(
    table = "payments_customers",
    timestamps,
)]
pub struct Customer {
    pub id: i64,
    pub provider: String,
    pub provider_customer_id: String,
    pub user_id: String,
    pub email: String,
    pub provider_metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub use customer::{ActiveModel, Column, Entity};
pub use customer::Model;
