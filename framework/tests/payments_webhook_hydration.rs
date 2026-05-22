//! Integration tests for webhook → mirror-table hydration.
//!
//! Each test:
//!   1. Spins up an in-memory SQLite with the payments migration applied.
//!   2. Registers a fresh `MockPaymentProvider` under a unique name and keeps
//!      a direct handle on it so the test can pre-populate provider state.
//!   3. Drives the route via a real TCP server + raw hyper client (same
//!      pattern as `payments_webhook_idempotency.rs`).
//!   4. Asserts the relevant mirror rows in `payments_subscriptions`,
//!      `payments_subscription_items`, `payments_transactions`, and
//!      `payments_customers`.
//!
//! Test isolation: every test uses a unique provider name; the mock keeps its
//! own in-process state via Arc<RwLock>, so parallel tests don't share data.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set, ActiveModelTrait};
use sea_orm_migration::MigratorTrait;
use serde_json::json;
use suprnova::handle_request;
use suprnova::payments::{
    entities::{customer, subscription, subscription_item, transaction, webhook_event},
    webhook_routes, MockPaymentProvider, PaymentProvider, PaymentProviderRegistry, SubscribeRequest,
    Subscription, UpdateSubscriptionRequest,
};
use suprnova::testing::TestDatabase;
use suprnova::{MiddlewareRegistry, Router};

// ── migrator ──────────────────────────────────────────────────────────────────

struct PaymentsTestMigrator;

#[async_trait::async_trait]
impl MigratorTrait for PaymentsTestMigrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        suprnova::payments::migrations::migrations()
    }
}

// ── test server helpers ───────────────────────────────────────────────────────

async fn spawn_server(router: Router, accepts: usize) -> SocketAddr {
    let router = Arc::new(router);
    let middleware = Arc::new(MiddlewareRegistry::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral listener");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        for _ in 0..accepts {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let io = TokioIo::new(stream);
            let router = router.clone();
            let middleware = middleware.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req: hyper::Request<Incoming>| {
                    let router = router.clone();
                    let middleware = middleware.clone();
                    async move {
                        Ok::<_, Infallible>(handle_request(router, middleware, req).await)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });

    addr
}

async fn send_webhook(
    addr: SocketAddr,
    path: &str,
    body: Bytes,
) -> (hyper::http::StatusCode, Bytes) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let content_len = body.len().to_string();
    let req = hyper::Request::builder()
        .method("POST")
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .header("Content-Length", content_len)
        .body(Full::new(body))
        .unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_webhook timeout")
        .expect("hyper send_request");
    let (parts, resp_body) = resp.into_parts();
    let collected = resp_body.collect().await.unwrap().to_bytes();
    (parts.status, collected)
}

