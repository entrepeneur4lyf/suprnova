use serde_json::json;
use suprnova::payments::*;

/// Trait-soundness proof — every adapter (Stripe, Paddle, future) MUST pass this flow:
/// create customer -> start session -> subscribe -> simulated webhook -> get -> cancel (both modes)
#[tokio::test]
async fn discriminator_subscribe_webhook_mirror_read_cancel() {
    let provider = MockPaymentProvider::new();

    // 1. Create customer
    let cus = provider
        .create_customer(CreateCustomerRequest {
            user_id: "user_42".into(),
            email: "alice@example.com".into(),
            name: Some("Alice".into()),
            metadata: None,
        })
        .await
        .unwrap();
    assert!(!cus.provider_customer_id.is_empty());
    assert_eq!(cus.email, "alice@example.com");

    // 2. Start a checkout session
    let session = provider
        .start_session(StartSessionRequest {
            mode: SessionMode::Subscription,
            customer_ref: cus.provider_customer_id.clone(),
            price_refs: vec!["price_pro_monthly".into()],
            success_return_url: "https://app.example/billing/success".into(),
            cancel_return_url: "https://app.example/billing/cancel".into(),
            amount_hint: None,
            idempotency_key: Some("idem_1".into()),
            metadata: None,
        })
        .await
        .unwrap();
    assert!(
        matches!(session, SessionPayload::Redirect { .. }),
        "mock must return SessionPayload::Redirect"
    );

    // 3. Create subscription (simulating successful checkout completion)
    let sub = provider
        .subscribe(SubscribeRequest {
            customer_ref: cus.provider_customer_id.clone(),
            price_refs: vec!["price_pro_monthly".into()],
            trial_days: None,
            idempotency_key: Some("idem_2".into()),
            metadata: None,
        })
        .await
        .unwrap();
    assert_eq!(sub.status, SubscriptionStatus::Active);
    assert_eq!(sub.items.len(), 1);
    assert_eq!(sub.items[0].provider_price_id, "price_pro_monthly");
    assert!(!sub.provider_subscription_id.is_empty());

    // 4. Simulated webhook arrives for subscription.created
    let webhook_body = json!({
        "id": "evt_subscription_created_1",
        "type": "subscription.created",
        "data": { "subscription_id": &sub.provider_subscription_id }
    })
    .to_string();
    let event = provider.parse_event(webhook_body.as_bytes()).unwrap();
    assert_eq!(event.neutral, Some(NeutralEventKind::SubscriptionCreated));
    assert_eq!(event.provider_event_id, "evt_subscription_created_1");
    assert_eq!(event.provider, "mock");

    // 5. Domain code reads subscription state
    let fetched = provider
        .get(&sub.provider_subscription_id)
        .await
        .unwrap();
    assert_eq!(
        fetched.provider_subscription_id,
        sub.provider_subscription_id
    );
    assert_eq!(fetched.status, SubscriptionStatus::Active);
    assert!(!fetched.cancel_at_period_end);

    // 6. Cancel at period end — status stays Active, flag is set
    let canceled = provider
        .cancel(&sub.provider_subscription_id, true)
        .await
        .unwrap();
    assert!(canceled.cancel_at_period_end);
    assert_eq!(canceled.status, SubscriptionStatus::Active);

    // 7. Cancel immediately — status transitions to Canceled
    let canceled_now = provider
        .cancel(&sub.provider_subscription_id, false)
        .await
        .unwrap();
    assert_eq!(canceled_now.status, SubscriptionStatus::Canceled);

    // 8. Verify as_payment() returns None — MockPaymentProvider deliberately omits Payment
    let provider_ref: &dyn PaymentProvider = &provider;
    assert!(
        provider_ref.as_payment().is_none(),
        "MockPaymentProvider must not implement Payment (Paddle-style optional)"
    );
}
