//! Integration and unit tests for the Stripe adapter.
//!
//! Env-gated tests (those that call out to the Stripe API) skip silently when
//! `STRIPE_SECRET_KEY` is not set.  Always-on tests exercise the webhook
//! signature verification path using locally-computed HMACs.

use suprnova::payments::*;
use suprnova_payments_stripe::StripeProvider;

// ---------------------------------------------------------------------------
// Crypto provider setup
// ---------------------------------------------------------------------------

/// Ensure the rustls ring CryptoProvider is installed exactly once per test
/// binary run.  `StripeProvider::new` constructs a `stripe::Client` which
/// initialises a hyper-rustls TLS stack; rustls panics on construction if no
/// provider has been installed when both `aws-lc-rs` and `ring` are present in
/// the dependency graph (as they are here via async-stripe's feature flags).
fn install_crypto_provider() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn provider() -> Option<StripeProvider> {
    install_crypto_provider();
    if std::env::var("STRIPE_SECRET_KEY").is_err() {
        return None;
    }
    StripeProvider::from_env().ok()
}

// ---------------------------------------------------------------------------
// Env-gated integration tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn integration_create_get_delete_customer() {
    let Some(provider) = provider() else {
        eprintln!("skipped: STRIPE_SECRET_KEY not set");
        return;
    };

    let cus = provider
        .create_customer(CreateCustomerRequest {
            user_id: "user_int_42".into(),
            email: format!("test+{}@example.com", uuid::Uuid::new_v4()),
            name: Some("Integration Test".into()),
            metadata: None,
        })
        .await
        .unwrap();
    let cus_id = cus.provider_customer_id.clone();

    let fetched = provider.get_customer(&cus_id).await.unwrap();
    assert_eq!(fetched.provider_customer_id, cus_id);

    // Cleanup — always run so test infrastructure stays tidy.
    provider.delete_customer(&cus_id).await.unwrap();
}

// ---------------------------------------------------------------------------
// Always-on webhook signature tests
// ---------------------------------------------------------------------------

#[test]
fn webhook_verify_rejects_bad_signature() {
    install_crypto_provider();
    let p = StripeProvider::new("sk_test", "pk_test", "whsec_secret");
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "stripe-signature",
        "t=1234567890,v1=deadbeef".parse().unwrap(),
    );
    let ctx = WebhookContext {
        body: b"{}",
        headers: &headers,
        remote_addr: None,
    };
    let err = p.verify(&ctx).unwrap_err();
    assert!(matches!(err, PaymentError::WebhookSignature(_)));
}

#[test]
fn webhook_verify_rejects_missing_header() {
    install_crypto_provider();
    let p = StripeProvider::new("sk_test", "pk_test", "whsec_secret");
    let headers = http::HeaderMap::new();
    let ctx = WebhookContext {
        body: b"{}",
        headers: &headers,
        remote_addr: None,
    };
    let err = p.verify(&ctx).unwrap_err();
    assert!(matches!(err, PaymentError::WebhookSignature(_)));
}

/// Compute the v1 Stripe signature hex for `<timestamp>.<body>` under `secret`.
fn compute_stripe_v1(secret: &str, timestamp: &str, body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

#[test]
fn webhook_verify_accepts_valid_signature() {
    install_crypto_provider();
    let secret = "whsec_test_secret";
    let body = br#"{"id":"evt_test","type":"payment_intent.succeeded"}"#;
    // Use the current wall clock so the verify path's timestamp-tolerance
    // check passes alongside the HMAC check — a fixed historical timestamp
    // would be rejected as outside the default 300-second window.
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let sig_hex = compute_stripe_v1(secret, &timestamp, body);

    let p = StripeProvider::new("sk_test", "pk_test", secret);
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "stripe-signature",
        format!("t={timestamp},v1={sig_hex}").parse().unwrap(),
    );
    let ctx = WebhookContext {
        body,
        headers: &headers,
        remote_addr: None,
    };
    p.verify(&ctx)
        .expect("valid HMAC-SHA256 signature must verify");
}

