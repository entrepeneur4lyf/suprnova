use crate::payments::{PaymentResult, WebhookContext, WebhookEvent};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[async_trait]
pub trait WebhookHandler: Send + Sync {
    fn verify(&self, ctx: &WebhookContext<'_>) -> PaymentResult<()>;
    fn parse_event(&self, body: &[u8]) -> PaymentResult<WebhookEvent>;

    /// Extract well-known entity IDs from the webhook's `raw_payload` so the
    /// framework can hydrate mirror tables. Providers override per their
    /// payload shape (Stripe uses `data.object.*`, Paddle uses `data.*`).
    ///
    /// Default impl returns no IDs — the audit row is still recorded but no
    /// mirror rows are touched.
    fn extract_payload_ids(&self, _event: &WebhookEvent) -> PayloadIds {
        PayloadIds::default()
    }

    /// Build a [`PaymentSnapshot`] from the webhook payload for payment- /
    /// invoice-type events. Providers override per their payload shape.
    ///
    /// Returning `None` means the framework will skip the
    /// `payments_transactions` upsert for this event.
    fn extract_payment_snapshot(&self, _event: &WebhookEvent) -> Option<PaymentSnapshot> {
        None
    }
}

/// IDs extracted from a webhook payload that identify which mirror rows to
/// upsert. Providers populate the fields they can find in their payload
/// shape; absent fields stay `None`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PayloadIds {
    pub subscription_id: Option<String>,
    pub customer_id: Option<String>,
    pub transaction_id: Option<String>,
}

/// Fully extracted transaction snapshot, ready to be upserted into
/// `payments_transactions`. Built by `WebhookHandler::extract_payment_snapshot`
/// from provider payload shapes (Stripe PaymentIntent / Invoice / Charge,
/// Paddle Transaction, etc).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaymentSnapshot {
    pub provider_transaction_id: String,
    pub provider_customer_id: String,
    pub provider_subscription_id: Option<String>,
    pub amount_total_minor: i64,
    pub amount_tax_minor: i64,
    pub currency: String,
    pub status: String,
    pub paid_at: Option<DateTime<Utc>>,
    pub provider_metadata: Value,
}
