//! Integration + always-on invariant tests for the Paddle adapter.
//!
//! The env-gated test exercises Paddle's sandbox API; the four always-on tests
//! validate adapter invariants without network access.

use suprnova::payments::*;
use suprnova_payments_paddle::{PaddleEnvironment, PaddleProvider};

fn provider() -> Option<PaddleProvider> {
    if std::env::var("PADDLE_API_KEY").is_err() {
        return None;
    }
    PaddleProvider::from_env().ok()
}

#[tokio::test]
async fn integration_create_get_customer_paddle() {
    let Some(provider) = provider() else {
        eprintln!("skipped: PADDLE_API_KEY not set");
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
        .expect("create_customer");
    let fetched = provider
        .get_customer(&cus.provider_customer_id)
        .await
        .expect("get_customer");
    assert_eq!(fetched.provider_customer_id, cus.provider_customer_id);
}

#[test]
fn webhook_verify_rejects_bad_signature() {
    let p = PaddleProvider::new(
        "pdl_sdbx_apikey_test",
        "pdl_ntfset_test",
        "live_client_test",
        PaddleEnvironment::Sandbox,
    )
    .expect("provider construction");
    let mut headers = http::HeaderMap::new();
    headers.insert("paddle-signature", "ts=1234,h1=deadbeef".parse().unwrap());
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
    let p = PaddleProvider::new(
        "pdl_sdbx_apikey_test",
        "pdl_ntfset_test",
        "live_client_test",
        PaddleEnvironment::Sandbox,
    )
    .expect("provider construction");
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
fn paddle_does_not_implement_payment_trait() {
    let p = PaddleProvider::new(
        "pdl_sdbx_apikey_test",
        "pdl_ntfset_test",
        "live_client_test",
        PaddleEnvironment::Sandbox,
    )
    .expect("provider construction");
    assert!(
        p.as_payment().is_none(),
        "Paddle MUST NOT implement Payment (no server-capture surface)"
    );
}

#[tokio::test]
async fn paddle_subscribe_returns_not_supported() {
    let p = PaddleProvider::new(
        "pdl_sdbx_apikey_test",
        "pdl_ntfset_test",
        "live_client_test",
        PaddleEnvironment::Sandbox,
    )
    .expect("provider construction");
    let err = p
        .subscribe(SubscribeRequest {
            customer_ref: "ctm_test".into(),
            price_refs: vec!["pri_test".into()],
            trial_days: None,
            idempotency_key: None,
            metadata: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, PaymentError::NotSupported(_)));
}

#[tokio::test]
async fn paddle_delete_customer_returns_not_supported() {
    let p = PaddleProvider::new(
        "pdl_sdbx_apikey_test",
        "pdl_ntfset_test",
        "live_client_test",
        PaddleEnvironment::Sandbox,
    )
    .expect("provider construction");
    let err = p.delete_customer("ctm_test").await.unwrap_err();
    assert!(matches!(err, PaymentError::NotSupported(_)));
}
