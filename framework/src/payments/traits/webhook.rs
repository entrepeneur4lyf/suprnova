//! Webhook ingress trait plus the snapshot types the framework extracts
//! from provider payloads to keep mirror tables fresh.

use crate::payments::{PaymentResult, WebhookContext, WebhookEvent};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use subtle::ConstantTimeEq;

/// Constant-time byte-slice comparison for webhook signature verification.
///
/// Provider adapters that authenticate webhooks with an HMAC (Stripe's
/// `Stripe-Signature`, a shared-secret digest, etc.) MUST compare the
/// computed digest against the header-supplied one with this helper
/// rather than `==`. A naive `==` over `&[u8]` short-circuits on the
/// first differing byte, leaking the position of the mismatch as a
/// timing side channel an attacker can use to forge a valid signature
/// byte by byte.
///
/// Backed by [`subtle::ConstantTimeEq`] — the same reviewed primitive
/// the CSRF and crypto layers use — so the comparison runs in time
/// independent of where the inputs diverge. A length mismatch returns
/// `false` immediately; for fixed-length digests (HMAC-SHA256 is always
/// 32 bytes) this is a structural reject, not a timing oracle for a
/// same-length attacker.
///
/// ```rust,no_run
/// use suprnova::payments::constant_time_eq;
///
/// fn verify_sig(computed: &[u8], header: &[u8]) -> bool {
///     constant_time_eq(computed, header)
/// }
/// ```
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// Webhook handling surface implemented by every payment provider.
///
/// The framework's webhook route ([`super::super::webhook_route::webhook_routes`])
/// calls [`Self::verify`] before anything else — a verification failure
/// aborts the request before the payload is touched. Once verified the
/// route calls [`Self::parse_event`] and dispatches downstream.
#[async_trait]
pub trait WebhookHandler: Send + Sync {
    /// Verify the inbound webhook against the provider's signing scheme.
    /// Must reject (return [`super::super::PaymentError::WebhookSignature`])
    /// on any tampering, replay outside the allowed window, or missing
    /// signature header.
    ///
    /// Implementations that compare an HMAC / digest against a
    /// header-supplied signature MUST use [`constant_time_eq`] (or another
    /// constant-time primitive) for that comparison — a byte-wise `==`
    /// leaks the mismatch position as a timing side channel and lets an
    /// attacker forge a signature byte by byte.
    fn verify(&self, ctx: &WebhookContext<'_>) -> PaymentResult<()>;
    /// Parse the raw body into a [`WebhookEvent`] with the provider's
    /// event identifier, type string, and (when recognised) a neutral
    /// classification.
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

    /// Build a [`CustomerSnapshot`] from a customer-type webhook payload.
    /// Providers override to surface the fields they expose (typically
    /// `email` plus a metadata blob).
    ///
    /// Returning `None` causes `update_customer_mirror` to skip the
    /// email/metadata refresh — it still bumps `updated_at` on the existing
    /// row so the operator can see the event was received.
    fn extract_customer_snapshot(&self, _event: &WebhookEvent) -> Option<CustomerSnapshot> {
        None
    }
}

/// IDs extracted from a webhook payload that identify which mirror rows to
/// upsert. Providers populate the fields they can find in their payload
/// shape; absent fields stay `None`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PayloadIds {
    /// Provider subscription identifier, when the payload carries one.
    pub subscription_id: Option<String>,
    /// Provider customer identifier, when the payload carries one.
    pub customer_id: Option<String>,
    /// Provider transaction / payment identifier, when the payload
    /// carries one.
    pub transaction_id: Option<String>,
}

/// Fully extracted transaction snapshot, ready to be upserted into
/// `payments_transactions`. Built by `WebhookHandler::extract_payment_snapshot`
/// from provider payload shapes (Stripe PaymentIntent / Invoice / Charge,
/// Paddle Transaction, etc).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaymentSnapshot {
    /// Provider transaction / payment identifier — natural key on
    /// `payments_transactions`.
    pub provider_transaction_id: String,
    /// Provider customer identifier — FK back to `payments_customers`.
    pub provider_customer_id: String,
    /// Provider subscription identifier when this transaction is a
    /// subscription invoice; `None` for one-off charges.
    pub provider_subscription_id: Option<String>,
    /// Total amount in the smallest currency unit (cents, satang, etc.).
    pub amount_total_minor: i64,
    /// Tax component in the smallest currency unit. `0` when the
    /// provider reports no tax breakdown.
    pub amount_tax_minor: i64,
    /// ISO-4217 currency code paired with the `_minor` amounts.
    pub currency: String,
    /// Provider-reported status string for the transaction (e.g.
    /// `"succeeded"`, `"refunded"`, `"failed"`).
    pub status: String,
    /// Wall-clock time the payment settled. `None` for pending or
    /// failed transactions.
    pub paid_at: Option<DateTime<Utc>>,
    /// Provider's raw transaction payload, preserved verbatim.
    pub provider_metadata: Value,
}

/// Customer fields extracted from a customer-event webhook payload. Built
/// by `WebhookHandler::extract_customer_snapshot`. The framework uses this
/// to refresh the `payments_customers` mirror row's `email` and
/// `provider_metadata` columns — `user_id` and `provider_customer_id` are
/// load-bearing keys that the webhook path never modifies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustomerSnapshot {
    /// Provider customer identifier — natural key on
    /// `payments_customers`. The webhook path never writes this column;
    /// it is supplied here so the mirror lookup can locate the row.
    pub provider_customer_id: String,
    /// New billing email reported by the provider, when present. `None`
    /// causes the mirror update to leave the existing column unchanged.
    pub email: Option<String>,
    /// Refreshed provider customer payload, preserved verbatim.
    pub provider_metadata: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_identical_slices() {
        assert!(constant_time_eq(b"abc123", b"abc123"));
        assert!(constant_time_eq(b"", b""));
        // 32-byte HMAC-SHA256 digest shape.
        let digest = [0x5au8; 32];
        assert!(constant_time_eq(&digest, &digest));
    }

    #[test]
    fn constant_time_eq_rejects_differing_slices() {
        assert!(!constant_time_eq(b"abc123", b"abc124"));
        assert!(!constant_time_eq(b"abc123", b"xbc123"));
    }

    #[test]
    fn constant_time_eq_rejects_length_mismatch() {
        assert!(!constant_time_eq(b"abc123", b"abc12"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(!constant_time_eq(b"a", b""));
    }
}
