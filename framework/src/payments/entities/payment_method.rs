use chrono::{DateTime, Utc};

#[suprnova::model(
    table = "payments_payment_methods",
    timestamps,
)]
pub struct PaymentMethod {
    pub id: i64,
    pub provider: String,
    pub provider_payment_method_id: String,
    pub provider_customer_id: String,
    pub method_type: String,
    pub method_details: serde_json::Value,
    pub is_default: bool,
    pub provider_metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub use payment_method::{ActiveModel, Column, Entity};
pub use payment_method::Model;
