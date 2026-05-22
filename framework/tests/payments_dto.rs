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

// Phase 12.1 — Mobile Money + status fidelity additions ---------------------

#[test]
fn phone_number_validates_and_normalizes_e164() {
    let p = PhoneNumber::new("260971234567").unwrap();
    assert_eq!(p.as_e164(), "+260971234567");
    assert_eq!(p.digits(), "260971234567");
    let p2 = PhoneNumber::new("+260971234567").unwrap();
    assert_eq!(p2.as_e164(), "+260971234567");
}

#[test]
fn phone_number_rejects_invalid_input() {
    assert!(matches!(PhoneNumber::new("123"), Err(PaymentError::InvalidPhoneNumber(_))));
    assert!(matches!(PhoneNumber::new("+abc1234567"), Err(PaymentError::InvalidPhoneNumber(_))));
    assert!(matches!(PhoneNumber::new(""), Err(PaymentError::InvalidPhoneNumber(_))));
    assert!(matches!(
        PhoneNumber::new("+12345678901234567890"),
        Err(PaymentError::InvalidPhoneNumber(_))
    ));
}

#[test]
fn phone_number_serde_roundtrip_as_transparent_string() {
    let p = PhoneNumber::new("+260971234567").unwrap();
    let j = serde_json::to_value(&p).unwrap();
    assert_eq!(j, serde_json::json!("+260971234567"));
    let back: PhoneNumber = serde_json::from_value(j).unwrap();
    assert_eq!(back, p);
}

#[test]
fn country_code_normalizes_to_uppercase() {
    let c = CountryCode::new("zm").unwrap();
    assert_eq!(c.as_str(), "ZM");
}

#[test]
fn country_code_rejects_invalid_input() {
    assert!(matches!(CountryCode::new("ZMB"), Err(PaymentError::InvalidCountryCode(_))));
    assert!(matches!(CountryCode::new("Z"), Err(PaymentError::InvalidCountryCode(_))));
    assert!(matches!(CountryCode::new("12"), Err(PaymentError::InvalidCountryCode(_))));
}

#[test]
fn country_code_serde_roundtrip_as_transparent_string() {
    let c = CountryCode::new("zm").unwrap();
    let j = serde_json::to_value(&c).unwrap();
    assert_eq!(j, serde_json::json!("ZM"));
    let back: CountryCode = serde_json::from_value(j).unwrap();
    assert_eq!(back, c);
}

#[test]
fn payment_method_mobile_money_round_trip() {
    let pm = PaymentMethod::MobileMoney {
        operator: MobileMoneyOperator::MtnMomo,
        phone: PhoneNumber::new("+260971234567").unwrap(),
        country: CountryCode::new("ZM").unwrap(),
    };
    let j = serde_json::to_value(&pm).unwrap();
    assert_eq!(j["type"], "mobile_money");
    assert_eq!(j["operator"]["kind"], "mtn_momo");
    assert_eq!(j["phone"], "+260971234567");
    assert_eq!(j["country"], "ZM");
    let back: PaymentMethod = serde_json::from_value(j).unwrap();
    assert_eq!(back, pm);
}

#[test]
fn mobile_money_operator_custom_variant_round_trip() {
    let op = MobileMoneyOperator::Custom { identifier: "tigopesa".into() };
    let j = serde_json::to_value(&op).unwrap();
    assert_eq!(j["kind"], "custom");
    assert_eq!(j["identifier"], "tigopesa");
    let back: MobileMoneyOperator = serde_json::from_value(j).unwrap();
    assert_eq!(back, op);
}

#[test]
fn payment_method_stablecoin_round_trip() {
    let pm = PaymentMethod::Stablecoin {
        asset: StablecoinAsset::Usdc,
        network: Some("ethereum".into()),
    };
    let j = serde_json::to_value(&pm).unwrap();
    assert_eq!(j["type"], "stablecoin");
    assert_eq!(j["asset"]["kind"], "usdc");
    assert_eq!(j["network"], "ethereum");
    let back: PaymentMethod = serde_json::from_value(j).unwrap();
    assert_eq!(back, pm);
}

#[test]
fn stablecoin_custom_variant_round_trip() {
    let a = StablecoinAsset::Custom { ticker: "PYUSD".into() };
    let j = serde_json::to_value(&a).unwrap();
    assert_eq!(j["kind"], "custom");
    assert_eq!(j["ticker"], "PYUSD");
    let back: StablecoinAsset = serde_json::from_value(j).unwrap();
    assert_eq!(back, a);
}

#[test]
fn session_payload_mobile_money_prompt_round_trip() {
    let p = SessionPayload::MobileMoneyPrompt {
        provider_transaction_id: "txn_lp_001".into(),
        message: "Check your phone for the MTN MoMo prompt".into(),
        operator: MobileMoneyOperator::MtnMomo,
    };
    let j = serde_json::to_value(&p).unwrap();
    assert_eq!(j["flow"], "mobile_money_prompt");
    assert_eq!(j["provider_transaction_id"], "txn_lp_001");
    assert_eq!(j["operator"]["kind"], "mtn_momo");
    let back: SessionPayload = serde_json::from_value(j).unwrap();
    match back {
        SessionPayload::MobileMoneyPrompt { provider_transaction_id, .. } => {
            assert_eq!(provider_transaction_id, "txn_lp_001");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn payment_status_expanded_variants_serialize_snake_case() {
    assert_eq!(serde_json::to_value(PaymentStatus::Created).unwrap(), serde_json::json!("created"));
    assert_eq!(serde_json::to_value(PaymentStatus::RequiresAction).unwrap(), serde_json::json!("requires_action"));
    assert_eq!(serde_json::to_value(PaymentStatus::Processing).unwrap(), serde_json::json!("processing"));
    assert_eq!(serde_json::to_value(PaymentStatus::Authorized).unwrap(), serde_json::json!("authorized"));
    assert_eq!(serde_json::to_value(PaymentStatus::Expired).unwrap(), serde_json::json!("expired"));
}
