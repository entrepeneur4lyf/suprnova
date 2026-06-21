//! Implementation of the `WebhookHandler` trait for `PaddleProvider`.
//!
//! Uses `Paddle::unmarshal` for signature verification — it handles the
//! `Paddle-Signature` header format (`ts=…,h1=…`) and HMAC validation with
//! timestamp-skew tolerance. No manual HMAC code needed.

use async_trait::async_trait;
use paddle_rust_sdk::{Paddle, webhooks::MaximumVariance};
use suprnova::payments::{
    CustomerSnapshot, NeutralEventKind, PayloadIds, PaymentError, PaymentResult, PaymentSnapshot,
    WebhookContext, WebhookEvent, WebhookHandler,
};

use crate::{PaddleProvider, event_map::paddle_event_to_neutral};

/// Parse a Paddle minor-unit amount field, accepting either the decimal-string
/// form Paddle normally sends (`"1234"`) or a bare JSON number (`1234`).
/// Returns `0` when the field is absent or unparseable so a malformed amount
/// degrades to an auditable zero rather than dropping the mirror write.
fn parse_minor(value: Option<&serde_json::Value>) -> i64 {
    match value {
        Some(serde_json::Value::String(s)) => s.parse::<i64>().unwrap_or(0),
        Some(v) => v.as_i64().unwrap_or(0),
        None => 0,
    }
}

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

        let body_str = std::str::from_utf8(ctx.body)
            .map_err(|_| PaymentError::WebhookSignature("non-utf8 webhook body".into()))?;

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
    ///
    /// Adjustment events (`adjustment.created` / `adjustment.updated`, mapped
    /// to [`NeutralEventKind::PaymentRefunded`]) are NOT transactions: their
    /// `id` is the adjustment id (`adj_…`) and the transaction they adjust is
    /// in a separate `transaction_id` field (`txn_…`). The mirror must be
    /// keyed off `transaction_id` so a refund updates the original transaction
    /// row rather than inserting a phantom row keyed by the adjustment id.
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
            // Adjustment payload: key off the referenced transaction, not the
            // adjustment's own `id`.
            Some(NeutralEventKind::PaymentRefunded | NeutralEventKind::PaymentDisputed) => {
                ids.transaction_id = data
                    .get("transaction_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                ids.customer_id = data
                    .get("customer_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                ids.subscription_id = data
                    .get("subscription_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
            // Transaction payload: `id` is the transaction id.
            Some(
                NeutralEventKind::PaymentSucceeded
                | NeutralEventKind::PaymentFailed
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

    /// Build a [`PaymentSnapshot`] from a Paddle payload.
    ///
    /// Paddle sends two structurally different shapes that both land on the
    /// `payments_transactions` mirror:
    ///
    /// - **Transaction** (`transaction.*` → succeeded / failed / invoice
    ///   paid). `data.id` is the transaction id (`txn_…`); totals live under
    ///   `data.details.totals.{total,tax}` as decimal-string minor units;
    ///   currency is `data.currency_code`; settle time is `data.billed_at`.
    /// - **Adjustment** (`adjustment.*` → refunded / chargeback). `data.id`
    ///   is the adjustment id (`adj_…`) — NOT a transaction — and the
    ///   transaction it adjusts is `data.transaction_id`. Totals live at
    ///   `data.totals.{total,tax}` (there is no `data.details`), currency is
    ///   `data.currency_code` at the top level. The mirror is keyed off
    ///   `transaction_id` so the refund/chargeback updates the original
    ///   transaction row instead of inserting a phantom `adj_…` row with a
    ///   zero amount.
    fn extract_payment_snapshot(&self, event: &WebhookEvent) -> Option<PaymentSnapshot> {
        let data = event.raw_payload.get("data")?;

        match event.neutral? {
            // Adjustment payload (refund / chargeback).
            kind @ (NeutralEventKind::PaymentRefunded | NeutralEventKind::PaymentDisputed) => {
                // Key off the referenced transaction, never the adjustment id.
                let provider_transaction_id = data.get("transaction_id")?.as_str()?.to_string();
                let provider_customer_id = data
                    .get("customer_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let provider_subscription_id = data
                    .get("subscription_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let amount_total_minor = parse_minor(data.pointer("/totals/total"));
                let amount_tax_minor = parse_minor(data.pointer("/totals/tax"));
                let currency = data
                    .get("currency_code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("USD")
                    .to_uppercase();
                let status = match kind {
                    NeutralEventKind::PaymentRefunded => "refunded",
                    NeutralEventKind::PaymentDisputed => "disputed",
                    _ => unreachable!(),
                }
                .to_string();
                Some(PaymentSnapshot {
                    provider_transaction_id,
                    provider_customer_id,
                    provider_subscription_id,
                    amount_total_minor,
                    amount_tax_minor,
                    currency,
                    status,
                    // An adjustment carries no settlement time; preserve the
                    // original transaction's `paid_at` (the upsert path leaves
                    // it untouched when the snapshot supplies `None`).
                    paid_at: None,
                    provider_metadata: data.clone(),
                })
            }
            // Transaction payload (charge / invoice).
            kind @ (NeutralEventKind::PaymentSucceeded
            | NeutralEventKind::PaymentFailed
            | NeutralEventKind::InvoicePaid
            | NeutralEventKind::InvoiceFailed) => {
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
                let amount_total_minor = parse_minor(data.pointer("/details/totals/total"));
                let amount_tax_minor = parse_minor(data.pointer("/details/totals/tax"));
                let currency = data
                    .get("currency_code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("USD")
                    .to_uppercase();
                let status = match kind {
                    NeutralEventKind::PaymentSucceeded | NeutralEventKind::InvoicePaid => {
                        "succeeded"
                    }
                    NeutralEventKind::PaymentFailed | NeutralEventKind::InvoiceFailed => "failed",
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

    /// Build a [`CustomerSnapshot`] from Paddle `customer.created` /
    /// `customer.updated` payloads. Paddle puts the Customer object directly
    /// under `data` (no `data.object` wrapper).
    fn extract_customer_snapshot(&self, event: &WebhookEvent) -> Option<CustomerSnapshot> {
        match event.neutral? {
            NeutralEventKind::CustomerCreated | NeutralEventKind::CustomerUpdated => {
                let data = event.raw_payload.get("data")?;
                let provider_customer_id = data.get("id")?.as_str()?.to_string();
                let email = data
                    .get("email")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                Some(CustomerSnapshot {
                    provider_customer_id,
                    email,
                    provider_metadata: data.clone(),
                })
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PaddleEnvironment, PaddleProvider};

    fn provider() -> PaddleProvider {
        // Dummy keys are fine — extractor tests don't hit the Paddle HTTP API.
        PaddleProvider::new(
            "pdl_test_apikey",
            "pdl_test_whkey",
            "test_clienttoken",
            PaddleEnvironment::Sandbox,
        )
        .expect("paddle provider construction")
    }

    fn event(neutral: NeutralEventKind, payload: serde_json::Value) -> WebhookEvent {
        WebhookEvent {
            provider: "paddle".into(),
            provider_event_id: "evt_test".into(),
            provider_event_type: format!("{neutral:?}"),
            neutral: Some(neutral),
            raw_payload: payload,
        }
    }

    #[test]
    fn extract_payload_ids_subscription_event() {
        let p = provider();
        let e = event(
            NeutralEventKind::SubscriptionCreated,
            serde_json::json!({ "data": { "id": "sub_pdl", "customer_id": "ctm_xyz" } }),
        );
        let ids = p.extract_payload_ids(&e);
        assert_eq!(ids.subscription_id.as_deref(), Some("sub_pdl"));
        assert_eq!(ids.customer_id.as_deref(), Some("ctm_xyz"));
        assert!(ids.transaction_id.is_none());
    }

    #[test]
    fn extract_payload_ids_transaction_event_includes_subscription_link() {
        let p = provider();
        let e = event(
            NeutralEventKind::PaymentSucceeded,
            serde_json::json!({ "data": {
                "id": "txn_done",
                "customer_id": "ctm_pay",
                "subscription_id": "sub_pdl"
            } }),
        );
        let ids = p.extract_payload_ids(&e);
        assert_eq!(ids.transaction_id.as_deref(), Some("txn_done"));
        assert_eq!(ids.customer_id.as_deref(), Some("ctm_pay"));
        assert_eq!(ids.subscription_id.as_deref(), Some("sub_pdl"));
    }

    #[test]
    fn extract_payment_snapshot_parses_string_totals() {
        let p = provider();
        let e = event(
            NeutralEventKind::PaymentSucceeded,
            serde_json::json!({ "data": {
                "id": "txn_str",
                "customer_id": "ctm_x",
                "currency_code": "eur",
                "details": { "totals": { "total": "1234", "tax": "100" } },
                "billed_at": "2026-05-22T12:00:00Z"
            } }),
        );
        let snap = p.extract_payment_snapshot(&e).expect("snapshot present");
        assert_eq!(snap.amount_total_minor, 1234, "string totals must parse");
        assert_eq!(snap.amount_tax_minor, 100);
        assert_eq!(snap.currency, "EUR");
        assert_eq!(snap.status, "succeeded");
        assert!(snap.paid_at.is_some(), "billed_at must parse to paid_at");
    }

    #[test]
    fn extract_payment_snapshot_handles_numeric_totals() {
        let p = provider();
        let e = event(
            NeutralEventKind::InvoicePaid,
            serde_json::json!({ "data": {
                "id": "txn_num",
                "customer_id": "ctm_n",
                "currency_code": "USD",
                "details": { "totals": { "total": 500, "tax": 50 } }
            } }),
        );
        let snap = p.extract_payment_snapshot(&e).expect("snapshot present");
        assert_eq!(snap.amount_total_minor, 500);
        assert_eq!(snap.amount_tax_minor, 50);
    }

    /// A realistic `adjustment.created` (refund) body: `data.id` is the
    /// adjustment id, the original transaction is in `data.transaction_id`,
    /// amounts live at `data.totals.*`, and currency is the top-level
    /// `data.currency_code`. The snapshot must key off `transaction_id` and
    /// carry the real amount — not insert a phantom `adj_…` row with amount 0.
    #[test]
    fn extract_payment_snapshot_adjustment_keys_off_transaction_id_with_real_amount() {
        let p = provider();
        let e = event(
            NeutralEventKind::PaymentRefunded,
            serde_json::json!({ "data": {
                "id": "adj_01h8xce4qhqc",
                "action": "refund",
                "transaction_id": "txn_01h8xc...original",
                "subscription_id": "sub_01h8x...",
                "customer_id": "ctm_01h8x...",
                "currency_code": "gbp",
                "reason": "Customer requested a refund",
                "status": "approved",
                "totals": {
                    "subtotal": "1000",
                    "tax": "200",
                    "total": "1200",
                    "fee": "60",
                    "earnings": "940",
                    "currency_code": "GBP"
                }
            } }),
        );
        let snap = p.extract_payment_snapshot(&e).expect("snapshot present");
        assert_eq!(
            snap.provider_transaction_id, "txn_01h8xc...original",
            "adjustment must key off the referenced transaction id, not adj_…"
        );
        assert_eq!(
            snap.amount_total_minor, 1200,
            "adjustment total must come from data.totals.total, not 0"
        );
        assert_ne!(snap.amount_total_minor, 0, "refund amount must not be 0");
        assert_eq!(snap.amount_tax_minor, 200);
        assert_eq!(snap.currency, "GBP");
        assert_eq!(snap.status, "refunded");
        assert_eq!(
            snap.provider_subscription_id.as_deref(),
            Some("sub_01h8x...")
        );
        assert_eq!(snap.provider_customer_id, "ctm_01h8x...");
        // No settlement time on an adjustment — preserve the original txn's.
        assert!(snap.paid_at.is_none());
    }

    /// `extract_payload_ids` for an adjustment must surface `transaction_id`
    /// as the transaction pointer — the framework keys the mirror upsert off
    /// `PayloadIds::transaction_id`, so reading `data.id` (the adjustment id)
    /// here would mis-route the refund.
    #[test]
    fn extract_payload_ids_adjustment_uses_transaction_id() {
        let p = provider();
        let e = event(
            NeutralEventKind::PaymentRefunded,
            serde_json::json!({ "data": {
                "id": "adj_refund_99",
                "transaction_id": "txn_being_refunded",
                "customer_id": "ctm_adj",
                "subscription_id": "sub_adj"
            } }),
        );
        let ids = p.extract_payload_ids(&e);
        assert_eq!(
            ids.transaction_id.as_deref(),
            Some("txn_being_refunded"),
            "must point at the referenced transaction, not the adjustment id"
        );
        assert_ne!(
            ids.transaction_id.as_deref(),
            Some("adj_refund_99"),
            "adjustment id must never become the transaction key"
        );
        assert_eq!(ids.customer_id.as_deref(), Some("ctm_adj"));
        assert_eq!(ids.subscription_id.as_deref(), Some("sub_adj"));
    }

    /// A transaction payload still reads `data.id` and `data.details.totals.*`
    /// — the adjustment branch must not regress the transaction path.
    #[test]
    fn extract_payload_ids_transaction_event_uses_data_id() {
        let p = provider();
        let e = event(
            NeutralEventKind::PaymentSucceeded,
            serde_json::json!({ "data": {
                "id": "txn_normal",
                "customer_id": "ctm_n",
                "subscription_id": "sub_n"
            } }),
        );
        let ids = p.extract_payload_ids(&e);
        assert_eq!(ids.transaction_id.as_deref(), Some("txn_normal"));
        assert_eq!(ids.customer_id.as_deref(), Some("ctm_n"));
        assert_eq!(ids.subscription_id.as_deref(), Some("sub_n"));
    }

    #[test]
    fn extract_customer_snapshot_reads_data_directly() {
        let p = provider();
        let e = event(
            NeutralEventKind::CustomerUpdated,
            serde_json::json!({ "data": {
                "id": "ctm_email",
                "email": "buyer@example.com",
                "name": "Buyer"
            } }),
        );
        let snap = p.extract_customer_snapshot(&e).expect("snapshot present");
        assert_eq!(snap.provider_customer_id, "ctm_email");
        assert_eq!(snap.email.as_deref(), Some("buyer@example.com"));
        assert_eq!(snap.provider_metadata["name"], "Buyer");
    }
}
