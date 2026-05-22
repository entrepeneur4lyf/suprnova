use std::sync::Arc;
use suprnova::payments::*;
use app::payments::BillableUser;

#[tokio::test]
async fn billing_e2e_mock_discriminator() {
    let mock = Arc::new(MockPaymentProvider::new()) as Arc<dyn PaymentProvider>;

    let user = BillableUser {
        user_id: "user_42".into(),
        email: "alice@example.com".into(),
    };

    // 1. BillableUser ergonomic — creates customer + session in one call
    let session = user
        .start_subscription(
            mock.clone(),
            "price_pro_monthly",
            "https://app.example/billing/success",
            "https://app.example/billing/cancel",
        )
        .await
        .expect("start_subscription via mock should succeed");
    assert!(matches!(session, SessionPayload::Redirect { .. }));

    // 2. Domain-side: create a separate customer for the subscription leg of
    //    the test (the BillableUser-created one is now in the mock but we
    //    don't need to thread its id through — the test demonstrates the
    //    ergonomic, then exercises the subscription lifecycle independently).
    let domain_cus = mock
        .create_customer(CreateCustomerRequest {
            user_id: "user_42".into(),
            email: "alice@example.com".into(),
            name: None,
            metadata: None,
        })
        .await
        .expect("create_customer");

    let sub = mock
        .subscribe(SubscribeRequest {
            customer_ref: domain_cus.provider_customer_id.clone(),
            price_refs: vec!["price_pro_monthly".into()],
            trial_days: None,
            idempotency_key: None,
            metadata: None,
        })
        .await
        .expect("subscribe via mock should succeed");

    assert_eq!(sub.status, SubscriptionStatus::Active);
    assert_eq!(sub.items.len(), 1);
    assert_eq!(sub.items[0].provider_price_id, "price_pro_monthly");

    // 3. Cancel at period end: status stays Active, cancel_at_period_end flips.
    let canceled = mock
        .cancel(&sub.provider_subscription_id, true)
        .await
        .expect("cancel(at_period_end=true)");
    assert!(canceled.cancel_at_period_end);
    assert_eq!(canceled.status, SubscriptionStatus::Active);

    // 4. Cancel immediately: status flips to Canceled.
    let canceled_now = mock
        .cancel(&sub.provider_subscription_id, false)
        .await
        .expect("cancel(at_period_end=false)");
    assert_eq!(canceled_now.status, SubscriptionStatus::Canceled);
}
