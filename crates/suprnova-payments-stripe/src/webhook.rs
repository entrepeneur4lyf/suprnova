//! Implementation of the `WebhookHandler` trait for `StripeProvider`.
//!
//! Verifies Stripe's `t=<ts>,v1=<hex_sig>` signature format using HMAC-SHA256
//! and parses the incoming event body into a `WebhookEvent`.

use crate::{event_map::stripe_event_to_neutral, StripeProvider};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use suprnova::payments::{PaymentError, PaymentResult, WebhookContext, WebhookEvent, WebhookHandler};

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

        let timestamp = timestamp
            .ok_or_else(|| PaymentError::WebhookSignature("missing timestamp in stripe-signature header".into()))?;

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
        let raw: serde_json::Value = serde_json::from_slice(body).map_err(|e| {
            PaymentError::Validation(format!("invalid stripe webhook body: {e}"))
        })?;

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
