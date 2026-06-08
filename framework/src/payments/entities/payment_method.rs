//! SeaORM mirror entity for `payments_payment_methods`.
//!
//! Stores tokenized payment instruments (cards, bank transfers, mobile-money
//! accounts, etc.) attached to a customer. The provider holds the sensitive
//! data; this mirror keeps only the display-safe envelope plus the provider's
//! reference for future operations.

use chrono::{DateTime, Utc};

/// Mirror row for a stored provider-side payment method. `method_type`
/// classifies the instrument (`"card"`, `"bank_transfer"`, `"mobile_money"`,
/// etc.); `method_details` carries the display-safe envelope (brand,
/// last4, expiry — never PAN or CVV). `is_default` flags the customer's
/// primary instrument.
#[suprnova::model(table = "payments_payment_methods", timestamps)]
pub struct PaymentMethod {
    /// Surrogate primary key.
    pub id: i64,
    /// Provider name (kebab-case — `"stripe"`, `"paddle"`, etc.).
    pub provider: String,
    /// Provider-issued payment-method identifier (e.g. Stripe's `pm_…`).
    pub provider_payment_method_id: String,
    /// FK reference back to the owning provider customer record.
    pub provider_customer_id: String,
    /// Method classification — `"card"`, `"bank_transfer"`, `"mobile_money"`,
    /// `"stablecoin"`, `"crypto"`, etc. Matches the [`super::super::dto::PaymentMethod`]
    /// enum variant's wire tag.
    pub method_type: String,
    /// Display-safe envelope (brand, last4, expiry, etc.). Never holds
    /// the full PAN or CVV — those live exclusively on the provider.
    pub method_details: serde_json::Value,
    /// Whether this is the customer's default payment instrument.
    pub is_default: bool,
    /// Provider's raw payment-method payload, preserved verbatim.
    pub provider_metadata: serde_json::Value,
    /// Row insert timestamp.
    pub created_at: DateTime<Utc>,
    /// Last row update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// SeaORM `Model` re-exported from the inner macro-generated module.
pub use payment_method::Model;
/// SeaORM `ActiveModel`, `Column`, and `Entity` from the inner module.
pub use payment_method::{ActiveModel, Column, Entity};