/// Register a mock provider under `name` and return the concrete Arc so the
/// test can drive its internal state.
fn register_mock(name: &'static str) -> Arc<MockPaymentProvider> {
    let mock = Arc::new(MockPaymentProvider::new());
    let as_trait: Arc<dyn PaymentProvider> = mock.clone();
    PaymentProviderRegistry::bind(name, as_trait);
    mock
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Sanity: plain SeaORM update through model.into() + Set + update().
/// If this fails, the framework's underlying ActiveModel pattern is broken
/// and the rest of the hydration suite would be moot.
#[tokio::test]
async fn sanity_seaorm_update_via_model_into_active_model() {
    let db = TestDatabase::fresh::<PaymentsTestMigrator>()
        .await
        .expect("TestDatabase::fresh");
    let conn = db.conn().clone();
    let now = chrono::Utc::now().to_rfc3339();

    let inserted = customer::ActiveModel {
        provider: Set("mock".into()),
        provider_customer_id: Set("cus_sanity_1".into()),
        user_id: Set("user_x".into()),
        email: Set("first@example.com".into()),
        provider_metadata: Set(json!({})),
        created_at: Set(now.clone()),
        updated_at: Set(now.clone()),
        ..Default::default()
    }
    .insert(&conn)
    .await
    .expect("insert");

    assert_eq!(inserted.email, "first@example.com");
    let id = inserted.id;

    let mut am: customer::ActiveModel = inserted.into();
    am.email = Set("second@example.com".into());
    am.updated_at = Set(now);
    am.update(&conn).await.expect("update");

    let after = customer::Entity::find_by_id(id)
        .one(&conn)
        .await
        .expect("db ok")
        .expect("row");
    assert_eq!(after.email, "second@example.com");
}


/// A `subscription.created` webhook arriving for a sub that exists in the
/// provider should insert a row in `payments_subscriptions` + per-item rows
/// in `payments_subscription_items`, hydrated from `Subscription::get`.
#[tokio::test]
async fn subscription_created_webhook_hydrates_mirror_with_items() {
    let provider_name: &'static str = "mock-hydration-sub-created";
    let mock = register_mock(provider_name);

    let db = TestDatabase::fresh::<PaymentsTestMigrator>()
        .await
        .expect("TestDatabase::fresh");
    let conn = Arc::new(db.conn().clone());

    // Pre-populate the provider: this gives us a known sub_id we can post a
    // webhook for, and `Subscription::get` will return the canonical state.
    let sub = mock
        .subscribe(SubscribeRequest {
            customer_ref: "cus_test_alpha".into(),
            price_refs: vec!["price_basic".into(), "price_seats".into()],
            trial_days: None,
            idempotency_key: None,
            metadata: None,
        })
        .await
        .expect("mock subscribe");

    let sub_id = sub.provider_subscription_id.clone();
    let cust_id = sub.provider_customer_id.clone();

    let router = webhook_routes(conn.clone());
    let addr = spawn_server(router, 2).await;
    let path = format!("/webhooks/payments/{provider_name}");

    let body = Bytes::from(
        json!({
            "id": "evt_sub_created_1",
            "type": "subscription.created",
            "data": { "object": { "id": sub_id, "customer": cust_id } }
        })
        .to_string(),
    );
    let (status, resp) = send_webhook(addr, &path, body).await;
    assert_eq!(
        status.as_u16(),
        200,
        "expected 200, got body: {}",
        String::from_utf8_lossy(&resp)
    );

    // Audit row exists and is marked processed.
    let audit = webhook_event::Entity::find()
        .filter(webhook_event::Column::ProviderEventId.eq("evt_sub_created_1"))
        .one(&*conn)
        .await
        .expect("db ok")
        .expect("audit row present");
    assert_eq!(audit.neutral_event_kind.as_deref(), Some("subscription_created"));
    assert!(audit.processed_at.is_some(), "processed_at must be set");
    assert!(audit.process_error.is_none(), "process_error must be empty");

    // Mirror row hydrated.
    let mirror = subscription::Entity::find()
        .filter(subscription::Column::Provider.eq("mock"))
        .filter(subscription::Column::ProviderSubscriptionId.eq(sub_id.clone()))
        .one(&*conn)
        .await
        .expect("db ok")
        .expect("subscription mirror row");
    assert_eq!(mirror.provider_customer_id, cust_id);
    assert_eq!(mirror.status, "active");
    assert!(!mirror.cancel_at_period_end);
    assert!(mirror.canceled_at.is_none());

    // Subscription items hydrated — one per price_ref.
    let items = subscription_item::Entity::find()
        .filter(subscription_item::Column::SubscriptionId.eq(mirror.id))
        .all(&*conn)
        .await
        .expect("db ok");
    let mut item_prices: Vec<String> = items.iter().map(|i| i.provider_price_id.clone()).collect();
    item_prices.sort();
    assert_eq!(item_prices, vec!["price_basic", "price_seats"]);
    for item in &items {
        assert_eq!(item.quantity, 1);
    }
}

/// A `subscription.updated` webhook for an existing mirror row should update
/// the row's status / period / metadata, AND remove items that have been
/// dropped from the provider-side subscription.
#[tokio::test]
async fn subscription_updated_webhook_syncs_items_and_removes_stale() {
    let provider_name: &'static str = "mock-hydration-sub-updated";
    let mock = register_mock(provider_name);

    let db = TestDatabase::fresh::<PaymentsTestMigrator>()
        .await
        .expect("TestDatabase::fresh");
    let conn = Arc::new(db.conn().clone());

    // Start with two items.
    let sub = mock
        .subscribe(SubscribeRequest {
            customer_ref: "cus_test_beta".into(),
            price_refs: vec!["price_a".into(), "price_b".into()],
            trial_days: None,
            idempotency_key: None,
            metadata: None,
        })
        .await
        .expect("mock subscribe");
    let sub_id = sub.provider_subscription_id.clone();
    let cust_id = sub.provider_customer_id.clone();

    let router = webhook_routes(conn.clone());
    let addr = spawn_server(router, 4).await;
    let path = format!("/webhooks/payments/{provider_name}");

    // First webhook: subscription.created → 2 items in mirror.
    let body1 = Bytes::from(
        json!({
            "id": "evt_sub_updated_initial",
            "type": "subscription.created",
            "data": { "object": { "id": sub_id, "customer": cust_id } }
        })
        .to_string(),
    );
    let (s1, _) = send_webhook(addr, &path, body1).await;
    assert_eq!(s1.as_u16(), 200);

    // Mock-side change: drop "price_b" via the standard update API.
    mock.update(UpdateSubscriptionRequest {
        provider_subscription_id: sub_id.clone(),
        new_price_refs: Some(vec!["price_a".into()]),
        cancel_at_period_end: None,
        idempotency_key: None,
    })
    .await
    .expect("mock update");

    // Second webhook: subscription.updated → mirror should have 1 item.
    let body2 = Bytes::from(
        json!({
            "id": "evt_sub_updated_change",
            "type": "subscription.updated",
            "data": { "object": { "id": sub_id, "customer": cust_id } }
        })
        .to_string(),
    );
    let (s2, _) = send_webhook(addr, &path, body2).await;
    assert_eq!(s2.as_u16(), 200);

    let mirror = subscription::Entity::find()
        .filter(subscription::Column::ProviderSubscriptionId.eq(sub_id.clone()))
        .one(&*conn)
        .await
        .expect("db ok")
        .expect("mirror present");
    let items = subscription_item::Entity::find()
        .filter(subscription_item::Column::SubscriptionId.eq(mirror.id))
        .all(&*conn)
        .await
        .expect("db ok");
    assert_eq!(items.len(), 1, "stale item must be removed");
    assert_eq!(items[0].provider_price_id, "price_a");
}

/// A `subscription.canceled` webhook should set `canceled_at` AND `status`
/// on an existing mirror row.
#[tokio::test]
async fn subscription_canceled_webhook_sets_canceled_at_and_status() {
    let provider_name: &'static str = "mock-hydration-sub-canceled";
    let mock = register_mock(provider_name);

    let db = TestDatabase::fresh::<PaymentsTestMigrator>()
        .await
        .expect("TestDatabase::fresh");
    let conn = Arc::new(db.conn().clone());

    let sub = mock
        .subscribe(SubscribeRequest {
            customer_ref: "cus_test_gamma".into(),
            price_refs: vec!["price_x".into()],
            trial_days: None,
            idempotency_key: None,
            metadata: None,
        })
        .await
        .expect("mock subscribe");
    let sub_id = sub.provider_subscription_id.clone();
    let cust_id = sub.provider_customer_id.clone();

    let router = webhook_routes(conn.clone());
    let addr = spawn_server(router, 4).await;
    let path = format!("/webhooks/payments/{provider_name}");

    // Hydrate initial mirror.
    let body1 = Bytes::from(
        json!({
            "id": "evt_sub_canceled_initial",
            "type": "subscription.created",
            "data": { "object": { "id": sub_id, "customer": cust_id } }
        })
        .to_string(),
    );
    let (s1, _) = send_webhook(addr, &path, body1).await;
    assert_eq!(s1.as_u16(), 200);

    // Cancel inside the mock, then deliver subscription.canceled.
    mock.cancel(&sub_id, false).await.expect("mock cancel");
    let body2 = Bytes::from(
        json!({
            "id": "evt_sub_canceled_event",
            "type": "subscription.canceled",
            "data": { "object": { "id": sub_id, "customer": cust_id } }
        })
        .to_string(),
    );
    let (s2, _) = send_webhook(addr, &path, body2).await;
    assert_eq!(s2.as_u16(), 200);

    let mirror = subscription::Entity::find()
        .filter(subscription::Column::ProviderSubscriptionId.eq(sub_id.clone()))
        .one(&*conn)
        .await
        .expect("db ok")
        .expect("mirror present");
    assert_eq!(mirror.status, "canceled");
    assert!(
        mirror.canceled_at.is_some(),
        "canceled_at must be set when neutral=SubscriptionCanceled"
    );
}

/// A `payment.succeeded` webhook should insert a row in
/// `payments_transactions` with the amount, currency, status, and customer
/// extracted from the payload.
#[tokio::test]
async fn payment_succeeded_webhook_hydrates_transaction_mirror() {
    let provider_name: &'static str = "mock-hydration-payment-ok";
    let _mock = register_mock(provider_name);

    let db = TestDatabase::fresh::<PaymentsTestMigrator>()
        .await
        .expect("TestDatabase::fresh");
    let conn = Arc::new(db.conn().clone());

    let router = webhook_routes(conn.clone());
    let addr = spawn_server(router, 2).await;
    let path = format!("/webhooks/payments/{provider_name}");

    let body = Bytes::from(
        json!({
            "id": "evt_pay_ok_1",
            "type": "payment.succeeded",
            "data": {
                "object": {
                    "id": "txn_one_shot_42",
                    "customer": "cus_buyer_42",
                    "amount": 4999,
                    "currency": "USD",
                    "paid_at": "2026-05-22T12:00:00+00:00"
                }
            }
        })
        .to_string(),
    );
    let (status, resp) = send_webhook(addr, &path, body).await;
    assert_eq!(
        status.as_u16(),
        200,
        "expected 200, got body: {}",
        String::from_utf8_lossy(&resp)
    );

    let row = transaction::Entity::find()
        .filter(transaction::Column::ProviderTransactionId.eq("txn_one_shot_42"))
        .one(&*conn)
        .await
        .expect("db ok")
        .expect("transaction mirror row");
    assert_eq!(row.provider, "mock");
    assert_eq!(row.provider_customer_id, "cus_buyer_42");
    assert_eq!(row.amount_total_minor, 4999);
    assert_eq!(row.currency, "USD");
    assert_eq!(row.status, "succeeded");
    assert!(row.paid_at.is_some(), "paid_at must be parsed from payload");
    assert!(
        row.provider_subscription_id.is_none(),
        "one-off charge should not have a subscription link"
    );
}

/// A `payment.refunded` webhook for the SAME transaction id should UPDATE
/// the existing mirror row (status flips to `refunded`) — proves the upsert
/// path on the transaction table.
#[tokio::test]
async fn payment_refunded_webhook_updates_existing_transaction() {
    let provider_name: &'static str = "mock-hydration-payment-refund";
    let _mock = register_mock(provider_name);

    let db = TestDatabase::fresh::<PaymentsTestMigrator>()
        .await
        .expect("TestDatabase::fresh");
    let conn = Arc::new(db.conn().clone());

    let router = webhook_routes(conn.clone());
    let addr = spawn_server(router, 4).await;
    let path = format!("/webhooks/payments/{provider_name}");

    // First: payment.succeeded → row exists with status=succeeded.
    let body1 = Bytes::from(
        json!({
            "id": "evt_pay_refund_initial",
            "type": "payment.succeeded",
            "data": {
                "object": {
                    "id": "txn_refundable_1",
                    "customer": "cus_refund_1",
                    "amount": 2500,
                    "currency": "USD"
                }
            }
        })
        .to_string(),
    );
    let (s1, _) = send_webhook(addr, &path, body1).await;
    assert_eq!(s1.as_u16(), 200);

    // Then: payment.refunded for the same txn id.
    let body2 = Bytes::from(
        json!({
            "id": "evt_pay_refund_refund",
            "type": "payment.refunded",
            "data": {
                "object": {
                    "id": "txn_refundable_1",
                    "customer": "cus_refund_1",
                    "amount": 2500,
                    "currency": "USD"
                }
            }
        })
        .to_string(),
    );
    let (s2, _) = send_webhook(addr, &path, body2).await;
    assert_eq!(s2.as_u16(), 200);

    // Exactly one mirror row for that txn — and status flipped.
    let rows = transaction::Entity::find()
        .filter(transaction::Column::ProviderTransactionId.eq("txn_refundable_1"))
        .all(&*conn)
        .await
        .expect("db ok");
    assert_eq!(rows.len(), 1, "upsert must not duplicate");
    assert_eq!(rows[0].status, "refunded");
}

/// A `customer.updated` webhook should update an existing
/// `payments_customers` row's `email` and `provider_metadata`. It must NOT
/// insert a new row when no match exists (we don't synthesize `user_id`).
#[tokio::test]
async fn customer_updated_webhook_updates_existing_customer_row_only() {
    let provider_name: &'static str = "mock-hydration-customer";
    let _mock = register_mock(provider_name);

    let db = TestDatabase::fresh::<PaymentsTestMigrator>()
        .await
        .expect("TestDatabase::fresh");
    let conn = Arc::new(db.conn().clone());

    // Pre-seed a customer row (mimics what an app does after CustomerStore::create_customer).
    let now = chrono::Utc::now().to_rfc3339();
    let inserted = customer::ActiveModel {
        provider: Set("mock".into()),
        provider_customer_id: Set("cus_app_known_1".into()),
        user_id: Set("user_42".into()),
        email: Set("before@example.com".into()),
        provider_metadata: Set(json!({"seed": true})),
        created_at: Set(now.clone()),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&*conn)
    .await
    .expect("seed customer row");
    assert_eq!(inserted.email, "before@example.com");

    let router = webhook_routes(conn.clone());
    let addr = spawn_server(router, 4).await;
    let path = format!("/webhooks/payments/{provider_name}");

    // Known customer: email should update.
    let body_known = Bytes::from(
        json!({
            "id": "evt_cust_updated_known",
            "type": "customer.updated",
            "data": {
                "object": {
                    "id": "cus_app_known_1",
                    "email": "after@example.com"
                }
            }
        })
        .to_string(),
    );
    let (sk, rb) = send_webhook(addr, &path, body_known).await;
    assert_eq!(
        sk.as_u16(),
        200,
        "status: {} body: {}",
        sk.as_u16(),
        String::from_utf8_lossy(&rb)
    );
    assert_eq!(rb.as_ref(), b"ok", "expected 'ok', hydration may have failed");

    let audit = webhook_event::Entity::find()
        .filter(webhook_event::Column::ProviderEventId.eq("evt_cust_updated_known"))
        .one(&*conn)
        .await
        .expect("db ok")
        .expect("audit row");
    assert!(
        audit.process_error.is_none(),
        "process_error must be None, got: {:?}",
        audit.process_error
    );

    let updated = customer::Entity::find()
        .filter(customer::Column::ProviderCustomerId.eq("cus_app_known_1"))
        .one(&*conn)
        .await
        .expect("db ok")
        .expect("mirror present");
    assert_eq!(updated.email, "after@example.com");
    assert_eq!(updated.user_id, "user_42", "user_id must be preserved");

    // Unknown customer: must NOT insert a synthesized row (no user_id available).
    let body_unknown = Bytes::from(
        json!({
            "id": "evt_cust_updated_unknown",
            "type": "customer.updated",
            "data": {
                "object": {
                    "id": "cus_out_of_band_99",
                    "email": "stranger@example.com"
                }
            }
        })
        .to_string(),
    );
    let (su, _) = send_webhook(addr, &path, body_unknown).await;
    assert_eq!(su.as_u16(), 200);

    let stranger = customer::Entity::find()
        .filter(customer::Column::ProviderCustomerId.eq("cus_out_of_band_99"))
        .one(&*conn)
        .await
        .expect("db ok");
    assert!(
        stranger.is_none(),
        "out-of-band customer must not be synthesized into the mirror"
    );

    // Exactly one customer row total.
    let all = customer::Entity::find()
        .all(&*conn)
        .await
        .expect("db ok");
    assert_eq!(all.len(), 1);
}

/// A webhook whose `neutral` is `None` (unmapped event type) must still
/// produce an audit row and return 200 — hydration is a no-op.
#[tokio::test]
async fn unmapped_event_records_audit_row_only() {
    let provider_name: &'static str = "mock-hydration-unmapped";
    let _mock = register_mock(provider_name);

    let db = TestDatabase::fresh::<PaymentsTestMigrator>()
        .await
        .expect("TestDatabase::fresh");
    let conn = Arc::new(db.conn().clone());

    let router = webhook_routes(conn.clone());
    let addr = spawn_server(router, 2).await;
    let path = format!("/webhooks/payments/{provider_name}");

    let body = Bytes::from(
        json!({
            "id": "evt_oddball_1",
            "type": "totally.unknown.event",
            "data": { "object": { "id": "x", "customer": "y" } }
        })
        .to_string(),
    );
    let (status, _) = send_webhook(addr, &path, body).await;
    assert_eq!(status.as_u16(), 200);

    let audit = webhook_event::Entity::find()
        .filter(webhook_event::Column::ProviderEventId.eq("evt_oddball_1"))
        .one(&*conn)
        .await
        .expect("db ok")
        .expect("audit row present");
    assert_eq!(audit.provider_event_type, "totally.unknown.event");
    assert!(audit.neutral_event_kind.is_none());
    assert!(audit.processed_at.is_some());

    // No mirror rows.
    let subs = subscription::Entity::find()
        .all(&*conn)
        .await
        .expect("db ok");
    assert!(subs.is_empty());
    let txns = transaction::Entity::find()
        .all(&*conn)
        .await
        .expect("db ok");
    assert!(txns.is_empty());
}