/// A signature that is otherwise valid but whose `t=<ts>` claim is far in the
/// past must be rejected. Without this gate, a captured signed body could be
/// replayed indefinitely against the endpoint. Stripe's official libraries
/// enforce a 300-second tolerance by default; we mirror that.
#[test]
fn webhook_verify_rejects_signature_with_stale_timestamp() {
    install_crypto_provider();
    let secret = "whsec_test_secret";
    let body = br#"{"id":"evt_test","type":"payment_intent.succeeded"}"#;
    // 24 hours in the past — well outside the 300s default window.
    let timestamp = (chrono::Utc::now().timestamp() - 24 * 60 * 60).to_string();
    let sig_hex = compute_stripe_v1(secret, &timestamp, body);

    let p = StripeProvider::new("sk_test", "pk_test", secret);
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "stripe-signature",
        format!("t={timestamp},v1={sig_hex}").parse().unwrap(),
    );
    let ctx = WebhookContext {
        body,
        headers: &headers,
        remote_addr: None,
    };
    let err = p
        .verify(&ctx)
        .expect_err("stale timestamp must reject even with valid HMAC");
    assert!(matches!(err, PaymentError::WebhookSignature(_)));
}

/// Symmetrically, a future-dated signature must also reject. Clock skew on a
/// caller's side and outright forged replays both manifest as out-of-window
/// timestamps in either direction.
#[test]
fn webhook_verify_rejects_signature_with_future_timestamp() {
    install_crypto_provider();
    let secret = "whsec_test_secret";
    let body = br#"{"id":"evt_test","type":"payment_intent.succeeded"}"#;
    let timestamp = (chrono::Utc::now().timestamp() + 24 * 60 * 60).to_string();
    let sig_hex = compute_stripe_v1(secret, &timestamp, body);

    let p = StripeProvider::new("sk_test", "pk_test", secret);
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "stripe-signature",
        format!("t={timestamp},v1={sig_hex}").parse().unwrap(),
    );
    let ctx = WebhookContext {
        body,
        headers: &headers,
        remote_addr: None,
    };
    let err = p
        .verify(&ctx)
        .expect_err("future-dated timestamp must reject");
    assert!(matches!(err, PaymentError::WebhookSignature(_)));
}

/// The tolerance setter must take effect: with a 60-second window and a
/// timestamp 5 minutes in the past, verify rejects; with a 1-hour window and
/// the same timestamp it passes. Locks in the override path and the
/// configurable knob the contract advertises.
#[test]
fn webhook_verify_respects_configured_tolerance() {
    install_crypto_provider();
    let secret = "whsec_test_secret";
    let body = br#"{"id":"evt_test","type":"payment_intent.succeeded"}"#;
    let timestamp = (chrono::Utc::now().timestamp() - 5 * 60).to_string();
    let sig_hex = compute_stripe_v1(secret, &timestamp, body);

    let mut headers = http::HeaderMap::new();
    headers.insert(
        "stripe-signature",
        format!("t={timestamp},v1={sig_hex}").parse().unwrap(),
    );
    let ctx = WebhookContext {
        body,
        headers: &headers,
        remote_addr: None,
    };

    let tight = StripeProvider::new("sk_test", "pk_test", secret).with_signature_tolerance(60);
    let err = tight
        .verify(&ctx)
        .expect_err("tight tolerance must reject 5-minute-old timestamp");
    assert!(matches!(err, PaymentError::WebhookSignature(_)));

    let lax = StripeProvider::new("sk_test", "pk_test", secret).with_signature_tolerance(3600);
    lax.verify(&ctx)
        .expect("lax tolerance must accept 5-minute-old timestamp");
}

/// A non-numeric `t=` value is a malformed header and must surface as a
/// WebhookSignature error, not silently skip the tolerance check.
#[test]
fn webhook_verify_rejects_non_numeric_timestamp() {
    install_crypto_provider();
    let p = StripeProvider::new("sk_test", "pk_test", "whsec_test_secret");
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "stripe-signature",
        "t=notatimestamp,v1=deadbeef".parse().unwrap(),
    );
    let ctx = WebhookContext {
        body: b"{}",
        headers: &headers,
        remote_addr: None,
    };
    let err = p
        .verify(&ctx)
        .expect_err("non-numeric timestamp must reject");
    assert!(matches!(err, PaymentError::WebhookSignature(_)));
}
