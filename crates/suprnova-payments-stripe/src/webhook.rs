//! Implementation of the `WebhookHandler` trait for `StripeProvider`.
//!
//! Verifies Stripe's `t=<ts>,v1=<hex_sig>` signature format using HMAC-SHA256
//! and parses the incoming event body into a `WebhookEvent`.

use crate::{StripeProvider, event_map::stripe_event_to_neutral};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use suprnova::payments::{
    CustomerSnapshot, NeutralEventKind, PayloadIds, PaymentError, PaymentResult, PaymentSnapshot,
    WebhookContext, WebhookEvent, WebhookHandler,
};

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl WebhookHandler for StripeProvider {
    /// Verify a Stripe webhook signature.
    ///
    /// Stripe sends a `Stripe-Signature` header with the format:
    /// `t=<unix_timestamp>,v1=<hex_hmac_sha256>[,v1=<additional_sig>]`
    ///
    /// We recompute HMAC-SHA256 over `"<timestamp>.<body>"` using the webhook
    /// signing secret and do a constant-time comparison against every `v1=` value
    /// in the header (Stripe can rotate keys without instant cutover).
    ///
    /// The timestamp is also compared against the local clock and rejected
    /// when the absolute delta exceeds
    /// [`StripeProvider::with_signature_tolerance`] (default 300 seconds,
    /// matching Stripe's official libraries). Without this check a signature
    /// remains valid forever, so a captured signed body could be replayed.
    fn verify(&self, ctx: &WebhookContext<'_>) -> PaymentResult<()> {
        let header = ctx
            .headers
            .get("stripe-signature")
            .ok_or_else(|| {
                PaymentError::WebhookSignature("missing stripe-signature header".into())
            })?
            .to_str()
            .map_err(|_| {
                PaymentError::WebhookSignature("non-ascii stripe-signature header".into())
            })?;

        let mut timestamp: Option<&str> = None;
        let mut v1_sigs: Vec<&str> = Vec::new();

        for pair in header.split(',') {
            let mut it = pair.splitn(2, '=');
            match (it.next(), it.next()) {
                (Some("t"), Some(v)) => timestamp = Some(v),
                (Some("v1"), Some(v)) => v1_sigs.push(v),
                _ => {}
            }
        }

        let timestamp = timestamp.ok_or_else(|| {
            PaymentError::WebhookSignature("missing timestamp in stripe-signature header".into())
        })?;

        // Reject implausible timestamps before any HMAC work: stale events
        // can be discarded without spending CPU cycles, and a malformed `t=`
        // value is itself a signature failure rather than a silent bypass.
        let ts: i64 = timestamp.parse().map_err(|_| {
            PaymentError::WebhookSignature(format!(
                "non-numeric timestamp in stripe-signature header: {timestamp}"
            ))
        })?;
        let tolerance = self.webhook_signature_tolerance_seconds();
        let now = chrono::Utc::now().timestamp();
        if (now - ts).abs() > tolerance {
            return Err(PaymentError::WebhookSignature(format!(
                "timestamp outside tolerance window of {tolerance}s (now={now}, sig_ts={ts})"
            )));
        }

        if v1_sigs.is_empty() {
            return Err(PaymentError::WebhookSignature(
                "no v1 signature in stripe-signature header".into(),
            ));
        }

        let mut mac = HmacSha256::new_from_slice(self.webhook_signing_secret().as_bytes())
            .map_err(|_| PaymentError::Internal("HMAC key error".into()))?;
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(ctx.body);
        let expected_bytes = mac.finalize().into_bytes();
        let expected_hex = hex::encode(expected_bytes);

        if v1_sigs
            .iter()
            .any(|s| constant_time_eq(s.as_bytes(), expected_hex.as_bytes()))
        {
            Ok(())
        } else {
            Err(PaymentError::WebhookSignature(
                "no matching v1 signature".into(),
            ))
        }
    }

    /// Parse a raw Stripe webhook body into a `WebhookEvent`.
    ///
    /// Extracts `id` and `type` from the JSON envelope and maps `type` to a
    /// `NeutralEventKind` via `stripe_event_to_neutral`. The full raw JSON
    /// is preserved in `raw_payload` for provider-specific handlers.
    fn parse_event(&self, body: &[u8]) -> PaymentResult<WebhookEvent> {
        let raw: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| PaymentError::Validation(format!("invalid stripe webhook body: {e}")))?;

        let provider_event_id = raw
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let provider_event_type = raw
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let neutral = stripe_event_to_neutral(&provider_event_type);

        Ok(WebhookEvent {
            provider: "stripe".into(),
            provider_event_id,
            provider_event_type,
            neutral,
            raw_payload: raw,
        })
    }

    /// Extract IDs from Stripe's `data.object.*` envelope.
    ///
    /// Stripe is consistent: every webhook puts the relevant entity at
    /// `data.object`, with `id` as its primary key and `customer` as the
    /// customer pointer where applicable. Invoice and PaymentIntent events
    /// also carry `subscription` when the charge is recurring.
    fn extract_payload_ids(&self, event: &WebhookEvent) -> PayloadIds {
        let obj = match event.raw_payload.pointer("/data/object") {
            Some(o) => o,
            None => return PayloadIds::default(),
        };

        let mut ids = PayloadIds::default();

        match event.neutral {
            Some(
                NeutralEventKind::SubscriptionCreated
                | NeutralEventKind::SubscriptionUpdated
                | NeutralEventKind::SubscriptionCanceled,
            ) => {
                ids.subscription_id = obj.get("id").and_then(|v| v.as_str()).map(String::from);
                ids.customer_id = obj
                    .get("customer")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
            Some(NeutralEventKind::CustomerCreated | NeutralEventKind::CustomerUpdated) => {
                ids.customer_id = obj.get("id").and_then(|v| v.as_str()).map(String::from);
            }
            Some(
                NeutralEventKind::PaymentSucceeded
                | NeutralEventKind::PaymentFailed
                | NeutralEventKind::PaymentRefunded
                | NeutralEventKind::PaymentDisputed,
            ) => {
                ids.transaction_id = obj.get("id").and_then(|v| v.as_str()).map(String::from);
                ids.customer_id = obj
                    .get("customer")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
            Some(NeutralEventKind::InvoicePaid | NeutralEventKind::InvoiceFailed) => {
                ids.transaction_id = obj.get("id").and_then(|v| v.as_str()).map(String::from);
                ids.customer_id = obj
                    .get("customer")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                ids.subscription_id = obj
                    .get("subscription")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
            None => {}
        }
        ids
    }

    /// Build a [`PaymentSnapshot`] from a Stripe payment / invoice payload.
    ///
    /// - `payment_intent.*` → uses `id`, `amount`, `currency`, `status`, `customer`
    /// - `charge.refunded` / `charge.dispute.created` → uses Charge fields
    /// - `invoice.*` → uses `id`, `amount_paid`, `tax`, `currency`, `customer`,
    ///   `subscription`, `status_transitions.paid_at`
    ///
    /// Returns `None` for subscription / customer events (those go through
    /// the `extract_payload_ids` + provider API path).
    fn extract_payment_snapshot(&self, event: &WebhookEvent) -> Option<PaymentSnapshot> {
        let obj = event.raw_payload.pointer("/data/object")?;
        let provider_transaction_id = obj.get("id")?.as_str()?.to_string();
        let provider_customer_id = obj
            .get("customer")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match event.neutral? {
            NeutralEventKind::PaymentSucceeded
            | NeutralEventKind::PaymentFailed
            | NeutralEventKind::PaymentRefunded
            | NeutralEventKind::PaymentDisputed => {
                // PaymentIntent or Charge — both expose amount + currency at the top level.
                let amount_total_minor = obj.get("amount").and_then(|v| v.as_i64()).unwrap_or(0);
                let currency = obj
                    .get("currency")
                    .and_then(|v| v.as_str())
                    .unwrap_or("usd")
                    .to_uppercase();
                let status = match event.neutral? {
                    NeutralEventKind::PaymentSucceeded => "succeeded",
                    NeutralEventKind::PaymentFailed => "failed",
                    NeutralEventKind::PaymentRefunded => "refunded",
                    NeutralEventKind::PaymentDisputed => "disputed",
                    _ => unreachable!(),
                }
                .to_string();
                let paid_at = if matches!(event.neutral, Some(NeutralEventKind::PaymentSucceeded)) {
                    obj.get("created")
                        .and_then(|v| v.as_i64())
                        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
                } else {
                    None
                };
                Some(PaymentSnapshot {
                    provider_transaction_id,
                    provider_customer_id,
                    provider_subscription_id: None,
                    amount_total_minor,
                    amount_tax_minor: 0,
                    currency,
                    status,
                    paid_at,
                    provider_metadata: obj.clone(),
                })
            }
            NeutralEventKind::InvoicePaid | NeutralEventKind::InvoiceFailed => {
                let amount_total_minor = obj
                    .get("amount_paid")
                    .and_then(|v| v.as_i64())
                    .or_else(|| obj.get("amount_due").and_then(|v| v.as_i64()))
                    .unwrap_or(0);
                let amount_tax_minor = obj.get("tax").and_then(|v| v.as_i64()).unwrap_or(0);
                let currency = obj
                    .get("currency")
                    .and_then(|v| v.as_str())
                    .unwrap_or("usd")
                    .to_uppercase();
                let status = if matches!(event.neutral, Some(NeutralEventKind::InvoicePaid)) {
                    "succeeded"
                } else {
                    "failed"
                }
                .to_string();
                let provider_subscription_id = obj
                    .get("subscription")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let paid_at = obj
                    .pointer("/status_transitions/paid_at")
                    .and_then(|v| v.as_i64())
                    .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0));
                Some(PaymentSnapshot {
                    provider_transaction_id,
                    provider_customer_id,
                    provider_subscription_id,
                    amount_total_minor,
                    amount_tax_minor,
                    currency,
                    status,
                    paid_at,
                    provider_metadata: obj.clone(),
                })
            }
            _ => None,
        }
    }

    /// Build a [`CustomerSnapshot`] from Stripe `customer.created` /
    /// `customer.updated` payloads. Stripe puts the full Customer object at
    /// `data.object` — we pull `id` + `email` and keep the rest in
    /// `provider_metadata` for downstream readers.
    fn extract_customer_snapshot(&self, event: &WebhookEvent) -> Option<CustomerSnapshot> {
        match event.neutral? {
            NeutralEventKind::CustomerCreated | NeutralEventKind::CustomerUpdated => {
                let obj = event.raw_payload.pointer("/data/object")?;
                let provider_customer_id = obj.get("id")?.as_str()?.to_string();
                let email = obj
                    .get("email")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                Some(CustomerSnapshot {
                    provider_customer_id,
                    email,
                    provider_metadata: obj.clone(),
                })
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Constant-time byte slice comparison to prevent timing attacks.
///
/// Returns `true` only when `a` and `b` have equal length and equal contents.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Install the rustls ring CryptoProvider exactly once — `StripeProvider::new`
    /// constructs a hyper-rustls client which panics at TLS init when both
    /// `aws-lc-rs` and `ring` are in the dep graph (as they are via async-stripe).
    fn install_crypto_provider() {
        static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    fn provider() -> StripeProvider {
        install_crypto_provider();
        StripeProvider::new("sk_test_dummy", "pk_test_dummy", "whsec_dummy")
    }

    fn event(neutral: NeutralEventKind, payload: serde_json::Value) -> WebhookEvent {
        WebhookEvent {
            provider: "stripe".into(),
            provider_event_id: "evt_test".into(),
            provider_event_type: format!("{neutral:?}"),
            neutral: Some(neutral),
            raw_payload: payload,
        }
    }

    #[test]
    fn extract_payload_ids_subscription_created() {
        let p = provider();
        let e = event(
            NeutralEventKind::SubscriptionCreated,
            serde_json::json!({
                "data": { "object": { "id": "sub_abc", "customer": "cus_xyz" } }
            }),
        );
        let ids = p.extract_payload_ids(&e);
        assert_eq!(ids.subscription_id.as_deref(), Some("sub_abc"));
        assert_eq!(ids.customer_id.as_deref(), Some("cus_xyz"));
        assert!(ids.transaction_id.is_none());
    }

    #[test]
    fn extract_payload_ids_invoice_paid_carries_subscription() {
        let p = provider();
        let e = event(
            NeutralEventKind::InvoicePaid,
            serde_json::json!({
                "data": { "object": {
                    "id": "in_99",
                    "customer": "cus_77",
                    "subscription": "sub_44"
                }}
            }),
        );
        let ids = p.extract_payload_ids(&e);
        assert_eq!(ids.transaction_id.as_deref(), Some("in_99"));
        assert_eq!(ids.customer_id.as_deref(), Some("cus_77"));
        assert_eq!(ids.subscription_id.as_deref(), Some("sub_44"));
    }

    #[test]
    fn extract_payload_ids_returns_empty_when_data_object_missing() {
        let p = provider();
        let e = event(
            NeutralEventKind::PaymentSucceeded,
            serde_json::json!({ "unexpected": "shape" }),
        );
        let ids = p.extract_payload_ids(&e);
        assert!(ids.subscription_id.is_none());
        assert!(ids.customer_id.is_none());
        assert!(ids.transaction_id.is_none());
    }

    #[test]
    fn extract_payment_snapshot_payment_succeeded() {
        let p = provider();
        let e = event(
            NeutralEventKind::PaymentSucceeded,
            serde_json::json!({
                "data": { "object": {
                    "id": "pi_test",
                    "customer": "cus_1",
                    "amount": 4242,
                    "currency": "usd",
                    "created": 1717000000
                }}
            }),
        );
        let snap = p.extract_payment_snapshot(&e).expect("snapshot present");
        assert_eq!(snap.provider_transaction_id, "pi_test");
        assert_eq!(snap.provider_customer_id, "cus_1");
        assert_eq!(snap.amount_total_minor, 4242);
        assert_eq!(snap.currency, "USD", "currency must be uppercased");
        assert_eq!(snap.status, "succeeded");
        assert!(
            snap.paid_at.is_some(),
            "PaymentSucceeded must parse `created` as paid_at"
        );
    }

    #[test]
    fn extract_payment_snapshot_invoice_paid_uses_amount_paid_and_tax() {
        let p = provider();
        let e = event(
            NeutralEventKind::InvoicePaid,
            serde_json::json!({
                "data": { "object": {
                    "id": "in_x",
                    "customer": "cus_x",
                    "subscription": "sub_x",
                    "amount_paid": 12345,
                    "amount_due": 99999,
                    "tax": 234,
                    "currency": "EUR",
                    "status_transitions": { "paid_at": 1717000000 }
                }}
            }),
        );
        let snap = p.extract_payment_snapshot(&e).expect("snapshot present");
        assert_eq!(
            snap.amount_total_minor, 12345,
            "amount_paid takes precedence"
        );
        assert_eq!(snap.amount_tax_minor, 234);
        assert_eq!(snap.currency, "EUR");
        assert_eq!(snap.provider_subscription_id.as_deref(), Some("sub_x"));
        assert!(snap.paid_at.is_some());
    }

    #[test]
    fn extract_payment_snapshot_falls_back_to_amount_due_when_amount_paid_absent() {
        let p = provider();
        let e = event(
            NeutralEventKind::InvoiceFailed,
            serde_json::json!({
                "data": { "object": {
                    "id": "in_fail",
                    "customer": "cus_y",
                    "amount_due": 5500,
                    "currency": "GBP"
                }}
            }),
        );
        let snap = p.extract_payment_snapshot(&e).expect("snapshot present");
        assert_eq!(snap.amount_total_minor, 5500);
        assert_eq!(snap.status, "failed");
    }

    #[test]
    fn extract_payment_snapshot_returns_none_for_subscription_event() {
        let p = provider();
        let e = event(
            NeutralEventKind::SubscriptionUpdated,
            serde_json::json!({
                "data": { "object": { "id": "sub_x" } }
            }),
        );
        assert!(p.extract_payment_snapshot(&e).is_none());
    }

    #[test]
    fn extract_customer_snapshot_pulls_email_from_data_object() {
        let p = provider();
        let e = event(
            NeutralEventKind::CustomerUpdated,
            serde_json::json!({
                "data": { "object": {
                    "id": "cus_email_test",
                    "email": "new@example.com",
                    "name": "Test User"
                }}
            }),
        );
        let snap = p.extract_customer_snapshot(&e).expect("snapshot present");
        assert_eq!(snap.provider_customer_id, "cus_email_test");
        assert_eq!(snap.email.as_deref(), Some("new@example.com"));
        assert_eq!(snap.provider_metadata["name"], "Test User");
    }

    #[test]
    fn extract_customer_snapshot_returns_none_for_non_customer_events() {
        let p = provider();
        let e = event(
            NeutralEventKind::PaymentSucceeded,
            serde_json::json!({"data": {"object": {"id": "pi_x", "email": "x@x.com"}}}),
        );
        assert!(p.extract_customer_snapshot(&e).is_none());
    }
}
