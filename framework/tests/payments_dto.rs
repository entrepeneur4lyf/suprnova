use suprnova::payments::*;

#[test]
fn charge_result_completed_roundtrip() {
    let r = ChargeResult::Completed {
        provider_transaction_id: "txn_123".into(),
        amount: Money::from_minor_units(2500, Currency::USD),
        status: PaymentStatus::Succeeded,
        provider_metadata: serde_json::json!({"stripe_charge_id": "ch_123"}),
    };
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["kind"], "completed");
    let back: ChargeResult = serde_json::from_value(json).unwrap();
    match back {
        ChargeResult::Completed { provider_transaction_id, .. } => {
            assert_eq!(provider_transaction_id, "txn_123");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn session_payload_paddle_inline_roundtrip() {
    let p = SessionPayload::PaddleInline {
        transaction_id: "txn_abc".into(),
        customer_token: Some("ctok_xyz".into()),
        client_token: "live_xyz".into(),
    };
    let json = serde_json::to_value(&p).unwrap();
    assert_eq!(json["flow"], "paddle_inline");
    let back: SessionPayload = serde_json::from_value(json).unwrap();
    match back {
        SessionPayload::PaddleInline { transaction_id, .. } => {
            assert_eq!(transaction_id, "txn_abc");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn webhook_event_neutral_taxonomy_serializes_snake_case() {
    let e = WebhookEvent {
        provider: "stripe".into(),
        provider_event_id: "evt_123".into(),
        provider_event_type: "invoice.paid".into(),
        neutral: Some(NeutralEventKind::InvoicePaid),
        raw_payload: serde_json::json!({}),
    };
    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["neutral"], "invoice_paid");
}

#[test]
fn payment_method_card_round_trip() {
    let pm = PaymentMethod::Card {
        brand: "visa".into(),
        last4: "4242".into(),
        exp_month: 12,
        exp_year: 2030,
    };
    let json = serde_json::to_value(&pm).unwrap();
    assert_eq!(json["type"], "card");
    assert_eq!(json["last4"], "4242");
}

#[test]
fn subscription_status_serializes_snake_case() {
    assert_eq!(serde_json::to_value(SubscriptionStatus::PastDue).unwrap(), serde_json::json!("past_due"));
}
