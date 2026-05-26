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

#[test]
fn webhook_verify_accepts_valid_signature() {
    install_crypto_provider();
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let secret = "whsec_test_secret";
    let timestamp = "1234567890";
    let body = br#"{"id":"evt_test","type":"payment_intent.succeeded"}"#;

    // Compute the expected signature exactly as Stripe does.
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    let sig_hex = hex::encode(mac.finalize().into_bytes());

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
