use chrono::{DateTime, Utc};

#[suprnova::model(table = "payments_transactions", timestamps)]
pub struct Transaction {
    pub id: i64,
    pub provider: String,
    pub provider_transaction_id: String,
    pub provider_customer_id: String,
    pub provider_subscription_id: Option<String>,
    pub amount_total_minor: i64,
    pub amount_tax_minor: i64,
    pub currency: String,
    pub status: String,
    pub provider_metadata: serde_json::Value,
    pub paid_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub use transaction::Model;
pub use transaction::{ActiveModel, Column, Entity};
