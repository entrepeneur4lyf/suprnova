//! Implementation of the `WebhookHandler` trait for `PaddleProvider`.
//!
//! Uses `Paddle::unmarshal` for signature verification — it handles the
//! `Paddle-Signature` header format (`ts=…,h1=…`) and HMAC validation with
//! timestamp-skew tolerance. No manual HMAC code needed.

use async_trait::async_trait;
use paddle_rust_sdk::{webhooks::MaximumVariance, Paddle};
use suprnova::payments::{
    NeutralEventKind, PaymentError, PaymentResult, PaymentSnapshot, PayloadIds, WebhookContext,
    WebhookEvent, WebhookHandler,
};

use crate::{event_map::paddle_event_to_neutral, PaddleProvider};

#[async_trait]
impl WebhookHandler for PaddleProvider {
    fn verify(&self, ctx: &WebhookContext<'_>) -> PaymentResult<()> {
        let signature = ctx
            .headers
            .get("paddle-signature")
            .ok_or_else(|| {
                PaymentError::WebhookSignature("missing paddle-signature header".into())
            })?
            .to_str()
            .map_err(|_| PaymentError::WebhookSignature("non-ascii signature header".into()))?;

        let body_str = std::str::from_utf8(ctx.body).map_err(|_| {
            PaymentError::WebhookSignature("non-utf8 webhook body".into())
        })?;

        Paddle::unmarshal(
            body_str,
            self.webhook_key(),
            signature,
            MaximumVariance::default(),
        )
        .map_err(|e| PaymentError::WebhookSignature(format!("paddle signature verify: {e}")))?;

        Ok(())
    }

    fn parse_event(&self, body: &[u8]) -> PaymentResult<WebhookEvent> {
        let raw: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| PaymentError::Validation(format!("invalid paddle webhook body: {e}")))?;

        let provider_event_id = raw
            .get("event_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let provider_event_type = raw
            .get("event_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let neutral: Option<NeutralEventKind> = paddle_event_to_neutral(&provider_event_type);

        Ok(WebhookEvent {
            provider: "paddle".into(),
            provider_event_id,
            provider_event_type,
            neutral,
            raw_payload: raw,
        })
    }

    /// Extract IDs from Paddle's `data.*` envelope.
    ///
    /// Paddle puts the entity directly under `data`, with `id` as its primary
    /// key and `customer_id` as the customer pointer. Transaction events also
    /// carry `subscription_id` when they belong to a subscription billing
    /// cycle.
    fn extract_payload_ids(&self, event: &WebhookEvent) -> PayloadIds {
        let data = match event.raw_payload.get("data") {
            Some(d) => d,
            None => return PayloadIds::default(),
        };

        let mut ids = PayloadIds::default();

        match event.neutral {
            Some(
                NeutralEventKind::SubscriptionCreated
                | NeutralEventKind::SubscriptionUpdated
                | NeutralEventKind::SubscriptionCanceled,
            ) => {
                ids.subscription_id = data.get("id").and_then(|v| v.as_str()).map(String::from);
                ids.customer_id = data
                    .get("customer_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
            Some(NeutralEventKind::CustomerCreated | NeutralEventKind::CustomerUpdated) => {
                ids.customer_id = data.get("id").and_then(|v| v.as_str()).map(String::from);
            }
            Some(
                NeutralEventKind::PaymentSucceeded
                | NeutralEventKind::PaymentFailed
                | NeutralEventKind::PaymentRefunded
                | NeutralEventKind::PaymentDisputed
                | NeutralEventKind::InvoicePaid
                | NeutralEventKind::InvoiceFailed,
            ) => {
                ids.transaction_id = data.get("id").and_then(|v| v.as_str()).map(String::from);
                ids.customer_id = data
                    .get("customer_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                ids.subscription_id = data
                    .get("subscription_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
            None => {}
        }
        ids
    }

    /// Build a [`PaymentSnapshot`] from a Paddle Transaction payload.
    ///
    /// Paddle transactions expose totals under `details.totals.{total,tax}`
    /// as strings (Paddle returns amounts as decimal strings — we parse to
    /// minor units). Currency is `currency_code`. The transaction's
    /// `billed_at` field is RFC3339, parsed best-effort.
    fn extract_payment_snapshot(&self, event: &WebhookEvent) -> Option<PaymentSnapshot> {
        let data = event.raw_payload.get("data")?;
        let provider_transaction_id = data.get("id")?.as_str()?.to_string();
        let provider_customer_id = data
            .get("customer_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let provider_subscription_id = data
            .get("subscription_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        match event.neutral? {
            NeutralEventKind::PaymentSucceeded
            | NeutralEventKind::PaymentFailed
            | NeutralEventKind::PaymentRefunded
            | NeutralEventKind::PaymentDisputed
            | NeutralEventKind::InvoicePaid
            | NeutralEventKind::InvoiceFailed => {
                let amount_total_minor = data
                    .pointer("/details/totals/total")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok())
                    .or_else(|| {
                        data.pointer("/details/totals/total")
                            .and_then(|v| v.as_i64())
                    })
                    .unwrap_or(0);
                let amount_tax_minor = data
                    .pointer("/details/totals/tax")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok())
                    .or_else(|| {
                        data.pointer("/details/totals/tax").and_then(|v| v.as_i64())
                    })
                    .unwrap_or(0);
                let currency = data
                    .get("currency_code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("USD")
                    .to_uppercase();
                let status = match event.neutral? {
                    NeutralEventKind::PaymentSucceeded | NeutralEventKind::InvoicePaid => {
                        "succeeded"
                    }
                    NeutralEventKind::PaymentFailed | NeutralEventKind::InvoiceFailed => "failed",
                    NeutralEventKind::PaymentRefunded => "refunded",
                    NeutralEventKind::PaymentDisputed => "disputed",
                    _ => unreachable!(),
                }
                .to_string();
                let paid_at = data
                    .get("billed_at")
                    .and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc));
                Some(PaymentSnapshot {
                    provider_transaction_id,
                    provider_customer_id,
                    provider_subscription_id,
                    amount_total_minor,
                    amount_tax_minor,
                    currency,
                    status,
                    paid_at,
                    provider_metadata: data.clone(),
                })
            }
            _ => None,
        }
    }
}
